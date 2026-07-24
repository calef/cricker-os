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

/// A synchronous IPC rendezvous point: the two wait queues and the pending-signal count.
///
/// **The state machine is the `ipc` crate**, which owns the queues and the decision logic (send,
/// recv, signal) and carries machine-checked proofs of its one invariant, "at most one wait queue is
/// ever non-empty" (DECISIONS §14, milestone 18; notes/verification.md). The six IPC functions below
/// decide *what* to do by calling the proved logic and spend their own code only on the bookkeeping
/// the queues cannot express (mailboxes, waking a thread onto a run queue, the one-shot Reply that
/// leaves a caller blocked).
///
/// Intrusive as of milestone 14 phase A.3: a wait-queue entry is the TCB itself, threaded through
/// the same link the run queues use, so blocking on an endpoint cannot allocate and "a thread waits
/// on one endpoint at a time" is physical (one link). The safety contract for the pointers is the
/// queue discipline at [`tcb_ptr`].
type Endpoint = ipc::Endpoint<Thread>;

/// The most threads that can be alive at once, whole machine (milestone 14 phase A). A documented
/// limit of the image rather than a heap that can be exhausted: spawn past it fails cleanly, the
/// same contract callers already have for out-of-memory. The table itself is ~2 KiB of pointers.
const MAX_THREADS: usize = 128;

/// **The TCB pool** (milestone 14 phase B.2; notes/tcb.md): every `Thread` lives here, in BSS,
/// from boot. Pool slot `i` is table slot `i`, so a Tid's low bits name the storage directly.
/// A slot's address never changes, which supplies the pinning the per-thread `Box` used to buy:
/// the context-switch assembly and the intrusive queues hold pointers into this array.
///
/// `MaybeUninit` because slots are dead between threads; the `Threads` table below is the single
/// source of truth for which slots are alive, and every access goes through it.
struct TcbPool {
    slots: core::cell::UnsafeCell<[core::mem::MaybeUninit<Thread>; MAX_THREADS]>,
}

// SAFETY: reached only through `Threads`, whose instance lives inside the SCHED mutex, so table
// access is serialized by that lock; the queues' raw pointers into the pool follow the intrusive
// discipline documented at tcb_ptr.
unsafe impl Sync for TcbPool {}

static TCB_POOL: TcbPool = TcbPool {
    slots: core::cell::UnsafeCell::new([const { core::mem::MaybeUninit::uninit() }; MAX_THREADS]),
};

/// The thread table: generational names (`crates/slots`, notes/generational-names.md) over the
/// static [`TcbPool`]. The `slots::Table` tracks which slots live and mints the Tids; the pool
/// holds the actual TCBs. `Box<Thread>` died here (milestone 14 phase B.2): spawn now writes the
/// new `Thread` into its pool slot in place, and the reaper drops it in place.
struct Threads {
    names: slots::Table<(), MAX_THREADS>,
}

impl Threads {
    const fn new() -> Self {
        Self {
            names: slots::Table::new(),
        }
    }

    fn get(&self, tid: Tid) -> Option<&Thread> {
        let i = self.names.slot_of(tid)?;
        // SAFETY: the table says slot i is live, so it was initialized at insert and will not be
        // dropped before `remove` kills the name; access is serialized by SCHED (see TcbPool).
        Some(unsafe { (*TCB_POOL.slots.get())[i].assume_init_ref() })
    }

    fn get_mut(&mut self, tid: Tid) -> Option<&mut Thread> {
        let i = self.names.slot_of(tid)?;
        // SAFETY: as `get`, and `&mut self` carries SCHED's exclusivity.
        Some(unsafe { (*TCB_POOL.slots.get())[i].assume_init_mut() })
    }

    /// Insert: the table claims a slot and mints the name, `f` builds the `Thread` (carrying its
    /// own name), and the value is written into the pool slot in place.
    fn insert_with(&mut self, f: impl FnOnce(Tid) -> Thread) -> Option<Tid> {
        let tid = self.names.insert_with(|_| ())?;
        let i = self.names.slot_of(tid).expect("just inserted");
        // SAFETY: the slot was free until the line above claimed it, so nothing lives here;
        // `write` moves the new Thread in without reading the dead bytes.
        unsafe { (*TCB_POOL.slots.get())[i].write(f(tid)) };
        Some(tid)
    }

