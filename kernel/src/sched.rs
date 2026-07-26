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

/// The thread table: generational names (`crates/slots`, notes/generational-names.md) over
/// **page-resident** TCBs (milestone 19c.2). Each `Thread` lives at the start of one page from
/// the kernel's own budget (`kmem`), so the static `MAX_THREADS`-sized BSS pool that B.2 built
/// as a scaffold is gone: the kernel reserves no per-thread memory it hasn't been handed, the
/// last uncovered corner of milestone 14's no-open-ended-spending thesis. B.2 named this moment
/// ("the pool upgrades to retype-backed storage behind the table when init lands"); this is it.
///
/// A page's address never changes (direct-mapped, and its `kmem` region is pinned), which
/// supplies the pinning the per-thread `Box` and then the pool both provided: the context-switch
/// assembly and the intrusive queues hold pointers straight into these pages. The table stores
/// the pointer; the generational name is what everything else carries (stale-safe as ever).
///
/// 19c.3 will let a user process retype a TCB from *its own* untyped by the same mechanism, the
/// page merely coming from a different budget; kernel threads keep drawing from `kmem`.
/// A TCB pointer that may cross cores. The pointer itself moving between cores is harmless: the
/// `Thread` it names is touched only under `SCHED` (which serializes all table access) and, for
/// its queue link, under the intrusive discipline at [`tcb_ptr`]. This is the same soundness the
/// old static `TcbPool`'s `unsafe impl Sync` rested on, now attached to the pointer the table
/// stores rather than a separate array.
#[derive(Clone, Copy)]
struct TcbPtr(*mut Thread);

// SAFETY: see the type's doc; sending the pointer is sound because dereferencing it is gated.
unsafe impl Send for TcbPtr {}

struct Threads {
    table: slots::Table<TcbPtr, MAX_THREADS>,
}

impl Threads {
    const fn new() -> Self {
        Self {
            table: slots::Table::new(),
        }
    }

    fn get(&self, tid: Tid) -> Option<&Thread> {
        let p = self.table.get(tid)?.0;
        // SAFETY: a pointer we stored at insert, into a live kmem page not yet recycled (remove
        // kills the name before recycling); SCHED serializes access.
        Some(unsafe { &*p })
    }

    fn get_mut(&mut self, tid: Tid) -> Option<&mut Thread> {
        let p = self.table.get(tid)?.0;
        // SAFETY: as `get`, and `&mut self` carries SCHED's exclusivity.
        Some(unsafe { &mut *p })
    }

    /// Insert: claim a page from the kernel budget, build the `Thread` (carrying its own minted
    /// name) into it, and store the pointer under that name. `None` (page recycled, `f` never
    /// run) if the budget or the table is exhausted.
    fn insert_with(&mut self, f: impl FnOnce(Tid) -> Thread) -> Option<Tid> {
        let page = crate::kmem::page()?;
        // A kernel thread's TCB page is `kmem`'s and comes home to it at death; if the table is
        // full it never held a Thread, so recycle now.
        let name = self.insert_at(page, f);
        if name.is_none() {
            crate::kmem::recycle(page);
        }
        name
    }

    /// Insert a Thread that already has a page (milestone 19c.3): a user-retyped TCB, whose page
    /// is its creator's region's, not `kmem`'s. On a full table the page is the region's to
    /// account (spend-only), so nothing is recycled here.
    fn insert_from_page(&mut self, page: u64, f: impl FnOnce(Tid) -> Thread) -> Option<Tid> {
        self.insert_at(page, f)
    }

    /// The shared engine: write the built Thread into `page` and name it. The Thread carries its
    /// own `tcb_kmem`, which `remove` reads to decide whether the page returns to `kmem`.
    fn insert_at(&mut self, page: u64, f: impl FnOnce(Tid) -> Thread) -> Option<Tid> {
        let ptr = crate::arch::mmu::phys_to_virt(page) as *mut Thread;
        self.table.insert_with(|tid| {
            // SAFETY: a fresh, exclusively-ours page; `write` moves the Thread in, no drop of
            // uninitialized bytes.
            unsafe { ptr.write(f(tid)) };
            TcbPtr(ptr)
        })
    }

