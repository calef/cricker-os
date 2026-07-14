//! Turning the MMU on.
//!
//! # The sketchiest moment in the kernel
//!
//! The instant we set `SCTLR_EL1.M`, **the very next instruction is fetched through the
//! MMU.** If the page currently executing isn't mapped, the CPU tries to fetch from an
//! address that no longer means anything and the machine simply stops existing.
//!
//! There is no fault. There is no message. `println!` cannot help, because the UART's
//! address is also now a virtual address, and if *that* isn't mapped either, there is
//! nowhere for the bytes to go.
//!
//! You get one shot. `cargo xtask gdb` is the tool that exists for this.
//!
//! # Why an identity map first
//!
//! We map every physical address to *itself*: VA == PA. So the instruction after the one
//! that enables the MMU is at the same address it was before, and execution continues as
//! though nothing happened.
//!
//! That is not a stepping stone we throw away. It is **how every kernel on earth survives
//! this transition**, including the ones that end up higher-half. You build a map that
//! contains the code you are currently running, flip the bit, and only *then* jump
//! somewhere else.
//!
//! See notes/mmu.md and notes/page-tables.md.

// map_page / unmap_page / flush_tlb exist ahead of their first non-test caller. They are the
// API milestone 7 uses to build and tear down a process address space, and the discipline they
// encode (break-before-make, an un-ignorable TLB obligation) is much cheaper to get right now
// than to retrofit across twenty call sites. The kernel tests exercise all of them.
#![allow(dead_code)]

use crate::memory;
use crate::println;
use aarch64_cpu::asm::barrier;
use aarch64_cpu::registers::{ID_AA64MMFR0_EL1, MAIR_EL1, SCTLR_EL1, TCR_EL1, TTBR1_EL1};
use core::ffi::c_void;
use paging::{Flags, Half, MapError, Mapper, PAGE_SIZE, PageTable, mair};
use tock_registers::interfaces::{Readable, Writeable};

/// Where the kernel lives, virtually.
///
/// Chosen so it touches only bits 63:48, which are **not translated** (see
/// notes/page-tables.md). Three things fall out of that, and all three are load-bearing:
///
/// 1. `VA = PA | KERNEL_VA_BASE` is exact, and reversible by masking. No arithmetic, no
///    overflow, no per-region offset table.
/// 2. A kernel virtual address has the **same page-table indices** as its physical address,
///    which is why boot.s can point TTBR0 and TTBR1 at *one* table and have it serve as both
///    the identity map and the high-half map.
/// 3. The kernel gets the whole top half, and userspace gets the whole bottom half, with no
///    negotiation between them.
pub const KERNEL_VA_BASE: u64 = 0xffff_0000_0000_0000;

/// A physical address, as the kernel names it.
///
/// This is the **direct map**: every byte of physical memory is visible at `pa |
/// KERNEL_VA_BASE`, permanently. It is how the kernel touches a frame the allocator just
/// handed it. Without it, a physical address the kernel cannot *name* is a physical address
/// it cannot use.
pub const fn phys_to_virt(pa: u64) -> u64 {
    pa | KERNEL_VA_BASE
}

pub const fn virt_to_phys(va: u64) -> u64 {
    va & !KERNEL_VA_BASE
}

/// The PL011. Mapped as **device** memory, and that word is load-bearing.
///
/// Map MMIO as *normal* memory and the CPU may cache it, reorder writes to it, merge two
/// writes into one, and speculatively read it. Speculatively reading a UART FIFO register
/// **consumes the byte**. See notes/page-tables.md.
const UART_BASE: u64 = 0x0900_0000;
const UART_SIZE: u64 = 0x1000;

/// How the kernel turns a physical address into something it can dereference.
///
/// The direct map. boot.s already established it (both TTBRs pointing at one table), and the
/// fine-grained tables we build below preserve it, so this is valid from the first
/// instruction of Rust to the last.
fn phys_to_ptr(pa: u64) -> *mut PageTable {
    phys_to_virt(pa) as *mut PageTable
}