    /// Remove and destroy: drop the TCB in place (its stack, address space, and quota token go
    /// with it), then kill the name so no copy of the Tid ever resolves again.
    fn remove(&mut self, tid: Tid) {
        let Some(i) = self.names.slot_of(tid) else {
            return;
        };
        // SAFETY: live per the table, exclusive per `&mut self`; the name dies on the next line,
        // so nothing can reach the dropped bytes afterward.
        unsafe { (*TCB_POOL.slots.get())[i].assume_init_drop() };
        self.names.remove(tid);
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    /// Every live TCB, for whole-table sweeps (revocation). Disjoint `&mut`s: `live_slots`
    /// yields each live index exactly once.
    fn iter_mut(&mut self) -> impl Iterator<Item = &mut Thread> + '_ {
        let pool = TCB_POOL.slots.get();
        // SAFETY: each yielded index is live (initialized, not yet dropped) and distinct;
        // `&mut self` carries SCHED's exclusivity across the whole iteration.
        self.names
            .live_slots()
            .map(move |i| unsafe { (*pool)[i].assume_init_mut() })
    }
}

struct Scheduler {
    /// The thread table: generational names over the static TCB pool. See [`Threads`] and
    /// [`TcbPool`] above; design/kernel-objects-from-untyped.md D2 records the path.
    threads: Threads,
    /// Neither the run queue nor `current` live here any more: both moved to per-CPU storage
    /// (`cpu::PerCpu`, DECISIONS.md §11 steps 3a and 3b), because a single shared queue and a
    /// single "running thread" are exactly what every core would otherwise contend on and
    /// overwrite. What stays is genuinely whole-machine: the thread table and the endpoints.
    ///
    /// Every IPC endpoint. Indexed by the `usize` inside an `Object::Endpoint` capability, which
    /// only the kernel mints, so the index is always in range.
    /// Fixed capacity (milestone 14 phase B.1): an `Endpoint` shrank to two queue heads and a
    /// counter at A.3, so the whole table is a few KiB and creating an endpoint touches no heap.
    /// `MAX_ENDPOINTS` is a documented limit of the image, like `MAX_THREADS`.
    endpoints: [Endpoint; MAX_ENDPOINTS],
    /// How many endpoints exist. They are created and never destroyed (an endpoint id lives
    /// inside capabilities), so this only grows; `create_endpoint` refuses past the cap.
    endpoint_count: usize,
}

/// The most endpoints that can ever exist. Endpoint teardown does not exist yet (ids live in
/// capabilities), so this bounds creations over the kernel's lifetime, not concurrent use.
const MAX_ENDPOINTS: usize = 256;

/// Rank **above the allocators**, because the reaper (`finish_switch`) drops a dead `Thread` in
/// its pool slot while holding this, and that drop *frees*: the kernel stack's pages go back to
/// the frame allocator through the kernel MMU lock, and the stack's VA range to its free list.
/// Freeing takes the same locks allocating does, so the rank must sit above them.
///
/// Nothing under this lock **allocates** any more (milestone 14 phase B.2): spawn writes the new
/// `Thread` into a static pool slot, and the queues have been intrusive since A.2, so a queue
/// operation is a couple of pointer writes, from the timer IRQ or anywhere else. §9's
/// no-allocation-in-IRQ rule holds by construction.
static SCHED: IrqSafeMutex<Option<Scheduler>> = IrqSafeMutex::new(rank::SCHED, None);

/// Adopt the context we are already running in as thread 0.
///
/// It has no stack of its own and no saved context. **The first switch *away* from it fills
/// that in**, which is why the boot thread needs no special case: a thread's context is written
/// by the act of leaving it.
pub fn init() {
    let mut sched = SCHED.lock();

    let mut threads = Threads::new();
    // The table names the boot thread at insert. The first name a fresh table mints is 0 by
    // construction (slot 0, generation 0), so "the boot thread is tid 0" survives, now as a
    // property of the table rather than a hardcoded key.
    let boot_tid = threads
        .insert_with(|tid| {
            let mut boot = Thread::boot();
            boot.id = tid;
            boot
        })
        .expect("a fresh table refused its first insert");

    *sched = Some(Scheduler {
        threads,
        endpoints: [const { Endpoint::new() }; MAX_ENDPOINTS],
        endpoint_count: 0,
    });
    drop(sched); // release before spawning, which takes the lock itself

    // This core (core 0) is running the boot thread.
    set_current_tid(boot_tid);

    // (The run queue and inbox used to have capacity reserved here, so a push from the timer IRQ
    // could never allocate. The queues are intrusive now — a push is two pointer writes and
    // *cannot* allocate — so there is nothing to reserve. §9's rule became structural.)

    // The idle thread. Its entire body is "wait for an interrupt, then let the scheduler look for
    // work." It is deliberately kept OUT of the ready queue (see cpu::PerCpu::idle): the scheduler picks it
    // only when nothing else is runnable, so it never steals a turn from real work.
    let idle = Thread::spawn(|| {
        loop {
            crate::arch::wait_for_interrupt();
            yield_now();
        }
    })
    .expect("could not create the idle thread");

    let mut sched = SCHED.lock();
    let s = sched.as_mut().unwrap();
    let idle_id = s
        .threads
        .insert_with(|tid| {
            let mut idle = idle;
            idle.id = tid;
            idle
        })
        .expect("thread table full at boot");
    drop(sched);
    // NOT pushed onto `ready`: the idle thread is a fallback, not a peer.
    cpu::current().idle.store(idle_id, Ordering::Relaxed);
}

