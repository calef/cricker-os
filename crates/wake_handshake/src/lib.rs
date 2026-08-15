//! **The scheduler's block/wake handshake**: the per-thread state machine that decides when a
//! blocked thread may become runnable, lifted out of `kernel/src/sched.rs` so loom can explore
//! every interleaving of it on the host (milestone 80's method, second retrofit after
//! `steal_request`).
//!
//! # What this is, and what it is not
//!
//! Every field here is written **under the scheduler's `SCHED` lock**. That is worth saying in the
//! first breath, because it is the opposite of what "extract the atomic protocol" usually means:
//! there are no hand-rolled atomics in this protocol and no orderings to tune. The concurrency is
//! in the *gaps between critical sections*. A thread that marks itself `Blocked` releases `SCHED`
//! and keeps running until its core's `switch_to` saves its context; a waker on another core can
//! take `SCHED` inside that window. Three real races have come out of that window, and each one is
//! a rule in this type:
//!
//! - **Wake-before-switch-out** (found by a 2-in-10 flake; notes/intrusive-queues.md). A waker that
//!   queues a thread still standing on its CPU lets another core switch into a stale context: two
//!   cores in one thread. Rule: [`try_wake`](Handshake::try_wake) finding
//!   [`on_cpu`](Handshake::on_cpu) set parks the wake in
//!   [`wake_pending`](Handshake::wake_pending) instead,
//!   and the thread's own core completes it in [`finish_switch`](Handshake::finish_switch) once the
//!   context is provably saved.
//! - **The undelivered wake.** A wake that delivers nothing would complete a parked `RECV` off a
//!   stale mailbox, with the TCB still linked on its endpoint's wait queue. Rule: the
//!   undelivered-wake gate. `try_wake` refuses to make a waiting thread `Ready` unless the waker's
//!   own critical section delivered something ([`ipc_served`](Handshake::ipc_served)) or aborted
//!   the wait ([`ipc_aborted`](Handshake::ipc_aborted)).
//!
//!   **This one is hardening rather than a fix for an observed failure, and the correction belongs
//!   here because this bullet is what a reader trusts.** It was built against the VisionFive 2's
//!   boot 8 (2026-08-14), read at the time as a boot thread taking a `wake:0x0` on a boot where no
//!   sender to its endpoint existed. The boot-8 lane's **fifth** bench stop (2026-08-15) re-read
//!   those dumps and overturned that: the wake was the worker's real send, and the state read as a
//!   stranded receiver was the terminal state of a completed tour. **The gate has never fired on a
//!   field failure, and `refuse:` has never appeared on a board ring.** It stays because the
//!   transition it forbids is genuinely unsafe and the loom harness below proves that, which is a
//!   property rather than an anecdote. The fifth stop's own write-up is still in flight in that
//!   lane, so no section is cited for it yet.
//! - **A steal racing a switch-out.** A preempted thread sits `Ready` in its core's run queue with
//!   `on_cpu` still set until that core's `finish_switch` runs; a work steal
//!   (`sched::serve_steal_request`) hands queued threads to other cores. The single-owner run-queue
//!   discipline (only the owning core pops, at scheduler entries that come after `finish_switch`)
//!   is the whole reason the thief never receives a thread whose context is still being saved.
//!   Rule: [`switch_in`](Handshake::switch_in) asserts `on_cpu` is clear, so the discipline has a
//!   tripwire at the one place every path to a CPU passes through. (The phrase "steal-then-STOP"
//!   has been used for this family; the nearest recorded artifacts are this edge and
//!   `steal_request`'s BUGS entry about a claimed slot a stopped victim never serves, which is a
//!   liveness question outside both crates' models.)
//!
//! # The division of labor with the kernel
//!
//! The kernel **calls** these transitions; it does not mirror them. The regions-crate precedent
//! (DECISIONS §16's extraction rule): a proof over a copy proves the copy. `Thread` embeds a
//! [`Handshake`] and `sched.rs` goes through these methods at every block, wake, switch and
//! finish-switch site. What stays kernel-side, on purpose:
//!
//! - **The queues.** Run queues, migration inboxes and endpoint wait queues are intrusive pointer
//!   structures under their own synchronization (notes/intrusive-queues.md). The verdict enums
//!   ([`WakeVerdict`], [`SwitchOutVerdict`]) tell the kernel when to push; the push itself, the
//!   trace ring, the SGI and the placement policy are the caller's.
//! - **The out-of-protocol state writes.** Spawn making a thread `Ready`, `depart` marking
//!   `Finished`/`Dead`, `START` promoting an `Embryo`, the killed-thread conversion, and
//!   `schedule()`'s keep-current heal all write [`state`](Handshake::state) directly. They are
//!   `SCHED`-held single-thread transitions with no cross-core window, and folding them in here
//!   would grow the model without growing the checked surface.
//! - **The self-switch refusal and the reply-role check** (boot 8's companions). Both read queue or
//!   capability state this type cannot see (`schedule()`'s popped tid, `ipc_reply`'s
//!   `WaitRole::Reply` match on [`wait_on`](Handshake::wait_on)'s payload).
//!
//! # EXAMPLES
//!
//! One rendezvous, as the kernel spells it, with the waker racing the receiver's switch-out:
//!
//! ```
//! use wake_handshake::{Handshake, RunState, SwitchOutVerdict, WakeVerdict};
//!
//! // The receiver, running, parks itself in ipc_recv (under SCHED):
//! let mut hs: Handshake<(u64, char)> = Handshake::on_cpu_now();
//! hs.park((0, 'r')); // Blocked, waiting on endpoint 0 as a receiver, nothing delivered yet
//!
//! // A spurious wake, nothing delivered: refused, the receiver stays parked (boot 8's gate).
//! assert_eq!(hs.try_wake(), WakeVerdict::Refused);
//! assert_eq!(hs.state, RunState::Blocked);
//!
//! // A sender stages the mailbox and wakes, but the receiver's core has not switched away yet
//! // (on_cpu is still set): the wake is deferred, not queued.
//! hs.serve();
//! assert_eq!(hs.try_wake(), WakeVerdict::Deferred);
//!
//! // The receiver's core finishes the switch: the context is saved, the deferred wake completes,
//! // and the caller queues the thread.
//! assert_eq!(hs.finish_switch(), SwitchOutVerdict::WakeCompleted);
//! assert_eq!(hs.state, RunState::Ready);
//!
//! // Some core pops it and switches in; the recv tail's tripwire holds.
//! hs.switch_in();
//! assert!(hs.delivered());
//! ```
//!
//! # BUGS
//!
//! - **Nothing makes a new block path use [`park`](Handshake::park).** The fields are public,
//!   because the kernel has legitimate out-of-protocol writers (listed above), so a future block
//!   site that writes `state = Blocked` by hand opts out of the gate silently. This is the same
//!   limitation notes/scheduler.md records for `wait_on`, one level down. The lint-shaped fix
//!   (private fields plus named methods for every out-of-protocol writer) was considered and not
//!   taken: it would add a dozen passthroughs whose names would imply protocol membership they do
//!   not have.
//! - **A stale [`ipc_aborted`](Handshake::ipc_aborted) weakens the gate for the next park.**
//!   [`park`](Handshake::park) clears `ipc_served` (matching the kernel's block sites) but not
//!   `ipc_aborted`, because the kernel's syscall layer owns that flag through `take_ipc_aborted`
//!   and clearing it here would eat the error. A kernel thread that blocks again without collecting
//!   an abort therefore parks with the gate already half-open. Every current kernel-side caller
//!   either collects the flag or never has it set; nothing enforces that for the next one.
//! - **The loom model checks this protocol under the kernel's locking discipline, not the
//!   discipline itself.** `SCHED` is modelled as a mutex and the run queue as a slot under it; that
//!   the kernel really does take `SCHED` at every one of these sites, really does keep run queues
//!   single-owner, and really does run `finish_switch` before the next scheduler entry is
//!   established by reading `sched.rs`, not by the model. See notes/interleaving.md for what the
//!   model covers and what it cannot reach.
//! - **Liveness is out of scope**, here as in `steal_request`: a refused or deferred wake depends
//!   on a later scheduler entry that loom cannot see (the reschedule SGI, the timer tick).
//!
//! Name: unrecorded. Provisional, minted 2026-08-14 (the block/wake loom extraction, the retrofit
//! the fourth bench stop's audit asked for) and not yet put to Chris. Named for the mechanism the
//! crate holds: the handshake between a waker and a thread that is parking or switching out.
//! Refused along the way: `block_wake` (two verbs; the tenet says nouns), `wake_gate` (names only
//! the boot-8 rule, one of three), `sched_state` and `run_state` (generic words that could name
//! almost anything in a scheduler), `parking` (a verb, and std's `park` vocabulary means something
//! narrower). Chris's call.

