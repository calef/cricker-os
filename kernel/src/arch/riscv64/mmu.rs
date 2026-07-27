//! **RISC-V virtual memory.** The Sv39/Sv48 page-table half of the `arch` contract.
//!
//! This is a scaffold for the compile milestone. The address arithmetic and the device-memory
//! constants are real; everything that walks or edits a page table is a loud `unimplemented!()`,
//! because doing it right is the Sv39 step, which is also where HAL leak #2 (the `paging` crate
//! encodes the aarch64 descriptor format) gets resolved by factoring the format behind a trait. See
//! notes/riscv-port.md.
//!
//! Until then the kernel runs bare (`satp = 0`, virtual == physical), so [`KERNEL_VA_BASE`] is 0 and
//! [`phys_to_virt`]/[`virt_to_phys`] are the identity. The boot and console steps need nothing more.

use crate::memory;
use core::arch::asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};
use paging::{Flags, Half, MapError, Mapper, PAGE_SIZE, PageTable, Sv39};

/// This architecture's page-table format. Portable code names it as `arch::mmu::Format` (see the
/// aarch64 module's alias for why), so the user-VA gate and the user `Mapper` land on Sv39 here.
pub type Format = Sv39;

/// The UART, mapped as device memory in the direct map. Without it the machine goes silent the
/// instant we switch off the coarse boot table.
const UART_BASE: u64 = 0x1000_0000;
const UART_SIZE: u64 = 0x1000;

/// The `satp` MODE field value for Sv39 (bits 63:60).
const SATP_MODE_SV39: u64 = 8 << 60;

/// The kernel's fine-map root, saved by [`init`] so a secondary hart can adopt it.
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

/// The base of the kernel's virtual address space: the Sv39 high half (bits 63:38 all one, the sign
/// extension of bit 38 = 1). Chosen exactly like aarch64's base so `VA = PA | KERNEL_VA_BASE` is
/// exact and a kernel VA shares its physical address's page-table indices. Matched to
/// `KERNEL_VA_BASE` in link-riscv.ld, and the kernel runs here from `boot.s`'s high-half jump on.
pub const KERNEL_VA_BASE: u64 = 0xffff_ffc0_0000_0000;

/// The boot page table: a single Sv39 root that maps the low physical range (to survive turning
/// paging on) and its high-half alias (where the kernel is linked). Four gigapage (1 GiB) leaves are
/// enough to run and print: index 0 and 2 identity-map the UART region and the kernel/RAM region;
/// 256 and 258 are the same two at `KERNEL_VA_BASE` (adding 256 to the top-level index). It is RWX
/// everywhere, like aarch64's coarse boot map: it exists to survive ~twenty instructions until
/// `mmu::init` builds the real W^X tables. See boot.s and notes/riscv-port.md.
///
/// `boot.s` reads its **physical** address (PC-relative) to load `satp`, so it must be a real static
/// with a stable symbol. It is `.data` (initialized), loaded at its low physical address.
#[repr(C, align(4096))]
struct BootTable([u64; paging::ENTRIES]);

const fn boot_table() -> BootTable {
    // Sv39 gigapage leaf: V R W X A D set. RWX is deliberate and temporary (see above).
    const LEAF: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 6) | (1 << 7);
    // A 1 GiB-aligned physical base, as a gigapage PTE (PPN at bits 53:10).
    const fn giga(pa: u64) -> u64 {
        ((pa >> 12) << 10) | LEAF
    }
    let mut t = [0u64; paging::ENTRIES];
    t[0] = giga(0x0000_0000); // identity: 0..1 GiB, covers the UART at 0x1000_0000
    t[2] = giga(0x8000_0000); // identity: 2..3 GiB, covers the kernel/RAM at 0x8020_0000
    t[256] = t[0]; // high alias of index 0 (KERNEL_VA_BASE adds 256 to the top-level index)
    t[258] = t[2]; // high alias of index 2
    BootTable(t)
}