/// Make **this (secondary) core** a scheduler participant.
///
/// The boot core is set up by [`init`]; a secondary calls this once, as it comes online. It adopts
/// the context it is already running on as this core's idle thread (`cpu::current`/`cpu::idle`), and
/// reserves this core's run queue so `schedule()`'s push never allocates from the timer IRQ (§9),
/// exactly as `init` does for the boot core. After this, the core's run queue is empty, so it runs
/// its idle thread until work lands on the queue.
///
/// Interrupts must be masked (the caller has not enabled them yet), which is what `with_runq` needs.
pub fn adopt_secondary_idle() {
    let idle = Thread::adopt_current();

    let id = {
        let mut guard = SCHED.lock();
        let sched = guard
            .as_mut()
            .expect("adopt_secondary_idle before sched::init");
        sched
            .threads
            .insert_with(|tid| {
                let mut idle = idle;
                idle.id = tid;
                idle
            })
            .expect("thread table full while bringing a core online")
    };

    // This core is currently running that thread, and it is also this core's idle fallback.
    cpu::current().current.store(id, Ordering::Relaxed);
    cpu::current().idle.store(id, Ordering::Relaxed);
    // (No queue capacity to reserve: the queues are intrusive and a push cannot allocate.)
}

/// The reschedule / migration SGI. When one core hands another a thread (via its inbox), it fires
/// this at the target; the target's handler drains its inbox and reschedules. INTID 0, distinct
/// from the endpoint-bound test SGIs (1 and 2). SMP step 3c.
pub const RESCHED_SGI: u32 = 0;

/// Drain this core's migration inbox into its run queue, and request a reschedule.
///
/// Called from the reschedule-SGI handler: another core pushed one or more threads into our inbox
/// and poked us. We move them onto our own (single-owner) run queue and set `need_resched`, so the
/// handler's tail runs `schedule()` and picks them up. IRQ context, so interrupts are masked, which
/// is what `with_runq` needs; we hold nothing else, so taking the inbox is rank-safe (§11).
pub fn drain_inbox() {
    let mut moved = false;
    let mut inbox = cpu::current().inbox.lock();
    while let Some(thread) = inbox.pop_front() {
        // SAFETY: the sender pushed a live Ready thread; popping it here is the only removal
        // path, so it is on no other queue. Nothing is dereferenced: the handoff is pure
        // pointer movement, which is why this needs no scheduler lock.
        cpu::current().with_runq(|q| unsafe { q.push_back(thread) });
        moved = true;
    }
    drop(inbox);
    if moved {
        cpu::current().need_resched.store(true, Ordering::Relaxed);
    }
}

/// The raw TCB pointer of a live thread, for queueing (milestone 14 phase A.2). Caller holds
/// `SCHED`.
///
/// The pointer's validity while queued is the queue discipline, stated once here: a thread on a
/// run queue or inbox is `Ready`, a thread on an endpoint wait queue is `Blocked` (A.3), the
/// reaper frees only `Finished` threads, and a thread is never two of those at once. The `Box` in
/// the table pins the address (see `Scheduler::threads`), so a pointer taken here is good until
/// the thread is popped, however many queue hops (inbox to run queue) it makes in between.
fn tcb_ptr(sched: &mut Scheduler, tid: Tid) -> *mut Thread {
    sched.threads.get_mut(tid).expect("tcb_ptr of a dead thread")
}

/// Put an already-created thread onto core `target`'s run queue. Caller holds `SCHED`.
///
/// Local: straight onto our own queue (SCHED masks interrupts, which `with_runq` needs). Remote:
/// into the target's inbox, and the SGI (sent after SCHED is released, by the caller) makes it
/// drain. The inbox push under SCHED is rank-safe (INBOX < SCHED), and the inbox's own lock supplies
/// the release/acquire that orders our thread-table insert before the target's drain (§11).
fn place_on(target: usize, thread: *mut Thread) {
    if target == cpu::id() {
        // SAFETY: `thread` is a live Ready thread (see tcb_ptr), on no other queue.
        cpu::current().with_runq(|q| unsafe { q.push_back(thread) });
    } else {
        // SAFETY: as above; the inbox mutex serializes access to the link.
        unsafe { cpu::inbox_of(target).lock().push_back(thread) };
    }
}

