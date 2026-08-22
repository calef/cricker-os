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
//!
//! # Examples
//!
//! The three decisions an endpoint makes, and the invariant holding across all of them. `T` is the
//! kernel's TCB; here it is a stand-in with the same one link, because one link is the whole reason
//! the invariant is structural rather than remembered.
//!
//! ```
//! use core::ptr::NonNull;
//! use intrusive::Node;
//! use ipc::{Endpoint, Recv, Send};
//!
//! struct Tcb {
//!     next: Option<NonNull<Tcb>>,
//! }
//!
//! // SAFETY: plain field storage, which is the whole of the `Node` contract.
//! unsafe impl Node for Tcb {
//!     fn next(&self) -> Option<NonNull<Self>> {
//!         self.next
//!     }
//!     fn set_next(&mut self, next: Option<NonNull<Self>>) {
//!         self.next = next;
//!     }
//! }
//!
//! // Declared before the endpoint, so they outlive it.
//! let mut server = Tcb { next: None };
//! let mut client = Tcb { next: None };
//! let mut ep: Endpoint<Tcb> = Endpoint::new();
//! assert!(ep.is_idle());
//!
//! // The server calls recv with nobody sending, so it queues. The caller blocks it.
//! // SAFETY: both are live locals declared before `ep`, on no queue, and this is the only accessor.
//! let waiting = unsafe { ep.recv(NonNull::from(&mut server)) };
//! assert_eq!(waiting, Recv::Blocked);
//! assert!(!ep.is_idle());
//! assert!(ep.one_queue_invariant());
//!
//! // Now a client sends. There is a receiver, so it **rendezvouses instead of queueing**, which is
//! // why at most one of the two queues is ever non-empty.
//! // SAFETY: as above.
//! let met = unsafe { ep.send(NonNull::from(&mut client)) };
//! assert_eq!(met, Send::Rendezvous(NonNull::from(&mut server)));
//! assert!(ep.is_idle()); // the receiver left the queue and the sender never joined one
//! assert!(ep.one_queue_invariant());
//! ```
//!
//! A signal is the operation that is deliberately **not** a rendezvous: it never queues the signaller
//! and it is never lost, which is what lets an interrupt handler use one.
//!
//! ```
//! # use core::ptr::NonNull;
//! # use intrusive::Node;
//! # use ipc::{Endpoint, Recv};
//! # struct Tcb { next: Option<NonNull<Tcb>> }
//! # unsafe impl Node for Tcb {
//! #     fn next(&self) -> Option<NonNull<Self>> { self.next }
//! #     fn set_next(&mut self, next: Option<NonNull<Self>>) { self.next = next; }
//! # }
//! let mut driver = Tcb { next: None };
//! let mut ep: Endpoint<Tcb> = Endpoint::new();
//!
//! // Two interrupts arrive with nobody in recv. Neither is dropped; both are counted.
//! assert!(ep.signal().is_none());
//! assert!(ep.signal().is_none());
//!
//! // The driver's next two receives drain them, and it never blocks.
//! // SAFETY: `driver` is a live local declared before `ep`, on no queue.
//! assert_eq!(unsafe { ep.recv(NonNull::from(&mut driver)) }, Recv::Signal);
//! // SAFETY: as above.
//! assert_eq!(unsafe { ep.recv(NonNull::from(&mut driver)) }, Recv::Signal);
//! // The third finds nothing left and queues.
//! // SAFETY: as above.
//! assert_eq!(unsafe { ep.recv(NonNull::from(&mut driver)) }, Recv::Blocked);
//!
//! // And a signal arriving now wakes it, already dequeued.
//! assert_eq!(ep.signal(), Some(NonNull::from(&mut driver)));
//! assert!(ep.is_idle());
//! ```
//!
//! Name: ratified 2026-08-01 (calef, the naming tenet in CLAUDE.md). Named in the group of standard
//! terms that are already right and must not be touched, because a name a reader knows from outside
//! this project costs nothing to learn and renaming it would destroy the recognition the tenet
//! exists to buy.

