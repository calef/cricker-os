//! **RISC-V virtual memory.** The Sv39 page-table half of the `arch` contract.
//!
//! The kernel lives in the Sv39 high half (`boot.s` does the higher-half transition on a coarse boot
//! table; [`init`] then builds the fine-grained W^X kernel tables and switches `satp` to them). The
//! page-table *format* (Sv39 descriptor bits, three levels) lives in `paging::Sv39` behind the
//! `PageFormat` trait (HAL leak #2, DECISIONS §17); this module is the RISC-V glue: `satp`
//! composition, the kernel and user mapping surface via `Mapper<_, _, Sv39>`, the single-`satp`
//! address-space model (`share_kernel_half`), and TLB maintenance (`sfence.vma`). See
//! notes/riscv-port.md.

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
/// `satp.ASID` sits at bits 59:44 on rv64 Sv39.
const SATP_ASID_SHIFT: u64 = 44;
/// `satp.PPN` is the low 44 bits.
const SATP_PPN_MASK: u64 = (1 << 44) - 1;

/// The kernel's fine-map root, saved by [`init`] so a secondary hart can adopt it.
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

/// Read `satp`.
fn read_satp() -> u64 {
    let satp: u64;
    // SAFETY: reads a CSR. No side effects.
    unsafe { asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack, preserves_flags)) };
    satp
}

/// Write `satp` and flush the TLB, installing a whole address space (kernel high + user low).
fn write_satp(satp: u64) {
    // SAFETY: the caller guarantees `satp` names a well-formed Sv39 root; sfence makes it take
    // effect and drops stale entries.
    unsafe { asm!("csrw satp, {}", "sfence.vma", in(reg) satp, options(nostack)) };
}

/// The physical address of the currently-installed root page table (`satp.PPN << 12`).
fn current_root_pa() -> u64 {
    (read_satp() & SATP_PPN_MASK) << 12
}

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
/// RISC-V's `virt` lays out 8 virtio-mmio slots 0x1000 apart (aarch64's are 32, 0x200 apart). The
/// probe (`virtio::find_block_device`) walks them.
pub const VIRTIO_SLOT_STRIDE: u64 = 0x1000;
pub const VIRTIO_SLOTS: u64 = 8;

/// The PCIe ECAM window on QEMU's riscv `virt` (the `pci@30000000` node): configuration space,
/// 4 KB per function, 1 MB per bus. We map (and therefore enumerate) **bus 0 only**: QEMU `virt`
/// is a flat root complex with every device on bus 0, and a 4 KB-page kernel map of all 256
/// buses would cost 64K PTEs for space that reads all-ones. Widening is one constant if a bridge
/// topology ever appears. The dtb fixture test cross-checks this base against the machine's own
/// `reg`, the same hardcode-with-a-witness pattern as the UART.
pub const PCI_ECAM_BASE: u64 = 0x3000_0000;
pub const PCI_ECAM_BUSES: u16 = 1;
pub const PCI_ECAM_MAPPED: u64 = PCI_ECAM_BUSES as u64 * 0x10_0000;

/// Where BARs get placed. QEMU `virt` reserves 0x4000_0000..0x8000_0000 as the 32-bit PCI memory
/// window, but with `-bios default` nobody has programmed a BAR before us (OpenSBI does no PCI),
/// so the kernel assigns them itself, bumping from this base. We map only a 2 MB slice: a virtio
/// function's register BAR is 16 KB, so this bounds the kernel's page-table spend while leaving
/// room for dozens of devices.
pub const PCI_BAR_BASE: u64 = 0x4000_0000;
pub const PCI_BAR_MAPPED: u64 = 0x20_0000;

/// The PLIC input for INTx line A on the `virt` board's root complex; B, C, D follow. A device's
/// line is `PCI_IRQ_BASE + ((dev + pin - 1) % 4)`, the standard swizzle (`pci::intx_irq`); the
/// dtb fixture test walks the machine's own `interrupt-map` and asserts the formula matches.
pub const PCI_IRQ_BASE: u32 = 32;

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

    // Probe after `install`, so the ASID tag is exercised against the real kernel root rather than
    // the coarse boot table. See `probe_asid_bits`: this validates the assumption `crates/asid`
    // is already built on, and it is the gate on ever removing the flush in `write_satp`.
    ASID_BITS.store(probe_asid_bits(), Ordering::Relaxed);
    // Read back through the accessor rather than the local, so the store/load path is exercised on
    // every boot and `asid_bits`'s "was it probed" assertion fires here if the ordering ever moves,
    // instead of at some later caller. Reported rather than only asserted in a test, because this is
    // a number whoever brings up a real board wants without running the suite: it says whether the
    // ASID allocator's numbers are distinguishable by this hardware at all. The test enforces >= 8.
    crate::println!("satp.ASID: {} bits implemented", asid_bits());
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

/// How many `satp.ASID` bits this hardware actually implements, discovered at boot.
///
/// `usize::MAX` until [`probe_asid_bits`] runs, so a read before the probe is loud rather than a
/// plausible zero.
static ASID_BITS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

/// The widest ASID the architecture allows in Sv39: `satp` bits 59:44.
const SATP_ASID_WIDTH: u32 = 16;