/// Spawn a thread and place it on a **specific** core (SMP step 3c).
///
/// The cross-core placement primitive. `spawn` puts work on the calling core; this puts it on
/// `target`, which is what lets the machine actually spread load. A remote target is handed the
/// thread through its inbox and then poked with the reschedule SGI. (Wiring `spawn` itself to
/// round-robin over `target` is the trivial next step, once the mechanism is proven.)
pub fn spawn_on<F: FnOnce() + Send + 'static>(target: usize, f: F) -> Option<Tid> {
    let thread = Thread::spawn(f)?;
    let remote = target != cpu::id();

    let id = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut()?;
        let id = sched.threads.insert_with(|tid| {
            let mut thread = thread;
            thread.id = tid;
            thread
        })?;
        place_on(target, tcb_ptr(sched, id));
        id
    }; // SCHED released here, before the SGI, so the target's schedule() can take it

    if remote {
        // Poke the target: its handler drains the inbox we just pushed to and reschedules.
        crate::drivers::gic::send_sgi(RESCHED_SGI, target);
    }
    Some(id)
}

pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> Option<Tid> {
    // Build the thread — which allocates a stack, maps four pages, and boxes the closure —
    // OUTSIDE the lock. Critical sections stay short (DECISIONS.md §9), and this one would
    // otherwise hold the scheduler across four page-table walks.
    let thread = Thread::spawn(f)?;

    let mut guard = SCHED.lock();
    let sched = guard.as_mut()?;
    let id = sched.threads.insert_with(|tid| {
        let mut thread = thread;
        thread.id = tid;
        thread
    })?;
    // Onto the spawning core's own queue. We hold SCHED, so interrupts are masked, which is what
    // `with_runq` needs. (Step 3c will let a spawn target another core via its inbox.)
    let ptr = tcb_ptr(sched, id);
    // SAFETY: freshly inserted, Ready, on no queue; see tcb_ptr for why it stays valid.
    cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });

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

    let mut thread = match Thread::spawn(f) {
        Some(t) => t,
        None => {
            // Out of kernel memory. Give the reserved slot back, since no thread will hold it.
            budget.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    thread.quota = Some(QuotaToken::new(budget)); // returned to `budget` when the thread is reaped

    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return None; // no scheduler: `thread` drops here and its QuotaToken returns the slot
    };
    // A full table is the same outcome as out-of-memory: `insert_with` never calls the closure,
    // `thread` drops uncalled, and its QuotaToken hands the reserved slot back.
    let id = sched.threads.insert_with(|tid| {
        thread.id = tid;
        thread
    })?;
    let ptr = tcb_ptr(sched, id);
    // SAFETY: freshly inserted, Ready, on no queue; this core's queue, SCHED held, IRQs masked.
    cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
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
        if let Some(t) = sched.threads.get_mut(current) {
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

        let current = current_tid();
        let state = sched.threads.get(current).map(|t| t.state);

        // **Only a still-Running thread goes back on the ready queue.** A thread that reached
        // here after marking itself `Blocked` (it is waiting for IPC) or `Finished` must not be
        // rescheduled, and this one line is what makes blocking work: `schedule()` can be
        // called from the timer IRQ *while* a thread is mid-way through blocking itself, and it
        // must not undo that by helpfully requeueing it.
        let runnable = state == Some(State::Running);

        let idle_tid = cpu::current().idle.load(Ordering::Relaxed);

        let next = match cpu::current().with_runq(|q| q.pop_front()) {
            // SAFETY: only live Ready threads are ever queued; reading the id is the last thing
            // that happens before the pointer is dropped in favor of the (validated) Tid.
            Some(t) => unsafe { (*t).id },
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
            sched.threads.get_mut(current).unwrap().state = State::Ready;
            let ptr = tcb_ptr(sched, current);
            // SAFETY: just marked Ready, coming off the CPU, on no queue. Round robin: the back.
            cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
        }

        {
            let t = sched.threads.get_mut(next).unwrap();
            t.state = State::Running;
            t.on_cpu = true; // cleared by ITS successor's finish_switch, one switch from now
        }
        set_current_tid(next);

        // Hand the outgoing thread to the incoming one to finish up AFTER the switch, when it is
        // provably off its stack: reap it if it Finished, clear its on_cpu (and complete a
        // deferred wake) otherwise. Not here, and not by another core: we are still running on
        // its stack this instant. `current` is the local (the outgoing tid); `set_current_tid`
        // above already moved the per-CPU current to `next`. See finish_switch.
        cpu::current()
            .switched_from
            .store(current, Ordering::Relaxed);

        // The incoming thread's low half. A kernel thread gets the empty reserved table, which
        // makes every low address fault, which is exactly right: it has no business down there.
        let next_root = sched.threads.get(next).unwrap()
            .space
            .as_ref()
            .map(|s| s.root())
            .unwrap_or_else(crate::arch::mmu::reserved_root);

        // Copy the two raw pointers out before the lock drops. The assembly writes through the
        // first and reads the second, and both threads' `Box`es keep their contents pinned.
        let prev_slot: *mut *mut Context = &mut sched.threads.get_mut(current).unwrap().context;
        let next_ctx: *mut Context = sched.threads.get(next).unwrap().context;

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

        // We are now the incoming thread, resuming. Reap whoever we switched away from, if it had
        // finished: it is off its stack now, and we are on the same core that set `to_reap`.
        finish_switch();
    }

    crate::arch::interrupts::restore(was_enabled);
}