#![cfg_attr(not(test), no_std)]

/// **Where a thread is in its life**, the scheduler's own vocabulary, moved here with the
/// handshake because [`Handshake::try_wake`] and [`Handshake::finish_switch`] branch on it. The
/// kernel re-exports this as `thread::State`, so `sched.rs` reads exactly as it did.
///
/// Only `Ready`, `Running` and `Blocked` take part in the handshake. `Embryo`, `Finished` and
/// `Dead` are entered by kernel-side transitions (retype, exit, fault) and the handshake only ever
/// reads them: a wake ignores them, and a finish-switch hands a `Finished` predecessor back to the
/// kernel to reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Retyped but not yet started (milestone 19c.3): in no queue, never picked, `START` makes it
    /// `Ready`.
    Embryo,
    /// On exactly one run queue or migration inbox, waiting for a CPU.
    Ready,
    /// On a CPU. See [`Handshake::on_cpu`] for the tail end, where a thread is no longer `Running`
    /// but still standing on its core's stack.
    Running,
    /// Waiting for an IPC rendezvous that has not happened yet; on at most one endpoint wait
    /// queue. Only [`Handshake::try_wake`] leaves this state.
    Blocked,
    /// Exited; reaped by its successor's [`Handshake::finish_switch`] the moment it is off its
    /// stack.
    Finished,
    /// A supervised corpse (DECISIONS §26): never runs again, never auto-reaped, persists for its
    /// supervisor's postmortem until revocation.
    Dead,
}

/// **What a wake attempt decided.** The caller (the kernel's `wake` / `wake_load_aware`, holding
/// `SCHED`) acts on the verdict: only [`Queue`](WakeVerdict::Queue) comes with an obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeVerdict {
    /// The thread is not `Blocked`; the wake is a no-op. (A second waker lost the race, or the
    /// target is an embryo or a corpse.)
    NotBlocked,
    /// **The undelivered-wake gate fired**: the thread waits on an endpoint and this wake
    /// delivered nothing. The thread stays parked and stays linked on its endpoint's wait queue;
    /// the rendezvous that has not happened is still pending. The kernel records `refuse:tid` on
    /// the event ring, and as of 2026-08-15 that record has never appeared on a board ring: see
    /// the undelivered-wake bullet above for why the gate is hardening rather than a repair.
    Refused,
    /// The thread is still standing on its CPU (its saved context is stale), so the wake was
    /// parked in [`Handshake::wake_pending`] instead of queueing a thread another core could
    /// switch into. Its own core completes it in [`Handshake::finish_switch`]. Nothing to queue.
    Deferred,
    /// The thread is `Ready` and off every queue: **the caller must queue it now**, on whatever
    /// core its policy picks (the waker's core for a rendezvous, the least-loaded for a device
    /// IRQ).
    Queue,
}