/// **Discover how many `satp.ASID` bits exist, because `crates/asid` assumes at least eight.**
///
/// `satp.ASID` is **WARL**: an implementation may hardwire any number of those bits to zero,
/// *including all of them*. That is not a hypothetical corner of the spec; it is the cheap option
/// for a small core, and the VisionFive 2's U74 has not been checked.
///
/// It matters because [`crates/asid`](asid) is built on an assumption it states out loud: 255 usable
/// numbers, "below even the smallest hardware ASID space (8-bit, 256)". That holds on aarch64, where
/// the architecture *mandates* at least 8 bits. RISC-V mandates none. On a machine with zero
/// implemented bits, every one of the 160 address spaces would carry ASID 0 in hardware and their
/// TLB entries would **alias** — one process reading another's memory, with nothing to signal it.
///
/// The reason that has not bitten us is an accident: `write_satp` follows every `csrw satp` with an
/// unconditional `sfence.vma`, throwing the whole TLB away on each switch, so no entry ever survives
/// long enough to alias. **The flush is currently load-bearing for correctness, not just slow.**
/// Whoever removes it (see notes/riscv-arch-tests.md) must gate that on this probe.
///
/// The probe writes ones into the ASID field of the *current* `satp`, leaving MODE and PPN alone, and
/// reads back which bits stuck. The address space is unchanged throughout — only the tag moves — so
/// the worst case is TLB misses that re-walk the same page table and find the same mappings.
fn probe_asid_bits() -> usize {
    let original = read_satp();
    let all_ones = original | (((1u64 << SATP_ASID_WIDTH) - 1) << SATP_ASID_SHIFT);
    // SAFETY: MODE and PPN are carried over from the live `satp`, so this installs the same root
    // page table under a different ASID tag and then restores it. Both writes are bracketed by
    // `sfence.vma` so no entry tagged with the probe value outlives the probe.
    let readback = unsafe {
        let got: u64;
        asm!(
            "csrw satp, {probe}",
            "sfence.vma",
            "csrr {got}, satp",
            "csrw satp, {orig}",
            "sfence.vma",
            probe = in(reg) all_ones,
            got = out(reg) got,
            orig = in(reg) original,
            options(nostack),
        );
        got
    };
    let implemented = (readback >> SATP_ASID_SHIFT) & ((1 << SATP_ASID_WIDTH) - 1);
    // WARL bits need not be contiguous in principle; count what is set rather than assuming a
    // low-bit mask, so a strange implementation is reported honestly instead of rounded.
    implemented.count_ones() as usize
}

/// How many `satp.ASID` bits this hardware implements. Panics if read before [`init`] probed.
pub fn asid_bits() -> usize {
    let n = ASID_BITS.load(core::sync::atomic::Ordering::Relaxed);
    assert_ne!(n, usize::MAX, "asid_bits() read before mmu::init probed it");
    n
}

/// Adopt the kernel's fine-grained Sv39 map on a secondary hart (SMP). `secondary_boot` brought this
/// hart up on the coarse `BOOT_PAGE_TABLE`, which reaches only the first few gigapages of the high
/// half, not the thread-stack area far above the direct map; switching `satp` to the shared
/// `KERNEL_ROOT` the primary built and verified gives this hart the same W^X map every other hart
/// runs on. All harts share the one kernel root, so there is nothing per-hart to build. The RISC-V
/// counterpart of the aarch64 `init_secondary`.
pub fn init_secondary() {
    let root = KERNEL_ROOT.load(Ordering::Relaxed);
    // SAFETY: the primary built this root and is running on it; it covers this code, this hart's
    // stack (mapped in the kernel image), and the UART, so the switch is seamless.
    unsafe { install(root) };
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

    // 6. The PLIC, device memory (milestone 20). Its base and size come from the device tree
    // (memory::init parsed it before this ran). Device-typed like the UART; the interrupt handler
    // and the boot demo reach it through the direct map. Absent on aarch64, so skip if unknown.
    if let Some((start, size)) = memory::plic_region() {
        direct_map(m, start, start + size, Flags::device())?;
    }

    // 7. The `sifive_test` finisher (0x10_0000), device memory: the MMIO word the test harness writes
    // to exit QEMU (arch::semihosting::exit). One page. Harmless to map in every build; only the test
    // build ever writes it. The boot tour halts with `wfi` and never touches it.
    direct_map(m, 0x10_0000, 0x10_1000, Flags::device())?;

    // 8. The virtio-mmio transport window (milestone 9 / parity C), device memory. The kernel probes
    // these slots for a block device (virtio::find_block_device) and owns the transport; the DMA
    // rings live in the driver's own region (notes/dma.md). Absent hardware here just reads as "no
    // device", so mapping it is harmless when no disk is attached.
    direct_map(
        m,
        VIRTIO_MMIO_BASE,
        VIRTIO_MMIO_BASE + VIRTIO_MMIO_SIZE,
        Flags::device(),
    )?;

    // 9. The PCIe windows (the PCIe transport): bus 0's ECAM config space, and the slice of the
    // 32-bit PCI memory window the kernel assigns BARs from. Device memory both. An absent device
    // reads all-ones in ECAM ("nobody home"), so mapping these is harmless without a PCI device.
    direct_map(
        m,
        PCI_ECAM_BASE,
        PCI_ECAM_BASE + PCI_ECAM_MAPPED,
        Flags::device(),
    )?;
    direct_map(
        m,
        PCI_BAR_BASE,
        PCI_BAR_BASE + PCI_BAR_MAPPED,
        Flags::device(),
    )?;

    Ok(())
}

