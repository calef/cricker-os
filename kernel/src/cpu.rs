//! Per-CPU state.
//!
//! Each core reaches its own private data in a single instruction, through a pointer the
//! architecture keeps in a scratch register for exactly this purpose (`TPIDR_EL1` on aarch64;
//! see [`crate::arch::set_percpu`]). This is the foundation the rest of SMP is built on: once a
//! core can name "my own state" cheaply, the scheduler's run queue, its current thread, and its
//! lock bookkeeping can each stop being a single machine-wide global. See DECISIONS.md §11.
//!
//! **Step 1 of §11.** For now the only thing that lives here is the lock-rank bookkeeping §9
//! keeps, which used to be one global (`HELD_RANK`) and would be clobbered the instant a second
//! core took a lock. Moving it here changes nothing on one core and is the smallest provable
//! piece of the per-CPU machinery. The block grows in step 3 to hold this core's run queue,
//! `current`, idle thread, reschedule flag, and migration inbox.

use crate::sync::rank;
use crate::thread::Tid;
use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

/// A `current`/`idle` slot holding no thread. Tids are small integers from 0 up, so `u64::MAX`
/// can never collide with a real one.
pub const NO_TID: Tid = u64::MAX;

/// The most cores we support. QEMU `virt` gives us as many as we ask for with `-smp`; four is
/// what the tests will run. A fixed maximum lets the blocks be a static array, so they exist
/// before there is a heap and can be pointed at from a core's very first Rust instruction.
pub const MAX_CPUS: usize = 4;

/// One core's private data.
pub struct PerCpu {
    /// The lowest lock rank this core currently holds (`rank::NONE` when it holds nothing).
    ///
    /// Only ever touched by this core with interrupts masked, so the atomic is for interior
    /// mutability through the shared static, not for cross-core synchronization: no other core
    /// can reach *this* core's block on the lock path. See [`crate::sync`] and DECISIONS.md §9.
    pub held_rank: AtomicU32,

    /// The thread currently running on this core (`NO_TID` before the core schedules).
    ///
    /// Was a single field on the global `Scheduler`; per-CPU as of §11 step 3b, because each core
    /// runs a different thread. An atomic because a remote core may *read* it (the reaper checks
    /// whether a thread is current on any core), even though only the owning core ever writes it.
    pub current: AtomicU64,

    /// This core's idle thread (`NO_TID` before it exists): what runs when this core's run queue
    /// is empty. Each core gets its own, so an idle core parks in its *own* `wfi`.
    pub idle: AtomicU64,

    /// Set by this core's timer tick, read on this core's return from the IRQ. Per-CPU so one
    /// core's tick cannot make another core reschedule. See DECISIONS.md §9's record-and-defer.
    pub need_resched: AtomicBool,

    /// This core's run queue: the threads ready to run here, in round-robin order.
    ///
    /// **No cross-core lock, by design (DECISIONS.md §11).** Only this core ever touches its own
    /// queue, and only with interrupts masked, which is exactly what makes the `UnsafeCell`
    /// sound. That a remote core cannot even *name* this queue is the point: it forces cross-core
    /// work movement onto the inbox/SGI path (step 3c) rather than letting one core reach into
    /// another's queue. Access it through [`with_runq`](Self::with_runq).
    runq: UnsafeCell<VecDeque<Tid>>,
}

// SAFETY: the only non-`Sync` field is `runq`, and the whole contract of this type is that a
// `PerCpu` block is touched only by its owning core, with interrupts masked (see `with_runq`). No
// two cores ever reach the same block, so there is no cross-core data race to guard against, and
// the atomics handle this core's own interrupt-vs-mainline reentrancy on the fields that need it.
unsafe impl Sync for PerCpu {}

impl PerCpu {
    const fn new() -> Self {
        Self {
            held_rank: AtomicU32::new(rank::NONE),
            current: AtomicU64::new(NO_TID),
            idle: AtomicU64::new(NO_TID),
            need_resched: AtomicBool::new(false),
            runq: UnsafeCell::new(VecDeque::new()),
        }
    }

    /// Run `f` with exclusive access to this core's run queue.
    ///
    /// # Invariant
    ///
    /// The caller must have interrupts masked (asserted in debug builds). Combined with
    /// single-owner access, that makes the `&mut` genuinely exclusive: this core cannot re-enter
    /// through an interrupt mid-borrow, and no other core can reach this block at all. This is the
    /// standard per-CPU pattern; the `UnsafeCell` is sound precisely because of that invariant.
    /// Every caller today already holds `SCHED` (which masks interrupts) or has masked them
    /// explicitly in `schedule()`.
    pub fn with_runq<R>(&self, f: impl FnOnce(&mut VecDeque<Tid>) -> R) -> R {
        debug_assert!(
            !crate::arch::interrupts::enabled(),
            "run queue touched with interrupts enabled: single-owner safety needs them masked",
        );
        // SAFETY: interrupts masked (asserted) and single-owner, so this `&mut` is exclusive.
        f(unsafe { &mut *self.runq.get() })
    }
}

/// The per-CPU blocks, one per core.
///
/// Statically allocated on purpose: a core's block must be reachable from its first instruction
/// of Rust, long before any allocator exists. `TPIDR_EL1` points at one element of this array.
static PERCPU: [PerCpu; MAX_CPUS] = [const { PerCpu::new() }; MAX_CPUS];

/// Point this core's `TPIDR_EL1` at its `PerCpu` block.
///
/// Call once, on each core, **before that core takes its first lock**, because the lock path
/// reads `current().held_rank`. On the boot core that means before `console::init` in
/// `kernel_main`; on a secondary it is the first thing the bring-up entry does.
pub fn init_this_cpu(id: usize) {
    assert!(id < MAX_CPUS, "cpu id {id} exceeds MAX_CPUS {MAX_CPUS}");
    crate::arch::set_percpu(&PERCPU[id] as *const PerCpu as usize);
}

/// This core's private block, in one instruction.
pub fn current() -> &'static PerCpu {
    // SAFETY: `init_this_cpu` set TPIDR_EL1 to the address of an element of the `'static`
    // `PERCPU` array, and nothing else ever writes TPIDR_EL1. The one window where this would be
    // wrong is before `init_this_cpu` has run on this core, which is exactly why that call is the
    // first thing `kernel_main` (and each secondary's entry) does, ahead of any lock.
    unsafe { &*(crate::arch::percpu() as *const PerCpu) }
}

/// This core's logical id: its index into `PERCPU`.
#[allow(dead_code)] // used by the tests now, and by the scheduler in step 3
pub fn id() -> usize {
    let base = PERCPU.as_ptr() as usize;
    (crate::arch::percpu() - base) / core::mem::size_of::<PerCpu>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::Ordering;

    /// The boot core set up its per-CPU pointer, and `current()` reaches a real block.
    ///
    /// Single-core still, so the boot core is id 0 and its block is `PERCPU[0]`.
    #[test_case]
    fn boot_cpu_percpu_is_reachable() {
        assert_eq!(id(), 0);
        assert_eq!(
            current() as *const PerCpu,
            &PERCPU[0] as *const PerCpu,
            "current() does not point at the boot core's block"
        );
    }

    /// The lock-rank bookkeeping lives in the per-CPU block now.
    ///
    /// A full lock cycle is exercised by the rank tests in sync.rs; this just proves the storage
    /// moved and reads back coherently. Between tests we hold nothing.
    #[test_case]
    fn held_rank_lives_in_the_percpu_block() {
        assert_eq!(current().held_rank.load(Ordering::Relaxed), rank::NONE);
    }
}
