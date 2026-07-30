//! An intrusive FIFO queue: **the link lives inside the node.**
//!
//! Milestone 14 phase A.2 (design/kernel-objects-from-untyped.md, decision D1). The per-CPU run
//! queues and migration inboxes used to be `VecDeque<Tid>`: every entry was heap-backed, pushing
//! could allocate (which is why the scheduler pre-reserved capacity, a standing apology to the
//! IRQ path), and every pop handed back a number that still had to be looked up.
//!
//! An intrusive queue stores nothing. The "next" pointer lives inside the thing being queued,
//! and the queue itself is two pointers, head and tail. Push and pop are a couple of pointer
//! writes, they cannot allocate and cannot fail, and what a pop hands back is the object itself.
//! This is what every kernel we learn from does (Linux `list_head`, seL4's TCB queues), for
//! exactly these reasons.
//!
//! # The price, stated plainly
//!
//! **One link means one queue.** A node can be on at most one `Fifo` at a time, because there is
//! only one `next` inside it. For a scheduler this is not a restriction, it is the *invariant*:
//! a thread is ready on one core's queue, or parked in one inbox, or blocked on one endpoint, or
//! running, and never two of those at once. The queue makes the state machine physical.
//!
//! **And the compiler cannot check it.** A `VecDeque` owns its entries; this queue borrows its
//! nodes with no lifetime the borrow checker can see. The rules the caller must keep (below) are
//! enforced by discipline and stated invariants, which is why the mutating half of the API is
//! `unsafe`. The queue is memory the caller owns, threaded through itself.
//!
//! # The caller's contract
//!
//! 1. A node is pushed onto at most one queue, and not pushed again until popped.
//! 2. A node outlives its time on the queue (the kernel: a queued thread is `Ready`, and only
//!    `Finished` threads are ever freed).
//! 3. All access to a queue and its nodes' links is serialized by the caller (the kernel: a run
//!    queue is single-core with interrupts masked; an inbox is behind its mutex).
//!
//! # What a static analyser sees here, and why it is right (DECISIONS §35)
//!
//! CodeQL flags the dereferences in [`Fifo::push_back`] and [`Fifo::pop_front`] as
//! `rust/access-invalid-pointer`. It is not wrong, and the alerts are dismissed with this paragraph
//! as the reason rather than silently suppressed.
//!
//! **Nullness is gone from the contract**: the API takes and returns [`NonNull`], and every caller
//! converts from a reference, so non-nullness is a fact of construction rather than a promise. That
//! half of the alert was a real defect and was fixed.
//!
//! **Validity is not expressible**, and that is the design. Rule 2 above says a node outlives its
//! time on the queue, and no type available to us can carry that for a structure whose entire purpose
//! is that the queue does *not* own its nodes. What upholds it is the kernel's state machine: only
//! `Finished` threads are ever freed, a `Finished` thread is on no queue, and one link per node makes
//! "on at most one queue" physical rather than remembered.
//!
//! So the honest statement is that **this queue is safe because its callers are correct, and nothing
//! in this crate can check that**. The Kani harness below proves the FIFO's *logic* over a symbolic
//! operation sequence against nodes it holds valid by construction; it answers "is the ordering
//! right", never "did a caller free a queued node". Neither tool covers that, and a reader should
//! know it rather than infer safety from two green checkmarks.

#![cfg_attr(not(test), no_std)]

use core::ptr::NonNull;

/// A type that carries its own queue link.
///
/// # Safety
///
/// `next`/`set_next` must be plain storage: reading back exactly what was stored, touching
/// nothing else. The queue threads its structure through these two methods, so a clever
/// implementation is a corrupted queue.
pub unsafe trait Node: Sized {
    fn next(&self) -> Option<NonNull<Self>>;
    fn set_next(&mut self, next: Option<NonNull<Self>>);
}

/// The queue: two pointers into nodes it does not own, plus a count.
///
/// FIFO because the scheduler's queues are round-robin: threads leave in the order they arrived.
pub struct Fifo<T: Node> {
    head: Option<NonNull<T>>,
    tail: Option<NonNull<T>>,
    len: usize,
}

// SAFETY: the queue owns no data; it holds nodes only under the caller's contract (rule 3: all
// access serialized by the caller). Same justification as the slab allocator's Send: sharing is
// the caller's problem, solved with a lock or a single-owner rule.
unsafe impl<T: Node> Send for Fifo<T> {}

impl<T: Node> Fifo<T> {
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Append a node.
    ///
    /// # Safety
    ///
    /// `node` must be valid to dereference, not currently on any queue, and must stay valid
    /// until popped (the contract in the crate docs). Null is no longer part of that contract:
    /// [`NonNull`] carries it in the type.
    pub unsafe fn push_back(&mut self, node: NonNull<T>) {
        // SAFETY: valid and exclusively ours to link, per the caller's contract.
        unsafe { (*node.as_ptr()).set_next(None) };

        match self.tail {
            // SAFETY: `tail` was pushed earlier under the same contract and not yet popped.
            Some(tail) => unsafe { (*tail.as_ptr()).set_next(Some(node)) },
            None => self.head = Some(node),
        }
        self.tail = Some(node);
        self.len += 1;
    }

    /// Detach and return the oldest node. The returned node's link is cleared: it leaves the
    /// queue carrying no dangling reference into it.
    pub fn pop_front(&mut self) -> Option<NonNull<T>> {
        let node = self.head?;
        // SAFETY: every node between head and tail was pushed under the contract and is still
        // valid (rule 2); we are the only accessor (rule 3).
        unsafe {
            self.head = (*node.as_ptr()).next();
            if self.head.is_none() {
                self.tail = None;
            }
            (*node.as_ptr()).set_next(None);
        }
        self.len -= 1;
        Some(node)
    }
}