#![cfg_attr(not(test), no_std)]
// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

use core::ptr::NonNull;

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
    Rendezvous(NonNull<T>),
    /// Nobody was waiting: the sender is now queued on this endpoint.
    Blocked,
}

/// What a [`recv`](Endpoint::recv) decided.
pub enum Recv<T> {
    /// A pending async signal was drained; the receiver does not block.
    Signal,
    /// This queued sender was collected; the caller decides whether to wake it.
    FromSender(NonNull<T>),
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
    /// An idle endpoint: both wait queues empty, no pending signal. `const` so the kernel can
    /// build the endpoint table at compile time rather than at boot.
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

    /// **No thread is blocked on this endpoint** (both wait queues empty). The pending signal count
    /// does not count: a signal holds no thread.
    pub fn is_idle(&self) -> bool {
        self.senders.is_empty() && self.receivers.is_empty()
    }

    /// Diagnostic: `(queued senders, queued receivers, pending signals)`. For a hang dump, a
    /// nonzero sender count with a zero receiver count on a request endpoint is a stalled server.
    pub fn debug_counts(&self) -> (usize, usize, u32) {
        (self.senders.len(), self.receivers.len(), self.pending)
    }

    /// **Empty both wait queues, handing every blocked thread back to `f`** (object revocation): the
    /// endpoint is about to be destroyed, so each waiter is popped off here (which frees its intrusive
    /// link, so `f` may re-queue it onto a run queue) and the caller wakes it with an error. After
    /// this both queues are empty, so [`is_idle`](Self::is_idle) holds and the one-queue invariant
    /// trivially does.
    pub fn drain_waiters(&mut self, mut f: impl FnMut(NonNull<T>)) {
        while let Some(w) = self.senders.pop_front() {
            f(w);
        }
        while let Some(w) = self.receivers.pop_front() {
            f(w);
        }
    }

    /// **Take one specific sender back off the queue**, returning whether it was there.
    ///
    /// The one operation an intrusive `Fifo` deliberately does not offer (arbitrary remove), needed
    /// here for one reason: a **corpse** can be a queued sender. A supervised thread that dies with
    /// nobody in `RECV` parks on its supervision endpoint's sender queue with the death message in
    /// its mailbox (DECISIONS §26 implementation note 2), and its supervisor may then reap it
    /// (§32's endpoint reap, or §16's `DESTROY`) *without* having collected the message. Freeing a
    /// TCB that is still linked into a queue leaves a dangling pointer the next `recv` would follow,
    /// so the reap has to unlink it first.
    ///
    /// Expressed as drain-and-repush over `pop_front`/`push_back` rather than as a `Fifo::remove`,
    /// which keeps the "one link, no arbitrary remove" contract intact in the queue itself: the cost
    /// is O(queued senders) on a teardown path, and the queue's own proved invariants are the only
    /// ones in play. FIFO order among the survivors is preserved.
    ///
    /// # Safety
    ///
    /// `victim` is compared by pointer, never dereferenced. Every *other* queued sender is popped
    /// and pushed again, so they must all still satisfy the queue's contract (they do: a queued
    /// sender is blocked or a corpse, and neither is freed while linked).
    pub unsafe fn remove_sender(&mut self, victim: NonNull<T>) -> bool {
        let mut kept: Fifo<T> = Fifo::new();
        let mut found = false;
        while let Some(node) = self.senders.pop_front() {
            if node == victim {
                found = true;
            } else {
                // SAFETY: just popped from this queue, so it is valid and on no queue.
                unsafe { kept.push_back(node) };
            }
        }
        self.senders = kept;
        found
    }

