//! The synchronous-rendezvous state machine, as pure logic (DECISIONS §14, milestone 18).
//!
//! This is the decision core of `kernel/src/sched.rs`'s `Endpoint`: given the two wait queues and
//! the pending-signal count, what does a send, a receive, or a signal *do*? The kernel wraps this
//! with the real bookkeeping (thread ids, mailboxes, waking a thread onto a run queue); the *policy*
//! lives here, where it can be proved.
//!
//! The one invariant the whole design rests on, stated in the `Endpoint` doc in `sched.rs`, is **"at
//! most one wait queue is ever non-empty."** A sender that finds a receiver waiting rendezvouses and
//! neither blocks, so a thread only ever joins a queue when nobody was waiting for it. This crate
//! proves that every operation preserves that invariant (verification module).
//!
//! Queues are modeled as **counts**, not lists of ids: the rendezvous *decision* depends only on
//! whether a queue is empty, so the count is its essence. Wiring this into the scheduler, so the
//! kernel's IPC path *is* this proved logic rather than a parallel copy, is the follow-up (Phase 2);
//! this crate is the proved target that refactor aims at. See notes/verification.md.

#![cfg_attr(not(test), no_std)]

/// The abstract state of one endpoint: the two wait queues (as counts) and the pending-signal count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Endpoint {
    /// Senders blocked here, waiting for a receiver.
    senders: u32,
    /// Receivers blocked here, waiting for a sender.
    receivers: u32,
    /// Async signals that arrived with nobody waiting. Drained by the next receive, never lost.
    pending: u32,
}

/// What a [`send`](Endpoint::send) decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Send {
    /// A receiver was waiting: rendezvous, and neither party blocks.
    Rendezvous,
    /// Nobody was waiting: the sender is now blocked on this endpoint.
    Blocked,
}

/// What a [`recv`](Endpoint::recv) decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recv {
    /// A pending async signal was drained; the receiver does not block.
    Signal,
    /// A blocked sender was collected; the caller should wake it.
    FromSender,
    /// Nobody was waiting: the receiver is now blocked on this endpoint.
    Blocked,
}

impl Endpoint {
    pub const fn new() -> Self {
        Self {
            senders: 0,
            receivers: 0,
            pending: 0,
        }
    }

    /// **At most one wait queue is ever non-empty.** The load-bearing invariant.
    pub const fn one_queue_invariant(&self) -> bool {
        self.senders == 0 || self.receivers == 0
    }

    /// A sender arrives. Rendezvous with a waiting receiver if there is one, otherwise block.
    pub fn send(&mut self) -> Send {
        if self.receivers > 0 {
            self.receivers -= 1;
            Send::Rendezvous
        } else {
            self.senders += 1;
            Send::Blocked
        }
    }

    /// A receiver arrives. Drain a pending signal first (never lose one behind a later sender), then
    /// collect a blocked sender, otherwise block.
    pub fn recv(&mut self) -> Recv {
        if self.pending > 0 {
            self.pending -= 1;
            Recv::Signal
        } else if self.senders > 0 {
            self.senders -= 1;
            Recv::FromSender
        } else {
            self.receivers += 1;
            Recv::Blocked
        }
    }

    /// An async signal arrives. Wake a waiting receiver, or count it for the next receive. Returns
    /// whether a receiver was woken.
    pub fn signal(&mut self) -> bool {
        if self.receivers > 0 {
            self.receivers -= 1;
            true
        } else {
            self.pending += 1;
            false
        }
    }
}

/// Machine-checked proofs of the rendezvous state machine (DECISIONS §14, milestone 18).
///
/// Every operation is proved to preserve the one-queue invariant, and the decision each makes is
/// proved to match the rule (a send rendezvouses exactly when a receiver waited; a receive drains a
/// pending signal before a sender). These are inductive-step proofs: assume a valid state, apply one
/// operation, check the invariant still holds. No loops, so Kani decides them instantly.
#[cfg(kani)]
mod verification {
    use super::*;

