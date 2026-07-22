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

use crate::cpu;
use crate::sync::{IrqSafeMutex, rank};
use crate::thread::{Context, QuotaToken, State, Thread, Tid, switch_to};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// How many times we have actually taken the CPU away from a thread. The number that says
/// preemption is real.
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);
static VOLUNTARY_SWITCHES: AtomicU64 = AtomicU64::new(0);

/// The thread running on **this core** right now.
///
/// Per-CPU as of §11 step 3b (`cpu::PerCpu::current`); it used to be one field on the global
/// `Scheduler`. Reading it is a plain atomic load and needs no lock: it is this core's own slot.
fn current_tid() -> Tid {
    cpu::current().current.load(Ordering::Relaxed)
}

fn set_current_tid(tid: Tid) {
    cpu::current().current.store(tid, Ordering::Relaxed);
}

/// This core's idle thread: **what runs when nothing else can.**
///
/// Before it existed, a moment where every thread was blocked waiting for I/O was a kernel panic.
/// The idle thread parks the CPU in `wfi` until an interrupt makes something runnable. It is never
/// in the ready queue: the scheduler runs it only as a last resort, so it never competes with real
/// work. Per-CPU as of §11 step 3b, so an idle core parks in its own `wfi`.
fn idle_tid() -> Tid {
    cpu::current().idle.load(Ordering::Relaxed)
}

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
    /// **Async signals that arrived with nobody waiting.** An interrupt is not a rendezvous: it
    /// happens whether or not the driver is currently blocked in `RECV`, and it must not be
    /// lost. So a signal delivered to an empty endpoint is *counted* here, and the next `RECV`
    /// drains it instead of blocking. Zero for an ordinary synchronous endpoint, always.
    pending: u32,
}

struct Scheduler {
    /// `Box` because the assembly writes through `&mut thread.context`, so a thread's address
    /// must never move. A `BTreeMap` reshuffles its nodes; a `Box`'s contents do not.
    threads: BTreeMap<Tid, Box<Thread>>,
    /// Neither the run queue nor `current` live here any more: both moved to per-CPU storage
    /// (`cpu::PerCpu`, DECISIONS.md §11 steps 3a and 3b), because a single shared queue and a
    /// single "running thread" are exactly what every core would otherwise contend on and
    /// overwrite. What stays is genuinely whole-machine: the thread table and the endpoints.
    ///
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
        endpoints: alloc::vec::Vec::new(),
    });
    drop(sched); // release before spawning, which takes the lock itself

    // This core (core 0) is running the boot thread, tid 0.
    set_current_tid(0);

    // Capacity up front on THIS core's run queue, so `schedule()`'s push can never reallocate
    // (it runs from the timer IRQ, where DECISIONS.md §9 forbids allocation). Interrupts are
    // still masked this early in boot, which is what `with_runq` requires. Each secondary does
    // the same for its own queue when it comes online (step 3b).
    cpu::current().with_runq(|q| q.reserve(64));

    // The idle thread. Its entire body is "wait for an interrupt, then let the scheduler look for
    // work." It is deliberately kept OUT of the ready queue (see cpu::PerCpu::idle): the scheduler picks it
    // only when nothing else is runnable, so it never steals a turn from real work.
    let idle = Thread::spawn(Box::new(|| {
        loop {
            crate::arch::wait_for_interrupt();
            yield_now();
        }
    }))
    .expect("could not create the idle thread");
    let idle_id = idle.id;
    cpu::current().idle.store(idle_id, Ordering::Relaxed);

    let mut sched = SCHED.lock();
    let s = sched.as_mut().unwrap();
    s.threads.insert(idle_id, Box::new(idle));
    // NOT pushed onto `ready`: the idle thread is a fallback, not a peer.
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
    // Onto the spawning core's own queue. We hold SCHED, so interrupts are masked, which is what
    // `with_runq` needs. (Step 3c will let a spawn target another core via its inbox.)
    cpu::current().with_runq(|q| q.push_back(id));

    Some(id)
}