    /// A sender `me` arrives. Rendezvous with a waiting receiver if there is one, otherwise `me`
    /// joins the sender queue (and the caller should block it).
    ///
    /// # Safety
    ///
    /// `me` must satisfy the intrusive contract: valid, on no queue, and it must stay valid for
    /// as long as it may be queued here. (The kernel's discipline: `me` is the running thread,
    /// and a thread queued here is `Blocked`, which the reaper never touches.)
    pub unsafe fn send(&mut self, me: NonNull<T>) -> Send<T> {
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
    pub unsafe fn recv(&mut self, me: NonNull<T>) -> Recv<T> {
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
    pub fn signal(&mut self) -> Option<NonNull<T>> {
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
///
/// # The obligations every `unsafe` call in here discharges
///
/// Stated once, rather than re-derived at each of the eleven sites; each site's own comment adds
/// only what is particular to it. (The `#[cfg(test)]` module below does the same thing for the same
/// reason.)
///
/// **Every node outlives the endpoint.** Each harness declares its `N`s in one `let` before it
/// declares `e`, and Rust drops locals in reverse declaration order, so the `Endpoint` is destroyed
/// first. A node still parked on a queue when the harness returns was therefore valid for the whole
/// of its time there, which is the "stays valid for as long as it may be queued" half of
/// `send`/`recv`'s contract and of `seed`'s.
///
/// **Every node is on no queue when it is passed.** Each `N::new()` starts with a null link, `e`
/// starts empty, and no harness hands the same node to two calls. That is why the harnesses carry a
/// separate `me` (and, in one case, a `me2`) rather than reusing `s` or `r`: it makes this
/// obligation a fact about how many locals there are, not an argument about what the previous
/// operation decided.
///
/// Both are properties the *harness* has, not properties Kani checks. Kani would catch a dangling
/// dereference if one of these were false and a proof reached it, but nothing here proves the
/// contract is kept; that is what the comments are for.
#[cfg(kani)]
mod verification {
    use super::*;

    /// A minimal node: a link and nothing else, the way the proofs like it.
    struct N {
        next: Option<NonNull<N>>,
    }

    impl N {
        fn new() -> Self {
            N { next: None }
        }
    }

    // SAFETY: `next` and `set_next` read and write the same `next` field and nothing else, which is
    // the whole of the `Node` contract.
    unsafe impl Node for N {
        fn next(&self) -> Option<NonNull<Self>> {
            self.next
        }
        fn set_next(&mut self, next: Option<NonNull<Self>>) {
            self.next = next;
        }
    }

    /// Put `e` into an arbitrary valid state: at most one queue non-empty (modeled as one
    /// waiter), and a symbolic pending count. The waiter nodes are the caller's locals, so they
    /// outlive the endpoint.
    ///
    /// # Safety
    /// `sender` and `receiver` must be valid, distinct, unqueued nodes outliving `e`.
    unsafe fn seed(e: &mut Endpoint<N>, sender: NonNull<N>, receiver: NonNull<N>) {
        e.pending = kani::any();
        match kani::any::<u8>() {
            // SAFETY: `sender` is valid, unqueued and outlives `e`, by this function's own
            // contract; the arms are exclusive, so it is pushed at most once.
            0 => unsafe { e.senders.push_back(sender) },
            // SAFETY: the same, for `receiver`, which the contract requires to be a *distinct*
            // node from `sender` and so separately unqueued.
            1 => unsafe { e.receivers.push_back(receiver) },
            _ => {} // both empty
        }
    }

    #[kani::proof]
    fn send_preserves_the_invariant() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        // SAFETY: `s`, `r` and `me` are three distinct fresh nodes declared before `e`, so each is
        // valid, on no queue, and outlives the endpoint. That is `seed`'s contract and `send`'s
        // both; `send` gets `me`, which `seed` never touches.
        unsafe {
            seed(&mut e, NonNull::from(&mut s), NonNull::from(&mut r));
            e.send(NonNull::from(&mut me));
        }
        assert!(e.one_queue_invariant());
    }

    #[kani::proof]
    fn recv_preserves_the_invariant() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        // SAFETY: as in `send_preserves_the_invariant` above; `recv`'s contract on `me` is `send`'s.
        unsafe {
            seed(&mut e, NonNull::from(&mut s), NonNull::from(&mut r));
            e.recv(NonNull::from(&mut me));
        }
        assert!(e.one_queue_invariant());
    }

    #[kani::proof]
    fn signal_preserves_the_invariant() {
        let (mut s, mut r) = (N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        // SAFETY: `s` and `r` are distinct fresh nodes declared before `e`, so they are valid,
        // unqueued and outlive it. `signal` is safe and takes no node, so `seed` is the only
        // obligation here.
        unsafe { seed(&mut e, NonNull::from(&mut s), NonNull::from(&mut r)) };
        e.signal();
        assert!(e.one_queue_invariant());
    }

    /// **A send rendezvouses exactly when a receiver was waiting**, and with exactly *that*
    /// receiver, else blocks. So a message is never dropped, a sender never blocks past a ready
    /// receiver, and the rendezvous partner is the queued thread and no other.
    #[kani::proof]
    fn send_rendezvous_iff_a_receiver_waited() {
        let (mut s, mut r, mut me) = (N::new(), N::new(), N::new());
        let receiver_ptr = NonNull::from(&mut r);
        let mut e: Endpoint<N> = Endpoint::new();
        // SAFETY: `s` and `r` are distinct fresh nodes declared before `e`, so they are valid,
        // unqueued and outlive it. `receiver_ptr` is `&mut r` taken once and kept, which is the
        // same pointer `seed` would have been given inline; no second pointer to `r` exists.
        unsafe { seed(&mut e, NonNull::from(&mut s), receiver_ptr) };

        let had_receiver = !e.receivers.is_empty();
        // SAFETY: `me` is a third fresh node declared before `e`, so it is valid, on no queue
        // (`seed` was given `s` and `r`, never `me`), and outlives the endpoint.
        match unsafe { e.send(NonNull::from(&mut me)) } {
            Send::Rendezvous(got) => {
                assert!(had_receiver);
                assert_eq!(
                    got, receiver_ptr,
                    "rendezvoused with a thread nobody queued"
                );
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
        // SAFETY: `s` and `r` are distinct fresh nodes declared before `e`: valid, unqueued,
        // outliving it.
        unsafe { seed(&mut e, NonNull::from(&mut s), NonNull::from(&mut r)) };
        if e.pending > 0 {
            // SAFETY: `me` is a third fresh node declared before `e`, never given to `seed`, so it
            // is valid, on no queue, and outlives the endpoint. This receive drains a signal rather
            // than queueing `me`, but that is what the harness asserts, not what makes the call
            // sound: the contract is met either way.
            assert_eq!(unsafe { e.recv(NonNull::from(&mut me)) }, Recv::Signal);
        }
    }

    /// **A collected sender is forgotten by the endpoint.** The endpoint half of the one-shot
    /// Reply guarantee (DECISIONS §12): a `CALL`er queues as a sender and blocks; when a server's
    /// receive collects it, the pop is destructive, so afterwards the endpoint holds no name for
    /// the caller in either queue and no later receive can produce it again. From that moment the
    /// kernel-minted Reply capability is the *only* name for the blocked caller anywhere, and the
    /// capability side (consume-on-use, proved in `crates/capability`) makes that name single-use.
    ///
    /// One waiter covers the general case here as everywhere in this module, plus one fact the
    /// queue cannot see: a blocked thread cannot run, so it cannot enqueue itself a second time.
    /// Stated through emptiness (the decision core's own vocabulary; a membership scan would hand
    /// the solver an unbounded loop for no added meaning).
    #[kani::proof]
    fn a_collected_sender_is_forgotten() {
        let (mut s, mut r, mut me, mut me2) = (N::new(), N::new(), N::new(), N::new());
        let mut e: Endpoint<N> = Endpoint::new();
        // SAFETY: `s` and `r` are distinct fresh nodes declared before `e`: valid, unqueued,
        // outliving it.
        unsafe { seed(&mut e, NonNull::from(&mut s), NonNull::from(&mut r)) };
        if matches!(
            unsafe {
                // SAFETY: `me` is a fresh node declared before `e` and never given to `seed`, so it
                // is valid, on no queue, and outlives the endpoint.
                e.recv(NonNull::from(&mut me))
            },
            Recv::FromSender(_)
        ) {
            assert!(e.senders.is_empty() && e.receivers.is_empty());
            assert!(!matches!(
                unsafe {
                    // SAFETY: `me2` is a fourth fresh node, declared before `e` and passed to
                    // nothing else. It exists so this second receive does not reuse `me`: `me` was
                    // not queued by the receive above (it returned `FromSender`), but a separate
                    // node makes this site's obligation independent of that reasoning.
                    e.recv(NonNull::from(&mut me2))
                },
                Recv::FromSender(_)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    //! Every `unsafe` call below satisfies the same two obligations, stated once here rather than
    //! re-derived at each of the twenty-odd sites.
    //!
    //! **The node outlives the endpoint.** Each test declares its nodes before its `Endpoint`, and
    //! Rust drops locals in reverse declaration order, so `e` is destroyed first. A node parked on a
    //! queue when the test ends is therefore still valid when the endpoint goes away, which is the
    //! "stays valid for as long as it may be queued" half of `send`/`recv`'s contract.
    //!
    //! **The node is on no queue when it is passed.** This is the half that is NOT free, because
    //! several tests reuse one receiver node: it holds only because a `recv` that returns `Signal`
    //! or `FromSender` never queues its argument. Where a site depends on that, its own comment
    //! says so.

    use super::*;

    struct N {
        next: Option<NonNull<N>>,
    }

    // SAFETY: `next` and `set_next` read and write the same `next` field and nothing else, which is the whole of the `Node` contract.
    unsafe impl Node for N {
        fn next(&self) -> Option<NonNull<Self>> {
            self.next
        }
        fn set_next(&mut self, next: Option<NonNull<Self>>) {
            self.next = next;
        }
    }

    fn node() -> Box<N> {
        Box::new(N { next: None })
    }

    /// The rendezvous, both orderings: whoever arrives first waits, the second completes the pair
    /// and gets the first: the very node, by identity, not a name to look up.
    #[test]
    fn sender_first_then_receiver_rendezvous() {
        let (mut s, mut r) = (node(), node());
        let sp = NonNull::from(&mut *s);
        let mut e: Endpoint<N> = Endpoint::new();

        // SAFETY: `sp` is a live node, on no queue (see the module note).
        assert_eq!(unsafe { e.send(sp) }, Send::Blocked); // nobody waiting: park the sender
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.recv(NonNull::from(&mut *r)) },
            Recv::FromSender(sp)
        ); // receiver collects it
        assert!(e.one_queue_invariant());
    }

    #[test]
    fn receiver_first_then_sender_rendezvous() {
        let (mut s, mut r) = (node(), node());
        let rp = NonNull::from(&mut *r);
        let mut e: Endpoint<N> = Endpoint::new();

        // SAFETY: `rp` is a live node, on no queue (see the module note).
        assert_eq!(unsafe { e.recv(rp) }, Recv::Blocked);
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.send(NonNull::from(&mut *s)) },
            Send::Rendezvous(rp)
        ); // sender meets the waiter
    }

    /// Two senders queue in FIFO order; two receivers drain them in the same order.
    #[test]
    fn senders_queue_fifo() {
        let (mut a, mut b, mut r) = (node(), node(), node());
        let (ap, bp) = (NonNull::from(&mut *a), NonNull::from(&mut *b));
        let mut e: Endpoint<N> = Endpoint::new();

        // SAFETY: `ap` and `bp` are live nodes, each on no queue (see the module note).
        assert_eq!(unsafe { e.send(ap) }, Send::Blocked);
        // SAFETY: as above.
        assert_eq!(unsafe { e.send(bp) }, Send::Blocked);
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.recv(NonNull::from(&mut *r)) },
            Recv::FromSender(ap)
        );
        // SAFETY: as above; the previous `recv` returned `FromSender`, so `r` was never queued and is still on no queue.
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.recv(NonNull::from(&mut *r)) },
            Recv::FromSender(bp)
        );
    }

    /// A signal with nobody waiting is counted; the next receives drain it, then block.
    #[test]
    fn a_signal_to_an_empty_endpoint_is_counted_then_drained() {
        let mut r = node();
        let mut e: Endpoint<N> = Endpoint::new();

        assert_eq!(e.signal(), None); // counted
        assert_eq!(e.signal(), None);
        // SAFETY: `r` is a live node, on no queue (see the module note). The first two calls drain counted signals and never queue it; only the third parks it.
        assert_eq!(unsafe { e.recv(NonNull::from(&mut *r)) }, Recv::Signal);
        // SAFETY: as above.
        assert_eq!(unsafe { e.recv(NonNull::from(&mut *r)) }, Recv::Signal);
        // SAFETY: as above.
        assert_eq!(unsafe { e.recv(NonNull::from(&mut *r)) }, Recv::Blocked);
    }

    /// The endpoint-destroy contract (object revocation): `drain_waiters` hands back every parked
    /// thread exactly once, in queue order, and leaves the endpoint idle. The kernel's `revoke`
    /// wakes each one with an error; if a waiter were skipped it would sleep forever on a dead
    /// endpoint, and if one were handed back twice it would be double-queued on a run queue.
    #[test]
    fn drain_hands_back_every_waiter_and_leaves_the_endpoint_idle() {
        let (mut a, mut b, mut r) = (node(), node(), node());
        let (ap, bp) = (NonNull::from(&mut *a), NonNull::from(&mut *b));
        // Via `default()`: the kernel retypes endpoint pages through it, not through `new()`.
        let mut e: Endpoint<N> = Endpoint::default();

        // SAFETY: `ap` and `bp` are live nodes, each on no queue (see the module note).
        assert_eq!(unsafe { e.send(ap) }, Send::Blocked);
        // SAFETY: as above.
        assert_eq!(unsafe { e.send(bp) }, Send::Blocked);
        assert!(!e.is_idle(), "parked senders hold the endpoint live");

        let mut drained = Vec::new();
        e.drain_waiters(|w| drained.push(w));
        assert_eq!(drained, [ap, bp]);
        assert!(e.is_idle());

        // The other queue drains through the same path: a receiver can be parked too.
        let rp = NonNull::from(&mut *r);
        // SAFETY: `rp` is a live node, on no queue (see the module note); `drain_waiters` emptied the queues above.
        assert_eq!(unsafe { e.recv(rp) }, Recv::Blocked);
        drained.clear();
        e.drain_waiters(|w| drained.push(w));
        assert_eq!(drained, [rp]);
        assert!(e.is_idle());
    }

    /// Pending signals do not hold an endpoint live: `is_idle` counts blocked threads, not
    /// counters. An endpoint whose only state is undelivered signals is safe to destroy (a
    /// signal holds no thread, so nobody is left sleeping), and revocation relies on that.
    #[test]
    fn pending_signals_do_not_make_an_endpoint_busy() {
        let mut e: Endpoint<N> = Endpoint::new();
        assert_eq!(e.signal(), None);
        assert_eq!(e.signal(), None);
        assert!(e.is_idle());
    }

    /// **A queued sender can be taken back out of the middle**, which is what reaping a corpse
    /// needs (DECISIONS §32, and §16's `DESTROY` before it): a supervised thread that died with
    /// nobody receiving is parked here with its death message, and freeing it while it is still
    /// linked would leave the next `recv` following a dangling pointer. The survivors keep FIFO
    /// order, the length drops by exactly one, and removing something that is not queued reports
    /// `false` and changes nothing.
    #[test]
    fn a_queued_sender_can_be_removed_from_the_middle() {
        let (mut a, mut b, mut c, mut r) = (node(), node(), node(), node());
        let (ap, bp, cp) = (
            NonNull::from(&mut *a),
            NonNull::from(&mut *b),
            NonNull::from(&mut *c),
        );
        let mut e: Endpoint<N> = Endpoint::new();

        for p in [ap, bp, cp] {
            // SAFETY: `ap`, `bp` and `cp` are live nodes, each on no queue (see the module note).
            assert_eq!(unsafe { e.send(p) }, Send::Blocked);
        }
        // SAFETY: `bp` is compared by pointer and never dereferenced; the senders that get re-queued are the same live locals.
        assert!(unsafe { e.remove_sender(bp) }, "b was queued");
        assert_eq!(e.debug_counts().0, 2, "exactly one sender left the queue");
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.recv(NonNull::from(&mut *r)) },
            Recv::FromSender(ap)
        );
        assert_eq!(
            // SAFETY: as above.
            unsafe { e.recv(NonNull::from(&mut *r)) },
            Recv::FromSender(cp)
        );
        // SAFETY: as above.
        assert_eq!(unsafe { e.recv(NonNull::from(&mut *r)) }, Recv::Blocked);