/// **What a finished switch found in the predecessor.** Returned by
/// [`Handshake::finish_switch`], running on the incoming thread after the switch, when the
/// predecessor is provably off its stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutVerdict {
    /// The predecessor `Finished`: the caller reaps it (drops the `Thread`, freeing stack and
    /// address space). The handshake is left untouched because the thread is about to not exist.
    Reap,
    /// A wake raced the switch-out and was parked ([`WakeVerdict::Deferred`]); the context is real
    /// now, so the wake completed here: the thread is `Ready` and **the caller must queue it**, on
    /// this core. (One non-load-aware wake in the rare IRQ-in-the-window case is the accepted
    /// cost; teaching this path a placement policy was judged not worth it.)
    WakeCompleted,
    /// The ordinary case: the predecessor's context is saved and other cores may now run it.
    /// Nothing to do.
    Cleared,
}

/// One thread's block/wake handshake: the fields `sched.rs` used to carry loose on `Thread`,
/// plus the transitions that were inlined at its call sites. Generic over `W`, the "what am I
/// waiting on" payload (the kernel uses `(EpId, WaitRole)`; the tests use small scalars), because
/// the handshake only ever asks whether it is present.
///
/// Every method is written to be called **under `SCHED`**. The struct itself has no interior
/// mutability and no atomics; see the module documentation for why that is the honest shape.
#[derive(Debug)]
pub struct Handshake<W> {
    /// See [`RunState`]. Out-of-protocol transitions (spawn, exit, kill, `START`) write this
    /// directly, and that is by design; the module's BUGS section names the cost.
    pub state: RunState,

    /// **Still standing on a CPU.** Set by [`switch_in`](Self::switch_in); cleared by that core's
    /// *successor* in [`finish_switch`](Self::finish_switch), after the switch away has saved this
    /// thread's context. In the window between "released `SCHED`" and "actually off its stack",
    /// this is what tells a waker the saved context is still stale.
    pub on_cpu: bool,

    /// **A wake arrived while this thread was still switching out.** Parked here by
    /// [`try_wake`](Self::try_wake), completed by [`finish_switch`](Self::finish_switch) on the
    /// thread's own core, when the context is provably saved.
    pub wake_pending: bool,

    /// **What this thread is blocked on**, or `None` when not waiting. Written by
    /// [`park`](Self::park) in the same statement as `state = Blocked`; cleared where a wake makes
    /// the thread `Ready`. The coupling is diagnostic as well as protocol: a `Blocked` beside a
    /// `None` here is a state byte no block path wrote (notes/visionfive2.md, fourth bench stop).
    pub wait_on: Option<W>,

    /// **The pending rendezvous was completed by the counterparty** (boot 8): a message staged, a
    /// signal counted, a reply filled. Set by [`serve`](Self::serve) in the same `SCHED` critical
    /// section that delivered, before the wake; cleared by [`park`](Self::park). One half of the
    /// undelivered-wake gate.
    pub ipc_served: bool,

    /// **The blocking IPC was aborted** (revocation tore the endpoint down, or it was stale at the
    /// call). Set by [`abort`](Self::abort); read and cleared by the syscall layer through
    /// [`take_aborted`](Self::take_aborted). The other half of the gate. Not cleared by `park`;
    /// see BUGS.
    pub ipc_aborted: bool,
}

impl<W> Handshake<W> {
    /// A thread adopted mid-run (the boot thread, a secondary core's idle): `Running`, standing on
    /// its CPU right now.
    #[must_use]
    pub const fn on_cpu_now() -> Self {
        Self {
            state: RunState::Running,
            on_cpu: true,
            wake_pending: false,
            wait_on: None,
            ipc_served: false,
            ipc_aborted: false,
        }
    }

    /// A freshly spawned thread: `Ready`, on no CPU, about to be queued by its spawner.
    #[must_use]
    pub const fn ready() -> Self {
        Self {
            state: RunState::Ready,
            on_cpu: false,
            wake_pending: false,
            wait_on: None,
            ipc_served: false,
            ipc_aborted: false,
        }
    }

    /// A retyped, unstarted TCB (milestone 19c.3): `Embryo`, inert to every transition here until
    /// the kernel's `START` makes it `Ready`.
    #[must_use]
    pub const fn embryo() -> Self {
        Self {
            state: RunState::Embryo,
            on_cpu: false,
            wake_pending: false,
            wait_on: None,
            ipc_served: false,
            ipc_aborted: false,
        }
    }

    /// **Park the running thread in a blocking IPC** (`sched.rs`'s block sites: send, recv, call,
    /// and their capability-carrying twins). `Blocked`, waiting on `wait`, with the gate armed:
    /// only a counterparty that delivers (or an abort) may complete this wait.
    ///
    /// The caller has already linked the thread on the endpoint's wait queue (or, for a `CALL`
    /// caller, deliberately on none) and stays under `SCHED` until both are recorded, so a
    /// timer-driven `schedule()` in the gap sees a consistent pair.
    pub fn park(&mut self, wait: W) {
        debug_assert!(
            self.state == RunState::Running,
            "a thread parked itself while not running"
        );
        self.state = RunState::Blocked;
        self.wait_on = Some(wait);
        self.ipc_served = false; // parked: only a delivering counterparty may wake us
    }