/// Build the kernel's page tables and turn the MMU on.
///
/// After this returns, every address in the kernel is a *virtual* address. They happen to
/// equal the physical ones, for now.
/// Replace boot.s's crude map with a fine-grained one, and free TTBR0 for userspace.
///
/// boot.s got us here with two 1 GiB blocks and permissions that would make a security
/// engineer weep: all of RAM executable, nothing read-only. It exists to survive twenty
/// instructions. This is where we build the real thing.
///
/// We are already running at high virtual addresses when this is called.
pub fn init() {
    let root = memory::alloc()
        .expect("no frame for the root page table")
        .addr();

    // SAFETY: a fresh frame. Zero it before the hardware can ever walk it: a page table
    // full of whatever was in RAM is a set of pointers to nowhere, followed at speed.
    unsafe {
        (*phys_to_ptr(root)).entries = [0; paging::ENTRIES];
    }

    // SAFETY: `root` is zeroed and page-aligned. `phys_to_ptr` is the identity, which is
    // correct while the MMU is off and stays correct afterwards because the map we are
    // about to build is an identity map.
    // Half::High: these tables go in TTBR1_EL1. The mapper refuses a low address, which is
    // the check that would have caught a whole class of ghost bugs.
    let mut mapper = unsafe {
        Mapper::new(
            root,
            Half::High,
            || memory::alloc().map(|f| f.addr()),
            phys_to_ptr,
        )
    };

    map_everything(&mut mapper).expect("failed to build the kernel page tables");

    // Prove the tables say what we think before we bet the machine on them. This walk is
    // the software version of what the hardware is about to do in silicon, and it is the
    // last chance to find out we are wrong while we can still print.
    verify(&mapper);

    // SAFETY: the map covers this function's code, its stack, and the UART. We checked.
    unsafe { install(root) };
}

/// Identity-map everything the kernel needs, each region with the tightest permissions that
/// still let it work.
fn map_everything<A, P>(m: &mut Mapper<A, P>) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    // 1. THE DIRECT MAP: all of physical RAM, visible at `pa | KERNEL_VA_BASE`, read/write,
    //    never executable.
    //
    // The kernel must be able to touch any frame the allocator hands it (to zero a new page
    // table, to fill a new user page). With paging on, a physical address it cannot *name*
    // is a physical address it cannot use.
    //
    // We skip the kernel image, whose sections get tighter permissions below. The mapper
    // deliberately refuses to overwrite an existing mapping, which turns an ordering mistake
    // here into an error instead of a silently-wrong permission.
    let image_lo = virt_to_phys(image_start());
    let image_hi = virt_to_phys(image_end());

    for (start, size) in memory::ram_regions() {
        let end = start + size;
        direct_map(m, start, image_lo.min(end), Flags::kernel_data())?;
        direct_map(m, image_hi.max(start), end, Flags::kernel_data())?;
    }

    // 2. The kernel image, section by section, at its LINKED virtual addresses. This is W^X.
    map_range(m, text_start(), text_end(), Flags::kernel_code())?;
    map_range(m, rodata_start(), rodata_end(), Flags::kernel_rodata())?;
    map_range(m, data_start(), bss_end(), Flags::kernel_data())?;

    // 3. THE GUARD PAGE IS NOT MAPPED. That is its entire job.
    //
    // The stack grows down into it and the MMU faults on the first byte past the end,
    // precisely, before any damage. Compare milestone 3, where a stack overflow wrote
    // through .bss and .data into .text and the kernel executed its own corrupted code, and
    // hung with no output for 150 seconds. See notes/stack.md.
    //
    // We simply skip [__stack_guard, __stack_guard + 4096) and carry on above it.

    // 4. The stack.
    map_range(m, stack_bottom(), stack_top(), Flags::kernel_data())?;

    // 5. The UART, as device memory, in the direct map. Without this the machine goes silent
    // the instant we switch tables, and a silent kernel cannot tell you why it is silent.
    direct_map(m, UART_BASE, UART_BASE + UART_SIZE, Flags::device())?;

    Ok(())
}

