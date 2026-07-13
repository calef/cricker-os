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

/// A spinlock that masks interrupts while it is held.
///
/// **Every lock in the kernel should be one of these.** See the discipline in
/// DECISIONS.md §9, particularly: keep the critical section short, because interrupts are
/// off for the whole of it.
pub struct IrqSafeMutex<T> {
    inner: spin::Mutex<T>,
}

// SAFETY: same reasoning as any mutex. The lock provides the exclusion.
unsafe impl<T: Send> Sync for IrqSafeMutex<T> {}
unsafe impl<T: Send> Send for IrqSafeMutex<T> {}

impl<T> IrqSafeMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
        }
    }

    pub fn lock(&self) -> IrqSafeGuard<'_, T> {
        // ORDER: mask first, THEN acquire. Reversing these reintroduces the deadlock.
        let irqs_were_enabled = interrupts::disable();

        IrqSafeGuard {
            guard: ManuallyDrop::new(self.inner.lock()),
            irqs_were_enabled,
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
}

impl<T> Drop for IrqSafeGuard<'_, T> {
    fn drop(&mut self) {
        // ORDER: release the lock, THEN restore interrupts. Reversing these leaves a window
        // where an interrupt can fire while we still hold the lock. Same deadlock.
        //
        // SAFETY: we drop the guard exactly once, here, and never touch it again.
        unsafe { ManuallyDrop::drop(&mut self.guard) };

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
