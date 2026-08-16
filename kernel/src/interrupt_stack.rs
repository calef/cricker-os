//! The per-CPU interrupt stack: where a trap taken on kernel code builds its frames.
//!
//! # The problem it solves
//!
//! A kernel thread runs on its own 24 KiB stack. When the timer interrupts it, the trap frame and
//! every frame of the handler above it were built **on that thread's stack**, at whatever depth the
//! thread happened to be. So the thread's stack had to be big enough for its own deepest chain
//! *plus* a whole interrupt, and a preemption arriving at the worst instant was the difference
//! between fitting and not.
//!
//! That is not a hypothesis. The 2026-08-15 CI overflows were symbolized to a thread spinning for
//! `SCHED` with about 272 bytes of stack left, and the arithmetic behind it charged ~2.3 KiB to the
//! interrupted thread for one preemption: trap frame, dispatch, the interrupt controller's claim,
//! the tick, and the scheduler. Milestone 124's interim fix grew the stack from 16 KiB to 24 KiB
//! with that measurement in hand and said in the same breath that the structural fix was **to bound
//! the cost rather than to pay it**. This is that fix. See notes/stack.md.
//!
//! # The shape
//!
//! One stack per core, over its own unmapped guard page, in a `(NOLOAD)` region the linker anchors
//! (`.interrupt_stacks`). A trap **from kernel mode** saves its frame on the interrupted stack, as
//! it always did, and then the handler body runs over here; a trap from user mode does not switch,
//! because a user thread's kernel stack is empty at that moment and because the syscall it is
//! probably taking will block, which this stack may not do.
//!
//! # The one rule, and why it is not negotiable
//!
//! **Nothing that runs on an interrupt stack may context-switch away from it.**
//!
//! A context switch parks the current `sp` in the outgoing thread's `Context` and resumes it later.
//! If that `sp` pointed here, the thread would resume on a stack that belongs to a core rather than
//! to it, and whose bytes the next interrupt on that core has already overwritten. So the deferred
//! `schedule()` at the tail of an interrupt runs **after** the handler returns to the interrupted
//! stack: `sched::preempt_if_needed` is called by the arch dispatcher's outer half, one frame
//! outside the switch. Three mechanisms hold it, deliberately overlapping, because a violation
//! would corrupt a stack rather than fault:
//!
//!   1. `sched::schedule` debug-asserts that `sp` is not on an interrupt stack.
//!   2. `script/stack-depth-check` proves in CI that no context switch is *reachable* from the
//!      interrupt-stack entry point, on both architectures.
//!   3. This paragraph, at the thing itself.
//!
//! # BUGS
//!
//! - **A trap from user mode does not switch**, so a deep syscall is still charged to the calling
//!   thread's kernel stack. That is correct rather than lazy (a syscall blocks, and a blocked
//!   thread's frames must live on its own stack), but it means this bounds the *interrupt* cost
//!   and not the *trap* cost.
//! - **The switch happens after the frame is saved**, so the trap frame itself (272 bytes on
//!   aarch64, 288 on riscv64) is still built on the interrupted stack. It has to be: a preempted
//!   thread's frame must survive until that thread runs again, and this stack cannot promise that.
//! - **A stack overflow whose first fault is the vector's own frame store still cascades.** The
//!   vector saves before anything can decide to switch, so this does not rescue an overflow that
//!   is already past the guard; it makes such an overflow much less likely instead.
//! - **The stacks are never reclaimed and cost `MAX_CPUS` slots whether or not the cores exist**,
//!   like the secondary stacks beside them. 160 KiB of address space and RAM for eight seats.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::cpu::MAX_CPUS;

/// The usable stack in one core's slot: four pages.
///
/// Sized against what a handler actually needs rather than against the thread stacks it relieves.
/// `script/stack-depth-check` measures the deepest chain reachable from the dispatcher (about 4 KiB
/// on both ISAs) and gates this number against it, so growth is caught by a build rather than by a
/// fault. The extra room over that is for the fatal path, which prints a register dump and scans a
/// dead stack from here.
pub const SIZE: usize = 4 * 4096;