    /// **Record a delivery** in the same `SCHED` critical section that staged it (a mailbox
    /// written, a signal counted, a reply filled, a death message collected). This is what lets
    /// the very next [`try_wake`](Self::try_wake) through the gate; a wake whose critical section
    /// did not call this is exactly the wake the gate exists to refuse. No such wake has been
    /// observed on hardware (see the undelivered-wake bullet at the top of this file); the
    /// reconstruction that produces one lives in the loom module.
    pub fn serve(&mut self) {
        self.ipc_served = true;
    }

    /// **Abort the wait** (the endpoint is stale or was revoked under the sleeper). Passes the
    /// gate like a delivery, because the woken thread does get something to return: the error the
    /// syscall layer reads through [`take_aborted`](Self::take_aborted).
    pub fn abort(&mut self) {
        self.ipc_aborted = true;
    }

    /// Whether a resume has something to return: the recv tail's tripwire (`debug_assert!` in the
    /// kernel), and the model's central assertion.
    #[must_use]
    pub fn delivered(&self) -> bool {
        self.ipc_served || self.ipc_aborted
    }

    /// **Read and clear the abort flag**, exactly `core::mem::take`: the syscall layer calls this
    /// right after a blocking primitive returns, and `true` means "hand the caller an error, not
    /// the mailbox".
    pub fn take_aborted(&mut self) -> bool {
        core::mem::take(&mut self.ipc_aborted)
    }

    /// **Try to make a blocked thread runnable.** The whole wake decision, in the order the
    /// kernel's `wake` established it: not blocked, then the undelivered-wake gate, then the
    /// switch-out deferral, then `Ready`. See [`WakeVerdict`] for what each answer obliges the
    /// caller to do; only [`Queue`](WakeVerdict::Queue) hands back a thread to place.
    ///
    /// On `Queue` the thread leaves its wait (`wait_on` cleared) but keeps `ipc_served` until its
    /// next [`park`](Self::park): the resumed IPC tail still needs to see what was delivered.
    pub fn try_wake(&mut self) -> WakeVerdict {
        if self.state != RunState::Blocked {
            return WakeVerdict::NotBlocked;
        }
        // The undelivered-wake gate (boot 8): a wake that delivered nothing has not dequeued the
        // thread from its endpoint and has nothing for its parked IPC to return. Refuse it and
        // leave the thread parked; the real counterparty completes the rendezvous later.
        if self.wait_on.is_some() && !self.delivered() {
            return WakeVerdict::Refused;
        }

        // The wake-before-switch-out race: still on a CPU means the saved context is stale, and
        // queueing now would let another core switch into it while its own core still runs the
        // present one. Park the wake; finish_switch completes it once the context is real.
        if self.on_cpu {
            self.wake_pending = true;
            return WakeVerdict::Deferred;
        }
        self.state = RunState::Ready;
        self.wait_on = None;
        WakeVerdict::Queue
    }

    /// **A core is switching this thread in** (`schedule()`'s chosen `next`): `Running`, and
    /// standing on a CPU until a successor's [`finish_switch`](Self::finish_switch) says
    /// otherwise.
    ///
    /// The `debug_assert` is the steal-vs-switch-out tripwire: every path onto a CPU passes here,
    /// and a thread arriving with `on_cpu` still set means some queue handed out a thread whose
    /// context is still being saved, which is the two-cores-in-one-thread catastrophe regardless
    /// of which queue it was. Debug builds fail loudly; the board build proceeds, because by the
    /// time this is observable the coherent recovery is the switch already underway.
    pub fn switch_in(&mut self) {
        debug_assert!(
            !self.on_cpu,
            "switching into a thread that is still on a cpu"
        );
        // Ready is the ordinary case (popped off a run queue). Running is the idle thread, which
        // lives outside the ready queue and is never marked Ready when a core switches away from
        // it, so it arrives here still saying Running from its last turn.
        debug_assert!(
            matches!(self.state, RunState::Ready | RunState::Running),
            "switching into a thread that was neither Ready nor a parked idle"
        );
        self.state = RunState::Running;
        self.on_cpu = true;
    }

    /// **The timer took this thread off its CPU with work left** (`schedule()`'s requeue): back to
    /// `Ready`, and the caller pushes it on this core's run queue. `on_cpu` deliberately stays set
    /// until the successor's [`finish_switch`](Self::finish_switch): the thread is in a queue
    /// *and* still standing on its core, which is the one legal overlap and the reason the
    /// single-owner queue discipline is load-bearing.
    pub fn preempt(&mut self) {
        debug_assert!(
            self.state == RunState::Running,
            "preempting a thread that was not running"
        );
        self.state = RunState::Ready;
    }

    /// **The switch away from this thread has completed**; we are its successor, on its core, and
    /// its context is provably saved (we are running, so `switch_to` finished writing it). Reap it
    /// if it finished; otherwise clear [`on_cpu`](Self::on_cpu) and complete a deferred wake if
    /// one was parked. See [`SwitchOutVerdict`] for the caller's obligations.
    pub fn finish_switch(&mut self) -> SwitchOutVerdict {
        if self.state == RunState::Finished {
            // Untouched: the caller drops the whole Thread, and clearing flags on it first would
            // only dress a corpse.
            return SwitchOutVerdict::Reap;
        }
        self.on_cpu = false;
        if self.wake_pending {
            // A wake raced the switch-out and was deferred precisely so that it would complete
            // here, where the context is real.
            self.wake_pending = false;
            self.state = RunState::Ready;
            self.wait_on = None;
            return SwitchOutVerdict::WakeCompleted;
        }
        SwitchOutVerdict::Cleared
    }
}