/// Map a range of *virtual* addresses to the physical ones they were linked against.
///
/// Kernel sections: their VA is what the linker gave them, and the PA is that minus the base.
fn map_range<A, P>(
    m: &mut Mapper<A, P>,
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
    m: &mut Mapper<A, P>,
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

/// Walk the tables in software and check the things that would kill us.
///
/// The hardware is about to do exactly this walk, in silicon, for every memory access
/// forever. Doing it once ourselves, while we can still print, is the difference between a
/// legible failure and a machine that vanishes.
fn verify<A, P>(m: &Mapper<A, P>)
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    // The code we are executing right now. If this isn't mapped executable, the instruction
    // after `msr sctlr_el1` never gets fetched.
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
        "our own .text is writable: W^X is broken"
    );

    // The stack. The first thing after the switch is a function return.
    let sp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    let (pa, flags) = m.translate(sp).expect("the stack is not mapped");
    assert_eq!(pa, virt_to_phys(sp), "the stack maps to the wrong frame");
    assert!(flags.is_writable(), "the stack is not writable");

    // The UART. Without it we cannot say anything, including why we died.
    let uart = phys_to_virt(UART_BASE);
    let (pa, flags) = m.translate(uart).expect("the UART is not mapped");
    assert_eq!(pa, UART_BASE);
    assert!(flags.is_writable());

    // The guard page must NOT be mapped. If it is, we've silently lost the protection and
    // would only find out during the next stack overflow, which is exactly when we can least
    // afford to be surprised.
    assert!(
        m.translate(stack_guard()).is_none(),
        "the guard page IS mapped: stack overflow protection is off"
    );
}

/// Switch TTBR1 to the new tables, and take TTBR0 away.
///
/// The MMU is already on (boot.s did that). What changes here is *which* tables it walks, and
/// the switch is live: the instruction after `msr ttbr1_el1` is fetched through the new
/// tables. They had better map it.
///
/// Disabling TTBR0 is the point of the whole exercise. **The kernel now lives entirely in
/// TTBR1**, which means TTBR0 can be swapped per-process at milestone 7 without unmapping the
/// kernel out from under itself. Until then, any access to a low address faults, which is
/// exactly what we want: there is no userspace yet, so there is nothing legitimate down
/// there.
///
/// # Safety
///
/// The tables at `root` must map, at minimum: the code executing this function, its stack,
/// and the UART. `verify` checks all three.
unsafe fn install(root: u64) {
    // Every write we made to the page tables must be visible to the page-table walker before
    // it can possibly walk them. The walker is a separate observer; ordinary program order
    // does not bind it.
    barrier::dsb(barrier::SY);

    // What the eight MAIR slots MEAN. boot.s already set this to the same value; we set it
    // again because this file owns the definition and a silent disagreement between the two
    // would map the UART as cacheable normal memory and make the machine behave like it is
    // haunted.
    MAIR_EL1.set(mair::VALUE);

    // How to walk. The traps here:
    //
    //   T0SZ = 16      -> 64 - 16 = 48-bit virtual addresses.
    //
    //   TG0 vs TG1     -> **THE ENCODINGS ARE DIFFERENT.** 4 KiB is 0b00 for TG0 and 0b10
    //                     for TG1. The `aarch64-cpu` crate spells both `KiB_4`, which is
    //                     exactly the kind of thing we pay a crate to get right.
    //
    //   EPD1 = disable -> we have no TTBR1 tables yet. If we left TTBR1 walks enabled, any
    //                     stray access to a high address would walk whatever garbage is in
    //                     TTBR1_EL1 and follow it. Better to fault.
    //
    //   IPS            -> how many physical address bits this CPU actually has. Read it
    //                     from the hardware rather than guessing; a value larger than the
    //                     implementation supports is UNPREDICTABLE.
    TCR_EL1.write(
        TCR_EL1::T0SZ.val(16)
            + TCR_EL1::TG0::KiB_4
            + TCR_EL1::SH0::Inner
            + TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            // TTBR0 is now OFF. The kernel is entirely in TTBR1, so nothing legitimate lives
            // at a low address until userspace exists. Any access down there should fault,
            // loudly, rather than walk a stale identity map we forgot to tear down.
            + TCR_EL1::EPD0::DisableTTBR0Walks
            + TCR_EL1::T1SZ.val(16)
            + TCR_EL1::TG1::KiB_4
            + TCR_EL1::SH1::Inner
            + TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::EPD1::EnableTTBR1Walks
            + TCR_EL1::IPS.val(ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::PARange)),
    );

    TTBR1_EL1.set_baddr(root);

    // The system register writes must take effect before the TLB work below.
    barrier::isb(barrier::SY);

    // Throw away every cached translation. There should not be any (the MMU has been off),
    // but "should not be any" is not a guarantee, and a single stale entry here maps some
    // address to somewhere we have never heard of.
    //
    // SAFETY: TLB maintenance is always sound.
    unsafe {
        core::arch::asm!(
            "tlbi vmalle1", // invalidate all EL1 translations
            "dsb ish",      // wait for it to finish, inner shareable domain
            "isb",
            options(nostack),
        );
    }

    // Make the CPU forget everything it knew. The old boot tables are still sitting in .bss
    // and the TLB may hold translations from them; a single stale entry maps some address to
    // somewhere we have never heard of.
    //
    // SAFETY: TLB maintenance is always sound.
    unsafe {
        core::arch::asm!("tlbi vmalle1", "dsb ish", "isb", options(nostack),);
    }

    // If you are reading this line's output, we survived. The kernel is now running out of
    // TTBR1, and TTBR0 is free.
}