/// The boot table instance `boot.s` points `satp` at. `#[unsafe(no_mangle)]` so the assembly can
/// name it; `pub` and read from asm, so not actually dead despite appearances.
#[unsafe(no_mangle)]
static BOOT_PAGE_TABLE: BootTable = boot_table();

/// The physical address of the virtio-mmio transport window on QEMU's `virt` machine. The `virt`
/// board lays out 8 virtio-mmio slots of 0x1000 each starting at 0x1000_1000, growing downward by
/// slot; this base and the count below describe that window for the driver layer.
pub const VIRTIO_MMIO_BASE: u64 = 0x1000_1000;
/// The size of the virtio-mmio window (8 slots of 0x1000).
pub const VIRTIO_MMIO_SIZE: u64 = 8 * 0x1000;
/// The first PLIC interrupt id for the virtio-mmio slots on the `virt` machine (irq 1 is the first
/// virtio slot; the driver adds the slot index).
pub const VIRTIO_IRQ_BASE: u32 = 1;

/// Physical to kernel-virtual. Identity in bare mode; `pa + KERNEL_VA_BASE` once the high-half exists.
pub const fn phys_to_virt(pa: u64) -> u64 {
    pa + KERNEL_VA_BASE
}

/// Kernel-virtual to physical. The inverse of [`phys_to_virt`].
pub const fn virt_to_phys(va: u64) -> u64 {
    va - KERNEL_VA_BASE
}

/// A physical page-table address as a kernel pointer. Identity in bare mode; the direct map makes it
/// valid once the Sv39 step maps all of RAM into the high-half. Same role as the aarch64 helper.
pub(crate) fn phys_to_ptr(pa: u64) -> *mut PageTable {
    phys_to_virt(pa) as *mut PageTable
}

/// Build the kernel's fine-grained Sv39 tables and switch `satp` to them, replacing the coarse RWX
/// boot table (BOOT_PAGE_TABLE) that `boot.s` installed. The new tables are W^X: `.text` executable
/// and read-only, `.rodata` read-only, everything else non-executable, the guard page unmapped.
///
/// We are already running in the high half on the boot table; the fine table maps the same kernel
/// VAs to the same frames, so the `csrw satp` is seamless (the next instruction fetch resolves
/// identically). This is the RISC-V counterpart of the aarch64 `mmu::init`, one register instead of
/// the TTBR0/TTBR1 pair.
pub fn init() {
    let root = memory::alloc()
        .expect("no frame for the root page table")
        .addr();
    // SAFETY: a fresh frame; zero it before the hardware can ever walk it.
    unsafe {
        (*phys_to_ptr(root)).entries = [0; paging::ENTRIES];
    }

    // SAFETY: `root` is zeroed and page-aligned; `phys_to_ptr` is valid because the boot table's
    // direct-map gigapages cover all of RAM (so every frame the mapper allocates is addressable).
    let mut mapper = unsafe {
        Mapper::<_, _, Sv39>::new(
            root,
            Half::High,
            || memory::alloc().map(|f| f.addr()),
            phys_to_ptr,
        )
    };

    map_everything(&mut mapper).expect("failed to build the kernel page tables");
    verify(&mapper);

    // SAFETY: the fine map covers this function's code, its stack, and the UART; we checked.
    unsafe { install(root) };

    KERNEL_ROOT.store(root, Ordering::Relaxed);
}

/// Switch `satp` to the Sv39 tables rooted at physical `root`, and flush the TLB.
///
/// # Safety
/// `root` must be a complete Sv39 kernel map covering the currently-executing code, stack, and any
/// memory touched before the next `sfence`; otherwise the instruction after the `csrw` faults.
unsafe fn install(root: u64) {
    let satp = SATP_MODE_SV39 | (root >> 12);
    // SAFETY: caller's contract. sfence.vma before and after brackets the switch so no stale
    // boot-table entry survives.
    unsafe {
        asm!(
            "sfence.vma",
            "csrw satp, {satp}",
            "sfence.vma",
            satp = in(reg) satp,
            options(nostack),
        );
    }
}