/// Spawn a thread against a **quota**: at most `budget` of these may be alive at once.
///
/// Reserving a slot is an atomic decrement; the slot lives inside the spawned `Thread` as a
/// [`QuotaToken`] and comes back when the thread is reaped. Returns `None` if the budget is
/// exhausted (too many children already alive) OR the kernel is out of memory — the caller cannot
/// tell the two apart, and does not need to: either way it could not spawn, and it must degrade
/// rather than panic. This is the bound that stops a spawn flood or a leaked-thread pile-up from
/// exhausting kernel memory. See notes/quotas.md and notes/security.md.
pub fn spawn_with_quota<F: FnOnce() + Send + 'static>(
    budget: &'static AtomicU32,
    f: F,
) -> Option<Tid> {
    // Reserve a slot: decrement only if there is one. A compare-exchange loop, so it is exactly
    // one atomic decrement and it never dips below zero (returning `None` = "quota exhausted").
    let mut remaining = budget.load(Ordering::Relaxed);
    loop {
        if remaining == 0 {
            return None;
        }
        match budget.compare_exchange_weak(
            remaining,
            remaining - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => remaining = actual,
        }
    }

    let mut thread = match Thread::spawn(Box::new(f)) {
        Some(t) => t,
        None => {
            // Out of kernel memory. Give the reserved slot back, since no thread will hold it.
            budget.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    thread.quota = Some(QuotaToken::new(budget)); // returned to `budget` when the thread is reaped
    let id = thread.id;

    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return None; // no scheduler: `thread` drops here and its QuotaToken returns the slot
    };
    sched.threads.insert(id, Box::new(thread));
    cpu::current().with_runq(|q| q.push_back(id)); // this core's queue; SCHED held, IRQs masked
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
        let current = current_tid();
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
    cpu::current().need_resched.store(true, Ordering::Relaxed);
}

pub fn take_need_resched() -> bool {
    cpu::current().need_resched.swap(false, Ordering::Relaxed)
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

    // A labeled block, so every exit path leaves through the SAME point: the guard drops at the
    // block's end and interrupts are restored ONCE, AFTER it. The earlier version called
    // `interrupts::restore(was_enabled)` and `return` from *inside* this block, which re-enabled
    // interrupts while still holding the scheduler lock — a one-instruction window in which a
    // timer could fire, re-enter `schedule()`, and try to take a lock we already held. It was
    // intermittent and it was real; see the lock-rank violation it produced.
    let switch = 'decide: {
        let mut guard = SCHED.lock();
        let Some(sched) = guard.as_mut() else {
            break 'decide None;
        };

        // Reap anything that finished. Safe *here* and nowhere else: whoever exited is no
        // longer running, because we are.
        reap(sched);

        let current = current_tid();
        let state = sched.threads.get(&current).map(|t| t.state);

        // **Only a still-Running thread goes back on the ready queue.** A thread that reached
        // here after marking itself `Blocked` (it is waiting for IPC) or `Finished` must not be
        // rescheduled, and this one line is what makes blocking work: `schedule()` can be
        // called from the timer IRQ *while* a thread is mid-way through blocking itself, and it
        // must not undo that by helpfully requeueing it.
        let runnable = state == Some(State::Running);

        let idle_tid = cpu::current().idle.load(Ordering::Relaxed);

        let next = match cpu::current().with_runq(|q| q.pop_front()) {
            Some(t) => t,
            None => {
                if runnable {
                    // Keep it. A thread yielding into an empty run queue simply carries on. (The
                    // idle thread lands here too: nothing to do, so it wfi's again.) No switch.
                    break 'decide None;
                }
                // Current is Blocked or Finished and the ready queue is empty. This is NOT a
                // deadlock: a thread blocked on a device interrupt is waiting for an event that
                // will arrive. Fall back to the idle thread, which wfi's until it does.
                if idle_tid == u64::MAX || current == idle_tid {
                    // No idle thread yet (before init finished), or the idle thread itself is
                    // somehow not runnable, which cannot happen. Either way there is genuinely
                    // nothing to run.
                    match state {
                        Some(State::Finished) => {
                            panic!("the last thread exited; nothing left to run")
                        }
                        _ => panic!("nothing runnable and no idle thread"),
                    }
                }
                idle_tid
            }
        };

        // Requeue the outgoing thread if it can still run — but never the idle thread, which
        // lives outside the ready queue.
        if runnable && current != idle_tid {
            sched.threads.get_mut(&current).unwrap().state = State::Ready;
            cpu::current().with_runq(|q| q.push_back(current)); // round robin: back of the queue
        }

        sched.threads.get_mut(&next).unwrap().state = State::Running;
        set_current_tid(next);

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
        .filter(|(id, t)| **id != 0 && **id != current_tid() && t.state == State::Finished)
        .map(|(id, _)| *id)
        .collect();

    for id in dead {
        sched.threads.remove(&id); // drops the Thread, drops the KernelStack, unmaps, frees
    }
}

/// intid -> endpoint id + 1 (0 means "not routed"). A hardware interrupt, delivered as a
/// message to whoever holds the matching endpoint.
///
/// **A plain atomic array, read lock-free from the interrupt handler.** The handler runs in a
/// context where taking a lock to *find out where to send the message* would be one more thing
/// that can go wrong; a bounded array of atomics cannot. 256 covers every INTID we will see
/// (SGIs 0-15, the timer PPI at 30, virtio SPIs in the 40s).
const MAX_INTID: usize = 256;
static IRQ_ROUTES: [core::sync::atomic::AtomicUsize; MAX_INTID] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; MAX_INTID];