    /// Remove and destroy: drop the TCB in place (its stack, address space, and quota token go
    /// with it), kill the name so no copy of the Tid ever resolves again, then recycle the page.
    fn remove(&mut self, tid: Tid) {
        let Some(&TcbPtr(ptr)) = self.table.get(tid) else {
            return;
        };
        // Read the page's origin BEFORE the drop consumes the Thread. A kernel TCB's page goes
        // home to `kmem`; a user TCB's page belongs to its region (spend-only, reclaimed only at
        // region destroy), so the reaper leaves it.
        // SAFETY: live per the table, exclusive per `&mut self`.
        let from_kmem = unsafe { (*ptr).tcb_kmem };
        // SAFETY: as above. Drop first (KernelStack's unmap-and-recycle, AddressSpace teardown,
        // the QuotaToken), then kill the name, then the page goes home: nothing can reach the
        // dropped Thread afterward.
        unsafe { core::ptr::drop_in_place(ptr) };
        self.table.remove(tid);
        if from_kmem {
            crate::kmem::recycle(crate::arch::mmu::virt_to_phys(ptr as u64));
        }
    }

    fn len(&self) -> usize {
        self.table.len()
    }

    /// Every live TCB, for whole-table sweeps (revocation). Each live name resolves to a
    /// distinct page pointer, so the `&mut`s are disjoint.
    fn iter_mut(&mut self) -> impl Iterator<Item = &mut Thread> + '_ {
        // SAFETY: each stored pointer is a distinct live page (one page per thread), and
        // `&mut self` carries SCHED's exclusivity across the whole sweep.
        self.table.values().map(|&TcbPtr(p)| unsafe { &mut *p })
    }
}

struct Scheduler {
    /// The thread table: generational names over page-resident TCBs. See [`Threads`];
    /// design/kernel-objects-from-untyped.md D2 records the path, notes/tcb.md the storage.
    threads: Threads,
    /// Neither the run queue nor `current` live here any more: both moved to per-CPU storage
    /// (`cpu::PerCpu`, DECISIONS.md §11 steps 3a and 3b), because a single shared queue and a
    /// single "running thread" are exactly what every core would otherwise contend on and
    /// overwrite. What stays is genuinely whole-machine: the thread table and the endpoints.
    ///
    /// Every IPC endpoint. Indexed by the `usize` inside an `Object::Endpoint` capability, which
    /// only the kernel mints, so the index is always in range.
    /// **The endpoint registry** (milestone 19a; design/init-and-granular-spawn.md). An endpoint
    /// is page-resident now: it lives at the start of a page retyped from some untyped region
    /// (a process's own, via `RETYPE_OBJ`, or the kernel's, via [`create_endpoint`]), and that
    /// region is pinned so the page can never be freed under a blocked thread. The registry
    /// entry is the page's physical address; the generational name (`crates/slots`, the same
    /// machinery as Tids) is what an `Object::Endpoint` capability carries, so the day endpoints
    /// can die, stale names will already fail safely.
    endpoints: slots::Table<u64, MAX_ENDPOINTS>,
    /// The kernel's own object region: where the kernel's endpoints (boot services, tests) are
    /// retyped from, so every endpoint lives uniformly in a pinned page regardless of who paid.
    /// Carved lazily on the first [`create_endpoint`].
    kernel_ep_region: Option<u64>,
}

/// The most endpoints that can ever exist: the registry's bound. Endpoint teardown does not
/// exist yet (regions hosting endpoints are pinned), so this caps creations over the kernel's
/// lifetime, not concurrent use.
const MAX_ENDPOINTS: usize = 256;

/// An endpoint's name: a generational `slots` name over the endpoint registry (19a). What an
/// `Object::Endpoint` capability carries. `u64` like a Tid, and stale-safe the same way.
pub type EpId = u64;

/// The pages the kernel carves for its own endpoints. 64 endpoints covers boot services plus
/// every test in the suite; exhaustion panics in [`create_endpoint`] with the number to raise.
const KERNEL_EP_PAGES: u64 = 64;

/// The endpoint behind a name. Caller holds `SCHED`.
///
/// Panics on a name that does not resolve, which is the old array's bounds panic wearing the new
/// naming: endpoint names reach here only out of kernel-minted capabilities, and endpoints are
/// never destroyed (their regions are pinned), so a miss is kernel corruption, not user input.
///
/// The `'static` is the page's pinned-ness made into a lifetime: the page is never freed, the
/// direct map always names it, and `SCHED` serializes every access to what it holds.
fn endpoint_of(sched: &Scheduler, ep: EpId) -> &'static mut Endpoint {
    let phys = *sched
        .endpoints
        .get(ep)
        .expect("endpoint name did not resolve");
    // SAFETY: retyped exclusively for this endpoint, region pinned (never freed), direct-mapped,
    // and serialized by SCHED, which every caller holds.
    unsafe { &mut *(crate::arch::mmu::phys_to_virt(phys) as *mut Endpoint) }
}

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
        endpoints: slots::Table::new(),
        kernel_ep_region: None,
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
    sched
        .threads
        .get_mut(tid)
        .expect("tcb_ptr of a dead thread")
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
        let next_root = sched
            .threads
            .get(next)
            .unwrap()
            .space
            .as_ref()
            .map(|s| s.ttbr0())
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
    let prev = cpu::current()
        .switched_from
        .swap(cpu::NO_TID, Ordering::Relaxed);
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
static IRQ_ROUTES: [AtomicU64; MAX_INTID] = [const { AtomicU64::new(0) }; MAX_INTID];

