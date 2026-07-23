//! The kernel's lock.
//!
//! # The deadlock this exists to prevent
//!
//! A plain spinlock in a kernel that takes interrupts is a hang waiting for a schedule.
//! On **one core**:
//!
//! ```text
//!   kernel code:  ALLOCATOR.lock()   <- acquired
//!                 ...working...
//!        TIMER INTERRUPT
//!   handler:      ALLOCATOR.lock()   <- spins
//!                                       spins waiting for a lock that only the code it
//!                                       interrupted can release, and that code cannot
//!                                       run until the handler returns.
//!                                       Dead. Permanently. On one core.
//! ```
//!
//! This is not a race. It is a **guaranteed** hang the moment the timing lines up, and it
//! looks exactly like the mystery we lost two hours to in milestone 3.
//!
//! The fix: **mask interrupts for as long as the lock is held.** The interrupt cannot fire,
//! so it cannot try to take the lock. This is Linux's `spin_lock_irqsave`.
//!
//! # Two orderings that are the entire point
//!
//! **Acquire: mask interrupts FIRST, then take the lock.** The other order leaves a window
//! where we hold the lock with interrupts still enabled, which is precisely the deadlock.
//!
//! **Release: drop the lock FIRST, then restore interrupts.** The other order leaves a
//! window where interrupts are live and we still hold the lock. Same deadlock, arrived at
//! from the other side.
//!
//! Both windows are one or two instructions wide. Both are fatal. Both are the kind of bug
//! that works fine in testing for months.
//!
//! # Restore, do not enable
//!
//! [`IrqSafeGuard`] restores the interrupt state that was in effect when the lock was
//! taken. It does **not** simply enable interrupts on release.
//!
//! The difference matters when a lock is taken inside a context that already had interrupts
//! masked (an interrupt handler, or an outer lock). Blindly enabling on release would unmask
//! interrupts *inside an interrupt handler*, and the resulting fault is one you will not
//! enjoy explaining. This is why Linux's is called `irqsave`/`irqrestore`.
//!
//! See notes/locking.md and DECISIONS.md §9.

use crate::arch::interrupts;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::Ordering;

/// # Lock ranking: the rule from DECISIONS.md §9, enforced by the machine
///
/// > Two locks? Define a global order and always take them in it. Otherwise **AB-BA
/// > deadlock**, which is a *real* race and far nastier than the interrupt deadlock this
/// > type's other half prevents.
///
/// We wrote that rule and then relied on discipline, which is to say on remembering. Now
/// every lock carries a **rank**, and the rule is:
///
/// > **You may only acquire a lock strictly LOWER than everything you currently hold.**
///
/// If every acquisition strictly decreases the rank, a cycle is **unrepresentable**. Not
/// unlikely. Impossible. That is why this is *prevention* and not *detection*: it kills the
/// circular-wait condition outright ([notes/deadlock.md](../notes/deadlock.md)), rather than
/// building a graph and hunting for cycles the way Linux's `lockdep` does.
///
/// FreeBSD (WITNESS) and Solaris use the same mechanism, for the same reason: it costs three
/// instructions and it cannot be wrong.
///
/// ## The hierarchy
///
/// ```text
///   60  SCHED           the run queue
///        |
///   55  STACK_VA        free thread-stack addresses
///        |
///   50  HEAP, SLAB      the allocators
///        |
///   30  FRAMES, RAM     the physical memory map
///        |
///   20  GIC             the interrupt controller
///        |
///   10  CONSOLE         the leaf: everyone may take it, it takes nothing
/// ```
///
/// Two locks at the **same** rank may never be nested (`R < R` is false), which is exactly
/// right: equal rank means we have declared no order between them, so nesting them would be
/// choosing one at random.
///
/// The nestings this permits, and they are the ones that actually happen:
///
/// - **SLAB (50) → FRAMES (30)**: a size class runs dry and takes a page from the frame
///   allocator, while holding its own lock.
/// - **anything → CONSOLE (10)**: a panic prints while holding a lock. Which is why the
///   console must be the leaf, and why it takes nothing itself.
///
/// ## A design this would have caught
///
/// `memory::ram_regions()` used to be an iterator that held the RAM lock while the caller
/// iterated. `mmu::map_everything` iterates it *and allocates frames inside the loop* — so it
/// would have held RAM (30) while taking FRAMES (30), and `30 < 30` is false. The ranking
/// would have failed it on the spot. (We happened to fix it for other reasons first.)
pub mod rank {
    /// The scheduler's run queue.
    ///
    /// **Above the allocators**, because `spawn` pushes into a `VecDeque` while holding it and
    /// that push may allocate. `schedule()` itself never allocates (it pops one and pushes one,
    /// so the deque cannot grow), which is what makes it safe to call from the timer interrupt
    /// where DECISIONS.md §9 forbids allocation.
    pub const SCHED: u32 = 60;

