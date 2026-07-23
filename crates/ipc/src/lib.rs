//! The synchronous-rendezvous state machine (DECISIONS §14, milestone 18; intrusive as of
//! milestone 14 phase A.3).
//!
//! This owns the decision core of `kernel/src/sched.rs`'s IPC: the two wait queues and the
//! pending-signal count, and what a send, a receive, or a signal *does* with them. The kernel
//! wraps it with the bookkeeping the queues cannot express (mailboxes, waking a thread onto a
//! run queue, the one-shot Reply that leaves a caller blocked); the *policy* lives here, proved,
//! and the scheduler calls it rather than hand-rolling the same branch six times.
//!
//! The wait queues are **intrusive** (`crates/intrusive`): generic over the node type, so in the
//! kernel a queue entry *is* the TCB, threaded through the same link the run queues use. One link
//! means one queue, so "a blocked thread waits on exactly one endpoint" is a property of there
//! being one field, not a rule anyone keeps. The queues are the kernel's real endpoint state, not
//! a model kept in sync; what changed at A.3 is only what a queue entry is (a TCB pointer, no
//! longer a Tid to be looked up) and that queueing can no longer allocate.
//!
//! The load-bearing invariant, unchanged since the original `Endpoint`: **"at most one wait
//! queue is ever non-empty."** A sender that finds a receiver rendezvouses instead of joining a
//! queue, so a thread only queues when nobody was waiting for it. Every operation is proved to
//! preserve it, now over the real intrusive queues (the `Fifo`'s own FIFO correctness is proved
//! separately, in its crate; here we prove the *decisions* made over it).

#![cfg_attr(not(test), no_std)]

use intrusive::{Fifo, Node};

/// One IPC endpoint: two intrusive wait queues and the pending-signal count.
pub struct Endpoint<T: Node> {
    /// Senders blocked here, waiting for a receiver.
    senders: Fifo<T>,
    /// Receivers blocked here, waiting for a sender.
    receivers: Fifo<T>,
    /// Async signals that arrived with nobody waiting. Drained by the next receive, never lost.
    pending: u32,
}

/// What a [`send`](Endpoint::send) decided.
pub enum Send<T> {
    /// A receiver was waiting: rendezvous with this one, and the sender does not join a queue.
    Rendezvous(*mut T),
    /// Nobody was waiting: the sender is now queued on this endpoint.
    Blocked,
}

/// What a [`recv`](Endpoint::recv) decided.
pub enum Recv<T> {
    /// A pending async signal was drained; the receiver does not block.
    Signal,
    /// This queued sender was collected; the caller decides whether to wake it.
    FromSender(*mut T),
    /// Nobody was waiting: the receiver is now queued on this endpoint.
    Blocked,
}

// Manual impls rather than derives: a derive would demand `T: PartialEq`/`T: Debug` even though
// only the *pointer* is stored and compared, and the kernel's `T` (a TCB) is neither.
impl<T> PartialEq for Send<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Send::Rendezvous(a), Send::Rendezvous(b)) => a == b,
            (Send::Blocked, Send::Blocked) => true,
            _ => false,
        }
    }
}
impl<T> Eq for Send<T> {}
impl<T> core::fmt::Debug for Send<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Send::Rendezvous(p) => f.debug_tuple("Rendezvous").field(p).finish(),
            Send::Blocked => f.write_str("Blocked"),
        }
    }
}

impl<T> PartialEq for Recv<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Recv::Signal, Recv::Signal) => true,
            (Recv::FromSender(a), Recv::FromSender(b)) => a == b,
            (Recv::Blocked, Recv::Blocked) => true,
            _ => false,
        }
    }
}
impl<T> Eq for Recv<T> {}
impl<T> core::fmt::Debug for Recv<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Recv::Signal => f.write_str("Signal"),
            Recv::FromSender(p) => f.debug_tuple("FromSender").field(p).finish(),
            Recv::Blocked => f.write_str("Blocked"),
        }
    }
}

impl<T: Node> Endpoint<T> {
    pub const fn new() -> Self {
        Self {
            senders: Fifo::new(),
            receivers: Fifo::new(),
            pending: 0,
        }
    }

    /// **At most one wait queue is ever non-empty.** The load-bearing invariant.
    pub fn one_queue_invariant(&self) -> bool {
        self.senders.is_empty() || self.receivers.is_empty()
    }

