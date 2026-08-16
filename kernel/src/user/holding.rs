//! **What a test-boot service holds, and the one call that hands it back.**
//!
//! # The problem this exists for
//!
//! A boot has one pool of physical frames. The aarch64 test suite builds a service, proves
//! something with it, and moves on; the service is still there when the next test starts, and every
//! frame it was given is spoken for until the machine stops. Measured on 2026-08-16, the suite
//! finished a boot with **216 free frames of the 29307 it started with, and no free run longer than
//! 106**, which is why an ordinary allocation began failing as `Unmappable(OutOfFrames)` in
//! whichever test happened to spawn last, about one run in three. Three milestones were misled by
//! that symptom, because the test that reports the failure is never the test that caused it.
//! notes/frames.md carries the ledger.
//!
//! # No new mechanism, and that is the point
//!
//! Everything here is DECISIONS §16 object revocation, already built and already proved:
//!
//! - [`crate::sched::kill_thread`] arms §16's kill on a thread by name (what `Untyped::DESTROY`
//!   does to a whole region, for one thread).
//! - [`crate::sched::reclaim_region`] tears every kernel object out of an untyped region, unpins it,
//!   and returns the memory. A first call that finds a live thread in the region **arms the kill on
//!   it and refuses**; the retry reclaims. `force_kill_tests` proves that loop on a runaway.
//! - A dead thread's address space dies with it (the `Drop` chain), which returns the region behind
//!   it without anyone naming that region at all.
//!
//! So a `Holding` is bookkeeping: remember what was handed out, and spend the existing verbs on it in
//! the order that works. There is no new syscall and no new kernel object.
//!
//! # BUGS
//!
//! - **A thread that only ever blocks cannot be reclaimed this way**, and the limit is §16's rather
//!   than this type's. The armed kill is spent by `schedule()`, which a `Blocked` thread never
//!   reaches, so [`Holding::release`](crate::user::holding::Holding::release) can end a thread that
//!   runs and cannot end one parked in `RECV` for good. What rescues the common case is that reclaiming a region **removes the endpoints
//!   inside it and wakes their waiters with an aborted IPC**, so a service blocked on an endpoint
//!   its own budget paid for does die. A service blocked on an endpoint from somewhere else does
//!   not, and [`Holding::release`](crate::user::holding::Holding::release) returns `false` rather than pretending: see
//!   `reap_region_objects`'s own note ("the cooperative tier's job").
//! - **`release` is destructive and cannot be used as a question.** Its first `reclaim_region` arms
//!   kills, so calling it to find out whether a service is idle ends the service. That is
//!   `reclaim_region`'s recorded BUGS entry, inherited here.
//! - **A holding names what the kernel handed out, not what the process built.** A process that
//!   retypes objects out of its own budget is covered (they are in the region); one that reaches
//!   memory some other way is not, and nothing here can see the difference.
//!
//! Name: **provisional** (this lane, 2026-08-16). `Holding` is a noun and says what the thing is:
//! what a service holds. It is deliberately *not* `Lease`, which reads better in the abstract and
//! would collide head-on with the DHCP lease this file's biggest caller talks about on every other
//! line (`notes/net.md`, `start_smb_serve`'s `(lease, smb)`). Crate, module and type names are
//! calef's call; expect this one to change.

use crate::sched;
use crate::thread::Tid;

/// The most threads one holding covers. Three is the widest service this boot builds (a `net_stack`
/// with its socket client and the SMB adapter beside it); a fourth is a design change rather than a
/// bigger number, so this fails the build instead of silently dropping one.
const MAX_THREADS: usize = 4;

/// The most untyped regions one holding covers, in **each** phase. The widest service this boot
/// builds is `net_stack` with two clients, which is three endpoint regions before death and six
/// budget-and-stack regions after it; eight leaves a little room and fails the build rather than
/// dropping one silently.
const MAX_REGIONS: usize = 8;

/// How long [`Holding::release`] waits for a teardown to land, in seconds of wall clock.
///
/// Wall clock rather than a yield count, for DECISIONS §28's reason: a killed thread is converted
/// to a corpse by **its own core's** next timer tick, and with threads scattered across cores a
/// hundred yields on this core can all complete inside one such tick. Two seconds is roughly two
/// hundred ticks, far beyond any honest teardown and far under the 60 s hang watchdog, so a
/// genuinely stuck service reports `false` instead of hanging the run.
const RELEASE_SECS: u64 = 2;

/// Reclaim one region, retrying until it lands or the deadline passes.
///
/// The retry is the mechanism, not politeness: the first pass is what **arms** §16's kill on a live
/// resident, and only a later pass can find it converted. `force_kill_tests` proves the same loop on
/// a runaway.
fn reclaim(region: u64, deadline: u64) -> bool {
    while crate::arch::timer::now() < deadline {
        if sched::reclaim_region(region).is_ok() {
            return true;
        }
        sched::yield_now();
    }
    false
}

/// A service's memory, remembered so it can be handed back. See the module note.
pub struct Holding {
    threads: [Option<Tid>; MAX_THREADS],
    regions: [Option<u64>; MAX_REGIONS],
    after: [Option<u64>; MAX_REGIONS],
}