/// Reap the thread this core just switched away from, if it had finished.
///
/// The safe half of the two-part reaper. `schedule()` records a finished outgoing thread in this
/// core's `to_reap` *before* the switch; this runs on the incoming thread *after* the switch, when
/// the outgoing thread is provably off its stack (its registers are saved and we are on a
/// different stack). Dropping the `Thread` unmaps its stack and frees its address space, which is
/// exactly why it must not happen while any core still stands on it.
///
/// Called from two places, because a thread can resume two ways: from `schedule()` (an existing
/// thread returning from `switch_to`) and from `thread_entry` (a brand-new thread, which never
/// passes through `schedule()`'s post-switch point). Both run on this core, so both see this core's
/// `to_reap`. See DECISIONS.md §11 and thread.rs.
pub(crate) fn finish_switch() {
    let prev = cpu::current().switched_from.swap(cpu::NO_TID, Ordering::Relaxed);
    if prev == cpu::NO_TID {
        return;
    }
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return;
    };
    let Some(t) = sched.threads.get_mut(prev) else {
        return;
    };
    if t.state == State::Finished {
        // Hoist the address space out BEFORE the in-place drop, to be torn down after the lock
        // is released: its teardown is untyped::destroy (milestone 14 phase B.4), whose §13
        // revocation sweep takes SCHED itself to delete stray Frame capabilities. Dropping it
        // here would deadlock on our own lock. The rest of the Thread (stack, quota) still
        // drops under SCHED, exactly as before.
        let space = t.space.take();
        sched.threads.remove(prev);
        drop(guard);
        drop(space);
        return;
    }
    // The predecessor's context is saved now (we are running, so switch_to completed), so it is
    // finally safe for other cores to run it.
    t.on_cpu = false;
    if t.wake_pending {
        // A wake raced its switch-out (see wake): complete it here, where the context is real.
        t.wake_pending = false;
        t.state = State::Ready;
        let ptr = tcb_ptr(sched, prev);
        // SAFETY: live, just made Ready, on no queue (a deferred wake was deferred precisely
        // because the waker did NOT queue it). IRQs are still masked on both callers' paths.
        cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
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

    // `signal` wakes a waiting receiver or counts the signal; it never blocks or joins a queue.
    if let Some(waiter) = sched.endpoints[ep].signal() {
        // SAFETY: only live Blocked threads sit on wait queues; reading the id revalidates it
        // through the table for everything after.
        let waiter = unsafe { (*waiter).id };
        sched.threads.get_mut(waiter).unwrap().mailbox = [1, 0, 0];
        wake(sched, waiter);
    }
}

/// Create an IPC endpoint. Returns its id, which is what goes inside an `Object::Endpoint`.
///
/// Panics when the fixed table is exhausted: every caller is the kernel or a test wiring a
/// service, so exhaustion is a misconfigured image, not a runtime condition to recover from.
pub fn create_endpoint() -> usize {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");
    assert!(
        sched.endpoint_count < MAX_ENDPOINTS,
        "out of endpoints: MAX_ENDPOINTS ({MAX_ENDPOINTS}) is a limit of the image"
    );
    sched.endpoint_count += 1;
    sched.endpoint_count - 1
}