    /// Untyped memory regions (milestone 11). **Above the allocators**, because creating a region
    /// grows a `Vec` (a heap allocation) while the lock is held. Below the scheduler, so it may be
    /// taken from a syscall that has no scheduler business.
    pub const UNTYPED: u32 = 58;

    /// The virtio transport table (DMA confinement). Above the allocators for the same reason as
    /// `UNTYPED`: registering a device grows a `Vec` under the lock. Taken from a syscall with no
    /// other lock held.
    pub const VIRTIO: u32 = 56;

    /// The free list of thread-stack virtual addresses.
    ///
    /// **Above the allocators** (it pushes into a `Vec`, which may allocate) and **below the
    /// scheduler** (a `KernelStack`'s `Drop` runs from `reap`, which holds SCHED).
    pub const STACK_VA: u32 = 55;

    pub const HEAP: u32 = 50;
    pub const SLAB: u32 = 50;

    /// The kernel's own page tables (`mmu::map_page` / `unmap_page`).
    ///
    /// Single-core, `kernel_mapper()` needed no lock: the callers happened not to race. SMP breaks
    /// that (two cores spawning threads both mutate the shared TTBR1 tables), so mapping is now
    /// serialized. **Below the scheduler** (a `KernelStack`'s `Drop` unmaps from under `reap`, which
    /// holds SCHED) and **below STACK_VA** (a stack's `new` maps pages), and **above the allocators**
    /// (mapping allocates intermediate page-table frames). See DECISIONS.md §11.
    pub const KERNEL_MMU: u32 = 45;

    pub const FRAMES: u32 = 30;
    pub const RAM: u32 = 30;

    /// The interrupt controller.
    ///
    /// Taken by the IRQ handler, which by our own rule (DECISIONS.md §9) holds nothing and
    /// allocates nothing. So it can sit low, just above the console: the handler may still
    /// `println!` a diagnostic while holding it.
    pub const GIC: u32 = 20;

    pub const CONSOLE: u32 = 10;

    /// Holding nothing.
    pub const NONE: u32 = u32::MAX;
}

/// The lowest rank currently held is kept in **this core's per-CPU block**
/// (`cpu::current().held_rank`), not a global.
///
/// It used to be a single `static`. That was correct on one core and a bug on two: a second
/// core taking a lock would clobber the first core's held-rank, and the ranking would start
/// reporting violations that never happened, which is worse than not checking. Moving it
/// per-CPU (DECISIONS.md §11, step 1) fixes that. It is still only ever touched by its owning
/// core with interrupts masked, so the atomic is for interior mutability, not synchronization.
///
/// What is the lowest-ranked lock we currently hold? Test support.
#[allow(dead_code)] // used by the tests, and by anyone debugging a lock-order violation
pub fn current_rank() -> u32 {
    crate::cpu::current().held_rank.load(Ordering::Relaxed)
}

/// Would taking a lock of this rank violate the hierarchy? Test support.
///
/// Exists because the violation itself is an `assert!`, and an assert in a kernel test is a
/// dead kernel. This lets the tests check the *predicate* without pulling the trigger.
#[allow(dead_code)]
pub fn would_violate(rank: u32) -> bool {
    rank >= current_rank()
}