// --- The ordinary host tests -----------------------------------------------------------------
//
// Single-threaded: the transition table, edge by edge. The *interleavings* are the loom module
// below, which is a different question and a different tool.
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn the_constructors_state_their_names() {
        let adopted: Handshake<u8> = Handshake::on_cpu_now();
        assert_eq!(adopted.state, RunState::Running);
        assert!(adopted.on_cpu);

        let spawned: Handshake<u8> = Handshake::ready();
        assert_eq!(spawned.state, RunState::Ready);
        assert!(!spawned.on_cpu);

        let retyped: Handshake<u8> = Handshake::embryo();
        assert_eq!(retyped.state, RunState::Embryo);
        assert!(!retyped.on_cpu);
    }

    #[test]
    fn an_undelivered_wake_is_refused_and_the_thread_stays_parked() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(3);
        assert_eq!(hs.try_wake(), WakeVerdict::Refused);
        assert_eq!(hs.state, RunState::Blocked, "a refused wake unparked");
        assert_eq!(hs.wait_on, Some(3), "a refused wake cleared the wait");
        assert!(
            !hs.wake_pending,
            "a refused wake left a pending wake behind"
        );
    }

    #[test]
    fn a_served_wake_defers_while_on_cpu_and_queues_after() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(3);
        hs.serve();
        assert_eq!(hs.try_wake(), WakeVerdict::Deferred);
        assert_eq!(hs.state, RunState::Blocked);
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::WakeCompleted);
        assert_eq!(hs.state, RunState::Ready);
        assert_eq!(hs.wait_on, None);
        assert!(!hs.wake_pending);

        // The same wake landing after the switch-out queues directly.
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(3);
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::Cleared);
        hs.serve();
        assert_eq!(hs.try_wake(), WakeVerdict::Queue);
        assert_eq!(hs.state, RunState::Ready);
    }

    #[test]
    fn an_abort_passes_the_gate_and_is_taken_exactly_once() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(3);
        hs.abort();
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::Cleared);
        assert_eq!(hs.try_wake(), WakeVerdict::Queue);
        assert!(hs.delivered());
        assert!(hs.take_aborted());
        assert!(!hs.take_aborted(), "the abort flag survived being taken");
    }

    #[test]
    fn a_wake_on_a_thread_that_is_not_blocked_is_a_no_op() {
        let mut ready: Handshake<u8> = Handshake::ready();
        assert_eq!(ready.try_wake(), WakeVerdict::NotBlocked);
        let mut embryo: Handshake<u8> = Handshake::embryo();
        assert_eq!(embryo.try_wake(), WakeVerdict::NotBlocked);
        let mut corpse: Handshake<u8> = Handshake::ready();
        corpse.state = RunState::Dead;
        assert_eq!(corpse.try_wake(), WakeVerdict::NotBlocked);
    }

    #[test]
    fn a_preempted_thread_keeps_standing_on_its_cpu_until_the_switch_finishes() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.preempt();
        assert_eq!(hs.state, RunState::Ready);
        assert!(
            hs.on_cpu,
            "preemption cleared on_cpu before the context was saved"
        );
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::Cleared);
        assert!(!hs.on_cpu);
        hs.switch_in();
        assert_eq!(hs.state, RunState::Running);
        assert!(hs.on_cpu);
    }

    #[test]
    fn a_finished_predecessor_is_handed_back_for_reaping_untouched() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.state = RunState::Finished; // depart's kernel-side write
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::Reap);
        assert!(
            hs.on_cpu,
            "finish_switch dressed a corpse instead of reaping it"
        );
    }

    #[test]
    fn a_second_wake_after_a_deferral_is_idempotent() {
        // Two wakers in one window (a rendezvous plus a revocation abort, say): the second finds
        // the first's parked wake and adds nothing, and finish_switch completes exactly one.
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(3);
        hs.serve();
        assert_eq!(hs.try_wake(), WakeVerdict::Deferred);
        hs.abort();
        assert_eq!(hs.try_wake(), WakeVerdict::Deferred);
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::WakeCompleted);
        assert_eq!(hs.try_wake(), WakeVerdict::NotBlocked);
    }

    #[test]
    fn park_rearms_the_gate_a_previous_delivery_opened() {
        let mut hs: Handshake<u8> = Handshake::on_cpu_now();
        hs.park(1);
        hs.serve();
        assert_eq!(hs.finish_switch(), SwitchOutVerdict::Cleared);
        assert_eq!(hs.try_wake(), WakeVerdict::Queue);
        hs.switch_in();
        // Parking again clears the stale ipc_served, or every later spurious wake would pass.
        hs.park(2);
        assert_eq!(hs.try_wake(), WakeVerdict::Refused);
    }
}

// --- The loom model --------------------------------------------------------------------------
#[cfg(all(test, loom))]
mod interleavings {
    //! The block/wake handshake's critical sections, interleaved every way loom can order them.
    //!
    //! Run with `script/interleaving-check`. What the model is and is not: `SCHED` is a
    //! `loom::sync::Mutex`, the run queue and the migration inbox are booleans under it, the
    //! thread's saved context is a `loom::cell::UnsafeCell` written **outside** the lock (exactly
    //! where `switch_to` writes the real one), and each loom thread is a core executing its
    //! critical sections in the kernel's program order. Loom then explores every interleaving of
    //! those sections, and the `UnsafeCell` turns "the resumer sees a saved context" from an
    //! argument into a checked ordering fact. What it cannot check is that the kernel keeps the
    //! discipline the model encodes (every site under `SCHED`, queues single-owner); that is
    //! established by reading `sched.rs`. See notes/interleaving.md.
    //!
    //! **Bounds, honestly.** Every harness here is run to exhaustion: no
    //! `LOOM_MAX_PREEMPTIONS`, no `max_branches` trimming. That is affordable because the models
    //! are three or four threads with two or three critical sections each, which is not modesty
    //! but the protocol's real shape: one waker, one victim core, one thief is the whole cast of
    //! every recorded race. The historical-semantics twins are `#[should_panic]`, so each race
    //! reconstruction is required to fail, not assumed to.