/// The unmapped page below each interrupt stack. `map_everything` skips exactly this page per core,
/// and `verify` refuses to install a map in which it translates. Same mechanism as the boot stack's
/// `__stack_guard`, a secondary's (smp.rs) and a kernel thread's (thread.rs).
const GUARD: usize = 4096;

/// A core's slot: its guard page, then the stack that grows down into it.
const SLOT: usize = GUARD + SIZE;

/// The stacks themselves.
///
/// **The `UnsafeCell` is load-bearing for the same reason it is in `smp::SECONDARY_STACKS`**: an
/// immutable `static` of plain arrays lands in `.rodata`, which the fine kernel map makes read-only,
/// and a stack you cannot write is not a stack. The bug is invisible on the coarse boot map and
/// fires the instant the fine map is installed.
///
/// The `link_section` is what makes the guard pages possible: in `.bss` the mapper would map the
/// whole region in one range and there would be nowhere to put a hole.
#[repr(C, align(4096))]
struct Stacks(UnsafeCell<[[u8; SLOT]; MAX_CPUS]>);

// SAFETY: each core writes only its own slot, and only as its stack through `sp`. No two cores ever
// touch the same bytes; the cell exists to place the stacks in writable memory, not to share them.
unsafe impl Sync for Stacks {}

#[unsafe(link_section = ".interrupt_stacks")]
static STACKS: Stacks = Stacks(UnsafeCell::new([[0; SLOT]; MAX_CPUS]));

/// Whether the switch is live.
///
/// False until [`init`] runs, which is **after the fine kernel map is installed**, because until
/// then these addresses are covered only by the coarse boot map and the guard pages are not holes
/// yet. A trap before that point simply keeps its old behaviour and runs on whatever stack it
/// interrupted, which is what every trap did before this module existed.
static ARMED: AtomicBool = AtomicBool::new(false);

/// The low end of core `id`'s slot, which is its **guard page**.
pub fn guard(id: usize) -> u64 {
    STACKS.0.get() as u64 + (id as u64) * SLOT as u64
}

/// Core `id`'s usable stack as `(bottom, top)`: the slot above its guard page.
pub fn span(id: usize) -> (u64, u64) {
    let bottom = guard(id) + GUARD as u64;
    (bottom, bottom + SIZE as u64)
}

/// The whole region, `(start, end)`, for the check against what the linker reserved.
#[cfg(test)]
pub fn region() -> (u64, u64) {
    let start = STACKS.0.get() as u64;
    (start, start + (MAX_CPUS * SLOT) as u64)
}

/// Arm the switch, once, on the boot core, after the fine kernel map is installed.
///
/// In a test build this also paints every slot for the high-water instrument (milestone 84). It is
/// safe to paint all of them here rather than each core painting its own, because no secondary is
/// up yet and no interrupt has been taken with the switch armed: every slot is genuinely unused at
/// this instant, which is exactly what `stack::paint` requires and what makes the later measurement
/// a measurement rather than a reading of somebody's live frame.
pub fn init() {
    #[cfg(test)]
    {
        for id in 0..MAX_CPUS {
            let (bottom, top) = span(id);
            // SAFETY: the slot is mapped (`map_everything` mapped every slot above its guard
            // page), page-aligned, and unused: nothing switches to an interrupt stack until the
            // store below.
            unsafe { crate::stack::paint(bottom, top) };
        }
    }
    // Release: the paint above, and the mapping the caller installed, must be visible to any core
    // that sees this flag set.
    ARMED.store(true, Ordering::Release);
}

/// **The stack a trap should run its handler on, or 0 to stay where it is.**
///
/// Called by each architecture's dispatcher with whether the trap came from user mode. Three
/// answers are "stay", and each is a rule rather than a special case:
///
///   - **before [`init`]**, because the map does not have the holes yet;
///   - **a trap from user mode**, because that thread's kernel stack is empty anyway and because
///     the syscall it is taking may block, which this stack may not do;
///   - **already on an interrupt stack**, which is the nesting case. A fault taken inside a handler
///     re-enters the dispatcher, and resetting `sp` to the top here would drop it straight through
///     the frames of the handler that faulted.
pub fn top_for_trap(from_user: bool) -> u64 {
    if from_user || !ARMED.load(Ordering::Acquire) {
        return 0;
    }
    let id = crate::cpu::id();
    let (bottom, top) = span(id);
    let sp = crate::arch::current_sp();
    if (bottom..top).contains(&sp) {
        return 0; // nesting: this core is already running on it
    }
    top
}