/// Build every mapping the kernel needs: the direct map of RAM, the W^X kernel sections, the stack,
/// and the UART. Mirrors the aarch64 `map_everything`.
fn map_everything<A, P>(m: &mut Mapper<A, P, Sv39>) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    // 1. The direct map: all of RAM at `pa | KERNEL_VA_BASE`, read/write, never executable, so the
    //    kernel can touch any frame the allocator hands it. Skip the kernel image, whose sections
    //    get tighter permissions below (the mapper refuses to overwrite, turning an ordering mistake
    //    into an error rather than a silently-wrong permission).
    let image_lo = virt_to_phys(image_start());
    let image_hi = virt_to_phys(image_end());
    for (start, size) in memory::ram_regions() {
        let end = start + size;
        direct_map(m, start, image_lo.min(end), Flags::kernel_data())?;
        direct_map(m, image_hi.max(start), end, Flags::kernel_data())?;
    }

    // 2. The kernel image, section by section, at its linked VAs. W^X.
    map_range(m, text_start(), text_end(), Flags::kernel_code())?;
    map_range(m, rodata_start(), rodata_end(), Flags::kernel_rodata())?;
    map_range(m, data_start(), bss_end(), Flags::kernel_data())?;

    // 3. The guard page is deliberately NOT mapped (stack-overflow trap). Skip it.

    // 4. The stack.
    map_range(m, stack_bottom(), stack_top(), Flags::kernel_data())?;

    // 5. The UART, device memory, in the direct map. Silence otherwise, the instant we switch.
    direct_map(m, UART_BASE, UART_BASE + UART_SIZE, Flags::device())?;

    Ok(())
}

/// Map a range of *virtual* addresses to the physical ones they were linked against.
fn map_range<A, P>(
    m: &mut Mapper<A, P, Sv39>,
    va_start: u64,
    va_end: u64,
    flags: Flags,
) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    if va_end <= va_start {
        return Ok(());
    }
    let pages = (va_end - va_start).div_ceil(PAGE_SIZE);
    m.map_range(va_start, virt_to_phys(va_start), pages, flags)
}

/// Map a range of *physical* addresses into the direct map at `pa | KERNEL_VA_BASE`.
fn direct_map<A, P>(
    m: &mut Mapper<A, P, Sv39>,
    pa_start: u64,
    pa_end: u64,
    flags: Flags,
) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    if pa_end <= pa_start {
        return Ok(());
    }
    let pages = (pa_end - pa_start).div_ceil(PAGE_SIZE);
    m.map_range(phys_to_virt(pa_start), pa_start, pages, flags)
}

/// Walk the tables in software and check the things that would kill us, before the hardware bets the
/// machine on them. The RISC-V counterpart of the aarch64 `verify`.
fn verify<A, P>(m: &Mapper<A, P, Sv39>)
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    // The code we are executing right now must be mapped executable, or the instruction after the
    // `csrw satp` never gets fetched.
    let here = init as *const () as u64;
    let (pa, flags) = m
        .translate(here)
        .expect("the code switching tables is not mapped: we would die on the next fetch");
    assert_eq!(pa, virt_to_phys(here), "our .text maps to the wrong frame");
    assert!(flags.is_kernel_executable(), "our own .text is not executable");
    assert!(!flags.is_writable(), "our own .text is writable (W^X violated)");

    // The UART, so `println!` keeps working across the switch.
    assert!(
        m.translate(phys_to_virt(UART_BASE)).is_some(),
        "the UART is not mapped: the machine would go silent"
    );
}

macro_rules! linker_symbol {
    ($name:ident, $sym:ident) => {
        fn $name() -> u64 {
            unsafe extern "C" {
                static $sym: c_void;
            }
            (&raw const $sym) as u64
        }
    };
}