/// Route a hardware interrupt to an endpoint. From now on, when `intid` fires, whoever is
/// blocked on `ep` wakes; if nobody is, the signal is remembered so it is not lost.
pub fn bind_irq(intid: u32, ep: EpId) {
    assert!((intid as usize) < MAX_INTID, "intid {intid} out of range");
    // +1 so 0 keeps meaning "not routed". A name can never be u64::MAX (the registry mints
    // (generation << 32) | slot with slot < 256), so the increment cannot wrap.
    IRQ_ROUTES[intid as usize].store(ep + 1, Ordering::Release);
}

/// The endpoint an interrupt is routed to, if any. Read from the IRQ handler; lock-free.
pub fn irq_route(intid: u32) -> Option<EpId> {
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
pub fn irq_notify(ep: EpId) {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");

    // `signal` wakes a waiting receiver or counts the signal; it never blocks or joins a queue.
    if let Some(waiter) = endpoint_of(sched, ep).signal() {
        // SAFETY: only live Blocked threads sit on wait queues; reading the id revalidates it
        // through the table for everything after.
        let waiter = unsafe { (*waiter).id };
        sched.threads.get_mut(waiter).unwrap().mailbox = [1, 0, 0];
        wake(sched, waiter);
    }
}

/// Create an endpoint **in `region`'s memory** (milestone 19a): one page retyped and pinned, the
/// endpoint at its start, a fresh generational name in the registry. The shared engine of the
/// `RETYPE_OBJ` syscall and the kernel's own [`create_endpoint`]. `None` when the region is out
/// of budget or the registry is full (in which case the retyped page is spent but unused: a
/// process-local loss on its own budget, same as every failed spend since B.4).
pub fn create_endpoint_from(region: u64) -> Option<EpId> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut()?;

    // Rank: UNTYPED (58) under SCHED (60) is a legal descent; the pin rides in the same lock
    // hold as the carve, so no destroy can race the page away (see retype_object_page).
    let phys = crate::untyped::retype_object_page(region)?;

    // The page arrives zeroed, and an all-zero Endpoint happens to be valid; write it explicitly
    // anyway, because "happens to be" is the kind of truth that stops being one silently.
    // SAFETY: fresh page, exclusively ours, direct-mapped.
    unsafe { (crate::arch::mmu::phys_to_virt(phys) as *mut Endpoint).write(Endpoint::new()) };

    sched.endpoints.insert_with(|_| phys)
}

