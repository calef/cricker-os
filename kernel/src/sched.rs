//! A round-robin scheduler, and the preemption that makes it mean something.
//!
//! # The whole point of the project, arriving
//!
//! DECISIONS.md §5, written before a line of kernel existed:
//!
//! > A userspace process is an arbitrary ELF binary. It has its own stack, it never yields, and
//! > it will loop forever because we will write a bug. Under cooperative scheduling, one bad
//! > user program hangs the machine permanently.
//!
//! This file is where that stops being true. The timer fires, the handler calls [`schedule`],
//! and the CPU is **taken away** from a thread that never asked to give it up.
//!
//! There is a test named `a_thread_that_never_yields_is_preempted_anyway`. It spawns a thread
//! whose entire body is `loop { count += 1 }` — no yields, no syscalls, not even a function
//! call. Under any cooperative scheduler that is a hung machine. Here it is a Tuesday.
//!
//! # Three rules, and each of them is a bug if you get it wrong
//!
//! **1. Release the run-queue lock BEFORE switching.** Switch away while holding it and the
//!    lock is now held by a thread that is not running. The next thread to want it spins
//!    forever waiting for a thread that will never be scheduled, because scheduling requires
//!    the lock. A deadlock of a shape that would take a day to find.
//!
//! **2. Interrupts stay masked across the switch.** Between "I decided to switch" and "I
//!    switched" there must be no window for a timer interrupt to decide *again*. And the mask
//!    is per-thread, because each thread's `schedule()` frame lives on its own stack, which is
//!    exactly what makes this work at all.
//!
//! **3. A brand-new thread must unmask interrupts itself.** Every *resumed* thread gets its
//!    interrupt state back from `eret` restoring `SPSR_EL1`. A thread that has never run has no
//!    `SPSR` to restore. `thread_trampoline` does `msr daifclr, #2` for exactly this reason,
//!    and without it the first thread you spawn can never be preempted — which would be a
//!    cooperative scheduler with extra steps.

// current(), voluntary_switches() and friends have no non-test caller yet. They are the API a
// scheduler is expected to have, and milestone 7 (processes) is the first real consumer.
#![allow(dead_code)]

use crate::sync::{IrqSafeMutex, rank};
use crate::thread::{Context, State, Thread, Tid, switch_to};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Set by the timer handler. Read by the *return path* out of the handler.
///
/// The handler itself does not switch — it **records** that a switch is wanted, which is
/// DECISIONS.md §9 ("interrupt handlers record and defer") applied to the most important
/// deferral in the kernel.
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);

/// How many times we have actually taken the CPU away from a thread. The number that says
/// preemption is real.
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);
static VOLUNTARY_SWITCHES: AtomicU64 = AtomicU64::new(0);

/// A synchronous IPC rendezvous point.
///
/// **At most one of these queues is ever non-empty.** A sender that finds a receiver waiting
/// delivers immediately and neither blocks; a receiver that finds a sender waiting collects
/// immediately. So a thread only ends up in a queue when *nobody* was waiting for it, and the
/// two queues are two sides of the same coin: whichever kind of thread arrived first and had to
/// wait. Keeping both is not redundant, it just spares us reasoning about which coin we are
/// holding.
#[derive(Default)]
struct Endpoint {
    senders: VecDeque<Tid>,
    receivers: VecDeque<Tid>,
}

struct Scheduler {
    /// `Box` because the assembly writes through `&mut thread.context`, so a thread's address
    /// must never move. A `BTreeMap` reshuffles its nodes; a `Box`'s contents do not.
    threads: BTreeMap<Tid, Box<Thread>>,
    /// Ready to run, in order. Round robin: pop the front, push the back.
    ready: VecDeque<Tid>,
    current: Tid,
    /// Every IPC endpoint. Indexed by the `usize` inside an `Object::Endpoint` capability, which
    /// only the kernel mints, so the index is always in range.
    endpoints: alloc::vec::Vec<Endpoint>,
}

/// Rank **above the allocators**, because `spawn` pushes into a `VecDeque` while holding this,
/// and that push may allocate.
///
/// `schedule()` itself never allocates: it pops one Tid and pushes one back, so the deque's
/// length never exceeds its capacity and it cannot grow. That is what makes it safe to call
/// from the timer interrupt, where DECISIONS.md §9 forbids allocation.
///
/// (A real kernel uses an **intrusive** list — the next-pointer lives inside the `Thread`
/// itself — so the run queue can never allocate at all. Worth doing if this ever bites.)
static SCHED: IrqSafeMutex<Option<Scheduler>> = IrqSafeMutex::new(rank::SCHED, None);