    use loom::cell::UnsafeCell;
    use loom::sync::{Arc, Mutex};
    use loom::thread;

    use super::*;

    /// Non-vacuity, `steal_request`'s pattern: a real (non-loom) atomic that accumulates across
    /// every execution of the search, asserted afterwards, so a bound that quietly empties the
    /// interesting branch fails loudly. See notes/verification.md's non-vacuity section.
    struct Reached(core::sync::atomic::AtomicBool);

    impl Reached {
        const fn new() -> Self {
            Self(core::sync::atomic::AtomicBool::new(false))
        }
        fn mark(&self) {
            self.0.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        fn assert(&self, what: &str) {
            assert!(
                self.0.load(core::sync::atomic::Ordering::Relaxed),
                "vacuous harness: no execution ever reached {what}, so nothing was checked"
            );
        }
    }

    /// The world one modelled thread lives in: its handshake and its queue membership, under the
    /// one lock, plus its saved context outside it. `queued` counts queue events rather than
    /// holding a flag so that conservation ("woken exactly once") is an assertion, not a hope.
    struct Machine {
        sched: Mutex<Core>,
        /// The thread's saved context: written by its core's `switch_to` after `SCHED` is
        /// released, read by whichever core switches it back in. The loom `UnsafeCell` is the
        /// instrument: any two accesses not ordered by the model's own edges fail the run.
        ctx: UnsafeCell<u32>,
    }

    struct Core {
        hs: Handshake<u8>,
        /// How many times the thread has been handed to a run queue. Exactly one wake may queue.
        queued: u32,
    }