/// Create an IPC endpoint on the kernel's own budget. Returns the name that goes inside an
/// `Object::Endpoint`. The kernel's object region is carved lazily on first use and pinned like
/// any other endpoint host.
///
/// Panics on exhaustion: every caller is the kernel or a test wiring a service, so running out
/// is a misconfigured image (raise KERNEL_EP_PAGES or MAX_ENDPOINTS), not a runtime condition.
pub fn create_endpoint() -> EpId {
    let region = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        match sched.kernel_ep_region {
            Some(r) => r,
            None => {
                let r = crate::untyped::create(KERNEL_EP_PAGES)
                    .expect("no memory for the kernel's endpoint region");
                sched.kernel_ep_region = Some(r);
                r
            }
        }
    };
    create_endpoint_from(region).expect("out of endpoints: raise KERNEL_EP_PAGES / MAX_ENDPOINTS")
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
pub fn ipc_send(ep: EpId, msg: [u64; 3]) {
    let block = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // SAFETY: `me` is the running thread (live, on no queue), and if queued it stays live:
        // a thread queued on an endpoint is Blocked, which the reaper never touches. See tcb_ptr.
        match unsafe { endpoint_of(sched, ep).send(me) } {
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
pub fn ipc_recv(ep: EpId) -> [u64; 3] {
    let immediate = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send: the running thread, and Blocked-while-queued keeps it live.
        match unsafe { endpoint_of(sched, ep).recv(me) } {
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
pub fn ipc_send_cap(ep: EpId, data: u64, cap: crate::cap::Cap) {
    let block = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send.
        match unsafe { endpoint_of(sched, ep).send(me) } {
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
pub fn ipc_recv_cap(ep: EpId) -> [u64; 3] {
    let immediate = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send.
        match unsafe { endpoint_of(sched, ep).recv(me) } {
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
pub fn ipc_call(ep: EpId, msg: [u64; 2]) -> [u64; 3] {
    {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();
        let reply = crate::cap::reply_cap(current);

        // `send` decides the rendezvous exactly as a plain SEND: a waiting server, or block. The
        // difference is the caller *always* blocks awaiting the reply, whether or not it met a server.
        let me = tcb_ptr(sched, current);
        // SAFETY: as in ipc_send; a caller queued here is Blocked until its Reply arrives.
        match unsafe { endpoint_of(sched, ep).send(me) } {
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

/// **Retype a TCB out of `region`** (milestone 19c.3): an embryo thread, page-resident in a
/// page of the creator's own untyped, in the thread table but in no queue and not runnable.
/// Returns its Tid (what an `Object::Tcb` capability carries) or `None` if the region is out of
/// budget or the table is full.
pub fn create_tcb(region: u64) -> Option<Tid> {
    let page = crate::untyped::retype_object_page(region)?;
    let mut guard = SCHED.lock();
    let sched = guard.as_mut()?;
    let name = sched.threads.insert_from_page(page, |tid| {
        let mut t = Thread::embryo();
        t.id = tid;
        t
    });
    // On a full table the page stays the region's (spend-only); nothing to recycle. The region
    // is already pinned by retype_object_page, so its destroy is refused regardless.
    name
}

/// Tear down every kernel object whose backing page lies in `[base, end)`, so `untyped::destroy`
/// can reclaim the region (object revocation). `Err` if a **live** thread (`Ready`/`Running`/
/// `Blocked`) sits in the region: its owner must let it finish first, and the region stays pinned.
/// `Embryo` and `Finished` threads are removed here (dropped, and their generational names killed,
/// so every outstanding `Tcb` capability to them goes stale on its next use). Endpoints and address
/// spaces are the later phases of this milestone.
///
/// Takes `SCHED`, so it must run **outside** any teardown `Drop`: this is the caller-driven half of
/// revocation, and `untyped::destroy` is the `SCHED`-free half (which is why the reaper's
/// `Drop` -> `destroy` path cannot deadlock against it). See `untyped::unpin`.
fn reap_region_objects(base: u64, end: u64) -> Result<(), ()> {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return Err(());
    };
    // A TCB sits at the start of its page, so the page's physical address is the thread pointer
    // translated back. That is the whole test for "this object lives in the region".
    let page_of = |t: &Thread| crate::arch::mmu::virt_to_phys(t as *const Thread as u64);

    // --- Refuse phase: change nothing until every object in the region can be torn down. ---

    // A live thread (Ready/Running/Blocked) in the region: freeing its page would pull the stack, or
    // the running address space, out from under a thread that can still be scheduled.
    for t in sched.threads.iter_mut() {
        let phys = page_of(t);
        let live = matches!(t.state, State::Ready | State::Running | State::Blocked);
        if base <= phys && phys < end && live {
            return Err(());
        }
    }
    // An endpoint with a thread blocked on it: reclaiming its page would strand that thread. This is
    // the safe subset; error-return-to-the-waiter (the chosen richer semantic) is a deferred IPC-core
    // change, so for now a blocked waiter refuses the reclaim, as a live thread does. An idle
    // endpoint holds no thread and is torn down below.
    for (_, &phys) in sched.endpoints.iter() {
        if base <= phys && phys < end {
            // SAFETY: the endpoint lives at `phys` (retyped for it), its region pinned until we free
            // it, SCHED held. Reading its wait queues is sound.
            let ep = unsafe { &*(crate::arch::mmu::phys_to_virt(phys) as *const Endpoint) };
            if !ep.is_idle() {
                return Err(());
            }
        }
    }

    // --- Removal phase: every object in the region is reapable. ---

    // Threads: collect before removing (`remove` mutates the table). Both Embryo and Finished go.
    let mut doomed = [0u64; MAX_THREADS];
    let mut n = 0;
    for t in sched.threads.iter_mut() {
        let phys = page_of(t);
        if base <= phys && phys < end {
            doomed[n] = t.id;
            n += 1;
        }
    }
    for &tid in &doomed[..n] {
        sched.threads.remove(tid);
    }

    // Endpoints: the idle ones in the region. Removing bumps the name's generation, so every
    // Endpoint capability to it fails to resolve; the page itself is freed by the enclosing destroy.
    let mut doomed_eps = [0u64; MAX_ENDPOINTS];
    let mut ne = 0;
    for (name, &phys) in sched.endpoints.iter() {
        if base <= phys && phys < end {
            doomed_eps[ne] = name;
            ne += 1;
        }
    }
    for &name in &doomed_eps[..ne] {
        sched.endpoints.remove(name);
    }

    Ok(())
}

/// **Reclaim an untyped region and every object retyped from it** (object revocation, the region-
/// ownership half). The owner, holding the untyped capability, reclaims: tear the region's objects
/// down (refusing if any is still live), unpin, and return the memory. Generational names make
/// every capability to the now-dead objects stale on next use, so there is no capability tree to
/// walk and no copies to hunt (contrast seL4's CDT; DECISIONS records the choice).
///
/// Must run outside any `Drop`, because the reap takes `SCHED` (see `reap_region_objects`); the
/// `unpin` + `destroy` that follow are `SCHED`-free.
pub fn reclaim_region(region: u64) -> Result<(), ()> {
    // A region carved into children cannot be reclaimed: its child regions own part of its run and
    // free those pages themselves. The owner must destroy the children first. Refuse before any
    // teardown, so a refused reclaim leaves the region exactly as it was.
    if crate::untyped::has_children(region) {
        return Err(());
    }
    let (base, size) = crate::untyped::region_bounds(region).ok_or(())?;
    // Threads first (SCHED), then any unbound address spaces (the aspace registry lock). Two
    // separate lock domains, sequenced, never nested: neither is held across the other. Bound
    // spaces need no step here, they died with their thread in the reap above.
    reap_region_objects(base, base + size)?;
    crate::user::reap_aspaces_in_region(base, base + size);
    crate::untyped::unpin(region);
    crate::untyped::destroy(region);
    Ok(())
}

/// **Configure an embryo** (milestone 19c.3): bind the address space named by `aspace_name`
/// (moved out of the user-aspace registry into the TCB, so it now dies with the thread) and set
/// the EL0 entry and user stack. Refuses anything but an `Embryo`, so a running thread cannot be
/// reconfigured under itself. `Ok(())` or a reason.
pub fn configure_tcb(
    tid: Tid,
    entry: u64,
    user_sp: u64,
    aspace_name: u64,
) -> Result<(), abi::Error> {
    // Take the space out of the registry FIRST (outside SCHED: it takes the aspace lock, ranked
    // above SCHED). If the TCB then turns out not to be a configurable embryo, put nothing back
    // is wrong, so check the embryo state first, under SCHED, and only take the space once the
    // bind will succeed.
    {
        let guard = SCHED.lock();
        let sched = guard.as_ref().ok_or(abi::Error::NoSuchSlot)?;
        let t = sched.threads.get(tid).ok_or(abi::Error::NoSuchSlot)?;
        if t.state != State::Embryo {
            return Err(abi::Error::WrongObject); // only an unstarted TCB may be configured
        }
    }
    let space = crate::user::take_user_aspace(aspace_name).ok_or(abi::Error::NoSuchSlot)?;

    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(abi::Error::NoSuchSlot)?;
    let Some(t) = sched.threads.get_mut(tid) else {
        // The TCB vanished between the checks (it cannot, without a teardown path, but be
        // honest): give the space back to the registry rather than leak it.
        drop(guard);
        crate::user::readopt_user_aspace(space);
        return Err(abi::Error::NoSuchSlot);
    };
    if t.state != State::Embryo {
        drop(guard);
        crate::user::readopt_user_aspace(space);
        return Err(abi::Error::WrongObject);
    }
    t.space = Some(space);
    t.entry = (entry, user_sp);
    Ok(())
}

/// **Install a capability into an embryo's cspace** (milestone 19c.3): the child's initial
/// authority, granted one slot at a time before it runs. Refuses a non-embryo. Returns the child
/// slot the capability landed in.
pub fn tcb_insert_cap(tid: Tid, cap: crate::cap::Cap) -> Result<u64, abi::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(abi::Error::NoSuchSlot)?;
    let t = sched.threads.get_mut(tid).ok_or(abi::Error::NoSuchSlot)?;
    if t.state != State::Embryo {
        return Err(abi::Error::WrongObject);
    }
    t.cspace.insert(cap).map_err(|_| abi::Error::OutOfMemory)
}

/// **Start an embryo** (milestone 19c.3): the no-start-before-whole gate, then make it runnable.
/// Refuses a TCB that is not an embryo, or one with no bound address space or no entry set: a
/// half-built thread must never run. On success the thread gets its kernel stack and entry
/// context and joins this core's run queue.
pub fn start_tcb(tid: Tid, args: [u64; 3]) -> Result<(), abi::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(abi::Error::NoSuchSlot)?;
    let t = sched.threads.get_mut(tid).ok_or(abi::Error::NoSuchSlot)?;

    if t.state != State::Embryo {
        return Err(abi::Error::WrongObject); // already started (or not a TCB)
    }
    // WHOLE, or refuse: a bound address space and an entry point. Either missing is a half-built
    // thread, and starting it would drop to EL0 with no low half or no code.
    if t.space.is_none() || t.entry.0 == 0 {
        return Err(abi::Error::NotPermitted); // configure it first
    }
    t.start_args = args; // the child's x0, x1, x2 (19d/19e)
    if !t.arm_for_start() {
        return Err(abi::Error::OutOfMemory); // no kernel stack to be had
    }
    t.state = State::Ready;
    let ptr = tcb_ptr(sched, tid);
    // SAFETY: freshly Ready, on no queue (it was an embryo, queued nowhere); SCHED held, IRQs
    // masked, so this core's run queue is ours.
    cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
    Ok(())
}

/// Hand the current thread an address space, and install it.
///
/// From here the thread owns its low half: the reaper's `drop` will unmap and free it, and
/// every context switch back to this thread will re-install it.
pub fn adopt_address_space(space: crate::user::AddressSpace) {
    let ttbr = space.ttbr0();

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

    crate::arch::mmu::switch_user_root(ttbr);
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

/// Print every thread's scheduler state, for diagnosing a hang. A lost IPC wakeup leaves a thread
/// `Blocked` forever with nothing to wake it; this shows which thread, and the `on_cpu`/`wake_pending`
/// flags that would reveal a botched wake-before-switch-out handoff. Takes SCHED, which is free when
/// the hang is a blocked thread (not a lock deadlock). Used by the test watchdog.
#[cfg_attr(not(test), allow(dead_code))]
pub fn dump_threads() {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        crate::println!("  dump_threads: no scheduler");
        return;
    };
    crate::println!("--- thread dump (hang diagnostic) ---");
    for t in sched.threads.iter_mut() {
        crate::println!(
            "  tid={:#06x} state={:?} on_cpu={} wake_pending={} has_outgoing_cap={}",
            t.id,
            t.state,
            t.on_cpu,
            t.wake_pending,
            t.outgoing_cap.is_some(),
        );
    }
    for c in 0..crate::smp::online_count() {
        let pc = cpu::of(c);
        let inbox_len = pc.inbox.lock().len();
        crate::println!(
            "  core {c}: current={:#06x} idle={:#06x} switched_from={:#06x} need_resched={} inbox_len={}",
            pc.current.load(Ordering::Relaxed),
            pc.idle.load(Ordering::Relaxed),
            pc.switched_from.load(Ordering::Relaxed),
            pc.need_resched.load(Ordering::Relaxed),
            inbox_len,
        );
    }
    crate::println!("--- end thread dump ---");
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

    /// **Object revocation reclaims a region holding an unstarted TCB** (the smallest proof of the
    /// mechanism). Retype a bare embryo into a fresh region, then `reclaim_region`: the TCB is torn
    /// down (its table slot freed, its generational name dead), the region's memory returns, and the
    /// free-frame count lands exactly where it began. No scheduler run, no address space, no reaper
    /// timing: find the object, kill it, unpin, free. The larger cases (a started-then-exited
    /// thread, its address space, the spawn-to-reap loop) build on this one.
    #[test_case]
    fn reclaim_frees_an_embryo_tcbs_region() {
        let frames_before = crate::memory::free_frames();
        let threads_before = crate::sched::thread_count();

        let region = crate::untyped::create(2).expect("a fresh 2-page region");
        let _tid = crate::sched::create_tcb(region).expect("retype a TCB from the region");

        assert_eq!(
            crate::sched::thread_count(),
            threads_before + 1,
            "the embryo should be in the table before reclaim"
        );
        assert!(
            crate::memory::free_frames() < frames_before,
            "creating the region should have spent frames"
        );

        crate::sched::reclaim_region(region)
            .expect("reclaim a region whose only object is an unstarted TCB");

        assert_eq!(
            crate::sched::thread_count(),
            threads_before,
            "the TCB's table slot must be freed by reclaim"
        );
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "reclaim must return the region's memory exactly to baseline"
        );
    }

    /// **Object revocation reclaims a region holding an unbound address space** (the address-space
    /// case of piece 1's mechanism). Create a space in its own region, not bound to any TCB, then
    /// reclaim: the space is torn down (its name goes stale, its ASID is freed by `Drop`) and the
    /// region's memory returns exactly to baseline. This is what retires the "an unbound space
    /// leaks" note the registry carried since 19b.
    #[test_case]
    fn reclaim_frees_an_unbound_address_spaces_region() {
        let frames_before = crate::memory::free_frames();

        let region = crate::untyped::create(8).expect("a fresh region");
        let name =
            crate::user::user_aspace_create(region).expect("an address space from the region");

        assert!(
            crate::user::user_aspace_root(name).is_some(),
            "the space should resolve before reclaim"
        );
        assert!(
            crate::memory::free_frames() < frames_before,
            "creating the space should have spent frames"
        );

        crate::sched::reclaim_region(region).expect("reclaim the space's own region");

        assert!(
            crate::user::user_aspace_root(name).is_none(),
            "the space's name must be stale after reclaim"
        );
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "reclaim must return the region's memory exactly to baseline"
        );
    }

    /// **Untyped SPLIT carves a reclaimable child, and a split parent is committed.** Create a
    /// region, carve it entirely into two children, and check the model: the parent now
    /// `has_children` and cannot be reclaimed (the children own its run), while each child is an
    /// ordinary region that retypes and reclaims on its own. With the parent fully delegated,
    /// reclaiming both children returns all the memory to baseline (only the parent's region-table
    /// slot is left behind, which generational region slots will fix). This is the subdivision that
    /// lets a spawner give each child its own reclaimable region.
    #[test_case]
    fn split_carves_reclaimable_children_and_commits_the_parent() {
        let frames_before = crate::memory::free_frames();

        let parent = crate::untyped::create(8).expect("parent region");
        let child_a = crate::untyped::split(parent, 4).expect("split child a");
        let child_b = crate::untyped::split(parent, 4).expect("split child b");
        assert_ne!(child_a, child_b, "children are distinct regions");

        // Parent fully carved: no budget left, and it cannot be reclaimed while children own its run.
        assert!(
            crate::untyped::has_children(parent),
            "the parent must record that it was split"
        );
        assert!(
            crate::sched::reclaim_region(parent).is_err(),
            "a region split into children must refuse reclaim",
        );
        assert!(
            crate::untyped::split(parent, 1).is_none(),
            "an exhausted parent cannot split further",
        );

        // A child is an ordinary region: retype a page from it, then reclaim it.
        let _p = crate::untyped::retype_page(child_a).expect("retype a page from a child");
        crate::sched::reclaim_region(child_a).expect("reclaim child a");
        crate::sched::reclaim_region(child_b).expect("reclaim child b");

        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "a fully-delegated parent's memory returns once its children are reclaimed",
        );
    }

    /// **A destroyed region's table slot is reused** (generational regions). Create and destroy a
    /// region far more times than the table has slots: without reuse the 257th `create` would fail
    /// with the table full, the lifetime cap that made a long-running system untenable. With reuse
    /// each `destroy` frees the slot, so one free slot serves the whole loop, and the free-frame
    /// count nets to zero every iteration. This is the property that lets the kernel run workloads
    /// that come and go without end.
    #[test_case]
    fn destroyed_region_slots_are_reused() {
        let frames_before = crate::memory::free_frames();
        // Comfortably more than MAX_REGIONS (256): without reuse this exhausts the table well before
        // the end. With reuse, one freed slot serves every iteration.
        for _ in 0..320 {
            let r = crate::untyped::create(1).expect("a region slot must be reused, not exhausted");
            crate::untyped::destroy(r);
        }
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "each create+destroy of a region must net zero frames",
        );
    }

    /// **Object revocation reclaims a region holding an idle endpoint.** An endpoint nobody is
    /// blocked on is torn down with its region: removed from the registry (its name goes stale, so
    /// every Endpoint capability to it fails), and its page returned. Frames back to baseline.
    #[test_case]
    fn reclaim_frees_a_regions_idle_endpoint() {
        let frames_before = crate::memory::free_frames();
        let region = crate::untyped::create(2).expect("region");
        let _ep = crate::sched::create_endpoint_from(region).expect("endpoint from region");
        assert!(
            crate::memory::free_frames() < frames_before,
            "creating the endpoint should have spent frames"
        );
        crate::sched::reclaim_region(region).expect("reclaim a region with only an idle endpoint");
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "the idle endpoint's region must return to baseline",
        );
    }

    /// **A region whose endpoint has a blocked waiter refuses reclaim, and reclaims once it is
    /// idle.** This is the safe subset of endpoint revocation: rather than wake a blocked waiter
    /// with an error (the intended richer semantic, a deferred IPC-core change), reclaim refuses
    /// while a thread is blocked on an endpoint in the region, exactly as it refuses a live thread.
    #[test_case]
    fn reclaim_refuses_a_region_whose_endpoint_has_a_waiter() {
        static DONE: AtomicBool = AtomicBool::new(false);
        DONE.store(false, Ordering::SeqCst);

        let region = crate::untyped::create(2).expect("region");
        let ep = crate::sched::create_endpoint_from(region).expect("endpoint from region");

        // A thread that blocks receiving on the endpoint.
        crate::sched::spawn(move || {
            let _ = crate::sched::ipc_recv(ep);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn a waiter");

        // Single core: one yield lets the waiter run and block on the recv.
        crate::sched::yield_now();

        assert!(
            crate::sched::reclaim_region(region).is_err(),
            "a region whose endpoint has a blocked waiter must refuse reclaim",
        );

        // Unblock the waiter (rendezvous), let it finish, and the endpoint goes idle.
        crate::sched::ipc_send(ep, [7, 0, 0]);
        for _ in 0..50 {
            if DONE.load(Ordering::SeqCst) {
                break;
            }
            crate::sched::yield_now();
        }
        assert!(DONE.load(Ordering::SeqCst), "the waiter never woke");

        crate::sched::reclaim_region(region).expect("reclaim once the endpoint is idle again");
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

    /// **Milestone 19c.1: the kernel cannot spend beyond its boot carve, for stacks.** Spawn a
    /// batch of threads and let them reap; the frame allocator's free count must return to
    /// exactly where it started, because kernel stacks now come from the kernel's own budget
    /// region (`kmem`, carved once) and recycle within it, not from the allocator. This is the
    /// milestone-14 no-open-ended-kernel-spending thesis extended to the last thing it missed;
    /// before 19c.1 this test would show four stacks' worth of frames gone per batch.
    ///
    /// The carve itself happens on the very first spawn ever (the idle thread, at boot), so by
    /// the time this test runs the region exists and steady state is flat.
    #[test_case]
    fn kernel_stacks_do_not_touch_the_frame_allocator_in_steady_state() {
        use core::sync::atomic::{AtomicU64, Ordering};
        static REAPED: AtomicU64 = AtomicU64::new(0);

        let baseline = super::thread_count();
        // Warm up: reach steady state (first spawn after boot may still be settling VAs).
        for _ in 0..2 {
            super::spawn(|| {}).expect("warmup spawn");
            while super::thread_count() > baseline {
                super::yield_now();
            }
        }

        let free_before = crate::memory::stats().unwrap().free();
        REAPED.store(0, Ordering::SeqCst);
        for _ in 0..6 {
            super::spawn(|| {}).expect("spawn failed");
            while super::thread_count() > baseline {
                super::yield_now();
            }
        }
        assert_eq!(
            crate::memory::stats().unwrap().free(),
            free_before,
            "six threads came and went and the frame allocator's count moved: a kernel stack              is still drawing from the allocator instead of the kernel budget",
        );
    }

    /// **Milestone 19a: an endpoint retyped from a region carries IPC, and pins its region.**
    /// The kernel-level half of the granular-construction story: `create_endpoint_from` carves a
    /// page, the endpoint lives in it, rendezvous works over it exactly as over a kernel-wired
    /// endpoint, and `untyped::destroy` refuses the now-pinned region, because freeing the page
    /// under a live endpoint would dangle every queued thread. The refusal is measured, not
    /// assumed: the allocator's free count must not move.
    #[test_case]
    fn a_retyped_endpoint_carries_ipc_and_pins_its_region() {
        use core::sync::atomic::{AtomicU64, Ordering};
        static GOT: AtomicU64 = AtomicU64::new(0);

        let region = crate::untyped::create(2).expect("no region");
        let ep = super::create_endpoint_from(region).expect("no endpoint from region");
        let kernel_ep = super::create_endpoint();
        assert_ne!(ep, kernel_ep, "registry names collide");

        super::spawn(move || {
            GOT.store(super::ipc_recv(ep)[0], Ordering::SeqCst);
        })
        .expect("spawn failed");
        super::ipc_send(ep, [0x2A, 0, 0]);
        for _ in 0..200 {
            if GOT.load(Ordering::SeqCst) != 0 {
                break;
            }
            super::yield_now();
        }
        assert_eq!(
            GOT.load(Ordering::SeqCst),
            0x2A,
            "no rendezvous over the retyped endpoint"
        );

        let free_before = crate::memory::stats().unwrap().free();
        crate::untyped::destroy(region);
        assert_eq!(
            crate::memory::stats().unwrap().free(),
            free_before,
            "destroy reclaimed a pinned region hosting a live endpoint",
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