    /// A sender `me` arrives. Rendezvous with a waiting receiver if there is one, otherwise `me`
    /// joins the sender queue (and the caller should block it).
    ///
    /// # Safety
    ///
    /// `me` must satisfy the intrusive contract: valid, on no queue, and it must stay valid for
    /// as long as it may be queued here. (The kernel's discipline: `me` is the running thread,
    /// and a thread queued here is `Blocked`, which the reaper never touches.)
    pub unsafe fn send(&mut self, me: *mut T) -> Send<T> {
        if let Some(receiver) = self.receivers.pop_front() {
            Send::Rendezvous(receiver)
        } else {
            // SAFETY: the caller's contract is exactly the queue's.
            unsafe { self.senders.push_back(me) };
            Send::Blocked
        }
    }

    /// A receiver `me` arrives. Drain a pending signal first (never lose one behind a later
    /// sender), then collect a queued sender, otherwise `me` joins the receiver queue (and the
    /// caller should block it).
    ///
    /// # Safety
    ///
    /// As for [`send`](Self::send).
    pub unsafe fn recv(&mut self, me: *mut T) -> Recv<T> {
        if self.pending > 0 {
            self.pending -= 1;
            Recv::Signal
        } else if let Some(sender) = self.senders.pop_front() {
            Recv::FromSender(sender)
        } else {
            // SAFETY: the caller's contract is exactly the queue's.
            unsafe { self.receivers.push_back(me) };
            Recv::Blocked
        }
    }

    /// An async signal arrives. Wake a waiting receiver (returned, already dequeued), or count it
    /// for the next receive. **Not a rendezvous:** it never joins the sender queue and is never
    /// lost. Safe: signalling queues nothing.
    pub fn signal(&mut self) -> Option<*mut T> {
        if let Some(receiver) = self.receivers.pop_front() {
            Some(receiver)
        } else {
            self.pending = self.pending.saturating_add(1);
            None
        }
    }
}