    /// A symbolic endpoint that satisfies the invariant, with counts below their overflow point (the
    /// kernel bounds them by the live thread count; this rules out the `u32 += 1` wrapping, which is
    /// unreachable in the kernel but must be excluded to reason about the pure model).
    fn any_valid_endpoint() -> Endpoint {
        let e = Endpoint {
            senders: kani::any(),
            receivers: kani::any(),
            pending: kani::any(),
        };
        kani::assume(e.one_queue_invariant());
        kani::assume(e.senders < u32::MAX);
        kani::assume(e.receivers < u32::MAX);
        kani::assume(e.pending < u32::MAX);
        e
    }

    /// **`send` preserves the one-queue invariant**, from any valid state.
    #[kani::proof]
    fn send_preserves_the_invariant() {
        let mut e = any_valid_endpoint();
        e.send();
        assert!(e.one_queue_invariant());
    }

    /// **`recv` preserves the one-queue invariant**, from any valid state.
    #[kani::proof]
    fn recv_preserves_the_invariant() {
        let mut e = any_valid_endpoint();
        e.recv();
        assert!(e.one_queue_invariant());
    }

    /// **`signal` preserves the one-queue invariant**, from any valid state.
    #[kani::proof]
    fn signal_preserves_the_invariant() {
        let mut e = any_valid_endpoint();
        e.signal();
        assert!(e.one_queue_invariant());
    }

    /// **A send rendezvouses exactly when a receiver was waiting**, and blocks otherwise. So a
    /// message is never dropped and a sender never blocks past a ready receiver.
    #[kani::proof]
    fn send_rendezvous_iff_a_receiver_waited() {
        let mut e = any_valid_endpoint();
        let had_receiver = e.receivers > 0;
        let outcome = e.send();
        assert_eq!(outcome == Send::Rendezvous, had_receiver);
        assert_eq!(outcome == Send::Blocked, !had_receiver);
    }

    /// **A pending signal is taken before a blocked sender.** A receive drains a counted signal
    /// first, so an async signal delivered with nobody waiting is never lost behind a later
    /// synchronous sender.
    #[kani::proof]
    fn recv_drains_a_pending_signal_first() {
        let mut e = any_valid_endpoint();
        if e.pending > 0 {
            assert_eq!(e.recv(), Recv::Signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendezvous, both orderings: whoever arrives first waits, the second completes the pair.
    #[test]
    fn sender_first_then_receiver_rendezvous() {
        let mut e = Endpoint::new();
        assert_eq!(e.send(), Send::Blocked); // nobody waiting: park
        assert_eq!(e.recv(), Recv::FromSender); // collects the waiting sender
        assert!(e.one_queue_invariant());
        assert_eq!(e, Endpoint::new()); // back to empty
    }

    #[test]
    fn receiver_first_then_sender_rendezvous() {
        let mut e = Endpoint::new();
        assert_eq!(e.recv(), Recv::Blocked);
        assert_eq!(e.send(), Send::Rendezvous);
        assert_eq!(e, Endpoint::new());
    }

    /// A signal with nobody waiting is counted, and the next receive drains it without blocking.
    #[test]
    fn a_signal_to_an_empty_endpoint_is_counted_then_drained() {
        let mut e = Endpoint::new();
        assert!(!e.signal()); // counted, no receiver woken
        assert!(!e.signal());
        assert_eq!(e.recv(), Recv::Signal); // drains one
        assert_eq!(e.recv(), Recv::Signal); // drains the other
        assert_eq!(e.recv(), Recv::Blocked); // now nothing: block
    }

    /// A signal with a receiver waiting wakes it directly and counts nothing.
    #[test]
    fn a_signal_wakes_a_waiting_receiver() {
        let mut e = Endpoint::new();
        assert_eq!(e.recv(), Recv::Blocked);
        assert!(e.signal()); // wakes the receiver
        assert_eq!(e, Endpoint::new());
    }
}