        // Not queued (already collected, the ordinary case): a no-op that says so.
        let mut e2: Endpoint<N> = Endpoint::new();
        // SAFETY: `ap` is not queued on `e2` at all, and `remove_sender` only compares pointers, so nothing is dereferenced.
        assert!(!unsafe { e2.remove_sender(ap) });
        assert!(e2.is_idle());
    }

    /// Removing the *only* queued sender leaves the endpoint idle rather than a queue with a stale
    /// tail: the classic drained-to-empty bug, which matters here because the single-corpse case is
    /// the common one.
    #[test]
    fn removing_the_only_sender_leaves_the_endpoint_idle() {
        let (mut a, mut r) = (node(), node());
        let ap = NonNull::from(&mut *a);
        let mut e: Endpoint<N> = Endpoint::new();

        // SAFETY: `ap` is a live node, on no queue (see the module note).
        assert_eq!(unsafe { e.send(ap) }, Send::Blocked);
        // SAFETY: `ap` is compared by pointer, never dereferenced.
        assert!(unsafe { e.remove_sender(ap) });
        assert!(e.is_idle(), "the endpoint still holds a sender");
        // SAFETY: as above.
        assert_eq!(unsafe { e.recv(NonNull::from(&mut *r)) }, Recv::Blocked);
        // And it can be used again afterwards: push, pop, no ghost.
        assert!(e.one_queue_invariant());
    }

    /// A signal with a receiver waiting hands it back directly and counts nothing.
    #[test]
    fn a_signal_wakes_a_waiting_receiver() {
        let mut r = node();
        let rp = NonNull::from(&mut *r);
        let mut e: Endpoint<N> = Endpoint::new();

        // SAFETY: `rp` is a live node, on no queue (see the module note).
        assert_eq!(unsafe { e.recv(rp) }, Recv::Blocked);
        assert_eq!(e.signal(), Some(rp)); // the waiter, dequeued
        assert!(e.one_queue_invariant());
    }

    /// The manual `PartialEq` and `Debug` impls answer for themselves. Every use above compares
    /// equal values, so an eq stuck at `true` passed (milestone 85); different variants must
    /// disagree, and the rendering is the string a hang dump prints.
    #[test]
    fn verdict_variants_are_distinct_and_print_their_names() {
        let mut s = node();
        let sp = NonNull::from(&mut *s);
        assert_ne!(Send::<N>::Blocked, Send::Rendezvous(sp));
        assert_ne!(Recv::<N>::Signal, Recv::Blocked);
        assert_ne!(Recv::<N>::FromSender(sp), Recv::Signal);
        assert_eq!(format!("{:?}", Send::<N>::Blocked), "Blocked");
        assert_eq!(format!("{:?}", Recv::<N>::Signal), "Signal");
        assert!(format!("{:?}", Recv::<N>::FromSender(sp)).starts_with("FromSender"));
    }
}