impl Holding {
    pub const fn new() -> Self {
        Holding {
            threads: [None; MAX_THREADS],
            regions: [None; MAX_REGIONS],
            after: [None; MAX_REGIONS],
        }
    }

    /// Remember a thread this service runs, so [`release`](Self::release) can end it.
    ///
    /// # Panics
    /// If more than [`MAX_THREADS`] are added, because a silently dropped thread is a leak that
    /// looks like a fix.
    pub fn thread(mut self, tid: Tid) -> Self {
        let slot = self
            .threads
            .iter_mut()
            .find(|s| s.is_none())
            .expect("a holding holds at most MAX_THREADS threads");
        *slot = Some(tid);
        self
    }

    /// Remember an untyped region the kernel carved for this service, to be reclaimed **while the
    /// service's threads still exist**. That is the ordinary case and it is the phase that does the
    /// waking: reclaiming a region removes the endpoints inside it and aborts whoever is blocked on
    /// them, which is how a service parked in `RECV` becomes schedulable enough to die.
    ///
    /// Regions are reclaimed in the order they were added, which matters when one was `SPLIT` from
    /// another: the child must go first, because a parent with live children refuses.
    ///
    /// # Panics
    /// If more than [`MAX_REGIONS`] are added; see [`thread`](Self::thread).
    pub fn region(mut self, region: u64) -> Self {
        let slot = self
            .regions
            .iter_mut()
            .find(|s| s.is_none())
            .expect("a holding holds at most MAX_REGIONS regions");
        *slot = Some(region);
        self
    }

    /// Remember a region that may only be reclaimed **once every thread is gone**: memory a live
    /// thread is standing on.
    ///
    /// The case that forces the distinction is a process's **stack**. Stack pages arrive through
    /// `Spawn::maps`, which is `AddressSpace::map_physical`, which deliberately does not record the
    /// mapping (the page is shared or a device's, and the address space does not own it; see
    /// notes/frames.md). So §13's `revoke_region` cannot unmap them, and freeing that region while
    /// the thread is still executing hands its live stack back to the allocator: a use-after-free
    /// with a window as long as one timer tick, which is exactly long enough for another process to
    /// be handed the page.
    ///
    /// # Panics
    /// If more than [`MAX_REGIONS`] are added; see [`thread`](Self::thread).
    pub fn region_after_death(mut self, region: u64) -> Self {
        let slot = self
            .after
            .iter_mut()
            .find(|s| s.is_none())
            .expect("a holding holds at most MAX_REGIONS after-death regions");
        *slot = Some(region);
        self
    }

    /// **Hand it all back.** Arm the kill on every thread, reclaim every region (retrying, because
    /// the first pass is what arms the kill on residents), then wait for the threads to actually go.
    ///
    /// Returns whether everything came back. A `false` is worth asserting on in a test: it means a
    /// service outlived its holding, which is the leak this type exists to remove, and a caller that
    /// ignores it has an instrument that reports success either way.
    pub fn release(&self) -> bool {
        // Arm first, so that a thread woken by the region reclaim below is already doomed and
        // cannot get further work done with capabilities that are about to go stale.
        for tid in self.threads.iter().flatten() {
            sched::kill_thread(*tid);
        }

        let deadline = crate::arch::timer::now() + RELEASE_SECS * crate::arch::timer::frequency();
        let mut all = true;

        for region in self.regions.iter().flatten() {
            all &= reclaim(*region, deadline);
        }

        // Then wait for the threads, because a thread's own address space (and the region behind it)
        // is freed by the `Drop` chain at reap, and the frame ledger reads the count right after this
        // returns. Waiting for the name to stop resolving is what makes that reading honest.
        for tid in self.threads.iter().flatten() {
            let mut gone = false;
            while crate::arch::timer::now() < deadline {
                if !sched::thread_present(*tid) {
                    gone = true;
                    break;
                }
                sched::yield_now();
            }
            all &= gone;
        }

        // And only now the memory a live thread was standing on. See `region_after_death`: a stack
        // is not a recorded mapping, so revocation cannot pull it, and freeing it under a thread that
        // is still running is a use-after-free rather than a fault.
        for region in self.after.iter().flatten() {
            all &= reclaim(*region, deadline);
        }

        all
    }

    /// [`release`](Self::release), and fail the run if anything did not come back.
    ///
    /// The form every test should use, because the alternative is an instrument that reports success
    /// either way. `what` names the service, so the failure says which one outlived its holding
    /// rather than only that something did.
    pub fn release_or_fail(&self, what: &str) {
        assert!(
            self.release(),
            "{what} could not hand its memory back. A thread it started is still alive, or a region \
             still refuses: a service blocked on an endpoint that is not in any region this holding \
             names cannot be woken, and so cannot spend the kill (see this module's BUGS). This is \
             the leak rather than a flake. Left alone it takes the boot's free frames down until an \
             innocent later test fails as Unmappable(OutOfFrames); notes/frames.md.",
        );
    }
}