/// Route a hardware interrupt to an endpoint. From now on, when `intid` fires, whoever is
/// blocked on `ep` wakes; if nobody is, the signal is remembered so it is not lost.
pub fn bind_irq(intid: u32, ep: usize) {
    assert!((intid as usize) < MAX_INTID, "intid {intid} out of range");
    IRQ_ROUTES[intid as usize].store(ep + 1, Ordering::Release);
}

/// The endpoint an interrupt is routed to, if any. Read from the IRQ handler; lock-free.
pub fn irq_route(intid: u32) -> Option<usize> {
    if (intid as usize) >= MAX_INTID {
        return None;
    }
    match IRQ_ROUTES[intid as usize].load(Ordering::Acquire) {
        0 => None,
        n => Some(n - 1),
    }
}

/// **Deliver an interrupt as a message.** Called from the IRQ handler.
///
/// If a thread is blocked waiting on the endpoint, wake it. If not, count the signal so the
/// next `RECV` returns immediately rather than blocking on an interrupt that already happened.
/// **An interrupt is not a rendezvous**: it must not wait for a receiver, and it must not be
/// lost if the receiver is briefly busy.
///
/// Safe to call from IRQ context: it takes the scheduler lock, which the interrupted code
/// cannot have been holding, because `IrqSafeMutex` masks interrupts for exactly as long as it
/// is held. See DECISIONS §9.
pub fn irq_notify(ep: usize) {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");

    if let Some(waiter) = sched.endpoints[ep].receivers.pop_front() {
        sched.threads.get_mut(&waiter).unwrap().mailbox = [1, 0, 0];
        wake(sched, waiter);
    } else {
        sched.endpoints[ep].pending = sched.endpoints[ep].pending.saturating_add(1);
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
            // Onto this core's queue. Every caller (ipc_*, irq_notify) holds SCHED, so interrupts
            // are masked. Step 3c makes this place the thread on the *right* core via its inbox.
            cpu::current().with_runq(|q| q.push_back(tid));
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
        let current = current_tid();

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
        let current = current_tid();

        if sched.endpoints[ep].pending > 0 {
            // An interrupt already fired while we were not waiting. Take it and do not block.
            sched.endpoints[ep].pending -= 1;
            Some([1, 0, 0])
        } else if let Some(sender) = sched.endpoints[ep].senders.pop_front() {
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
            sched.threads.get(&current_tid()).unwrap().mailbox
        }
    }
}

/// The x1 value a `RECV_CAP` returns when no capability accompanied the message. Mirrors
/// `abi::endpoint::NO_CAP`; kept here too so the scheduler names it without reaching into the ABI.
const NO_CAP: u64 = u64::MAX;