linker_symbol!(image_start, __image_start);
linker_symbol!(image_end, __image_end);
linker_symbol!(text_start, __text_start);
linker_symbol!(text_end, __text_end);
linker_symbol!(rodata_start, __rodata_start);
linker_symbol!(rodata_end, __rodata_end);
linker_symbol!(data_start, __data_start);
linker_symbol!(bss_end, __bss_end);
linker_symbol!(stack_bottom, __stack_bottom);
linker_symbol!(stack_top, __stack_top);

/// Replay the kernel mapping on a secondary hart. The SMP + MMU step.
pub fn init_secondary() {
    unimplemented!("riscv secondary MMU bring-up: the SMP step")
}

/// The `satp` value naming a user address space: mode (Sv39) | ASID | root PPN. The RISC-V analog of
/// aarch64's `ttbr0_value`; kept under the aarch64-flavoured name for now so portable `user.rs` does
/// not change (renaming the concept is a separate cleanup, noted in riscv-port.md).
pub fn ttbr0_value(root: u64, asid: u16) -> u64 {
    let _ = (root, asid);
    unimplemented!("riscv satp composition (Sv39 | asid | ppn): the MMU step")
}

/// Discharge TLB entries for an ASID (`sfence.vma x0, asid`). The MMU step.
pub fn flush_asid(asid: u16) {
    let _ = asid;
    unimplemented!("riscv sfence.vma by asid: the MMU step")
}

/// Install a user address space (write `satp`). The MMU step.
///
/// # Safety
/// `satp` must name a well-formed Sv39 root; the caller owns that invariant, as on aarch64.
pub unsafe fn activate_user(satp: u64) {
    let _ = satp;
    unimplemented!("riscv activate user satp: the MMU step")
}

/// Remove the user address space from this hart. The MMU step.
pub fn deactivate_user() {
    unimplemented!("riscv deactivate user satp: the MMU step")
}

/// Whether U-mode may read `va` in the installed address space. The MMU step.
pub fn user_can_read(va: u64) -> bool {
    let _ = va;
    unimplemented!("riscv user_can_read (SUM / page walk): the MMU step")
}

/// Whether U-mode may write `va` in the installed address space. The MMU step.
pub fn user_can_write(va: u64) -> bool {
    let _ = va;
    unimplemented!("riscv user_can_write (SUM / page walk): the MMU step")
}

/// The root (satp PPN) of the currently installed user address space. The MMU step.
pub fn current_user_root() -> u64 {
    unimplemented!("riscv current user satp root: the MMU step")
}

/// Map one user page at `va`, allocating the leaf and any tables from `alloc`. The MMU step.
pub fn map_current_user_page(
    va: u64,
    flags: Flags,
    alloc: impl FnMut() -> Option<u64>,
) -> Result<u64, MapError> {
    let _ = (va, flags, alloc);
    unimplemented!("riscv map user page (Sv39 walk + PTE): the MMU step")
}

/// Unmap one user page at `va` in the space rooted at `root`, returning the frame it named. MMU step.
pub fn unmap_user_at(root: u64, va: u64) -> Option<u64> {
    let _ = (root, va);
    unimplemented!("riscv unmap user page: the MMU step")
}

/// Translate `va` in the space rooted at `root`. The MMU step.
pub fn translate_at(root: u64, va: u64) -> Option<(u64, Flags)> {
    let _ = (root, va);
    unimplemented!("riscv translate in a given root: the MMU step")
}

/// Map one user page at `va` onto the existing frame `phys`. The MMU step.
pub fn map_current_user_frame(
    va: u64,
    phys: u64,
    flags: Flags,
    alloc: impl FnMut() -> Option<u64>,
) -> Result<(), MapError> {
    let _ = (va, phys, flags, alloc);
    unimplemented!("riscv map user frame (Sv39 walk + PTE): the MMU step")
}