impl<T: Node> Default for Endpoint<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-checked proofs of the rendezvous state machine (DECISIONS §14, milestone 18; restated
/// over the intrusive queues at milestone 14 phase A.3, so the rewire did not demote proved code
/// back to argued code).
///
/// Every operation is proved to preserve the one-queue invariant, and each decision is proved to
/// match the rule. These are inductive-step proofs: assume a valid state, apply one operation,
/// check. A non-empty queue is modeled with a single waiter, because the decision and the
/// invariant depend only on whether a queue is *empty*, never on its length: an operation pops
/// (only shrinking) and pushes only to a queue that was empty, so the emptiness pattern
/// transitions identically for one waiter or many. FIFO order within a queue is the `intrusive`
/// crate's own proof; these harnesses prove the decisions made over it.
#[cfg(kani)]
mod verification {
    use super::*;

    /// A minimal node: a link and nothing else, the way the proofs like it.
    struct N {
        next: *mut N,
    }

    impl N {
        fn new() -> Self {
            N {
                next: core::ptr::null_mut(),
            }
        }
    }

    unsafe impl Node for N {
        fn next(&self) -> *mut Self {
            self.next
        }
        fn set_next(&mut self, next: *mut Self) {
            self.next = next;
        }
    }

    /// Put `e` into an arbitrary valid state: at most one queue non-empty (modeled as one
    /// waiter), and a symbolic pending count. The waiter nodes are the caller's locals, so they
    /// outlive the endpoint.
    ///
    /// # Safety
    /// `sender` and `receiver` must be valid, distinct, unqueued nodes outliving `e`.
    unsafe fn seed(e: &mut Endpoint<N>, sender: *mut N, receiver: *mut N) {
        e.pending = kani::any();
        match kani::any::<u8>() {
            // SAFETY: caller's contract.
            0 => unsafe { e.senders.push_back(sender) },
            1 => unsafe { e.receivers.push_back(receiver) },
            _ => {} // both empty
        }
    }

    #[kani::proof]
    fn send_preserves_the_invariant() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe {
            seed(&mut e, &mut s, &mut r);
            e.send(&mut me);
        }
        assert!(e.one_queue_invariant());
    }

    #[kani::proof]
    fn recv_preserves_the_invariant() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe {
            seed(&mut e, &mut s, &mut r);
            e.recv(&mut me);
        }
        assert!(e.one_queue_invariant());
    }

    #[kani::proof]
    fn signal_preserves_the_invariant() {
        let (mut s, mut r) = (N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe { seed(&mut e, &mut s, &mut r) };
        e.signal();
        assert!(e.one_queue_invariant());
    }

    /// **A send rendezvouses exactly when a receiver was waiting**, and with exactly *that*
    /// receiver, else blocks. So a message is never dropped, a sender never blocks past a ready
    /// receiver, and the rendezvous partner is the queued thread and no other.
    #[kani::proof]
    fn send_rendezvous_iff_a_receiver_waited() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let receiver_ptr: *mut N = &mut r;
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe { seed(&mut e, &mut s, receiver_ptr) };

        let had_receiver = !e.receivers.is_empty();
        match unsafe { e.send(&mut me) } {
            Send::Rendezvous(got) => {
                assert!(had_receiver);
                assert_eq!(got, receiver_ptr, "rendezvoused with a thread nobody queued");
            }
            Send::Blocked => assert!(!had_receiver),
        }
    }

    /// **A pending signal is taken before a queued sender.** A receive drains a counted signal
    /// first, so an async signal delivered with nobody waiting is never lost behind a later
    /// synchronous sender.
    #[kani::proof]
    fn recv_drains_a_pending_signal_first() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe { seed(&mut e, &mut s, &mut r) };
        if e.pending > 0 {
            assert_eq!(unsafe { e.recv(&mut me) }, Recv::Signal);
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
    /// Stated through emptiness (the decision core's own vocabulary; a membership scan would hand
    /// the solver an unbounded loop for no added meaning).
    #[kani::proof]
    fn a_collected_sender_is_forgotten() {
        let (mut s, mut r, mut me, mut me2) = (N::new(), N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        unsafe { seed(&mut e, &mut s, &mut r) };
        if matches!(unsafe { e.recv(&mut me) }, Recv::FromSender(_)) {
            assert!(e.senders.is_empty() && e.receivers.is_empty());
            assert!(!matches!(
                unsafe { e.recv(&mut me2) },
                Recv::FromSender(_)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N {
        next: *mut N,
    }

    unsafe impl Node for N {
        fn next(&self) -> *mut Self {
            self.next
        }
        fn set_next(&mut self, next: *mut Self) {
            self.next = next;
        }
    }

    fn node() -> Box<N> {
        Box::new(N {
            next: core::ptr::null_mut(),
        })
    }

    /// The rendezvous, both orderings: whoever arrives first waits, the second completes the pair
    /// and gets the first — the very node, by identity, not a name to look up.
    #[test]
    fn sender_first_then_receiver_rendezvous() {
        let (mut s, mut r) = (node(), node());
        let sp: *mut N = &mut *s;
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(unsafe { e.send(sp) }, Send::Blocked); // nobody waiting: park the sender
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::FromSender(sp)); // receiver collects it
        assert!(e.one_queue_invariant());
    }

    #[test]
    fn receiver_first_then_sender_rendezvous() {
        let (mut s, mut r) = (node(), node());
        let rp: *mut N = &mut *r;
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(unsafe { e.recv(rp) }, Recv::Blocked);
        assert_eq!(unsafe { e.send(&mut *s) }, Send::Rendezvous(rp)); // sender meets the waiter
    }

    /// Two senders queue in FIFO order; two receivers drain them in the same order.
    #[test]
    fn senders_queue_fifo() {
        let (mut a, mut b, mut r) = (node(), node(), node());
        let (ap, bp): (*mut N, *mut N) = (&mut *a, &mut *b);
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(unsafe { e.send(ap) }, Send::Blocked);
        assert_eq!(unsafe { e.send(bp) }, Send::Blocked);
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::FromSender(ap));
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::FromSender(bp));
    }

    /// A signal with nobody waiting is counted; the next receives drain it, then block.
    #[test]
    fn a_signal_to_an_empty_endpoint_is_counted_then_drained() {
        let mut r = node();
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(e.signal(), None); // counted
        assert_eq!(e.signal(), None);
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::Signal);
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::Signal);
        assert_eq!(unsafe { e.recv(&mut *r) }, Recv::Blocked);
    }

    /// A signal with a receiver waiting hands it back directly and counts nothing.
    #[test]
    fn a_signal_wakes_a_waiting_receiver() {
        let mut r = node();
        let rp: *mut N = &mut *r;
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(unsafe { e.recv(rp) }, Recv::Blocked);
        assert_eq!(e.signal(), Some(rp)); // the waiter, dequeued
        assert!(e.one_queue_invariant());
    }
}