/// The kernel's live page tables, as a `Mapper`.
///
/// Reads `TTBR1_EL1` back out of the hardware, so this walks what the CPU is actually walking,
/// not a copy of what we intended.
fn kernel_mapper() -> Mapper<impl FnMut() -> Option<u64>, fn(u64) -> *mut PageTable> {
    let root = TTBR1_EL1.get_baddr();

    // SAFETY: TTBR1_EL1 holds the root we installed, and the direct map makes `phys_to_ptr`
    // valid for any frame.
    unsafe {
        Mapper::new(
            root,
            Half::High,
            || memory::alloc().map(|f| f.addr()),
            phys_to_ptr,
        )
    }
}

/// Map one page into the kernel's address space.
///
/// Refuses to overwrite an existing mapping (`MapError::AlreadyMapped`), which is what forces
/// break-before-make: to *change* a mapping you must [`unmap_page`] first.
pub fn map_page(va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
    kernel_mapper().map(va, pa, flags)
}

/// Remove one page from the kernel's address space, and **invalidate the TLB**.
///
/// Returns the physical frame, which is the caller's to free: the mapper never owned it.
///
/// The `TlbFlush` obligation is discharged here, properly, with a real `tlbi`. It cannot be
/// forgotten: dropping one un-discharged panics.
pub fn unmap_page(va: u64) -> Result<u64, MapError> {
    let (pa, flush) = kernel_mapper().unmap(va)?;
    flush.flush(flush_tlb);
    Ok(pa)
}

/// Invalidate the TLB entry for one virtual address.
///
/// This is what discharges a `paging::TlbFlush`. The `paging` crate is pure logic and emits no
/// instructions; the architecture supplies this.
///
///   `tlbi vaae1is`  — invalidate by **VA**, **A**ll ASIDs, **E1**, **I**nner **S**hareable.
///
/// The operand is the address shifted right by 12: the TLB is indexed by page, not by byte.
///
/// `dsb ish` afterwards because **TLB maintenance is not synchronous**. Without it, the next
/// instruction may still be using the translation you just told the CPU to forget. And `isb`
/// because instruction fetch may have already prefetched through the old mapping.
pub fn flush_tlb(va: u64) {
    // SAFETY: TLB maintenance is always sound. Getting it wrong means a stale translation, not
    // memory unsafety in the Rust sense; but a stale translation IS memory unsafety in the
    // sense that matters here.
    unsafe {
        core::arch::asm!(
            "dsb ishst",             // our page table write must land first
            "tlbi vaae1is, {page}",  // then forget the translation
            "dsb ish",               // wait for every core to have done so
            "isb",                   // and discard anything fetched through the old mapping
            page = in(reg) va >> 12,
            options(nostack),
        );
    }
}