impl<T: Node> Default for Fifo<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-checked proofs (DECISIONS §14). The interesting property of a queue is not one push
/// or one pop but what holds across *any* interleaving of them, so the harness drives the real
/// `Fifo` with a symbolic operation sequence and checks it against a trivially-correct model
/// (an array and two counters). Every push/pop pattern up to the bound is covered at once,
/// including the regrowth-from-empty transitions where head/tail bugs live.
#[cfg(kani)]
mod verification {
    use super::*;

    struct N {
        next: Option<NonNull<N>>,
        tag: usize,
    }

    unsafe impl Node for N {
        fn next(&self) -> Option<NonNull<Self>> {
            self.next
        }
        fn set_next(&mut self, next: Option<NonNull<Self>>) {
            self.next = next;
        }
    }

    /// **FIFO order, no loss, no invention, across every operation sequence.** Six symbolic
    /// steps over three nodes: each step pushes some not-currently-queued node or pops. The
    /// model records the order tags went in; the queue must hand them back in exactly that
    /// order, agree about emptiness and length at every step, and never dereference a stale
    /// link (which Kani would report as an invalid pointer access).
    #[kani::proof]
    fn any_push_pop_interleaving_is_fifo_and_lossless() {
        let mut nodes = [
            N { next: None, tag: 0 },
            N { next: None, tag: 1 },
            N { next: None, tag: 2 },
        ];
        // NonNull, taken from references, so the harness proves the same API the kernel calls.
        let ptrs: [NonNull<N>; 3] = [
            NonNull::from(&mut nodes[0]),
            NonNull::from(&mut nodes[1]),
            NonNull::from(&mut nodes[2]),
        ];

        let mut q: Fifo<N> = Fifo::new();

        // The model: a ring of tags in push order, and which nodes are currently queued.
        let mut model = [usize::MAX; 6];
        let (mut m_head, mut m_tail) = (0usize, 0usize);
        let mut queued = [false; 3];

        for _ in 0..6 {
            let choice: usize = kani::any();
            kani::assume(choice <= 3);
            if choice < 3 {
                if !queued[choice] {
                    queued[choice] = true;
                    model[m_tail] = choice;
                    m_tail += 1;
                    // SAFETY: the node is valid (a stack local), not queued (checked), and
                    // outlives the harness.
                    unsafe { q.push_back(ptrs[choice]) };
                }
            } else {
                let popped = q.pop_front();
                if m_head == m_tail {
                    assert!(popped.is_none(), "popped from an empty queue");
                } else {
                    let expect = model[m_head];
                    m_head += 1;
                    queued[expect] = false;
                    // SAFETY: pop returns only nodes we pushed, all still valid.
                    let got = unsafe { (*popped.expect("lost a node").as_ptr()).tag };
                    assert_eq!(got, expect, "not FIFO");
                }
            }
            assert_eq!(q.len(), m_tail - m_head);
            assert_eq!(q.is_empty(), m_head == m_tail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N {
        next: Option<NonNull<N>>,
        tag: u32,
    }

    unsafe impl Node for N {
        fn next(&self) -> Option<NonNull<Self>> {
            self.next
        }
        fn set_next(&mut self, next: Option<NonNull<Self>>) {
            self.next = next;
        }
    }

    fn node(tag: u32) -> Box<N> {
        Box::new(N { next: None, tag })
    }

    #[test]
    fn fifo_order() {
        let (mut a, mut b, mut c) = (node(1), node(2), node(3));
        let mut q: Fifo<N> = Fifo::new();
        assert!(q.is_empty());

        unsafe {
            q.push_back(NonNull::from(&mut *a));
            q.push_back(NonNull::from(&mut *b));
            q.push_back(NonNull::from(&mut *c));
        }
        assert_eq!(q.len(), 3);

        let tags: Vec<u32> = core::iter::from_fn(|| q.pop_front())
            .map(|p| unsafe { (*p.as_ptr()).tag })
            .collect();
        assert_eq!(tags, vec![1, 2, 3]);
        assert!(q.is_empty());
    }

    /// The empty-again transitions: a queue drained to empty accepts new nodes correctly (the
    /// classic tail-pointer bug), and a popped node can be pushed again.
    #[test]
    fn drain_then_reuse() {
        let (mut a, mut b) = (node(1), node(2));
        let mut q: Fifo<N> = Fifo::new();

        unsafe { q.push_back(NonNull::from(&mut *a)) };
        assert_eq!(q.pop_front().map(|p| unsafe { (*p.as_ptr()).tag }), Some(1));
        assert!(q.pop_front().is_none());

        unsafe {
            q.push_back(NonNull::from(&mut *b));
            q.push_back(NonNull::from(&mut *a)); // popped above, so it may be queued again
        }
        assert_eq!(q.pop_front().map(|p| unsafe { (*p.as_ptr()).tag }), Some(2));
        assert_eq!(q.pop_front().map(|p| unsafe { (*p.as_ptr()).tag }), Some(1));
        assert!(q.is_empty());
    }

    /// A popped node leaves with a clean link: it does not secretly point back into the queue.
    #[test]
    fn a_popped_node_carries_no_link() {
        let (mut a, mut b) = (node(1), node(2));
        let mut q: Fifo<N> = Fifo::new();
        unsafe {
            q.push_back(NonNull::from(&mut *a));
            q.push_back(NonNull::from(&mut *b));
        }
        let p = q.pop_front().unwrap();
        assert!(unsafe { (*p.as_ptr()).next() }.is_none());
    }
}