/// Forget everything we thought we held.
///
/// # Safety
///
/// **Panic and fault paths only**, alongside `console::force_unlock`.
///
/// If we panic while holding the console lock (rank 10), then this core's held-rank is 10, and
/// the panic handler's own attempt to print would try to take rank 10 again. `10 < 10` is
/// false, so the ranking would fire a *lock-order violation panic inside the panic handler*, and
/// we would lose the original message to a recursive panic.
///
/// The bookkeeping is a debugging aid. It must never be the thing that stops us saying what
/// went wrong. Resets only the calling core's block, which is exactly right: a fault is handled
/// on the core that took it.
pub unsafe fn force_reset_ranks() {
    crate::cpu::current().held_rank.store(rank::NONE, Ordering::Relaxed);
}

/// A spinlock that masks interrupts while it is held, and enforces a global lock order.
///
/// **Every lock in the kernel should be one of these.** See the discipline in
/// DECISIONS.md §9, particularly: keep the critical section short, because interrupts are
/// off for the whole of it.
pub struct IrqSafeMutex<T> {
    inner: spin::Mutex<T>,
    rank: u32,
}

// SAFETY: same reasoning as any mutex. The lock provides the exclusion.
unsafe impl<T: Send> Sync for IrqSafeMutex<T> {}
unsafe impl<T: Send> Send for IrqSafeMutex<T> {}

impl<T> IrqSafeMutex<T> {
    /// `rank` comes from [`rank`]. See that module: the number is not decoration, it is the
    /// thing that makes an AB-BA deadlock unrepresentable.
    pub const fn new(rank: u32, value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
            rank,
        }
    }

    pub fn lock(&self) -> IrqSafeGuard<'_, T> {
        // ORDER: mask first, THEN acquire. Reversing these reintroduces the deadlock.
        let irqs_were_enabled = interrupts::disable();

        // From here to the matching restore in `drop`, interrupts are off, so this core is the
        // only thing that can touch its own held-rank.
        let held_rank = &crate::cpu::current().held_rank;
        let held = held_rank.load(Ordering::Relaxed);

        assert!(
            self.rank < held,
            "LOCK ORDER VIOLATION: taking a rank-{} lock while holding rank {}. \
             Locks must be acquired in strictly decreasing rank. See kernel/src/sync.rs.",
            self.rank,
            held,
        );

        held_rank.store(self.rank, Ordering::Relaxed);

        IrqSafeGuard {
            guard: ManuallyDrop::new(self.inner.lock()),
            irqs_were_enabled,
            previous_rank: held,
        }
    }

    /// Break the lock open, whoever holds it.
    ///
    /// # Safety
    ///
    /// **For the panic and fault paths only, and nothing else, ever.**
    ///
    /// If we panic while holding the console lock (a fault taken in the middle of a
    /// `println!`, say), then the panic handler's own attempt to print would take that same
    /// lock and hang. We would lose the one message that mattered, at the exact moment we
    /// needed it. Linux does the same thing and calls it `bust_spinlocks`.
    ///
    /// The caller must accept that whatever the previous holder was doing is now
    /// half-finished, and that its data may be inconsistent. That is an acceptable trade
    /// when the alternative is a silent hang, and an unacceptable one at any other time.
    pub unsafe fn force_unlock(&self) {
        unsafe { self.inner.force_unlock() }
    }
}

pub struct IrqSafeGuard<'a, T> {
    guard: ManuallyDrop<spin::MutexGuard<'a, T>>,
    irqs_were_enabled: bool,
    previous_rank: u32,
}