/// Adopt the context we are already running in as thread 0.
///
/// It has no stack of its own and no saved context. **The first switch *away* from it fills
/// that in**, which is why the boot thread needs no special case: a thread's context is written
/// by the act of leaving it.
pub fn init() {
    let mut sched = SCHED.lock();
    let boot = Box::new(Thread::boot());

    let mut threads = BTreeMap::new();
    threads.insert(0, boot);

    *sched = Some(Scheduler {
        threads,
        // Capacity up front so `schedule()`'s push can never reallocate. See the rank note.
        ready: VecDeque::with_capacity(64),
        current: 0,
        endpoints: alloc::vec::Vec::new(),
    });
}

pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> Option<Tid> {
    // Build the thread — which allocates a stack, maps four pages, and boxes the closure —
    // OUTSIDE the lock. Critical sections stay short (DECISIONS.md §9), and this one would
    // otherwise hold the scheduler across four page-table walks.
    let thread = Thread::spawn(Box::new(f))?;
    let id = thread.id;

    let mut guard = SCHED.lock();
    let sched = guard.as_mut()?;
    sched.threads.insert(id, Box::new(thread));
    sched.ready.push_back(id);

    Some(id)
}

/// Give up the CPU voluntarily.
pub fn yield_now() {
    VOLUNTARY_SWITCHES.fetch_add(1, Ordering::Relaxed);
    schedule();
}

/// The current thread is done. Never returns.
pub fn exit() -> ! {
    {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("exit before sched::init");
        let current = sched.current;
        if let Some(t) = sched.threads.get_mut(&current) {
            t.state = State::Finished;
        }
        // Deliberately NOT pushed back onto the ready queue, and deliberately not removed from
        // `threads` either: we are still running on its stack. Dropping the `Thread` here would
        // unmap the very stack these instructions are using.
        //
        // The reaping happens in `schedule()`, from the *next* thread, once we are safely off
        // this stack. Classic, and the reason every kernel has something called a reaper.
    }

    schedule();
    unreachable!("a finished thread was scheduled again");
}

/// Called from the timer IRQ. **Records** that a switch is wanted; does not switch.
pub fn on_tick() {
    NEED_RESCHED.store(true, Ordering::Relaxed);
}

pub fn take_need_resched() -> bool {
    NEED_RESCHED.swap(false, Ordering::Relaxed)
}