    impl Machine {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sched: Mutex::new(Core {
                    hs: Handshake::on_cpu_now(),
                    queued: 0,
                }),
                ctx: UnsafeCell::new(0),
            })
        }
    }

    const SAVED: u32 = 7;

    /// **A waker races the receiver's switch-out, and the resumer always sees a saved context.**
    ///
    /// The wake-before-switch-out race (notes/intrusive-queues.md), as the kernel runs it: the
    /// receiver parks under `SCHED`, releases, saves its context (the write outside the lock),
    /// and its successor's `finish_switch` clears `on_cpu`; a sender on another core delivers and
    /// wakes somewhere in between. Whichever side ends up queueing the thread, the switch-in that
    /// follows must read the context the victim wrote, and exactly one side may queue.
    #[test]
    fn a_wake_racing_a_switch_out_resumes_on_a_saved_context() {
        // Both windows must be explored, or the deferral is being checked against a search that
        // never raced it.
        static DEFERRED: Reached = Reached::new();
        static QUEUED_DIRECT: Reached = Reached::new();

        loom::model(|| {
            let m = Machine::new();

            // The receiver parks itself (under SCHED), sequenced before both cores below, exactly
            // as the endpoint queue linking sequences the real waker behind the real park.
            m.sched.lock().unwrap().hs.park(3);

            // The receiver's core: save the context, then finish the switch. A completed deferred
            // wake is queued and resumed here, on this core, as finish_switch's contract says.
            let victim = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: the model's write side of the property under test: nothing may
                        // read this cell until an edge orders it after this write, and loom fails
                        // the run if anything does.
                        unsafe { *c = SAVED }
                    });
                    let completed = {
                        let mut core = m.sched.lock().unwrap();
                        match core.hs.finish_switch() {
                            SwitchOutVerdict::WakeCompleted => {
                                core.queued += 1;
                                true
                            }
                            SwitchOutVerdict::Cleared => false,
                            SwitchOutVerdict::Reap => unreachable!("nothing finished"),
                        }
                    };
                    if completed {
                        DEFERRED.mark();
                        resume(&m);
                    }
                })
            };

            // The waker's core: deliver and wake in one critical section, then run the thread if
            // the wake queued it.
            let waker = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    let queued = {
                        let mut core = m.sched.lock().unwrap();
                        core.hs.serve();
                        match core.hs.try_wake() {
                            WakeVerdict::Queue => {
                                core.queued += 1;
                                true
                            }
                            WakeVerdict::Deferred => false,
                            v => unreachable!("a served wake of a parked thread decided {v:?}"),
                        }
                    };
                    if queued {
                        QUEUED_DIRECT.mark();
                        resume(&m);
                    }
                })
            };

            victim.join().unwrap();
            waker.join().unwrap();

            let core = m.sched.lock().unwrap();
            assert_eq!(
                core.queued, 1,
                "one wake queued the thread {} times",
                core.queued
            );
            assert_eq!(core.hs.state, RunState::Running, "the thread never resumed");
        });

        DEFERRED.assert("an interleaving where the wake lands mid-switch-out and is deferred");
        QUEUED_DIRECT.assert("an interleaving where the wake lands after the switch-out");
    }

    /// Switch the thread in and read its saved context: the shared tail of every harness, and the
    /// property carrier. The asserts here restate `switch_in`'s tripwires as real asserts,
    /// because `script/interleaving-check` compiles `--release` and a `debug_assert` would leave
    /// the `#[should_panic]` reconstructions with nothing to fire; the `ctx` read is the
    /// loom-checked ordering fact.
    fn resume(m: &Machine) {
        {
            let mut core = m.sched.lock().unwrap();
            assert!(core.hs.delivered(), "resumed with nothing delivered");
            assert!(
                !core.hs.on_cpu,
                "switching into a thread that is still on a cpu"
            );
            core.hs.switch_in();
        }
        m.ctx.with(|c| {
            // SAFETY: the read side of the property: a resume is ordered after the victim's
            // context save by the handshake (defer until finish_switch) plus the lock edges, and
            // loom fails the run if that ordering is ever absent.
            assert_eq!(unsafe { *c }, SAVED, "resumed on an unsaved context");
        });
    }

    /// **The historical semantics: a wake that ignores `on_cpu` lets two cores share one
    /// thread.** The pre-fix `wake()` had the gate's other pieces but queued a still-switching
    /// thread directly; this is that wake, reconstructed, and loom is required to find the
    /// catastrophe: an interleaving in which the waker's resume switches into a thread whose
    /// core has not finished saving its context. `resume`'s still-on-a-cpu assert names it; the
    /// `ctx` cell race behind it is the second net.
    #[test]
    #[should_panic(expected = "still on a cpu")]
    fn a_wake_that_ignores_on_cpu_switches_into_an_unsaved_context() {
        loom::model(|| {
            let m = Machine::new();
            m.sched.lock().unwrap().hs.park(3);

            let victim = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: deliberately unsound under the broken wake below, which is the
                        // point: with the deferral gone, nothing orders the resumer's read after
                        // this write, and the harness requires the model to fail.
                        unsafe { *c = SAVED }
                    });
                    let mut core = m.sched.lock().unwrap();
                    let _ = core.hs.finish_switch();
                })
            };

            let waker = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    {
                        let mut core = m.sched.lock().unwrap();
                        core.hs.serve();
                        // The historical wake: gate respected, deferral skipped.
                        assert_eq!(core.hs.state, RunState::Blocked);
                        core.hs.state = RunState::Ready;
                        core.hs.wait_on = None;
                        core.queued += 1;
                    }
                    resume(&m);
                })
            };

            victim.join().unwrap();
            waker.join().unwrap();
        });
    }

    /// **A stolen thread is never switched in before its context is saved.**
    ///
    /// The steal edge of the same window: a preempted thread sits `Ready` in its core's run queue
    /// with `on_cpu` still set. The kernel's discipline is that only the owning core pops that
    /// queue, at scheduler entries that come after `finish_switch`, so by the time
    /// `serve_steal_request` hands the thread to a thief its context is saved. The model encodes
    /// the discipline as program order on the victim core and checks that the handoff (inbox
    /// slot, its own lock in the kernel; a flag under the model's lock here) carries the ordering
    /// to the thief's resume.
    #[test]
    fn a_stolen_thread_resumes_on_its_saved_context() {
        static STOLEN: Reached = Reached::new();

        loom::model(|| {
            let m = Machine::new();
            let inbox = Arc::new(Mutex::new(false));

            // The victim core preempts T: Ready, still on_cpu, in the victim's queue.
            {
                let mut core = m.sched.lock().unwrap();
                core.hs.preempt();
                core.queued += 1;
            }

            // The victim core's continuation, in the kernel's program order: save the context,
            // finish the switch, then (at the next scheduler entry) serve the steal by moving the
            // queued thread into the thief's inbox.
            let victim = {
                let m = Arc::clone(&m);
                let inbox = Arc::clone(&inbox);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: as in the first harness: the write the resume must be ordered
                        // after, checked by loom rather than argued.
                        unsafe { *c = SAVED }
                    });
                    {
                        let mut core = m.sched.lock().unwrap();
                        assert_eq!(core.hs.finish_switch(), SwitchOutVerdict::Cleared);
                    }
                    {
                        let mut core = m.sched.lock().unwrap();
                        assert_eq!(core.queued, 1);
                        core.queued -= 1;
                    }
                    *inbox.lock().unwrap() = true;
                })
            };

            // The thief: poll the inbox (bounded; loom's scheduler runs the victim between
            // polls), and switch the stolen thread in.
            let thief = {
                let m = Arc::clone(&m);
                let inbox = Arc::clone(&inbox);
                thread::spawn(move || {
                    for _ in 0..2 {
                        let arrived = core::mem::take(&mut *inbox.lock().unwrap());
                        if arrived {
                            {
                                // A steal moves a Ready thread, not a parked one, so there is no
                                // delivery to assert; the invariant is the switch-in one, spelled
                                // as a real assert for the reason resume() gives.
                                let mut core = m.sched.lock().unwrap();
                                assert!(
                                    !core.hs.on_cpu,
                                    "switching into a thread that is still on a cpu"
                                );
                                core.hs.switch_in();
                            }
                            m.ctx.with(|c| {
                                // SAFETY: the read side; ordered through the victim's program
                                // order plus the inbox handoff, or loom fails the run.
                                assert_eq!(unsafe { *c }, SAVED, "stole an unsaved context");
                            });
                            STOLEN.mark();
                            return;
                        }
                        thread::yield_now();
                    }
                })
            };

            victim.join().unwrap();
            thief.join().unwrap();
        });

        STOLEN.assert("an execution in which the thief actually receives the stolen thread");
    }

    /// **The historical shape: a thief that pops the victim's queue directly steals a thread
    /// whose context is still being saved.** This is the single-owner run-queue rule broken on
    /// purpose: the thief takes the queued thread the instant it appears, racing the victim's
    /// context save, and `switch_in`'s tripwire (or loom's cell check, whichever the interleaving
    /// reaches first) is required to fire.
    #[test]
    #[should_panic(expected = "still on a cpu")]
    fn a_thief_that_pops_a_foreign_queue_steals_an_unsaved_context() {
        loom::model(|| {
            let m = Machine::new();
            {
                let mut core = m.sched.lock().unwrap();
                core.hs.preempt();
                core.queued += 1;
            }

            let victim = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: deliberately racy under the broken thief; the harness requires
                        // the model to fail.
                        unsafe { *c = SAVED }
                    });
                    let mut core = m.sched.lock().unwrap();
                    let _ = core.hs.finish_switch();
                })
            };

            let thief = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    let took = {
                        let mut core = m.sched.lock().unwrap();
                        if core.queued == 1 {
                            core.queued -= 1;
                            // The tripwire, as a real assert (see resume()): a thief popping a
                            // foreign queue can take a thread whose core is still saving its
                            // context, and this is required to catch it.
                            assert!(
                                !core.hs.on_cpu,
                                "switching into a thread that is still on a cpu"
                            );
                            core.hs.switch_in();
                            true
                        } else {
                            false
                        }
                    };
                    if took {
                        m.ctx.with(|c| {
                            // SAFETY: the deliberately unsound read; either this cell check or
                            // the tripwire above ends the run.
                            assert_eq!(unsafe { *c }, SAVED);
                        });
                    }
                })
            };

            victim.join().unwrap();
            thief.join().unwrap();
        });
    }

    /// **An undelivered wake racing a park strands nobody** (boot 8). A spurious waker fires at a
    /// parked receiver with nothing delivered, racing the receiver's own switch-out; the gate
    /// refuses it in every interleaving, the receiver stays parked and waiting, and the real
    /// sender that follows still completes the rendezvous, after which the resume sees a
    /// delivery and a saved context.
    #[test]
    fn an_undelivered_wake_racing_a_park_strands_nobody() {
        static REFUSED_MID_SWITCH: Reached = Reached::new();
        static REFUSED_AFTER: Reached = Reached::new();

        loom::model(|| {
            let m = Machine::new();
            m.sched.lock().unwrap().hs.park(3);

            let victim = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: as in the first harness.
                        unsafe { *c = SAVED }
                    });
                    let completed = {
                        let mut core = m.sched.lock().unwrap();
                        match core.hs.finish_switch() {
                            SwitchOutVerdict::WakeCompleted => {
                                core.queued += 1;
                                true
                            }
                            SwitchOutVerdict::Cleared => false,
                            SwitchOutVerdict::Reap => unreachable!("nothing finished"),
                        }
                    };
                    if completed {
                        resume(&m);
                    }
                })
            };

            // One loom thread carries the spurious waker and then the real sender, in that
            // order, each racing the victim's switch-out.
            let wakers = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    {
                        // The spurious wake: no delivery in this critical section. The gate must
                        // refuse it whether or not the victim has finished switching out.
                        let mut core = m.sched.lock().unwrap();
                        assert_eq!(
                            core.hs.try_wake(),
                            WakeVerdict::Refused,
                            "an undelivered wake was granted"
                        );
                        assert_eq!(core.hs.state, RunState::Blocked);
                        assert!(
                            core.hs.wait_on.is_some(),
                            "a refused wake unlinked the wait"
                        );
                        if core.hs.on_cpu {
                            REFUSED_MID_SWITCH.mark();
                        } else {
                            REFUSED_AFTER.mark();
                        }
                    }
                    let queued = {
                        // The real sender: deliver and wake in one critical section.
                        let mut core = m.sched.lock().unwrap();
                        core.hs.serve();
                        match core.hs.try_wake() {
                            WakeVerdict::Queue => {
                                core.queued += 1;
                                true
                            }
                            WakeVerdict::Deferred => false,
                            v => unreachable!("a served wake of a parked thread decided {v:?}"),
                        }
                    };
                    if queued {
                        resume(&m);
                    }
                })
            };

            victim.join().unwrap();
            wakers.join().unwrap();

            let core = m.sched.lock().unwrap();
            assert_eq!(
                core.queued, 1,
                "the receiver was queued {} times",
                core.queued
            );
            assert_eq!(
                core.hs.state,
                RunState::Running,
                "the receiver never resumed"
            );
        });

        REFUSED_MID_SWITCH.assert("a spurious wake refused while the receiver was mid-switch-out");
        REFUSED_AFTER.assert("a spurious wake refused after the receiver was fully parked");
    }

    /// **The historical semantics: without the gate, the spurious wake completes a rendezvous
    /// that never happened.** The pre-boot-8 `wake()`, reconstructed: it respects the switch-out
    /// deferral (that fix predates it by weeks) but wakes without asking whether anything was
    /// delivered. Every interleaving strands the receiver, which is exactly what the bench saw:
    /// the resume's "resumed with nothing delivered" tripwire is required to fire.
    #[test]
    #[should_panic(expected = "resumed with nothing delivered")]
    fn without_the_gate_a_spurious_wake_completes_an_empty_rendezvous() {
        loom::model(|| {
            let m = Machine::new();
            m.sched.lock().unwrap().hs.park(3);

            let victim = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    m.ctx.with_mut(|c| {
                        // SAFETY: single writer; the failure this harness requires is the
                        // delivery tripwire, not a cell race.
                        unsafe { *c = SAVED }
                    });
                    let completed = {
                        let mut core = m.sched.lock().unwrap();
                        matches!(core.hs.finish_switch(), SwitchOutVerdict::WakeCompleted)
                    };
                    if completed {
                        // The deferred spurious wake completed: the receiver resumes with an
                        // empty mailbox, which is the strand the gate forbids.
                        resume(&m);
                    }
                })
            };

            let spurious = {
                let m = Arc::clone(&m);
                thread::spawn(move || {
                    let queued = {
                        let mut core = m.sched.lock().unwrap();
                        // The historical wake: deferral kept, gate absent.
                        assert_eq!(core.hs.state, RunState::Blocked);
                        if core.hs.on_cpu {
                            core.hs.wake_pending = true;
                            false
                        } else {
                            core.hs.state = RunState::Ready;
                            core.hs.wait_on = None;
                            true
                        }
                    };
                    if queued {
                        resume(&m);
                    }
                })
            };

            victim.join().unwrap();
            spurious.join().unwrap();
        });
    }
}