impl<T> Drop for IrqSafeGuard<'_, T> {
    fn drop(&mut self) {
        // ORDER: release the lock, THEN restore interrupts. Reversing these leaves a window
        // where an interrupt can fire while we still hold the lock. Same deadlock.
        //
        // SAFETY: we drop the guard exactly once, here, and never touch it again.
        unsafe { ManuallyDrop::drop(&mut self.guard) };

        // RESTORE the rank we found, not `NONE`. Exactly the same reasoning as the interrupt
        // state one line below: a lock released inside an outer lock must not report that we
        // are now holding nothing. Interrupts are still masked here (restored below), so this
        // core still owns its per-CPU block.
        crate::cpu::current().held_rank.store(self.previous_rank, Ordering::Relaxed);

        // RESTORE, not enable. See the module docs.
        interrupts::restore(self.irqs_were_enabled);
    }
}

impl<T> Deref for IrqSafeGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for IrqSafeGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the kernel's lock.
    //!
    //! `irq_safe_mutex_restores_rather_than_enables` is the important one, and it was verified
    //! against a deliberately broken `restore()`: it fails with "dropping the guard ENABLED
    //! interrupts inside an IRQ-disabled context". A test that cannot fail is not a test.

    /// The lock must mask interrupts for as long as it is held.
    ///
    /// If it doesn't, a timer interrupt can land inside a critical section, try to take the
    /// same lock, and spin forever waiting for code that cannot run until it returns. On one
    /// core. Permanently. See notes/locking.md.
    #[test_case]
    fn irq_safe_mutex_masks_interrupts_while_held() {
        use crate::arch::interrupts;
        use crate::sync::{IrqSafeMutex, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 7);

        interrupts::enable();
        assert!(interrupts::enabled(), "test setup: IRQs should be on");

        {
            let guard = M.lock();
            assert_eq!(*guard, 7);
            assert!(
                !interrupts::enabled(),
                "IRQs are still live while the lock is held: this is the deadlock"
            );
        }

        assert!(
            interrupts::enabled(),
            "IRQs were not restored after the guard dropped"
        );
    }

    /// **The important one.** The guard must RESTORE the previous state, not enable.
    ///
    /// A lock taken inside a context that already had interrupts masked (an interrupt
    /// handler, or inside an outer lock) must not unmask them on release. Blindly enabling
    /// would turn interrupts back on *inside an interrupt handler*, and the resulting fault
    /// is one you will not enjoy explaining.
    ///
    /// This is exactly why Linux's is called `irqsave`/`irqrestore` rather than
    /// `irqoff`/`irqon`, and it is the single easiest thing to get wrong here.
    #[test_case]
    fn irq_safe_mutex_restores_rather_than_enables() {
        use crate::arch::interrupts;
        use crate::sync::{IrqSafeMutex, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        // Pretend we are inside an interrupt handler: IRQs already masked.
        let outer = interrupts::disable();
        assert!(!interrupts::enabled());

        {
            let _guard = M.lock();
            assert!(!interrupts::enabled());
        }

        assert!(
            !interrupts::enabled(),
            "dropping the guard ENABLED interrupts inside an IRQ-disabled context"
        );

        interrupts::restore(outer);
    }

    /// Nesting must not corrupt the state either.
    #[test_case]
    fn nested_locks_restore_correctly() {
        use crate::arch::interrupts;
        use crate::sync::{IrqSafeMutex, rank};

        // A is taken first, so A must OUTRANK B. Before the ranking existed this test could
        // nest any two locks in any order; now the hierarchy is part of the type's contract and
        // the test has to declare which one is the outer.
        static A: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::SLAB, 1);
        static B: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 2);

        interrupts::enable();

        {
            let a = A.lock();
            assert!(!interrupts::enabled());
            {
                let b = B.lock();
                assert!(!interrupts::enabled());
                assert_eq!(*a + *b, 3);
            }
            // The INNER guard dropped. It must not have re-enabled interrupts, because the
            // outer one is still held.
            assert!(
                !interrupts::enabled(),
                "the inner guard re-enabled IRQs while the outer lock is still held"
            );
        }

        assert!(interrupts::enabled(), "the outer guard failed to restore");
    }

    // --- lock ranking (DECISIONS.md §9) ---

    /// Holding nothing means anything may be taken.
    #[test_case]
    fn holding_nothing_permits_any_rank() {
        use crate::sync::{current_rank, rank, would_violate};

        assert_eq!(current_rank(), rank::NONE, "a previous test leaked a lock");
        assert!(!would_violate(rank::HEAP));
        assert!(!would_violate(rank::CONSOLE));
    }

    /// The rank tracker follows the locks.
    #[test_case]
    fn taking_a_lock_records_its_rank() {
        use crate::sync::{IrqSafeMutex, current_rank, rank};

        static M: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        assert_eq!(current_rank(), rank::NONE);
        {
            let _g = M.lock();
            assert_eq!(current_rank(), rank::FRAMES);
        }
        assert_eq!(
            current_rank(),
            rank::NONE,
            "the guard did not restore the rank"
        );
    }

    /// **The rule.** While holding a lock, you may only take a strictly LOWER one.
    ///
    /// If every acquisition strictly decreases, a cycle is *unrepresentable*. Not unlikely.
    /// Impossible. That is why this is prevention rather than detection: it destroys the
    /// circular-wait condition outright (notes/deadlock.md), instead of building a graph and
    /// hunting for cycles the way Linux's lockdep does.
    #[test_case]
    fn the_hierarchy_permits_only_strictly_decreasing_ranks() {
        use crate::sync::{IrqSafeMutex, rank, would_violate};

        static FRAMES: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        let _g = FRAMES.lock();

        // Lower: fine. This is the panic path printing while holding the allocator.
        assert!(
            !would_violate(rank::CONSOLE),
            "console must be takeable from anywhere"
        );

        // Higher: forbidden. Taking the heap while holding frames is the other half of an
        // AB-BA deadlock waiting to be written.
        assert!(
            would_violate(rank::HEAP),
            "rank 50 while holding rank 30 must be refused"
        );

        // EQUAL: also forbidden, and this is the subtle one. Same rank means we have declared
        // no order between the two locks, so nesting them would be choosing one at random.
        assert!(
            would_violate(rank::RAM),
            "two locks of equal rank must never nest: we never said which comes first"
        );
    }

    /// Nesting restores the OUTER rank, not `NONE`.
    ///
    /// Exactly the same shape as the interrupt save/restore two lines away in `drop`, and wrong
    /// in exactly the same way if you get it wrong: releasing an inner lock must not report
    /// that we are now holding nothing, or the next acquisition would be checked against the
    /// wrong ceiling.
    #[test_case]
    fn releasing_an_inner_lock_restores_the_outer_rank() {
        use crate::sync::{IrqSafeMutex, current_rank, rank};

        static OUTER: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::SLAB, 0);
        static INNER: IrqSafeMutex<u32> = IrqSafeMutex::new(rank::FRAMES, 0);

        let _o = OUTER.lock();
        assert_eq!(current_rank(), rank::SLAB);
        {
            let _i = INNER.lock();
            assert_eq!(current_rank(), rank::FRAMES);
        }
        assert_eq!(
            current_rank(),
            rank::SLAB,
            "dropping the inner guard reported that we hold nothing, while the outer lock is \
             still held"
        );
    }

    /// The one real nesting in the kernel actually happens, and is legal.
    ///
    /// A slab size class runs dry and takes a page from the frame allocator **while holding its
    /// own lock**. SLAB (50) → FRAMES (30). If that were ever inverted, this would fire.
    #[test_case]
    fn the_slab_may_take_a_frame_while_holding_its_own_lock() {
        use alloc::boxed::Box;

        // Force a class to run dry: allocate enough 2 KiB objects to exhaust a page (two per
        // page) and demand another. If SLAB -> FRAMES were a violation, the assert in
        // `IrqSafeMutex::lock` would kill us right here.
        let mut keep = alloc::vec::Vec::new();
        for _ in 0..8 {
            keep.push(Box::new([0u8; 2048]));
        }
        core::hint::black_box(&keep);
    }
}