/// Pick another thread and go there.
///
/// May be called from normal context (a voluntary `yield_now`) or from the tail of the timer
/// IRQ handler (a preemption). The two paths are identical from here down, which is a large
/// part of why this is only forty lines.
pub fn schedule() {
    // Rule 2: no interrupts across the decision *or* the switch. Between "I chose a thread" and
    // "I am running it" there must be no window for the timer to choose again.
    //
    // The saved state is a local, on **this thread's stack**, which is exactly what makes it
    // correct: when someone eventually switches back to us, `switch_to` returns here, and this
    // frame — with the right `was_enabled` in it — is still sitting where we left it.
    let was_enabled = crate::arch::interrupts::disable();

    let switch = {
        let mut guard = SCHED.lock();
        let Some(sched) = guard.as_mut() else {
            crate::arch::interrupts::restore(was_enabled);
            return;
        };

        // Reap anything that finished. Safe *here* and nowhere else: whoever exited is no
        // longer running, because we are.
        reap(sched);

        let current = sched.current;
        let state = sched.threads.get(&current).map(|t| t.state);

        // **Only a still-Running thread goes back on the ready queue.** A thread that reached
        // here after marking itself `Blocked` (it is waiting for IPC) or `Finished` must not be
        // rescheduled, and this one line is what makes blocking work: `schedule()` can be
        // called from the timer IRQ *while* a thread is mid-way through blocking itself, and it
        // must not undo that by helpfully requeueing it.
        let runnable = state == Some(State::Running);

        let Some(next) = sched.ready.pop_front() else {
            // Nobody else wants the CPU.
            if runnable {
                // Keep it. A thread yielding into an empty run queue simply carries on.
                crate::arch::interrupts::restore(was_enabled);
                return;
            }
            // We are Blocked or Finished and there is nothing to switch to.
            match state {
                Some(State::Finished) => panic!("the last thread exited; nothing left to run"),
                _ => panic!(
                    "every thread is blocked on IPC: a deadlock, and a real one — no interrupt \
                     will ever make an endpoint ready"
                ),
            }
        };

        if runnable {
            sched.threads.get_mut(&current).unwrap().state = State::Ready;
            sched.ready.push_back(current); // round robin: back of the queue
        }

        sched.threads.get_mut(&next).unwrap().state = State::Running;
        sched.current = next;

        // The incoming thread's low half. A kernel thread gets the empty reserved table, which
        // makes every low address fault, which is exactly right: it has no business down there.
        let next_root = sched.threads[&next]
            .space
            .as_ref()
            .map(|s| s.root())
            .unwrap_or_else(crate::arch::mmu::reserved_root);

        // Copy the two raw pointers out before the lock drops. The assembly writes through the
        // first and reads the second, and both threads' `Box`es keep their contents pinned.
        let prev_slot: *mut *mut Context = &mut sched.threads.get_mut(&current).unwrap().context;
        let next_ctx: *mut Context = sched.threads.get(&next).unwrap().context;

        Some((prev_slot, next_ctx, next_root))
    };
    // Rule 1: THE LOCK IS RELEASED HERE, before the switch. Holding it across `switch_to` would
    // leave it held by a thread that is not running, and the next thread to want it would spin
    // forever waiting for a thread that can only be scheduled by taking the lock.

    if let Some((prev_slot, next_ctx, next_root)) = switch {
        // Install the incoming thread's address space FIRST. `TTBR0_EL1` is one register, shared
        // by everybody, and a thread that resumes at EL0 in the previous thread's low half is
        // running a stranger's code. (No-ops, including no TLB flush, when the root is already
        // right — which is every switch between two kernel threads.)
        crate::arch::mmu::switch_user_root(next_root);

        // SAFETY: both pointers name live `Context`s owned by boxed `Thread`s in the map, and
        // interrupts are masked so nothing can reorder underneath us.
        //
        // This call does not return here. It returns *in another thread*, at the point where
        // that thread last called `switch_to`. We come back only when somebody switches to us.
        unsafe { switch_to(prev_slot, next_ctx) };
    }

    crate::arch::interrupts::restore(was_enabled);
}

/// Free the stacks of threads that have finished.
///
/// Must run from a *different* thread than the one being reaped: dropping a `KernelStack`
/// unmaps its pages, and a thread cannot unmap the stack it is standing on.
fn reap(sched: &mut Scheduler) {
    let dead: alloc::vec::Vec<Tid> = sched
        .threads
        .iter()
        .filter(|(id, t)| **id != 0 && **id != sched.current && t.state == State::Finished)
        .map(|(id, _)| *id)
        .collect();

    for id in dead {
        sched.threads.remove(&id); // drops the Thread, drops the KernelStack, unmaps, frees
    }
}

/// Create an IPC endpoint. Returns its id, which is what goes inside an `Object::Endpoint`.
pub fn create_endpoint() -> usize {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");
    sched.endpoints.push(Endpoint::default());
    sched.endpoints.len() - 1
}

/// Move a blocked thread back to the ready queue. Caller holds the lock.
fn wake(sched: &mut Scheduler, tid: Tid) {
    if let Some(t) = sched.threads.get_mut(&tid) {
        if t.state == State::Blocked {
            t.state = State::Ready;
            sched.ready.push_back(tid);
        }
    }
}

/// **Send three words to an endpoint, blocking until a receiver takes them.**
///
/// The synchronous rendezvous, sender's half:
///
/// - **A receiver is already waiting.** Drop the message straight into its mailbox, wake it, and
///   carry on. Nobody blocked; the rendezvous was instantaneous.
/// - **Nobody is waiting.** Park the message in our own mailbox, join the endpoint's sender
///   queue, mark ourselves `Blocked`, and `schedule()` away. A future receiver will reach into
///   our mailbox, wake us, and we return from `schedule()` as if no time had passed.
///
/// Callable by a kernel thread directly (this function) or by a user thread through the `SEND`
/// method on an endpoint capability (see syscall.rs). Same code underneath.
pub fn ipc_send(ep: usize, msg: [u64; 3]) {
    let block = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = sched.current;

        if let Some(receiver) = sched.endpoints[ep].receivers.pop_front() {
            sched.threads.get_mut(&receiver).unwrap().mailbox = msg;
            wake(sched, receiver);
            false
        } else {
            let me = sched.threads.get_mut(&current).unwrap();
            me.mailbox = msg;
            me.state = State::Blocked;
            sched.endpoints[ep].senders.push_back(current);
            true
        }
    };

    // Block OUTSIDE the lock (rule 1), and only after we have already recorded ourselves as
    // blocked, so a timer-driven `schedule()` in the gap does the right thing either way.
    if block {
        schedule();
    }
}