/// Move a blocked thread back to the ready queue. Caller holds the lock.
fn wake(sched: &mut Scheduler, tid: Tid) {
    if let Some(t) = sched.threads.get_mut(tid)
        && t.state == State::Blocked
    {
        // **The wake-before-switch-out race** (found by a 2-in-10 test flake; the Blocked twin
        // of the §11 reaper race). A thread marks itself Blocked and releases SCHED, but is
        // still running on its core until schedule() switches away; its saved context is stale
        // until then. A rendezvous or interrupt can wake it in that window. Queueing it here
        // would let another core switch INTO the stale context while its core still runs the
        // present one: two cores in one thread. So: if it is still on a CPU, park the wake;
        // its own core's finish_switch completes it once the context is provably saved.
        if t.on_cpu {
            t.wake_pending = true;
            return;
        }
        t.state = State::Ready;
        let ptr: *mut Thread = t;
        // Onto this core's queue. Every caller (ipc_*, irq_notify) holds SCHED, so interrupts
        // are masked. Step 3c makes this place the thread on the *right* core via its inbox.
        // SAFETY: just transitioned Blocked -> Ready, so it was on no queue and now joins one.
        cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
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

        let me = tcb_ptr(sched, current);
        // SAFETY: `me` is the running thread (live, on no queue), and if queued it stays live:
        // a thread queued on an endpoint is Blocked, which the reaper never touches. See tcb_ptr.
        match unsafe { sched.endpoints[ep].send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver).id };
                sched.threads.get_mut(receiver).unwrap().mailbox = msg;
                wake(sched, receiver);
                false
            }
            ipc::Send::Blocked => {
                // `send` has already queued `current` as a sender; we record why it is parked.
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = msg;
                me.state = State::Blocked;
                true
            }
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

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send: the running thread, and Blocked-while-queued keeps it live.
        match unsafe { sched.endpoints[ep].recv(me) } {
            // An interrupt already fired while we were not waiting. Take it and do not block.
            ipc::Recv::Signal => Some([1, 0, 0]),
            ipc::Recv::FromSender(sender) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let sender = unsafe { (*sender).id };
                let msg = sched.threads.get(sender).unwrap().mailbox;
                // A caller (its outgoing cap is the one-shot Reply the kernel minted for a CALL, §12)
                // is awaiting a *reply*, which a plain RECV cannot furnish: only RECV_CAP delivers the
                // reply capability. Deliver the words but leave the caller blocked rather than wake it
                // with its own request masquerading as a reply. Serve CALL endpoints with RECV_CAP; a
                // plain RECV here leaves the caller hung, the same no-timeout limitation as a reply
                // that never comes.
                let is_caller = matches!(
                    sched.threads.get(sender).unwrap().outgoing_cap,
                    Some(c) if matches!(c.object, crate::cap::Object::Reply(_))
                );
                if !is_caller {
                    wake(sched, sender);
                }
                Some(msg)
            }
            ipc::Recv::Blocked => {
                // `recv` has already queued `current` as a receiver.
                sched.threads.get_mut(current).unwrap().state = State::Blocked;
                None
            }
        }
    };

    match immediate {
        Some(msg) => msg,
        None => {
            schedule(); // blocks; a sender fills our mailbox and wakes us
            let guard = SCHED.lock();
            let sched = guard.as_ref().expect("no scheduler");
            sched.threads.get(current_tid()).unwrap().mailbox
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

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send.
        match unsafe { sched.endpoints[ep].send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver).id };
                let r = sched.threads.get_mut(receiver).unwrap();
                let slot = r.cspace.insert(cap).unwrap_or(NO_CAP);
                r.mailbox = [data, slot, 0];
                wake(sched, receiver);
                false
            }
            ipc::Send::Blocked => {
                // `send` queued `current`; we park the data word and the capability to hand over.
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = [data, 0, 0];
                me.outgoing_cap = Some(cap);
                me.state = State::Blocked;
                true
            }
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

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send.
        match unsafe { sched.endpoints[ep].recv(me) } {
            // An interrupt signal is not a delegation; it carries no capability.
            ipc::Recv::Signal => Some([1, NO_CAP, 0]),
            ipc::Recv::FromSender(sender) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let sender = unsafe { (*sender).id };
                let msg = sched.threads.get(sender).unwrap().mailbox;
                let cap = sched.threads.get_mut(sender).unwrap().outgoing_cap.take();
                // A caller's outgoing cap is the one-shot Reply the kernel minted for its CALL (§12); a
                // SEND_CAP sender's is the capability it chose to delegate. The difference is liveness:
                // a caller stays blocked awaiting its reply, so it must NOT be woken here; a SEND_CAP
                // sender's rendezvous is complete the moment we take the cap.
                let is_reply =
                    matches!(cap, Some(c) if matches!(c.object, crate::cap::Object::Reply(_)));
                let slot = match cap {
                    Some(c) => sched
                        .threads
                        .get_mut(current)
                        .unwrap()
                        .cspace
                        .insert(c)
                        .unwrap_or(NO_CAP),
                    None => NO_CAP,
                };
                if !is_reply {
                    wake(sched, sender);
                }
                // x0 = word0, x1 = the delivered slot, x2 = word1 (a CALL's second word; 0 for a plain
                // SEND_CAP, whose sender parked mailbox[1] = 0).
                Some([msg[0], slot, msg[1]])
            }
            ipc::Recv::Blocked => {
                sched.threads.get_mut(current).unwrap().state = State::Blocked;
                None
            }
        }
    };

    match immediate {
        Some(msg) => msg,
        None => {
            schedule(); // a capability-carrying sender fills our mailbox and wakes us
            let guard = SCHED.lock();
            let sched = guard.as_ref().expect("no scheduler");
            sched.threads.get(current_tid()).unwrap().mailbox
        }
    }
}