/// A reserved, empty user root installed when no process is current. The MMU step.
pub fn reserved_root() -> u64 {
    unimplemented!("riscv reserved user root: the MMU step")
}

/// Install a user root (write `satp`) without the full activate bookkeeping. The MMU step.
pub fn switch_user_root(satp: u64) {
    let _ = satp;
    unimplemented!("riscv switch user satp: the MMU step")
}

/// Serializes edits to the kernel's live tables: two harts must not mutate them at once. Same role
/// (and lock rank) as the aarch64 module's `KERNEL_MMU`.
static KERNEL_MMU: crate::sync::IrqSafeMutex<()> =
    crate::sync::IrqSafeMutex::new(crate::sync::rank::KERNEL_MMU, ());

/// The kernel's live page tables, as a `Mapper` rooted at the saved fine-table root. Reads
/// `KERNEL_ROOT` rather than `satp` back because both harts share one kernel root; **call only while
/// holding [`KERNEL_MMU`].**
#[allow(clippy::type_complexity)]
fn kernel_mapper() -> Mapper<impl FnMut() -> Option<u64>, fn(u64) -> *mut PageTable, Sv39> {
    let root = KERNEL_ROOT.load(Ordering::Relaxed);
    // SAFETY: `root` is the fine kernel table built by `init`; the direct map makes `phys_to_ptr`
    // valid for every table frame.
    unsafe {
        Mapper::new(
            root,
            Half::High,
            || memory::alloc().map(|f| f.addr()),
            phys_to_ptr,
        )
    }
}

/// Map one page into the kernel's own (high-half) address space.
pub fn map_page(va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
    let _guard = KERNEL_MMU.lock(); // exclusive: two harts must not mutate the tables at once
    kernel_mapper().map(va, pa, flags)
}

/// Remove one page from the kernel's address space, invalidate the TLB, and return the physical
/// frame (the caller's to free; the mapper never owned it). The `TlbFlush` obligation is discharged
/// here with a real `sfence.vma`; dropping one un-discharged panics.
pub fn unmap_page(va: u64) -> Result<u64, MapError> {
    let _guard = KERNEL_MMU.lock(); // exclusive: see map_page
    let (pa, flush) = kernel_mapper().unmap(va)?;
    flush.flush(flush_tlb);
    Ok(pa)
}

/// Invalidate the TLB entry for one virtual address. This is what discharges a `paging::TlbFlush`;
/// the `paging` crate is pure logic and emits no instructions.
///
/// `sfence.vma rs1, rs2` with `rs1` = the address and `rs2` = `x0` (all ASIDs) invalidates that
/// page's translation. Unlike aarch64's `tlbi`, `sfence.vma` also orders the preceding page-table
/// write and completes locally, so no separate barrier is needed.
pub fn flush_tlb(va: u64) {
    // SAFETY: TLB maintenance is always sound; getting it wrong means a stale translation, which is
    // the memory-unsafety that matters here, not Rust unsafety.
    unsafe { asm!("sfence.vma {}, zero", in(reg) va, options(nostack)) };
}

/// Whether paging is on: `satp`'s MODE field is not Bare (0). True from `boot.s`'s Sv39 switch on.
pub fn is_enabled() -> bool {
    let satp: u64;
    // SAFETY: reads a CSR. No side effects.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack, preserves_flags)) };
    satp >> 60 != 0
}

/// Translate a kernel virtual address through the live kernel tables.
pub fn translate(va: u64) -> Option<(u64, Flags)> {
    let _guard = KERNEL_MMU.lock();
    kernel_mapper().translate(va)
}

/// Translate a user virtual address through the installed user tables. The MMU step.
pub fn translate_user(va: u64) -> Option<(u64, Flags)> {
    let _ = va;
    unimplemented!("riscv translate user va: the MMU step")
}

/// Print a human summary of the kernel's mapping (the boot tour). The MMU step.
pub fn print_summary() {
    unimplemented!("riscv mapping summary: the MMU step")
}