/// **Receive three words from an endpoint, blocking until one arrives.** The mirror of
/// [`ipc_send`].
pub fn ipc_recv(ep: usize) -> [u64; 3] {
    let immediate = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = sched.current;

        if let Some(sender) = sched.endpoints[ep].senders.pop_front() {
            let msg = sched.threads.get(&sender).unwrap().mailbox;
            wake(sched, sender);
            Some(msg)
        } else {
            sched.threads.get_mut(&current).unwrap().state = State::Blocked;
            sched.endpoints[ep].receivers.push_back(current);
            None
        }
    };

    match immediate {
        Some(msg) => msg,
        None => {
            schedule(); // blocks; a sender fills our mailbox and wakes us
            let guard = SCHED.lock();
            let sched = guard.as_ref().expect("no scheduler");
            sched.threads.get(&sched.current).unwrap().mailbox
        }
    }
}

/// Look up a capability in the **current thread's** table.
///
/// The lookup that is the security mechanism. `slot` came from userspace, in a register, and it
/// indexes an array that lives in kernel memory and that userspace has never seen. An empty slot
/// is `NoSuchSlot`, which is not "permission denied": **there is nothing there.**
pub fn current_cap(slot: u64) -> Result<crate::cap::Cap, crate::cap::Error> {
    let guard = SCHED.lock();
    let sched = guard.as_ref().ok_or(crate::cap::Error::NoSuchSlot)?;
    sched
        .threads
        .get(&sched.current)
        .ok_or(crate::cap::Error::NoSuchSlot)?
        .cspace
        .get(slot)
}

/// Hand the current thread a capability. **The only way authority ever enters a process.**
pub fn grant(cap: crate::cap::Cap) -> Result<u64, crate::cap::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(crate::cap::Error::NoFreeSlot)?;
    let current = sched.current;
    sched
        .threads
        .get_mut(&current)
        .ok_or(crate::cap::Error::NoFreeSlot)?
        .cspace
        .insert(cap)
}

/// Hand a **specific** thread a capability. Used to wire up a scenario before the thread runs.
pub fn grant_to(tid: Tid, cap: crate::cap::Cap) -> Result<u64, crate::cap::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(crate::cap::Error::NoFreeSlot)?;
    sched
        .threads
        .get_mut(&tid)
        .ok_or(crate::cap::Error::NoFreeSlot)?
        .cspace
        .insert(cap)
}

/// Hand the current thread an address space, and install it.
///
/// From here the thread owns its low half: the reaper's `drop` will unmap and free it, and
/// every context switch back to this thread will re-install it.
pub fn adopt_address_space(space: crate::user::AddressSpace) {
    let root = space.root();

    {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = sched.current;
        sched.threads.get_mut(&current).expect("no current thread").space = Some(space);
    }

    crate::arch::mmu::switch_user_root(root);
}

/// The top of the current thread's kernel stack: **where its `TrapFrame` belongs.**
///
/// `None` for the boot thread, which runs on the stack `boot.s` set up and does not own it.
///
/// A user thread's TrapFrame is not an ordinary local. It must sit at exactly the address the
/// vector table's `SAVE_CONTEXT` will rebuild it at when the user traps in, because `eret`
/// leaves `SP_EL1` pointing just past it and the hardware does not consult our intentions.
pub fn current_kernel_stack_top() -> Option<u64> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    sched
        .threads
        .get(&sched.current)?
        .stack
        .as_ref()
        .map(|s| s.top())
}

pub fn current() -> Tid {
    SCHED.lock().as_ref().map_or(0, |s| s.current)
}

pub fn thread_count() -> usize {
    SCHED.lock().as_ref().map_or(0, |s| s.threads.len())
}