/// **Call: send two words and block until replied** (milestone 12). The atomic send-and-wait a
/// one-shot reply capability makes safe. At the rendezvous the kernel mints a `Reply` capability
/// naming *this* caller and hands it to the server (through [`ipc_recv_cap`]); we then block,
/// discoverable **only** through that capability, until the server invokes it. Returns the reply
/// words. See DECISIONS §12 and notes/ipc-naming.md.
///
/// If the server's cspace is full the reply cap is dropped (the server sees `NO_CAP`, exactly as a
/// delegated cap would be) and, having no way to answer, the caller blocks until torn down: the same
/// no-timeout limitation as a reply that never comes, and self-inflicted by the server.
pub fn ipc_call(ep: usize, msg: [u64; 2]) -> [u64; 3] {
    {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();
        let reply = crate::cap::reply_cap(current);

        // `send` decides the rendezvous exactly as a plain SEND: a waiting server, or block. The
        // difference is the caller *always* blocks awaiting the reply, whether or not it met a server.
        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send; a caller queued here is Blocked until its Reply arrives.
        match unsafe { sched.endpoints[ep].send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver).id };
                // A server is parked in RECV_CAP: hand it the reply cap and the two words now.
                let r = sched.threads.get_mut(receiver).unwrap();
                let slot = r.cspace.insert(reply).unwrap_or(NO_CAP);
                r.mailbox = [msg[0], slot, msg[1]];
                wake(sched, receiver);
            }
            ipc::Send::Blocked => {
                // No server yet; `send` queued us as a sender. Park the words and ride the reply cap
                // in `outgoing_cap` so the eventual RECV_CAP hands it over and, seeing a Reply, leaves
                // us blocked (see ipc_recv_cap).
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = [msg[0], msg[1], 0];
                me.outgoing_cap = Some(reply);
            }
        }
        // Either way we block until the reply arrives. We are NOT queued as a receiver; the Reply
        // capability, which carries our tid, is the only thing that can wake us.
        sched.threads.get_mut(current).unwrap().state = State::Blocked;
    }

    schedule(); // returns once ipc_reply has filled our mailbox and woken us

    let guard = SCHED.lock();
    let sched = guard.as_ref().expect("no scheduler");
    sched.threads.get(current_tid()).unwrap().mailbox
}

/// **Reply: deliver two words to a blocked caller and wake it** (milestone 12). The other half of
/// [`ipc_call`], reached by invoking the one-shot Reply capability, which carries the caller's `tid`.
/// The caller is blocked awaiting exactly this. If it is already gone (it cannot be, while blocked,
/// but be defensive), the reply is simply dropped.
pub fn ipc_reply(caller: Tid, msg: [u64; 2]) {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");
    if let Some(t) = sched.threads.get_mut(caller) {
        t.mailbox = [msg[0], msg[1], 0];
        wake(sched, caller);
    }
}

/// Delete every `Frame` capability naming `phys` from every thread's cspace (§13). Part of
/// revocation: once a frame is being revoked, no holder may keep a capability that could re-map it.
/// The caller's own cap is deleted too, which is intended: a revoke destroys all access to the page.
pub fn delete_frame_caps(phys: u64) {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return;
    };
    let target = crate::cap::Object::Frame(phys);
    for t in sched.threads.iter_mut() {
        for slot in 0..t.cspace.len() as u64 {
            if t.cspace.get(slot).is_ok_and(|c| c.object == target) {
                let _ = t.cspace.delete(slot);
            }
        }
    }
}