/// **Delegate a capability plus one data word to an endpoint.** The sender's half of a
/// capability-carrying rendezvous, mirroring [`ipc_send`]. The one thing it adds: at the moment
/// sender and receiver meet, `cap` moves out of the sender and into the receiver's cspace.
///
/// - **A receiver is already waiting.** Insert the capability into its cspace right now, record the
///   slot in its mailbox alongside the data word, and wake it.
/// - **Nobody is waiting.** Park the data word in our mailbox and the capability in `outgoing_cap`,
///   join the sender queue, and block. A future receiver reaches in, takes the capability, and
///   files it in its own cspace.
///
/// If the receiver's cspace is full the capability is dropped and the receiver sees `NO_CAP`; the
/// data word still arrives. The syscall layer has already checked the sender may delegate this
/// capability (it holds `GRANT`) and that the rights only narrow.
pub fn ipc_send_cap(ep: usize, data: u64, cap: crate::cap::Cap) {
    let block = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        if let Some(receiver) = sched.endpoints[ep].receivers.pop_front() {
            let r = sched.threads.get_mut(&receiver).unwrap();
            let slot = r.cspace.insert(cap).unwrap_or(NO_CAP);
            r.mailbox = [data, slot, 0];
            wake(sched, receiver);
            false
        } else {
            let me = sched.threads.get_mut(&current).unwrap();
            me.mailbox = [data, 0, 0];
            me.outgoing_cap = Some(cap);
            me.state = State::Blocked;
            sched.endpoints[ep].senders.push_back(current);
            true
        }
    };

    if block {
        schedule();
    }
}

/// **Receive a data word and, if one was sent, a capability.** The mirror of [`ipc_send_cap`], and
/// the receiver's half of delegation. Returns `[data, received_slot, 0]`, where `received_slot` is
/// where an incoming capability landed in *our* cspace, or [`NO_CAP`] if the message carried none.
///
/// A capability-carrying send and this share the ordinary sender/receiver queues, so either side
/// may arrive first, exactly as with the plain path.
pub fn ipc_recv_cap(ep: usize) -> [u64; 3] {
    let immediate = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        if sched.endpoints[ep].pending > 0 {
            // An interrupt signal is not a delegation; it carries no capability.
            sched.endpoints[ep].pending -= 1;
            Some([1, NO_CAP, 0])
        } else if let Some(sender) = sched.endpoints[ep].senders.pop_front() {
            let data = sched.threads.get(&sender).unwrap().mailbox[0];
            let cap = sched.threads.get_mut(&sender).unwrap().outgoing_cap.take();
            let slot = match cap {
                Some(c) => sched
                    .threads
                    .get_mut(&current)
                    .unwrap()
                    .cspace
                    .insert(c)
                    .unwrap_or(NO_CAP),
                None => NO_CAP,
            };
            wake(sched, sender);
            Some([data, slot, 0])
        } else {
            sched.threads.get_mut(&current).unwrap().state = State::Blocked;
            sched.endpoints[ep].receivers.push_back(current);
            None
        }
    };

    match immediate {
        Some(msg) => msg,
        None => {
            schedule(); // a capability-carrying sender fills our mailbox and wakes us
            let guard = SCHED.lock();
            let sched = guard.as_ref().expect("no scheduler");
            sched.threads.get(&current_tid()).unwrap().mailbox
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
        .get(&current_tid())
        .ok_or(crate::cap::Error::NoSuchSlot)?
        .cspace
        .get(slot)
}

/// Hand the current thread a capability. **The only way authority ever enters a process.**
pub fn grant(cap: crate::cap::Cap) -> Result<u64, crate::cap::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(crate::cap::Error::NoFreeSlot)?;
    let current = current_tid();
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
        let current = current_tid();
        sched
            .threads
            .get_mut(&current)
            .expect("no current thread")
            .space = Some(space);
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
        .get(&current_tid())?
        .stack
        .as_ref()
        .map(|s| s.top())
}