/// Is `addr` on some core's interrupt stack (not its guard page)? The question `sched::schedule`
/// asks about its own `sp`, and the one the depth gate's assertion rests on.
pub fn contains(addr: u64) -> bool {
    (0..MAX_CPUS).any(|id| {
        let (bottom, top) = span(id);
        (bottom..top).contains(&addr)
    })
}

/// Which core's interrupt-stack guard page `addr` falls in, if any. Used by `stack::guard_page_at`
/// so a fault down here is named rather than reported as an address nothing recognises.
pub fn guard_page_at(addr: u64) -> Option<usize> {
    (0..MAX_CPUS).find(|&id| {
        let g = guard(id);
        (g..g + GUARD as u64).contains(&addr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The linker reserved exactly what this module lays out.
    ///
    /// The two numbers are written in different files and neither can see the other: the slot
    /// layout and `MAX_CPUS` are here, and `.interrupt_stacks` is anchored in two linker scripts.
    /// The failure this catches is a region smaller than the stacks, which would put the last
    /// core's stack over whatever the linker put next and produce corruption with no fault at all.
    /// Copied from `smp::the_secondary_stack_region_is_the_size_the_linker_reserved`, which exists
    /// for the same reason one region over.
    #[test_case]
    fn the_interrupt_stack_region_is_the_size_the_linker_reserved() {
        unsafe extern "C" {
            static __interrupt_stacks_start: core::ffi::c_void;
            static __interrupt_stacks_end: core::ffi::c_void;
        }
        let start = (&raw const __interrupt_stacks_start) as u64;
        let end = (&raw const __interrupt_stacks_end) as u64;
        let (ours_start, ours_end) = region();
        assert_eq!(
            start, ours_start,
            "the region does not start where the linker put it"
        );
        assert!(
            ours_end <= end,
            "the interrupt stacks ({} bytes) do not fit the {} bytes the linker reserved",
            ours_end - ours_start,
            end - start,
        );
    }

    /// **A slot's guard page begins exactly one byte past the slot below it.**
    ///
    /// The same geometry `sched::a_slots_guard_page_begins_where_the_slot_below_it_ends` pins for
    /// thread stacks, and it is pinned here for the reason that one was written: a fault at a guard
    /// base is ambiguous between an overflow of this slot and a store one word past the top of the
    /// slot below, and a reader can only tell those apart if the layout is nailed down somewhere.
    #[test_case]
    fn a_slots_guard_page_begins_where_the_slot_below_it_ends() {
        for id in 1..MAX_CPUS {
            let (_, below_top) = span(id - 1);
            assert_eq!(
                guard(id),
                below_top,
                "slot {id}'s guard does not abut slot {} 's top",
                id - 1
            );
        }
    }

    /// **Traps really do run over here.** The paint from [`init`] is intact until something builds
    /// a frame on the stack, so a nonzero high-water on the boot core's slot is proof that the
    /// switch fires, taken from the machine rather than from reading the assembly.
    ///
    /// It is deliberately an existence check and not a threshold: how deep a handler goes is what
    /// `stack::report_high_water` prints and what `script/stack-depth-check` bounds, and asserting
    /// a depth here would be a second, weaker copy of both.
    ///
    /// It asks about *any* core rather than this one on purpose. Kernel tests are not pinned (see
    /// `cpu::percpu_is_self_consistent_on_whatever_core_we_run`), and a core that spent its whole
    /// life running user threads would take every trap from user mode, where the switch correctly
    /// does not happen. "Some core switched" is the claim this test can make without asserting
    /// where it runs.
    #[test_case]
    fn interrupts_are_handled_on_the_interrupt_stack() {
        let used = (0..MAX_CPUS).any(|id| {
            let (bottom, top) = span(id);
            // SAFETY: a slot of the region `init` mapped and painted, read one word at a time.
            unsafe { crate::stack::high_water(bottom, top) > 0 }
        });
        assert!(
            used,
            "no core's interrupt stack has been touched: no trap has run on one, so either the \
             switch never armed or the dispatcher is not asking for it",
        );
    }
}