/// Remove a capability from the **current thread's** table. Used to consume a one-shot Reply
/// capability the instant it is invoked (§12), which is what makes a second reply impossible.
pub fn delete_current_cap(slot: u64) -> Result<(), crate::cap::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(crate::cap::Error::NoSuchSlot)?;
    let current = current_tid();
    sched
        .threads
        .get_mut(current)
        .ok_or(crate::cap::Error::NoSuchSlot)?
        .cspace
        .delete(slot)
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
        .get(current_tid())
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
        .get_mut(current)
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
        .get_mut(tid)
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
            .get_mut(current)
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
        .get(current_tid())?
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

        for c in &COUNTS {
            crate::sched::spawn(move || {
                while !STOP.load(Ordering::SeqCst) {
                    c.fetch_add(1, Ordering::SeqCst);
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

        let stack = KernelStack::new().expect("could not allocate a thread stack");

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

    /// **Milestone 12: a call gets a reply, over one endpoint, via a one-shot Reply cap.**
    ///
    /// The client `CALL`s and blocks; the server `RECV_CAP`s (receiving the request word plus a
    /// kernel-minted `Reply` cap naming the caller), answers through that cap, and consumes it. One
    /// endpoint, not the two the pre-`Call` pattern needs, and the server was never wired to this
    /// client.
    #[test_case]
    fn a_call_gets_a_reply() {
        static ANSWER: AtomicU64 = AtomicU64::new(0);
        static DONE: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();

        super::spawn(move || {
            let m = super::ipc_recv_cap(ep); // [n, reply_slot, second_word]
            let slot = m[1];
            let crate::cap::Object::Reply(caller) = super::current_cap(slot).unwrap().object else {
                panic!("RECV_CAP of a CALL did not deliver a Reply capability");
            };
            super::ipc_reply(caller, [m[0] + 1, 0]);
            super::delete_current_cap(slot).expect("consume the one-shot reply");
        })
        .expect("spawn failed");

        super::spawn(move || {
            let r = super::ipc_call(ep, [41, 0]);
            ANSWER.store(r[0], Ordering::SeqCst);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        for _ in 0..200 {
            if DONE.load(Ordering::SeqCst) {
                break;
            }
            super::yield_now();
        }
        assert!(DONE.load(Ordering::SeqCst), "the call never returned");
        assert_eq!(ANSWER.load(Ordering::SeqCst), 42, "wrong reply");
    }

    /// **Milestone 12: a reply reaches the caller that called, not another.**
    ///
    /// Two clients call and block at once; the server answers each through *its* Reply cap. Client A
    /// (sent 100) must get 111 and client B (sent 200) must get 211. A shared reply endpoint cannot
    /// guarantee this: whichever client's `RECV` runs grabs the reply. The Reply cap, naming the
    /// specific blocked caller, makes misrouting unrepresentable.
    #[test_case]
    fn a_reply_reaches_the_caller_that_called() {
        static GOT_A: AtomicU64 = AtomicU64::new(0);
        static GOT_B: AtomicU64 = AtomicU64::new(0);

        let ep = super::create_endpoint();

        // The server: field two calls, reply each caller its own word + 11, via its own cap.
        super::spawn(move || {
            for _ in 0..2 {
                let m = super::ipc_recv_cap(ep);
                let (word, slot) = (m[0], m[1]);
                let crate::cap::Object::Reply(caller) = super::current_cap(slot).unwrap().object
                else {
                    panic!("not a reply cap");
                };
                super::ipc_reply(caller, [word + 11, 0]);
                super::delete_current_cap(slot).unwrap();
            }
        })
        .expect("spawn failed");

        super::spawn(move || {
            let r = super::ipc_call(ep, [100, 0]);
            GOT_A.store(r[0], Ordering::SeqCst);
        })
        .expect("spawn failed");
        super::spawn(move || {
            let r = super::ipc_call(ep, [200, 0]);
            GOT_B.store(r[0], Ordering::SeqCst);
        })
        .expect("spawn failed");

        for _ in 0..300 {
            if GOT_A.load(Ordering::SeqCst) != 0 && GOT_B.load(Ordering::SeqCst) != 0 {
                break;
            }
            super::yield_now();
        }
        assert_eq!(
            GOT_A.load(Ordering::SeqCst),
            111,
            "client A got the wrong caller's reply"
        );
        assert_eq!(
            GOT_B.load(Ordering::SeqCst),
            211,
            "client B got the wrong caller's reply"
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
        crate::drivers::gic::send_sgi(SGI, 0); // self (core 0) in the test

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
        crate::drivers::gic::send_sgi(SGI, 0); // self (core 0) in the test
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
