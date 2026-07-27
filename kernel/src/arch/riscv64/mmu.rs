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

use paging::{Flags, MapError, PageTable, Sv39};

/// This architecture's page-table format. Portable code names it as `arch::mmu::Format` (see the
/// aarch64 module's alias for why), so the user-VA gate and the user `Mapper` land on Sv39 here.
pub type Format = Sv39;

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

/// Build the kernel's own page tables and turn the MMU on. The Sv39 step.
pub fn init() {
    unimplemented!("riscv MMU init (Sv39 root, direct map, high-half, satp): the MMU step")
}

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

/// Map one page into the kernel's own address space. The MMU step.
pub fn map_page(va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
    let _ = (va, pa, flags);
    unimplemented!("riscv map kernel page: the MMU step")
}

/// Unmap one page from the kernel's address space, returning the frame it named. The MMU step.
pub fn unmap_page(va: u64) -> Result<u64, MapError> {
    let _ = va;
    unimplemented!("riscv unmap kernel page: the MMU step")
}

/// Discharge the TLB entry for one address (`sfence.vma va`). The MMU step.
pub fn flush_tlb(va: u64) {
    let _ = va;
    unimplemented!("riscv sfence.vma one address: the MMU step")
}

/// Whether paging is on (`satp` mode != Bare). The MMU step; false while bare.
pub fn is_enabled() -> bool {
    // Honest even as a stub: until the MMU step runs, paging is off.
    false
}

/// Translate a kernel virtual address through the installed tables. The MMU step.
pub fn translate(va: u64) -> Option<(u64, Flags)> {
    let _ = va;
    unimplemented!("riscv translate kernel va: the MMU step")
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
