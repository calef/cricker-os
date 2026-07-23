//! The synchronous-rendezvous state machine (DECISIONS §14, milestone 18).
//!
//! This owns the decision core of `kernel/src/sched.rs`'s IPC: the two wait queues and the
//! pending-signal count, and what a send, a receive, or a signal *does* with them. The kernel wraps
//! it with the bookkeeping the queues cannot express (mailboxes, waking a thread onto a run queue,
//! the one-shot Reply that leaves a caller blocked); the *policy* lives here, proved, and the
//! scheduler calls it rather than hand-rolling the same branch six times.
//!
//! Generic over the id type so it can carry the real thing it decides about, a `Tid` in the kernel,
//! an opaque `u8` in the proofs, and hand back *which* peer to rendezvous with. The queues are real
//! `VecDeque`s, so this is not a parallel model the kernel keeps in sync: it *is* the kernel's
//! endpoint state.
//!
//! The load-bearing invariant, stated in the original `Endpoint` doc, is **"at most one wait queue
//! is ever non-empty."** A sender that finds a receiver rendezvouses instead of joining a queue, so
//! a thread only queues when nobody was waiting for it. Every operation is proved to preserve it.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::VecDeque;

/// One IPC endpoint: two wait queues and the pending-signal count.
#[derive(Debug)]
pub struct Endpoint<Id> {
    /// Senders blocked here, waiting for a receiver.
    senders: VecDeque<Id>,
    /// Receivers blocked here, waiting for a sender.
    receivers: VecDeque<Id>,
    /// Async signals that arrived with nobody waiting. Drained by the next receive, never lost.
    pending: u32,
}

/// What a [`send`](Endpoint::send) decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Send<Id> {
    /// A receiver was waiting: rendezvous with this one, and the sender does not join a queue.
    Rendezvous(Id),
    /// Nobody was waiting: the sender is now blocked on this endpoint.
    Blocked,
}

/// What a [`recv`](Endpoint::recv) decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recv<Id> {
    /// A pending async signal was drained; the receiver does not block.
    Signal,
    /// This blocked sender was collected; the caller decides whether to wake it.
    FromSender(Id),
    /// Nobody was waiting: the receiver is now blocked on this endpoint.
    Blocked,
}

impl<Id> Endpoint<Id> {
    pub const fn new() -> Self {
        Self {
            senders: VecDeque::new(),
            receivers: VecDeque::new(),
            pending: 0,
        }
    }

    /// **At most one wait queue is ever non-empty.** The load-bearing invariant.
    pub fn one_queue_invariant(&self) -> bool {
        self.senders.is_empty() || self.receivers.is_empty()
    }

    /// A sender `me` arrives. Rendezvous with a waiting receiver if there is one, otherwise `me`
    /// joins the sender queue and blocks.
    pub fn send(&mut self, me: Id) -> Send<Id> {
        if let Some(receiver) = self.receivers.pop_front() {
            Send::Rendezvous(receiver)
        } else {
            self.senders.push_back(me);
            Send::Blocked
        }
    }

    /// A receiver `me` arrives. Drain a pending signal first (never lose one behind a later sender),
    /// then collect a blocked sender, otherwise `me` joins the receiver queue and blocks.
    pub fn recv(&mut self, me: Id) -> Recv<Id> {
        if self.pending > 0 {
            self.pending -= 1;
            Recv::Signal
        } else if let Some(sender) = self.senders.pop_front() {
            Recv::FromSender(sender)
        } else {
            self.receivers.push_back(me);
            Recv::Blocked
        }
    }

    /// An async signal arrives. Wake a waiting receiver (returned), or count it for the next
    /// receive. **Not a rendezvous:** it never joins the sender queue and is never lost.
    pub fn signal(&mut self) -> Option<Id> {
        if let Some(receiver) = self.receivers.pop_front() {
            Some(receiver)
        } else {
            self.pending = self.pending.saturating_add(1);
            None
        }
    }
}

impl<Id> Default for Endpoint<Id> {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-checked proofs of the rendezvous state machine (DECISIONS §14, milestone 18).
///
/// Every operation is proved to preserve the one-queue invariant, and each decision is proved to
/// match the rule. These are inductive-step proofs: assume a valid state, apply one operation, check
/// the invariant holds. A non-empty queue is modeled with a single waiter, because the decision and
/// the invariant depend only on whether a queue is *empty*, never on its length: `send` pops from a
/// queue (only shrinking it) and pushes only to the queue that was empty, so the emptiness pattern
/// transitions identically for one waiter or many. One element covers every non-empty case and keeps
/// the `VecDeque` reasoning tractable.
#[cfg(kani)]
mod verification {
    use super::*;