pub fn preemptions() -> u64 {
    PREEMPTIONS.load(Ordering::Relaxed)
}

pub fn count_preemption() {
    PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
}

pub fn voluntary_switches() -> u64 {
    VOLUNTARY_SWITCHES.load(Ordering::Relaxed)
}

pub fn is_running() -> bool {
    SCHED.lock().is_some()
}

#[cfg(test)]
mod tests {
    //! Tests for threads, the context switch, and preemption.
    //!
    //! `a_thread_that_never_yields_is_preempted_anyway` is the one this whole project has been
    //! arguing about since DECISIONS.md §5. Everything else here is scaffolding for it.

    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A spawned thread actually runs, and its closure's captured state comes with it.
    #[test_case]
    fn a_spawned_thread_runs() {
        static RAN: AtomicBool = AtomicBool::new(false);
        static SAW: AtomicU64 = AtomicU64::new(0);

        let captured = 0xdead_beefu64;
        crate::sched::spawn(move || {
            SAW.store(captured, Ordering::SeqCst);
            RAN.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        // Yield until it has had a turn. Round robin, so this is quick.
        for _ in 0..100 {
            if RAN.load(Ordering::SeqCst) {
                break;
            }
            crate::sched::yield_now();
        }

        assert!(RAN.load(Ordering::SeqCst), "the thread never ran");
        assert_eq!(
            SAW.load(Ordering::SeqCst),
            0xdead_beef,
            "the closure's captured value did not survive the switch"
        );
    }

    /// Several threads take turns.
    #[test_case]
    fn threads_round_robin() {
        static COUNTS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
        static STOP: AtomicBool = AtomicBool::new(false);

        for i in 0..3usize {
            crate::sched::spawn(move || {
                while !STOP.load(Ordering::SeqCst) {
                    COUNTS[i].fetch_add(1, Ordering::SeqCst);
                    crate::sched::yield_now();
                }
            })
            .expect("spawn failed");
        }

        // Let them run.
        for _ in 0..300 {
            crate::sched::yield_now();
        }
        STOP.store(true, Ordering::SeqCst);
        for _ in 0..20 {
            crate::sched::yield_now();
        }

        for (i, c) in COUNTS.iter().enumerate() {
            assert!(c.load(Ordering::SeqCst) > 0, "thread {i} never ran");
        }
    }

    /// **THE TEST.**
    ///
    /// From DECISIONS.md §5, written before a single line of this kernel existed:
    ///
    /// > A userspace process is an arbitrary ELF binary. It has its own stack, **it never
    /// > yields**, and it will loop forever because we will write a bug. Under cooperative
    /// > scheduling, one bad user program hangs the machine permanently.
    ///
    /// So: a thread whose entire body is a tight loop. **No `yield_now`. No syscall. Not even a
    /// function call** — nothing a cooperative scheduler could possibly hook.
    ///
    /// Under async/await, or Go before 1.14, or any cooperative runtime, this thread takes the
    /// CPU and never gives it back, and the machine is gone. The only thing that can take it
    /// back is a timer interrupt landing between two instructions of that loop and switching
    /// the stack out from under it.
    ///
    /// If this test passes, the argument was right and the kernel can host untrusted code.
    /// If it hangs, it was wrong.
    #[test_case]
    fn a_thread_that_never_yields_is_preempted_anyway() {
        static SPINNING: AtomicU64 = AtomicU64::new(0);
        static STOP: AtomicBool = AtomicBool::new(false);
        static OTHER_RAN: AtomicBool = AtomicBool::new(false);

        let preemptions_before = crate::sched::preemptions();

        // The hostile thread. This is the arbitrary ELF binary, in miniature.
        crate::sched::spawn(|| {
            while !STOP.load(Ordering::Relaxed) {
                SPINNING.fetch_add(1, Ordering::Relaxed);
                // Deliberately nothing else. No yield. No call. Nothing to cooperate with.
            }
        })
        .expect("spawn failed");

        // A well-behaved thread that just wants a turn.
        crate::sched::spawn(|| {
            OTHER_RAN.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        // And now we wait, WITHOUT yielding either. If preemption does not work, nobody moves
        // and this hangs forever, which is its own kind of answer.
        let deadline = crate::arch::timer::now() + crate::arch::timer::frequency(); // 1 second
        while !OTHER_RAN.load(Ordering::SeqCst) {
            assert!(
                crate::arch::timer::now() < deadline,
                "ONE SECOND AND THE POLITE THREAD NEVER RAN. The spinner still owns the CPU, \
                 which means preemption is not working and a single bad program can hang this \
                 machine. This is precisely the failure DECISIONS.md §5 predicted for \
                 cooperative scheduling."
            );
            core::hint::spin_loop();
        }

        STOP.store(true, Ordering::Relaxed);

        assert!(
            SPINNING.load(Ordering::Relaxed) > 0,
            "the spinner never ran at all"
        );
        assert!(
            crate::sched::preemptions() > preemptions_before,
            "the CPU was never taken away from anyone: no preemption happened"
        );

        // Let the spinner notice STOP and exit, so it does not haunt the rest of the suite.
        for _ in 0..50 {
            crate::sched::yield_now();
        }
    }

    /// A finished thread's stack is unmapped and its frames returned.
    ///
    /// The reaping cannot happen in `exit()` — a thread cannot unmap the stack it is standing
    /// on. It happens in `schedule()`, from the *next* thread, once we are safely off it. Every
    /// kernel has something called a reaper, and this is why.
    #[test_case]
    fn a_finished_thread_is_reaped_and_its_memory_returned() {
        let threads_before = crate::sched::thread_count();

        fn batch_of_eight() {
            for _ in 0..8 {
                crate::sched::spawn(|| {}).expect("spawn failed");
            }
            // Let them all run and exit, and let the reaper catch up.
            for _ in 0..200 {
                crate::sched::yield_now();
            }
        }

        // The FIRST batch legitimately costs a couple of frames: the stack area is a fresh
        // region of virtual address space, so `map_page` has to build an L2 and an L3 page
        // table for it. Those are a one-time cost, not a leak — `unmap_page` frees the leaf
        // mapping but leaves the intermediate tables standing (see the TODO on `paging::unmap`).
        batch_of_eight();

        assert_eq!(
            crate::sched::thread_count(),
            threads_before,
            "finished threads were never reaped"
        );

        // The SECOND batch must cost EXACTLY NOTHING. The page tables exist, and the dead
        // threads' virtual address ranges went back on the free list, so eight new threads land
        // in the same addresses with the same tables.
        //
        // If this ever regresses, the kernel leaks two frames of page tables per 2 MiB of stack
        // address space consumed, forever, and threads come and go.
        let before = crate::memory::stats().unwrap().used;
        batch_of_eight();
        let after = crate::memory::stats().unwrap().used;

        assert_eq!(
            after,
            before,
            "a second batch of eight threads leaked {} frames: stack address ranges are not \
             being reused, so page tables accumulate forever",
            after.saturating_sub(before)
        );
    }

    /// Every thread stack has a guard page.
    ///
    /// A thread stack is 16 KiB — an eighth of the boot stack's — and threads are where deep
    /// recursion actually happens. Milestone 3's stack overflow hung the machine for 150
    /// seconds; a guard page turns the same bug into an instant fault naming the exact byte.
    #[test_case]
    fn every_thread_stack_has_a_guard_page() {
        use crate::arch::mmu;
        use crate::thread::{KernelStack, STACK_PAGES};

        let stack = KernelStack::new(STACK_PAGES).expect("could not allocate a thread stack");

        assert_eq!(
            mmu::translate(stack.guard()),
            None,
            "a thread stack's guard page IS MAPPED: an overflow would silently eat whatever is \
             below it"
        );

        // And the stack itself is real, writable memory directly above the hole.
        for i in 0..STACK_PAGES as u64 {
            let va = stack.bottom() + i * 4096;
            let (_, flags) = mmu::translate(va).expect("thread stack page is not mapped");
            assert!(flags.is_writable());
            assert!(
                !flags.is_kernel_executable(),
                "a thread stack is EXECUTABLE"
            );
        }
    }

    /// **The rendezvous, receiver-first.** A thread blocks on an empty endpoint, and stays
    /// blocked, and a *later* sender is what frees it — carrying the message.
    #[test_case]
    fn a_receiver_blocks_until_a_sender_arrives() {
        static GOT: AtomicU64 = AtomicU64::new(0);
        static RECEIVED: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();

        super::spawn(move || {
            let msg = super::ipc_recv(ep); // nobody is sending yet: this BLOCKS
            GOT.store(msg[0], Ordering::SeqCst);
            RECEIVED.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        // Let the receiver run and block. It must NOT have received anything: there is no sender.
        for _ in 0..50 {
            super::yield_now();
        }
        assert!(
            !RECEIVED.load(Ordering::SeqCst),
            "a receiver returned from an endpoint nobody had sent to",
        );

        // Now send. This should hand the receiver its message and wake it.
        super::ipc_send(ep, [0xABCD, 0, 0]);

        for _ in 0..50 {
            if RECEIVED.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(RECEIVED.load(Ordering::SeqCst), "the receiver never woke");
        assert_eq!(GOT.load(Ordering::SeqCst), 0xABCD, "wrong message delivered");
    }

    /// **The rendezvous, sender-first.** The other order: a sender blocks on an endpoint with no
    /// receiver, and a later receiver collects the parked message and wakes it.
    #[test_case]
    fn a_sender_blocks_until_a_receiver_arrives() {
        static SENT_RETURNED: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();

        super::spawn(move || {
            super::ipc_send(ep, [0x1234, 0x5678, 0x9abc]); // nobody receiving yet: BLOCKS
            SENT_RETURNED.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        for _ in 0..50 {
            super::yield_now();
        }
        assert!(
            !SENT_RETURNED.load(Ordering::SeqCst),
            "a send returned before anyone received it",
        );

        let msg = super::ipc_recv(ep); // collects the parked message, wakes the sender
        assert_eq!(msg, [0x1234, 0x5678, 0x9abc], "wrong message received");

        for _ in 0..50 {
            if SENT_RETURNED.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(
            SENT_RETURNED.load(Ordering::SeqCst),
            "the sender never woke after its message was taken",
        );
    }

    /// **A request and a reply, over two endpoints.** The shape milestone 8's console server
    /// will have: a client sends a request and blocks for the answer; a server loops on the
    /// request endpoint, does the work, and replies on the reply endpoint.
    ///
    /// All three message words survive the round trip, which is what proves the receiver's
    /// `x1`/`x2` handling and the mailbox are correct end to end.
    #[test_case]
    fn a_request_gets_a_reply() {
        static ANSWER: AtomicU64 = AtomicU64::new(0);
        static DONE: AtomicBool = AtomicBool::new(false);

        let req = super::create_endpoint();
        let rep = super::create_endpoint();

        // The server: receive n on `req`, send n + 1 back on `rep`.
        super::spawn(move || {
            let m = super::ipc_recv(req);
            super::ipc_send(rep, [m[0] + 1, m[1], m[2]]);
        })
        .expect("spawn failed");

        // The client.
        super::spawn(move || {
            super::ipc_send(req, [41, 0, 0]);
            let answer = super::ipc_recv(rep);
            ANSWER.store(answer[0], Ordering::SeqCst);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        for _ in 0..200 {
            if DONE.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(DONE.load(Ordering::SeqCst), "the request/reply never completed");
        assert_eq!(ANSWER.load(Ordering::SeqCst), 42, "the server computed the wrong answer");
    }

    /// A blocked thread is genuinely off the CPU: other threads keep running while it waits.
    ///
    /// If `Blocked` were not respected in `schedule()` — if a blocked thread were helpfully
    /// requeued — this would still pass, so it is not the whole story (the two rendezvous tests
    /// above are). But it is the cheap, direct statement of what blocking is *for*: a waiting
    /// thread must not burn the CPU.
    #[test_case]
    fn other_threads_run_while_one_is_blocked() {
        static PROGRESS: AtomicU64 = AtomicU64::new(0);
        static STOP: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();

        super::spawn(move || {
            super::ipc_recv(ep); // blocks forever (nobody sends); must not starve the worker
        })
        .expect("spawn failed");

        super::spawn(|| {
            while !STOP.load(Ordering::SeqCst) {
                PROGRESS.fetch_add(1, Ordering::SeqCst);
                super::yield_now();
            }
        })
        .expect("spawn failed");

        for _ in 0..100 {
            super::yield_now();
        }
        STOP.store(true, Ordering::SeqCst);

        assert!(
            PROGRESS.load(Ordering::SeqCst) > 0,
            "a worker made no progress while another thread was blocked on IPC",
        );

        // Free the blocked receiver so it does not sit in the endpoint queue forever.
        super::ipc_send(ep, [0, 0, 0]);
        for _ in 0..20 {
            super::yield_now();
        }
    }
}