pub fn is_enabled() -> bool {
    SCTLR_EL1.is_set(SCTLR_EL1::M)
}

/// Ask the *live* page tables what a virtual address maps to.
///
/// Reads `TTBR0_EL1` back out of the hardware, so this is the truth the CPU is using, not a
/// copy of what we intended. That distinction is the point: it lets the tests check the
/// tables the machine is actually walking.
#[allow(dead_code)] // used by the tests, and by anyone debugging a mapping
pub fn translate(va: u64) -> Option<(u64, Flags)> {
    let root = TTBR1_EL1.get_baddr();

    // SAFETY: TTBR1_EL1 holds the root we installed, and the direct map makes `phys_to_ptr`
    // valid.
    let mapper = unsafe { Mapper::new(root, Half::High, || None, phys_to_ptr) };
    mapper.translate(va)
}

pub fn print_summary() {
    println!(
        "  mmu             : {}, kernel in TTBR1 at {:#018x}, TTBR0 free for userspace",
        if is_enabled() { "on" } else { "OFF" },
        KERNEL_VA_BASE,
    );
    println!(
        "  stack guard     : {:#018x} (unmapped; a stack overflow faults here)",
        stack_guard(),
    );
}

// --- what the linker told us ---
//
// Each of these is a section boundary, page-aligned by link.ld precisely so that each can
// carry its own MMU permissions. Permissions are per-page: a section that shares a page with
// another section cannot have its own.