/// The page-table format this architecture's IOMMU walks: the RISC-V IOMMU's single-stage
/// (`iosatp`) translation is Sv39, the same format the CPU uses. The portable DMA-domain seam
/// (kernel/src/iommu.rs, paging::domain) builds device domains through this alias; its registers
/// live in a BAR inside the PCI window mapped above, so no extra MMIO region is needed here.
pub type DmaFormat = Sv39;

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
    assert!(
        flags.is_kernel_executable(),
        "our own .text is not executable"
    );
    assert!(
        !flags.is_writable(),
        "our own .text is writable (W^X violated)"
    );

    // The UART, so `println!` keeps working across the switch.
    assert!(
        m.translate(phys_to_virt(UART_BASE)).is_some(),
        "the UART is not mapped: the machine would go silent"
    );

    // The guard page must NOT be mapped, or the stack-overflow protection is silently off and we
    // would only find out during an overflow, which is when it is no use. The aarch64 `verify` has
    // always checked this; riscv reached the same layout (link-riscv.ld reserves the page) without
    // ever asserting it.
    assert!(
        m.translate(stack_guard()).is_none(),
        "the guard page IS mapped: stack overflow protection is off"
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
linker_symbol!(stack_guard, __stack_guard);
linker_symbol!(stack_bottom, __stack_bottom);
linker_symbol!(stack_top, __stack_top);

/// Compose the `satp` value naming an address space: Sv39 mode, ASID, and the root PPN. The RISC-V
/// analog of aarch64's `ttbr0_value`, kept under that name so portable `user.rs` does not change.
/// **Returns a full `satp` value** (not a bare root), so `switch_user_root`/`activate_user` write it
/// directly; the `root` a process stores must itself contain the kernel high-half (see the note on
/// the single-`satp` model at `switch_user_root`).
pub fn ttbr0_value(root: u64, asid: u16) -> u64 {
    SATP_MODE_SV39 | ((asid as u64) << SATP_ASID_SHIFT) | (root >> 12)
}

/// Discharge every TLB entry tagged with `asid`: `sfence.vma x0, asid` (all addresses, one ASID).
pub fn flush_asid(asid: u16) {
    // SAFETY: TLB maintenance is always sound.
    unsafe { asm!("sfence.vma zero, {}", in(reg) asid as u64, options(nostack)) };
}

/// Install a user address space by writing its composed `satp`.
///
/// # Safety
/// `satp` must name a well-formed Sv39 root that includes the kernel high-half (else the next
/// instruction fetch faults); the caller owns that invariant, as on aarch64.
pub unsafe fn activate_user(satp: u64) {
    write_satp(satp);
}

/// Remove the user address space from this hart: fall back to the kernel-only reserved root.
pub fn deactivate_user() {
    switch_user_root(reserved_root());
}

/// Whether U-mode may read `va` in the installed address space. RISC-V has no address-translation
/// instruction like aarch64's `AT S1E0R`, so we walk the current tables and check the U bit.
///
/// No syscall in the ABI dereferences a user pointer, so this has no caller in the running kernel;
/// see the aarch64 twin for the full disposition. It is proved rather than merely allowed, by
/// `the_page_tables_say_u_mode_cannot_read_the_kernels_memory` (milestone 41, which is when this
/// ISA got the confused-deputy test aarch64 had had all along).
#[cfg_attr(not(test), allow(dead_code))]
pub fn user_can_read(va: u64) -> bool {
    translate_user(va).is_some_and(|(_, f)| f.is_user_accessible())
}

/// Whether U-mode may write `va`: user-accessible and writable. Same disposition, same test.
#[cfg_attr(not(test), allow(dead_code))]
pub fn user_can_write(va: u64) -> bool {
    translate_user(va).is_some_and(|(_, f)| f.is_user_accessible() && f.is_writable())
}

/// The physical root of the currently installed address space (`satp.PPN << 12`).
pub fn current_user_root() -> u64 {
    current_root_pa()
}

/// Map one user page at `va`, allocating the leaf and any intermediate tables from `alloc`, into the
/// currently installed address space. Returns the leaf's physical address (for revocation records).
pub fn map_current_user_page(
    va: u64,
    flags: Flags,
    mut alloc: impl FnMut() -> Option<u64>,
) -> Result<u64, MapError> {
    let leaf = alloc().ok_or(MapError::OutOfFrames)?;
    map_current_user_frame(va, leaf, flags, alloc)?;
    Ok(leaf)
}

/// Unmap one user page at `va` in the space rooted at `root`, invalidate the TLB, and return the
/// frame it named.
pub fn unmap_user_at(root: u64, va: u64) -> Option<u64> {
    // SAFETY: `root` is a live low-half-owning root; `unmap` allocates nothing; the direct map makes
    // `phys_to_ptr` valid.
    let mut mapper = unsafe { Mapper::<_, _, Sv39>::new(root, Half::Low, || None, phys_to_ptr) };
    let (pa, flush) = mapper.unmap(va).ok()?;
    flush.flush(flush_tlb);
    Some(pa)
}

/// Translate `va` in the space rooted at physical `root`.
pub fn translate_at(root: u64, va: u64) -> Option<(u64, Flags)> {
    // SAFETY: `root` is a page table; the direct map makes `phys_to_ptr` valid; no allocation.
    let mapper = unsafe { Mapper::<_, _, Sv39>::new(root, Half::Low, || None, phys_to_ptr) };
    mapper.translate(va)
}

/// Map one user page at `va` onto the already-owned physical frame `phys` in the current address
/// space, drawing any intermediate tables from `alloc`, then flush the TLB for `va` (RISC-V may
/// require an `sfence.vma` to make a freshly-valid leaf visible, unlike aarch64).
pub fn map_current_user_frame(
    va: u64,
    phys: u64,
    flags: Flags,
    alloc: impl FnMut() -> Option<u64>,
) -> Result<(), MapError> {
    let root = current_root_pa();
    // SAFETY: `root` is the live installed root; the direct map makes `phys_to_ptr` valid.
    let mut mapper = unsafe { Mapper::<_, _, Sv39>::new(root, Half::Low, alloc, phys_to_ptr) };
    mapper.map(va, phys, flags)?;
    flush_tlb(va);
    Ok(())
}

/// The root a thread with no user address space runs on. On RISC-V the whole address space is one
/// `satp`, so "the reserved user root" is simply the kernel root: its low half is empty, so any user
/// address faults, which is exactly right for a kernel thread. (On aarch64 this is a separate empty
/// `TTBR0` table; the single-`satp` model folds it into the kernel root.)
pub fn reserved_root() -> u64 {
    ttbr0_value(KERNEL_ROOT.load(Ordering::Relaxed), 0)
}

/// Install the address space rooted at physical `root` by writing `satp`, and flush the TLB.
///
/// **This is the RISC-V single-`satp` model.** aarch64 has separate `TTBR0` (user) and `TTBR1`
/// (kernel), so switching a process swaps only `TTBR0` and leaves the kernel mapped. RISC-V has one
/// `satp` for the whole address space, so a process's root table must *itself* contain the kernel's
/// high-half entries (shared at address-space creation), and switching threads rewrites the whole
/// `satp`. For a kernel thread, `root` is [`reserved_root`] (the kernel root), so this is a no-op
/// switch that still flushes.
pub fn switch_user_root(satp: u64) {
    // `satp` is already a composed value (from `ttbr0_value` or `reserved_root`); write it directly.
    write_satp(satp);
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
    // Local first. SAFETY: TLB maintenance is always sound; getting it wrong means a stale
    // translation, which is the memory-unsafety that matters here, not Rust unsafety.
    unsafe { asm!("sfence.vma {}, zero", in(reg) va, options(nostack)) };

    // Then the other online harts (SMP shootdown). The kernel root is shared, so a page mapped or
    // unmapped here must be sfence'd on every hart that might run a thread touching it, or a migrated
    // thread faults on a translation this hart already invalidated but the others still cache. RISC-V
    // has no hardware TLB broadcast, so we IPI via SBI RFENCE. Skipped entirely until a second hart is
    // online (single-hart boot maps a great many pages; there is no one to shoot down).
    let others = crate::smp::online_harts_mask() & !(1usize << crate::cpu::id());
    if others != 0 {
        super::sbi_remote_sfence_vma(others, va as usize, PAGE_SIZE as usize);
    }
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

/// Translate a user virtual address through the currently installed address space.
pub fn translate_user(va: u64) -> Option<(u64, Flags)> {
    translate_at(current_root_pa(), va)
}

/// Does `va` have a translation at all in the address space installed on this hart, in **either**
/// half?
///
/// This exists for the fault classifier, and it is the one question RISC-V's `scause` refuses to
/// answer: it says "load page fault" whether the leaf was absent or present-and-forbidden, so the
/// only way to tell a permission refusal from a missing mapping is to walk the tables the hardware
/// just walked. See `exceptions::user_fault` for the caveat that carries.
///
/// Both halves, because a user thread reaching for the *kernel's* memory names a high-half address,
/// and the process root carries the kernel half ([`share_kernel_half`]). [`translate_user`] alone
/// would say "not mapped" for it and turn the most interesting case into the wrong answer.
pub fn is_mapped_in_current_space(va: u64) -> bool {
    let root = current_root_pa();
    // SAFETY: `root` is the live installed root; the direct map makes `phys_to_ptr` valid; a
    // translate allocates nothing, so the `|| None` allocator is never called.
    let half = |h| unsafe { Mapper::<_, _, Sv39>::new(root, h, || None, phys_to_ptr) };
    half(Half::Low).translate(va).is_some() || half(Half::High).translate(va).is_some()
}

/// Populate a fresh process root's **high half** with the kernel's, so a single `satp` pointing at
/// it sees both the process's user pages (low half) and the whole kernel (high half).
///
/// This is the RISC-V single-`satp` requirement with no aarch64 counterpart: aarch64 keeps the
/// kernel in a separate `TTBR1` that every process shares implicitly, but RISC-V has one root per
/// address space, so every process root must carry copies of the kernel root's top-level entries.
/// The kernel high half is the top 256 entries (index 256..512, `KERNEL_VA_BASE`'s top-level index
/// and up); they point at shared kernel intermediate tables, so copying the entries shares the whole
/// kernel map. Called by `user::AddressSpace` right after it allocates a root.
pub fn share_kernel_half(root: u64) {
    let kernel_root = KERNEL_ROOT.load(Ordering::Relaxed);
    // SAFETY: both are page-aligned root tables reachable through the direct map. We copy only the
    // high-half entries; the low half stays zero for the process's own user mappings.
    unsafe {
        let dst = &mut (*phys_to_ptr(root)).entries;
        let src = &(*phys_to_ptr(kernel_root)).entries;
        dst[256..paging::ENTRIES].copy_from_slice(&src[256..paging::ENTRIES]);
    }
}

/// Print a human summary of the kernel's mapping (the boot tour). The MMU step.
///
/// The RISC-V boot tour is its only caller. A test build compiles the tour out, and so does the
/// `bench` boot mode, which diverges into `bench::run` before it.
#[cfg_attr(any(test, feature = "bench"), allow(dead_code))]
pub fn print_summary() {
    unimplemented!("riscv mapping summary: the MMU step")
}

#[cfg(test)]
mod tests {
    //! Tests for the Sv39 MMU: the live page tables, W^X, the guard page, and TLB invalidation.
    //!
    //! These are the RISC-V twins of the aarch64 module's, written against the same *properties*
    //! rather than the same mechanisms (DECISIONS §19). Where the two ISAs differ, the difference is
    //! stated where it matters: one `satp` instead of the TTBR0/TTBR1 pair, three levels instead of
    //! four, a single `X` bit whose privilege is decided by `U` instead of the PXN/UXN pair, and a
    //! software RSW bit standing in for a memory type base Sv39 has no encoding for.
    //!
    //! `translate` walks the live kernel root, so these inspect the tables the hardware is
    //! **actually walking**, not a copy of what we intended.

    /// **The hardware must implement at least 8 `satp.ASID` bits, because `crates/asid` assumes it.****
    ///
    /// Not a curiosity test. `asid::ASIDS` is 256 and its header justifies that as "below even the
    /// smallest hardware ASID space (8-bit, 256)" — true of aarch64, which mandates 8 bits, and
    /// **not guaranteed by RISC-V at all**, which permits zero. With zero implemented bits every
    /// address space carries ASID 0 in hardware and their TLB entries alias, which is one process
    /// reading another's memory.
    ///
    /// This passes on QEMU. It exists for the board: if a real core implements fewer than 8 bits
    /// this fails loudly at boot, rather than the ASID allocator quietly handing out numbers the
    /// hardware cannot tell apart. Today the unconditional `sfence.vma` in `write_satp` masks the
    /// whole problem by discarding the TLB on every switch — which is exactly why removing that
    /// flush has to be gated on this number.
    #[test_case]
    fn the_hardware_has_at_least_the_asid_bits_the_allocator_assumes() {
        let bits = super::asid_bits();
        assert!(
            bits <= super::SATP_ASID_WIDTH as usize,
            "satp.ASID reported {bits} implemented bits, wider than the architectural 16",
        );
        assert!(
            bits >= 8,
            "satp.ASID implements {bits} bits; asid::ASIDS is {} and needs 8. \
             Address spaces would alias in the TLB. The flush in write_satp is what is saving us.",
            asid::ASIDS,
        );
    }

    /// Paging is on, and we are alive to say so.
    ///
    /// Weaker than it looks on this ISA, and worth saying so: the kernel runs at `KERNEL_VA_BASE`,
    /// which does not exist unless Sv39 is on, so a machine that reached this line has paging. What
    /// the assertion adds is that [`is_enabled`](super::is_enabled) reads the right field: `satp`'s
    /// MODE is bits 63:60, and a helper that looked at the wrong bits would answer "paging is off"
    /// on a paging machine, which is the sort of quiet wrongness that only shows up in whatever
    /// decides to trust it.
    #[test_case]
    fn mmu_is_enabled() {
        assert!(crate::arch::mmu::is_enabled(), "satp.MODE reads as Bare");
    }

    /// The kernel is running in the Sv39 high half.
    ///
    /// The reason is the same as aarch64's, arrived at differently. There the kernel lives in
    /// `TTBR1`, which a process switch never touches. Here there is one `satp` per address space and
    /// every process root carries a **copy of the kernel's top-level entries** (`share_kernel_half`),
    /// so the kernel is reachable from every space. Either way the kernel must be in the half that
    /// is not the process's, or the first switch into userspace deletes the kernel.
    #[test_case]
    fn the_kernel_lives_in_the_high_half() {
        use crate::arch::mmu::KERNEL_VA_BASE;

        // Our own code.
        let pc = crate::kernel_main as *const () as u64;
        assert!(
            pc >= KERNEL_VA_BASE,
            "kernel .text is at {pc:#x}, not in the high half"
        );

        // Our stack.
        let sp = crate::arch::current_sp();
        assert!(
            sp >= KERNEL_VA_BASE,
            "the stack is at {sp:#x}, not in the high half"
        );

        // And a static.
        static IN_BSS: u64 = 0;
        let addr = (&raw const IN_BSS) as u64;
        assert!(
            addr >= KERNEL_VA_BASE,
            "a static is at {addr:#x}, not in the high half"
        );
    }

    /// **The low half is empty when no process is running**, and on RISC-V that is a stronger claim
    /// than on aarch64.
    ///
    /// aarch64 can point `TTBR0` at an empty reserved table, so "no user space installed" is a
    /// separate register. RISC-V has *one* `satp`: the kernel runs on the kernel root, and that root's
    /// own low half is what a low address resolves through. Nothing but discipline stops
    /// `map_everything` from leaving something down there, and if it did, a stray low pointer in the
    /// kernel would silently succeed instead of faulting, and a process would inherit the mapping
    /// through `share_kernel_half`'s copy path the moment the entry moved up a level.
    ///
    /// The addresses are all inside Sv39's low half (`va >> 38 == 0`). That is deliberate: the
    /// `Mapper` returns `None` for anything outside its half *before it walks anything*, so a test
    /// address above 2^38 would pass without a single page-table read and prove nothing at all.
    #[test_case]
    fn a_low_address_does_not_translate_when_no_process_is_running() {
        use crate::arch::mmu::translate_user;

        for va in [
            0x1000u64,
            0x8020_0000, // where OpenSBI loaded us: the identity map, if it survived
            0x0000_003f_ffff_f000, // the top page of the Sv39 low half
        ] {
            assert!(
                translate_user(va).is_none(),
                "{va:#x} translates through the live satp: the boot table's identity gigapages \
                 may still be live",
            );
        }
    }

    /// The direct map: every physical address is nameable at `pa + KERNEL_VA_BASE`.
    ///
    /// This is how the kernel touches a frame the allocator just handed it. Without it, a physical
    /// address the kernel cannot NAME is a physical address it cannot use.
    #[test_case]
    fn the_direct_map_reaches_physical_memory() {
        use crate::arch::mmu::{phys_to_virt, virt_to_phys};

        let frame = crate::memory::alloc().expect("out of memory");
        let va = phys_to_virt(frame.addr());

        assert_eq!(
            virt_to_phys(va),
            frame.addr(),
            "the transform is not reversible"
        );

        let (pa, flags) = crate::arch::mmu::translate(va).expect("frame is NOT in the direct map");
        assert_eq!(pa, frame.addr());
        assert!(flags.is_writable());

        // And it is real memory: write through the virtual name, read it back.
        // SAFETY: the allocator just gave us this frame exclusively.
        unsafe {
            core::ptr::write_volatile(va as *mut u64, 0xfeed_face_cafe_f00d);
            assert_eq!(
                core::ptr::read_volatile(va as *const u64),
                0xfeed_face_cafe_f00d
            );
        }

        crate::memory::free(frame);
    }

    /// **The guard page must not be mapped.** That is its entire job.
    ///
    /// link-riscv.ld has reserved the page since the port landed, and `map_everything` skips it by
    /// mapping `.data..bss_end` and `stack_bottom..stack_top` as two ranges with a hole between
    /// them. Nothing asserted the hole was where it was supposed to be, so a linker-script edit that
    /// moved `__stack_guard` would have left the stack still mapped and the protection silently
    /// gone. `verify` now checks it at boot as well, as aarch64's always has.
    #[test_case]
    fn the_guard_page_is_a_hole() {
        use crate::arch::mmu;
        assert_eq!(
            mmu::translate(mmu::stack_guard()),
            None,
            "the guard page IS mapped: a stack overflow would silently eat .bss"
        );

        // And the pages either side of it must be mapped, or the hole is in the wrong place and is
        // protecting nothing.
        assert!(
            mmu::translate(mmu::stack_guard() - 4096).is_some(),
            "below the guard"
        );
        assert!(
            mmu::translate(mmu::stack_bottom()).is_some(),
            "the stack itself"
        );
    }

    /// W^X, checked against the tables the hardware is actually walking.
    ///
    /// **The `!is_user_executable` line is weaker here than on aarch64, and deliberately kept
    /// anyway.** aarch64 has two independent execute-never bits (PXN and UXN), so asserting both is
    /// two claims. Sv39 has one `X` bit whose privilege is decided by `U`, and `Sv39::leaf_flags`
    /// reports kernel-exec or user-exec accordingly, so given kernel-exec the user-exec assertion
    /// cannot fail. It stays because the *property* is what the kernel cares about and the format
    /// under it may change (Svpbmt, or a future format with separate bits); it is not carrying
    /// weight today.
    #[test_case]
    fn kernel_text_is_executable_and_not_writable() {
        use crate::arch::mmu;

        let (pa, flags) = mmu::translate(mmu::text_start()).expect(".text is not mapped");
        assert_eq!(
            pa,
            mmu::virt_to_phys(mmu::text_start()),
            ".text maps to the wrong frame"
        );

        assert!(flags.is_kernel_executable(), ".text is not executable");
        assert!(!flags.is_writable(), ".text is WRITABLE: W^X is broken");
        assert!(!flags.is_user_executable(), ".text is executable by U-mode");
    }

    /// Constants are read-only, and not executable by anyone.
    #[test_case]
    fn kernel_rodata_is_read_only_and_not_executable() {
        use crate::arch::mmu;

        let (_, flags) = mmu::translate(mmu::rodata_start()).expect(".rodata is not mapped");
        assert!(!flags.is_writable(), ".rodata is writable");
        assert!(!flags.is_kernel_executable(), ".rodata is executable");
    }

    /// The stack is writable and NOT executable.
    #[test_case]
    fn the_stack_is_writable_and_not_executable() {
        use crate::arch::mmu;

        let (_, flags) = mmu::translate(mmu::stack_bottom()).expect("stack is not mapped");
        assert!(flags.is_writable());
        assert!(
            !flags.is_kernel_executable(),
            "the stack is EXECUTABLE: data on the stack could be run as code"
        );
    }

    /// The UART is device-typed.
    ///
    /// **And the honest caveat, because this is the one place the two ISAs are not the same claim.**
    /// On aarch64 the device type is an architectural PTE field, and getting it wrong lets the CPU
    /// cache, reorder, merge and *speculatively read* MMIO; a speculative read of a UART FIFO
    /// register consumes the byte. Base Sv39 has no such field (that is the Svpbmt extension), so
    /// `paging::Sv39` carries the flag in an RSW software bit and QEMU's `virt` derives the real
    /// memory type from the physical address instead. So this asserts the kernel's *bookkeeping* is
    /// right, not that the hardware was told. It still fails for the mistake worth catching (mapping
    /// the UART with `Flags::kernel_data()`), and on a board with Svpbmt the same flag is what would
    /// drive the architectural bits. See notes/page-tables.md and crates/paging/src/sv39.rs.
    #[test_case]
    fn the_uart_is_mapped_as_device_memory() {
        use crate::arch::mmu;

        // The UART lives in the direct map, like every other physical address the kernel names: its
        // raw physical address stopped existing when the boot table's identity gigapages went away.
        let (_, flags) =
            mmu::translate(mmu::phys_to_virt(super::UART_BASE)).expect("the UART is not mapped");

        assert!(flags.is_device(), "the UART is not device memory");
        assert!(flags.is_writable(), "we do need to write to it");
        assert!(!flags.is_kernel_executable());
    }

    /// A frame from the allocator is still real, writable memory *through the MMU*.
    ///
    /// Proves the direct map covers everything the allocator can hand out. With paging on, a
    /// physical address the kernel cannot name is a physical address it cannot use, and the riscv
    /// direct map is built from `memory::ram_regions()` with the kernel image punched out of it,
    /// which is exactly the kind of arithmetic that can leave a hole nobody notices.
    #[test_case]
    fn an_allocated_frame_is_reachable_through_the_mmu() {
        use crate::arch::mmu;

        let frame = crate::memory::alloc().expect("out of memory");
        let va = mmu::phys_to_virt(frame.addr());
        let (pa, flags) = mmu::translate(va).expect("allocated frame is NOT MAPPED");

        assert_eq!(pa, frame.addr());
        assert!(flags.is_writable());
        assert!(!flags.is_kernel_executable(), "RAM is executable");

        crate::memory::free(frame);
    }

    /// **Prove the TLB is actually invalidated on unmap.**
    ///
    /// The landmine, and it is the same landmine on both ISAs even though the instruction differs
    /// (`sfence.vma` here, `tlbi` there). Change a mapping without discharging the flush and the CPU
    /// keeps using the *cached* translation: memory reads back as the previous owner's data. It is a
    /// security hole and it is close to undebuggable, because the page tables **in memory are
    /// correct**; the lie lives in a CPU structure you cannot inspect.
    ///
    /// So we make it observable:
    ///
    ///   1. map a spare VA to frame A, which holds 0xAAAA...
    ///   2. **read it**, which is what populates the TLB
    ///   3. unmap, and invalidate
    ///   4. map the *same VA* to frame B, which holds 0xBBBB...
    ///   5. read it again
    ///
    /// If step 5 returns 0xAAAA, the TLB is stale and we have exactly the bug. It must return
    /// 0xBBBB.
    #[test_case]
    fn unmap_invalidates_the_tlb() {
        use crate::arch::mmu::{self, phys_to_virt};
        use paging::Flags;

        const PATTERN_A: u64 = 0xaaaa_aaaa_aaaa_aaaa;
        const PATTERN_B: u64 = 0xbbbb_bbbb_bbbb_bbbb;

        // A high-half address well clear of everything the kernel maps: physical 4 GiB is not RAM on
        // QEMU's `virt` (RAM starts at 0x8000_0000 and the runner asks for far less than 2 GiB), and
        // it is above every device window. Sv39's high half is 256 GiB, so it is a nameable address.
        let test_va = mmu::phys_to_virt(0x1_0000_0000);
        assert_eq!(
            mmu::translate(test_va),
            None,
            "test address is already in use"
        );

        let a = crate::memory::alloc().expect("out of memory");
        let b = crate::memory::alloc().expect("out of memory");

        // SAFETY: two frames the allocator just gave us exclusively, reached via the direct map.
        unsafe {
            core::ptr::write_volatile(phys_to_virt(a.addr()) as *mut u64, PATTERN_A);
            core::ptr::write_volatile(phys_to_virt(b.addr()) as *mut u64, PATTERN_B);
        }

        mmu::map_page(test_va, a.addr(), Flags::kernel_data()).expect("map A");

        // SAFETY: just mapped, writable.
        let seen = unsafe { core::ptr::read_volatile(test_va as *const u64) };
        assert_eq!(seen, PATTERN_A, "the mapping didn't take");
        // ^ that read is the point: it pulls the translation into the TLB.

        let returned = mmu::unmap_page(test_va).expect("unmap");
        assert_eq!(returned, a.addr(), "unmap returned the wrong frame");

        mmu::map_page(test_va, b.addr(), Flags::kernel_data()).expect("map B");

        // SAFETY: mapped again, to a different frame.
        let seen = unsafe { core::ptr::read_volatile(test_va as *const u64) };

        assert_eq!(
            seen, PATTERN_B,
            "STALE TLB: the same virtual address still reads the OLD frame's data. \
             This is the bug that reads back another process's memory."
        );

        mmu::unmap_page(test_va).expect("cleanup");
        crate::memory::free(a);
        crate::memory::free(b);
    }

    /// Changing a mapping is forced through break-before-make.
    ///
    /// aarch64 needs this because a valid -> valid change can raise a TLB conflict abort. RISC-V is
    /// more forgiving about the hardware consequence, but the API is the same on both because the
    /// *software* hazard is the same: overwrite a leaf in place and the old frame is leaked with no
    /// record that it ever existed. The mapper makes it unrepresentable rather than merely unwise.
    #[test_case]
    fn the_kernel_mapper_refuses_to_overwrite() {
        use crate::arch::mmu;
        use paging::{Flags, MapError};

        let va = mmu::phys_to_virt(0x1_0100_0000);
        let f = crate::memory::alloc().unwrap();

        mmu::map_page(va, f.addr(), Flags::kernel_data()).unwrap();

        assert_eq!(
            mmu::map_page(va, f.addr(), Flags::kernel_data()),
            Err(MapError::AlreadyMapped)
        );

        mmu::unmap_page(va).unwrap();
        crate::memory::free(f);
    }

    /// **`satp` carries the address space's ASID, in the field the hardware reads.**
    ///
    /// This is as much of aarch64's `asid_tagging_keeps_address_spaces_apart_without_flushes` as is
    /// true on RISC-V today, and the gap is worth naming rather than papering over.
    ///
    /// That test proves two things: distinct spaces get distinct tags, *and* switching between them
    /// flushes nothing, so their TLB entries coexist. The second half cannot be proved here, because
    /// `write_satp` follows every `csrw satp` with a bare `sfence.vma`, which discards the whole TLB
    /// on every user switch. So the ASID is written and then immediately made irrelevant: an
    /// isolation test would pass with the tagging removed entirely, which makes it a test that
    /// cannot fail for its stated reason, and we do not ship those.
    ///
    /// What is left is real and untested until now: `ttbr0_value` must place the ASID at bits 59:44,
    /// where `satp` keeps it. It sits directly above the PPN, so a wrong shift either corrupts the
    /// root pointer (loud) or drops the tag on the floor (silent, and about to matter). Dropping the
    /// unconditional `sfence` is a change to the switching model, not a test fix; see
    /// notes/riscv-arch-tests.md.
    #[test_case]
    fn the_satp_carries_the_address_spaces_asid() {
        use crate::user::AddressSpace;

        let a = AddressSpace::new(2).expect("no space A");
        let b = AddressSpace::new(2).expect("no space B");

        // satp.ASID is bits 59:44 (16 bits) on rv64 Sv39.
        let asid_of = |satp: u64| (satp >> 44) & 0xffff;
        let (asid_a, asid_b) = (asid_of(a.ttbr0()), asid_of(b.ttbr0()));

        assert_ne!(asid_a, asid_b, "two live spaces share an ASID");
        assert_ne!(asid_a, 0, "a user space got the kernel's ASID 0");
        assert_ne!(asid_b, 0, "a user space got the kernel's ASID 0");

        // And the tag did not eat the rest of the word. `satp` packs MODE (63:60), ASID (59:44) and
        // PPN (43:0) with no slack between them, so a shift that is off by four in either direction
        // lands the ASID in the mode field or in the root pointer. Both other fields must still be
        // what they were: Sv39 mode, and two distinct root tables.
        for satp in [a.ttbr0(), b.ttbr0()] {
            assert_eq!(satp >> 60, 8, "satp.MODE is not Sv39: {satp:#x}");
        }
        assert_ne!(
            a.ttbr0() & super::SATP_PPN_MASK,
            b.ttbr0() & super::SATP_PPN_MASK,
            "two address spaces share a root table",
        );
    }
}