pub fn current() -> Tid {
    current_tid()
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

    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

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
        assert_eq!(
            GOT.load(Ordering::SeqCst),
            0xABCD,
            "wrong message delivered"
        );
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
        assert!(
            DONE.load(Ordering::SeqCst),
            "the request/reply never completed"
        );
        assert_eq!(
            ANSWER.load(Ordering::SeqCst),
            42,
            "the server computed the wrong answer"
        );
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

    /// **An interrupt becomes a message.** DECISIONS §10 and notes/interrupts.md, executed.
    ///
    /// A thread blocks waiting on an interrupt it can only name through an endpoint. We raise the
    /// interrupt from software (an SGI, so the test needs no device), the kernel's handler turns
    /// it into a notification, and the blocked thread wakes. This is the exact path a userspace
    /// driver will take when a real device interrupts, minus the device.
    #[test_case]
    fn an_interrupt_becomes_a_message() {
        // An SGI: a software-triggerable interrupt with no hardware behind it.
        const SGI: u32 = 1;

        static WOKE: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();
        super::bind_irq(SGI, ep);
        crate::drivers::gic::enable(SGI);

        super::spawn(move || {
            super::ipc_recv(ep); // blocks until the interrupt fires
            WOKE.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        // Let the waiter run and block. It must NOT have woken: no interrupt yet.
        for _ in 0..50 {
            super::yield_now();
        }
        assert!(
            !WOKE.load(Ordering::SeqCst),
            "the thread woke before the interrupt fired",
        );

        // Fire it. The GIC delivers the SGI, handle_irq routes it to `ep`, the waiter wakes.
        crate::drivers::gic::send_sgi(SGI);

        for _ in 0..100 {
            if WOKE.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(
            WOKE.load(Ordering::SeqCst),
            "a hardware interrupt fired and the thread waiting on it never woke",
        );
    }

    /// **A spawn quota caps how many children a spawner can have alive, and replenishes on death.**
    ///
    /// This is the resource-exhaustion bound from the security audit: a process cannot make the
    /// kernel spawn without limit. Two threads block on an endpoint nobody drains, holding their
    /// slots; a budget of two is then exhausted and a third spawn is refused. Waking one lets it
    /// exit and be reaped, which returns its slot, and a spawn succeeds again.
    #[test_case]
    fn a_spawn_quota_caps_live_children_and_replenishes_on_reap() {
        use core::sync::atomic::AtomicU32;
        static BUDGET: AtomicU32 = AtomicU32::new(2);

        let ep = super::create_endpoint();

        // Two children that block forever (nobody sends), each holding a quota slot.
        assert!(
            super::spawn_with_quota(&BUDGET, move || {
                super::ipc_recv(ep);
            })
            .is_some(),
            "first child should fit in the budget",
        );
        assert!(
            super::spawn_with_quota(&BUDGET, move || {
                super::ipc_recv(ep);
            })
            .is_some(),
            "second child should fit in the budget",
        );

        // Let them run and block, so both slots are genuinely held.
        for _ in 0..50 {
            super::yield_now();
        }

        // The budget is spent: a third spawn is refused, not panicked, not over-committed.
        assert!(
            super::spawn_with_quota(&BUDGET, || {}).is_none(),
            "the budget was exhausted but a third child spawned anyway",
        );

        // Wake one child. It returns from ipc_recv, its closure ends, it exits and is reaped,
        // and its QuotaToken drops, returning the slot.
        super::ipc_send(ep, [0, 0, 0]);
        for _ in 0..100 {
            super::yield_now();
        }

        // A slot is free again.
        assert!(
            super::spawn_with_quota(&BUDGET, || {}).is_some(),
            "a child exited but its quota slot was never returned",
        );

        // Clean up: wake the other blocked child so it does not sit forever.
        super::ipc_send(ep, [0, 0, 0]);
        for _ in 0..50 {
            super::yield_now();
        }
    }

    /// A signal that arrives while nobody is waiting is **remembered, not lost.** An interrupt is
    /// not a rendezvous: if it fires a hair before the driver calls `WAIT`, the driver must still
    /// see it. The `pending` count is what closes that window.
    #[test_case]
    fn an_interrupt_that_arrives_before_the_wait_is_not_lost() {
        const SGI: u32 = 2;

        let ep = super::create_endpoint();
        super::bind_irq(SGI, ep);
        crate::drivers::gic::enable(SGI);

        // Fire it with NOBODY waiting. The signal must be counted.
        crate::drivers::gic::send_sgi(SGI);
        // Give the interrupt time to be delivered and handled.
        for _ in 0..20 {
            super::yield_now();
        }

        static SAW: AtomicBool = AtomicBool::new(false);
        super::spawn(move || {
            super::ipc_recv(ep); // must return immediately: the signal is pending
            SAW.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        for _ in 0..50 {
            if SAW.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(
            SAW.load(Ordering::SeqCst),
            "an interrupt that fired before the WAIT was lost",
        );
    }
}