macro_rules! linker_symbol {
    ($name:ident, $sym:ident) => {
        pub fn $name() -> u64 {
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

#[cfg(test)]
mod tests {
    //! Tests for the MMU: the live page tables, W^X, the guard page, and TLB invalidation.
    //!
    //! `translate` reads `TTBR1_EL1` back out of the hardware, so these inspect the tables the CPU
    //! is *actually walking*, not a copy of what we intended.

    /// The MMU is on, and we are still alive to say so.
    #[test_case]
    fn mmu_is_enabled() {
        assert!(crate::arch::mmu::is_enabled(), "SCTLR_EL1.M is not set");
    }

    /// The kernel is running at high virtual addresses, out of TTBR1.
    ///
    /// This is what makes milestone 7 possible: TTBR0 can be swapped per-process without
    /// unmapping the kernel out from under itself. If the kernel lived in TTBR0, the first
    /// context switch into a user process would delete the kernel.
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
        let sp: u64;
        // SAFETY: reads a register.
        unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
        assert!(
            sp >= KERNEL_VA_BASE,
            "the stack is at {sp:#x}, not in the high half"
        );

        // And a heap allocation.
        let b = alloc::boxed::Box::new(0u64);
        let heap = (&raw const *b) as u64;
        assert!(
            heap >= KERNEL_VA_BASE,
            "the heap is at {heap:#x}, not in the high half"
        );
    }

    /// **TTBR0 is free.** Nothing of ours lives at a low address any more.
    ///
    /// The point of the whole exercise. `mmu::init` sets EPD0, so a low address doesn't even
    /// get a table walk: it faults. Which is what we want, because there is no userspace yet
    /// and so nothing legitimate is down there.
    #[test_case]
    fn ttbr0_is_disabled_and_available_for_userspace() {
        use aarch64_cpu::registers::TCR_EL1;
        use tock_registers::interfaces::Readable;

        assert_eq!(
            TCR_EL1.read(TCR_EL1::EPD0),
            1,
            "TTBR0 walks are still enabled: a stale identity map may still be live"
        );
    }

    /// The direct map: every physical address is nameable at `pa | KERNEL_VA_BASE`.
    ///
    /// This is how the kernel touches a frame the allocator just handed it. Without it, a
    /// physical address the kernel cannot NAME is a physical address it cannot use.
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
    /// Verified at boot too (mmu::verify panics if it's mapped), but stated here as well
    /// because it is the thing that closes the milestone 3 incident, and a protection you
    /// only discover is missing *during* a stack overflow is no protection at all.
    ///
    /// Proven by deliberate overflow: FAR_EL1 comes back as exactly this address.
    #[test_case]
    fn the_guard_page_is_a_hole() {
        use crate::arch::mmu;
        assert_eq!(
            mmu::translate(mmu::stack_guard()),
            None,
            "the guard page IS mapped: a stack overflow would silently eat .bss again"
        );

        // And the pages either side of it must be mapped, or we've put the hole in the
        // wrong place and are protecting nothing.
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
    /// Not a copy of what we intended: `translate` reads TTBR0_EL1 back out of the CPU and
    /// walks from there.
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
        assert!(!flags.is_user_executable(), ".text is executable by EL0");
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
    /// Map MMIO as normal memory and the CPU may cache it, reorder writes to it, merge two
    /// writes into one, and speculatively read it. Speculatively reading a UART FIFO
    /// register CONSUMES THE BYTE. See notes/page-tables.md.
    #[test_case]
    fn the_uart_is_mapped_as_device_memory() {
        use crate::arch::mmu;

        // The UART lives in the direct map, like every other physical address the kernel
        // names. Its raw physical address no longer exists as far as the CPU is concerned:
        // TTBR0 is off.
        let (_, flags) =
            mmu::translate(mmu::phys_to_virt(0x0900_0000)).expect("the UART is not mapped");

        // AttrIndx, bits [4:2], must name the MAIR slot that says Device-nGnRnE.
        let slot = (flags.bits() >> 2) & 0b111;
        assert_eq!(slot, paging::mair::DEVICE, "the UART is not device memory");

        assert!(flags.is_writable(), "we do need to write to it");
        assert!(!flags.is_kernel_executable());
    }

    /// A frame from the allocator is still real, writable memory *through the MMU*.
    ///
    /// Before, this proved the bookkeeping matched physical RAM. Now it also proves the
    /// identity map covers everything the allocator can hand out, which is a different and
    /// newly-necessary claim: with paging on, a physical address the kernel cannot NAME is a
    /// physical address it cannot use.
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
    /// This is the test for the landmine. Change a mapping without a `tlbi` and the CPU keeps
    /// using the *cached* translation: memory reads back as the previous owner's data. It is a
    /// security hole, and it is close to undebuggable, because the page tables **in memory are
    /// correct** — the lie lives in a CPU cache you cannot inspect.
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

        // A high-half address well away from the direct map: physical 0xff00_0000 is not RAM
        // (RAM is 0x4000_0000..0x4800_0000), so nothing is mapped here.
        let test_va = mmu::KERNEL_VA_BASE | 0xff00_0000;
        assert_eq!(
            mmu::translate(test_va),
            None,
            "test address is already in use"
        );

        let a = crate::memory::alloc().expect("out of memory");
        let b = crate::memory::alloc().expect("out of memory");

        // SAFETY: two frames the allocator just gave us exclusively, reached via the direct
        // map.
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
    #[test_case]
    fn the_kernel_mapper_refuses_to_overwrite() {
        use crate::arch::mmu;
        use paging::{Flags, MapError};

        let va = mmu::KERNEL_VA_BASE | 0xfe00_0000;
        let f = crate::memory::alloc().unwrap();

        mmu::map_page(va, f.addr(), Flags::kernel_data()).unwrap();

        // aarch64 does not permit valid -> valid directly: it can raise a TLB conflict abort.
        // The API makes it unrepresentable.
        assert_eq!(
            mmu::map_page(va, f.addr(), Flags::kernel_data()),
            Err(MapError::AlreadyMapped)
        );

        mmu::unmap_page(va).unwrap();
        crate::memory::free(f);
    }
}
