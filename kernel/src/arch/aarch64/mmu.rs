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

use crate::memory;
use crate::println;
use aarch64_cpu::asm::barrier;
use aarch64_cpu::registers::{ID_AA64MMFR0_EL1, MAIR_EL1, SCTLR_EL1, TCR_EL1, TTBR0_EL1};
use core::ffi::c_void;
use paging::{Flags, Half, MapError, Mapper, PAGE_SIZE, PageTable, mair};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

/// The PL011. Mapped as **device** memory, and that word is load-bearing.
///
/// Map MMIO as *normal* memory and the CPU may cache it, reorder writes to it, merge two
/// writes into one, and speculatively read it. Speculatively reading a UART FIFO register
/// **consumes the byte**. See notes/page-tables.md.
const UART_BASE: u64 = 0x0900_0000;
const UART_SIZE: u64 = 0x1000;

/// Physical addresses are directly usable, before *and* after the MMU comes on, because the
/// map is an identity map. When we move the kernel to the high half (step 4) this becomes
/// `pa + KERNEL_BASE` and nothing else changes.
fn phys_to_ptr(pa: u64) -> *mut PageTable {
    pa as *mut PageTable
}

/// Build the kernel's page tables and turn the MMU on.
///
/// After this returns, every address in the kernel is a *virtual* address. They happen to
/// equal the physical ones, for now.
pub fn init() {
    let root = memory::alloc().expect("no frame for the root page table").addr();

    // SAFETY: a fresh frame. Zero it before the hardware can ever walk it: a page table
    // full of whatever was in RAM is a set of pointers to nowhere, followed at speed.
    unsafe {
        (*phys_to_ptr(root)).entries = [0; paging::ENTRIES];
    }

    // SAFETY: `root` is zeroed and page-aligned. `phys_to_ptr` is the identity, which is
    // correct while the MMU is off and stays correct afterwards because the map we are
    // about to build is an identity map.
    let mut mapper = unsafe {
        Mapper::new(
            root,
            Half::Low,
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
    unsafe { enable(root) };
}

/// Identity-map everything the kernel needs, each region with the tightest permissions that
/// still let it work.
fn map_everything<A, P>(m: &mut Mapper<A, P>) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    // 1. All of RAM, read/write, never executable.
    //
    // The kernel has to be able to touch any frame the allocator hands it (to zero a new
    // page table, to fill a new user page). With the MMU on, a physical address it cannot
    // *name* is a physical address it cannot use.
    //
    // We skip the kernel image, because its sections get tighter permissions below, and the
    // mapper deliberately refuses to overwrite an existing mapping.
    for (start, size) in memory::ram_regions() {
        let end = start + size;
        map_range(m, start, image_start().min(end), Flags::kernel_data())?;
        map_range(m, image_end().max(start), end, Flags::kernel_data())?;
    }

    // 2. The kernel image, section by section. This is W^X.
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

    // 5. The UART, as device memory. Without this the machine goes silent the instant the
    // MMU comes on, and a silent kernel cannot tell you why it is silent.
    map_range(m, UART_BASE, UART_BASE + UART_SIZE, Flags::device())?;

    Ok(())
}

fn map_range<A, P>(m: &mut Mapper<A, P>, start: u64, end: u64, flags: Flags) -> Result<(), MapError>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    if end <= start {
        return Ok(());
    }
    let pages = (end - start).div_ceil(PAGE_SIZE);
    m.map_range(start, start, pages, flags)
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
        .expect("the code enabling the MMU is not mapped: we would die on the next fetch");
    assert_eq!(pa, here, "identity map is not identity at {here:#x}");
    assert!(flags.is_kernel_executable(), "our own .text is not executable");
    assert!(!flags.is_writable(), "our own .text is writable: W^X is broken");

    // The stack. The first thing after the MMU comes on is a function return.
    let sp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    let (pa, flags) = m.translate(sp).expect("the stack is not mapped");
    assert_eq!(pa, sp, "stack is not identity-mapped");
    assert!(flags.is_writable(), "the stack is not writable");

    // The UART. Without it we cannot say anything, including why we died.
    let (pa, flags) = m.translate(UART_BASE).expect("the UART is not mapped");
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

/// Flip the bit.
///
/// # Safety
///
/// The tables at `root` must map, at minimum: the code executing this function, its stack,
/// and the UART. `verify` checks all three.
unsafe fn enable(root: u64) {
    // Every write we made to the page tables must be visible to the page-table walker before
    // it can possibly walk them. With the MMU off, data accesses are Device-nGnRnE (they go
    // straight to memory, uncached), so this is a barrier and not a cache flush. But it is
    // not optional: the walker is a separate observer.
    barrier::dsb(barrier::SY);

    // What the eight MAIR slots MEAN. The descriptors we just wrote say "look up slot N";
    // this is what N is. If these two ever disagree, the UART gets mapped as cacheable
    // normal memory and the machine behaves like it is haunted.
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
            + TCR_EL1::EPD0::EnableTTBR0Walks
            + TCR_EL1::T1SZ.val(16)
            + TCR_EL1::TG1::KiB_4
            + TCR_EL1::EPD1::DisableTTBR1Walks
            + TCR_EL1::IPS.val(ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::PARange)),
    );

    TTBR0_EL1.set_baddr(root);

    // The system register writes must take effect before the TLB work below.
    barrier::isb(barrier::SY);

    // Throw away every cached translation. There should not be any (the MMU has been off),
    // but "should not be any" is not a guarantee, and a single stale entry here maps some
    // address to somewhere we have never heard of.
    //
    // SAFETY: TLB maintenance is always sound.
    unsafe {
        core::arch::asm!(
            "tlbi vmalle1",  // invalidate all EL1 translations
            "dsb ish",       // wait for it to finish, inner shareable domain
            "isb",
            options(nostack),
        );
    }

    // ---- the point of no return ----
    //
    // The instruction fetched AFTER this one goes through the MMU. If the page holding it
    // isn't mapped executable, there is no fault and no message. The machine stops.
    SCTLR_EL1.modify(
        SCTLR_EL1::M::Enable        // MMU on
            + SCTLR_EL1::C::Cacheable   // data cache on
            + SCTLR_EL1::I::Cacheable,  // instruction cache on
    );

    // Without this, the CPU may still execute instructions it fetched (or speculated)
    // *before* the MMU was enabled, under the old rules. The barrier makes it start again.
    barrier::isb(barrier::SY);

    // If you are reading this line's output, we survived.
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
    let root = TTBR0_EL1.get_baddr();

    // SAFETY: TTBR0_EL1 holds the root we installed, and the map is identity, so
    // `phys_to_ptr` still works.
    let mapper = unsafe { Mapper::new(root, Half::Low, || None, phys_to_ptr) };
    mapper.translate(va)
}

pub fn print_summary() {
    println!(
        "  mmu             : {}, identity-mapped, {} KiB stack guarded at {:#x}",
        if is_enabled() { "on" } else { "OFF" },
        (stack_top() - stack_bottom()) / 1024,
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