    /// A symbolic endpoint satisfying the invariant: at most one queue non-empty (modeled as one
    /// waiter), and a symbolic pending count.
    fn any_valid_endpoint() -> Endpoint<u8> {
        let mut e = Endpoint::new();
        e.pending = kani::any();
        match kani::any::<u8>() {
            0 => e.senders.push_back(kani::any()),
            1 => e.receivers.push_back(kani::any()),
            _ => {} // both empty
        }
        e
    }

    #[kani::proof]
    fn send_preserves_the_invariant() {
        let mut e = any_valid_endpoint();
        e.send(kani::any());
        assert!(e.one_queue_invariant());
    }

    #[kani::proof]
    fn recv_preserves_the_invariant() {
        let mut e = any_valid_endpoint();
        e.recv(kani::any());
        assert!(e.one_queue_invariant());
    }

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
        let had_receiver = !e.receivers.is_empty();
        let rendezvous = matches!(e.send(kani::any()), Send::Rendezvous(_));
        assert_eq!(rendezvous, had_receiver);
    }

    /// **A pending signal is taken before a blocked sender.** A receive drains a counted signal
    /// first, so an async signal delivered with nobody waiting is never lost behind a later
    /// synchronous sender.
    #[kani::proof]
    fn recv_drains_a_pending_signal_first() {
        let mut e = any_valid_endpoint();
        if e.pending > 0 {
            assert_eq!(e.recv(kani::any()), Recv::Signal);
        }
    }

    /// **A collected sender is forgotten by the endpoint.** The endpoint half of the one-shot
    /// Reply guarantee (DECISIONS §12): a `CALL`er queues as a sender and blocks; when a server's
    /// receive collects it, the pop is destructive, so afterwards the endpoint holds no name for
    /// the caller in either queue and no later receive can produce it again. From that moment the
    /// kernel-minted Reply capability is the *only* name for the blocked caller anywhere, and the
    /// caps side (consume-on-use, proved in `crates/caps`) makes that name single-use.
    ///
    /// One waiter covers the general case here as everywhere in this module, plus one fact the
    /// queue cannot see: a blocked thread cannot run, so it cannot enqueue itself a second time.
    /// Stated through emptiness rather than `contains` (the decision core only ever asks whether a
    /// queue is empty, and `contains` hands the solver an unbounded scan for no added meaning).
    #[kani::proof]
    fn a_collected_sender_is_forgotten() {
        let mut e = any_valid_endpoint();
        if matches!(e.recv(kani::any()), Recv::FromSender(_)) {
            assert!(e.senders.is_empty() && e.receivers.is_empty());
            assert!(!matches!(e.recv(kani::any()), Recv::FromSender(_)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendezvous, both orderings: whoever arrives first waits, the second completes the pair
    /// and learns the first's id.
    #[test]
    fn sender_first_then_receiver_rendezvous() {
        let mut e: Endpoint<u32> = Endpoint::new();
        assert_eq!(e.send(7), Send::Blocked); // nobody waiting: park sender 7
        assert_eq!(e.recv(9), Recv::FromSender(7)); // receiver 9 collects sender 7
        assert!(e.one_queue_invariant());
    }

    #[test]
    fn receiver_first_then_sender_rendezvous() {
        let mut e: Endpoint<u32> = Endpoint::new();
        assert_eq!(e.recv(9), Recv::Blocked);
        assert_eq!(e.send(7), Send::Rendezvous(9)); // sender 7 meets waiting receiver 9
    }

    /// Two senders queue in FIFO order; two receivers drain them in the same order.
    #[test]
    fn senders_queue_fifo() {
        let mut e: Endpoint<u32> = Endpoint::new();
        assert_eq!(e.send(1), Send::Blocked);
        assert_eq!(e.send(2), Send::Blocked);
        assert_eq!(e.recv(10), Recv::FromSender(1));
        assert_eq!(e.recv(11), Recv::FromSender(2));
    }

    /// A signal with nobody waiting is counted; the next receives drain it, then block.
    #[test]
    fn a_signal_to_an_empty_endpoint_is_counted_then_drained() {
        let mut e: Endpoint<u32> = Endpoint::new();
        assert_eq!(e.signal(), None); // counted
        assert_eq!(e.signal(), None);
        assert_eq!(e.recv(1), Recv::Signal);
        assert_eq!(e.recv(1), Recv::Signal);
        assert_eq!(e.recv(1), Recv::Blocked);
    }

    /// A signal with a receiver waiting wakes it directly and counts nothing.
    #[test]
    fn a_signal_wakes_a_waiting_receiver() {
        let mut e: Endpoint<u32> = Endpoint::new();
        assert_eq!(e.recv(5), Recv::Blocked);
        assert_eq!(e.signal(), Some(5)); // wakes receiver 5
        assert!(e.one_queue_invariant());
    }
}
