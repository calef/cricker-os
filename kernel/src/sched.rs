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
//! whose entire body is `loop { count += 1 }`: no yields, no syscalls, not even a function
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
//!    and without it the first thread you spawn can never be preempted, which would be a
//!    cooperative scheduler with extra steps.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use wake_handshake::{SwitchOutVerdict, WakeVerdict};

use crate::cpu;
use crate::sync::{IrqSafeMutex, rank};
use crate::thread::{Context, QuotaToken, State, Thread, Tid, WaitRole, switch_to};

/// How many times we have actually taken the CPU away from a thread. The number that says
/// preemption is real.
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);

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
pub(crate) const MAX_THREADS: usize = 128;

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

    /// `insert_with`'s place-writing twin: claim a page from the kernel budget and let `build`
    /// construct the Thread into it. Recycles the page on any failure, exactly as `insert_with`
    /// does, so a refused spawn costs nothing.
    fn insert_in_place(&mut self, build: impl FnOnce(Tid, *mut Thread) -> bool) -> Option<Tid> {
        let page = crate::kmem::page()?;
        let name = self.insert_at_in_place(page, build);
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

    /// **The place-writing insert** (milestone 124): `build` receives the minted name and the TCB
    /// page, and writes the Thread there itself. `false` from `build` means it wrote nothing, and
    /// the slot is left exactly as it was found.
    ///
    /// The difference from `insert_at` is where the Thread is constructed. That one takes a
    /// `FnOnce(Tid) -> Thread`, so the value travels through the closure's return and a temporary
    /// before `ptr.write` puts it on the page; a `Thread` is large and a debug build copies at
    /// every hop. This hands the destination down instead. See `Thread::spawn_into`.
    fn insert_at_in_place(
        &mut self,
        page: u64,
        build: impl FnOnce(Tid, *mut Thread) -> bool,
    ) -> Option<Tid> {
        let ptr = crate::arch::mmu::phys_to_virt(page) as *mut Thread;
        let mut built = false;
        let name = self.table.insert_with(|tid| {
            built = build(tid, ptr);
            TcbPtr(ptr)
        })?;
        if !built {
            // `build` declined (no kernel stack). Take the name back out: the slot never held a
            // Thread, so there is nothing to drop, and `remove` here would drop uninitialised
            // bytes. `forget_slot` bumps the generation and frees the slot without touching the
            // TCB page, which is the caller's to recycle. `Table::remove` is exactly right and
            // not a leak: the slot holds a `TcbPtr`, and dropping that drops a pointer. The
            // `Thread` drop lives in `Threads::remove`, which is not on this path because no
            // Thread was ever constructed.
            self.table.remove(name);
            return None;
        }
        Some(name)
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

    /// Every live TCB from slot `from` onward, with its slot index, for a **resumable** sweep
    /// (`endpoint::SURVEY`, milestone 126). The slot is the caller's cursor; see
    /// `slots::Table::iter_from` for why a position would not do.
    fn iter_from(&self, from: usize) -> impl Iterator<Item = (usize, &Thread)> + '_ {
        // SAFETY: as `iter_mut`, and shared rather than exclusive: each stored pointer is a
        // distinct live page, and `&self` carries SCHED for the walk.
        self.table
            .iter_from(from)
            .map(|(slot, _, &TcbPtr(p))| (slot, unsafe { &*p }))
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
    /// The kernel's **current** object chunk: where the kernel's endpoints (boot services, tests)
    /// are retyped from, so every endpoint lives uniformly in a pinned page regardless of who paid.
    /// Carved lazily on the first [`create_endpoint`] and **replaced when it fills**, which is what
    /// makes the kernel's endpoint supply grow instead of being a compile-time guess.
    ///
    /// A filled chunk's handle is deliberately forgotten. Its pages stay pinned and its endpoints
    /// stay live, and nothing ever hands a kernel chunk back: kernel endpoints are destroyed only by
    /// tearing down the region hosting them (see the `doomed_eps` walk), and no path tears down a
    /// kernel chunk. If one ever should, this becomes an array and that is the change to make.
    kernel_ep_region: Option<u64>,
    /// How many chunks have been carved, so growth is bounded by something rather than by nothing.
    kernel_ep_chunks: usize,
}

/// The most endpoints that can exist **at once**: the registry's bound.
///
/// This used to say it capped creations over the kernel's lifetime, on the grounds that endpoint
/// teardown did not exist. That went stale when object revocation made destruction real: tearing
/// down a region removes every endpoint whose page lives in it and `slots::Table::remove` frees the
/// slot for reuse, so this is a concurrent bound now. Corrected rather than left, because a stale
/// bound is the kind of comment that gets believed during a capacity argument.
const MAX_ENDPOINTS: usize = 512;

/// An endpoint's name: a generational `slots` name over the endpoint registry (19a). What an
/// `Object::Endpoint` capability carries. `u64` like a Tid, and stale-safe the same way.
pub type EpId = u64;

/// The pages in one of the kernel's endpoint chunks. **Not a ceiling.** When a chunk fills,
/// [`create_endpoint`] carves another, so this is a batch size and nothing else.
///
/// It used to be a ceiling, and it was the wrong shape of number, because it grew with the SUITE
/// rather than with the system: 64 lasted until the 27+28 merge, 96 until supervision and `std::net`
/// merged the same day, 128 until milestone 33's compositor tests, which wire 26 endpoints across
/// four scenes (a display, a doorbell, a report per client, an input endpoint per focusable client)
/// and wanted 160. Every parallel branch fit on its own, and the union
/// of their test boots is what crossed the line, which is a cost no branch can see before it merges.
/// So the failure mode was a merge-time panic telling whoever merged to raise a constant, over and
/// over, for a reason none of them caused. Growing on demand retires that whole class of papercut:
/// there is no number to raise, and the only remaining limit is [`MAX_ENDPOINTS`], which is a real
/// bound with a real meaning.
///
/// 32 pages (128 KiB) is deliberately modest. A normal boot carves exactly one chunk and the rest of
/// the supply is never touched, which is the point of carving lazily.
const KERNEL_EP_CHUNK_PAGES: u64 = 32;

/// How many chunks the kernel will carve before refusing. Derived so the **page supply can never be
/// the binding limit before the registry is**: enough chunks to host [`MAX_ENDPOINTS`] endpoints, one
/// page each. That is what makes exhaustion always report the honest reason (the registry is full)
/// rather than an arbitrary carve size. Derived rather than written down so the two cannot drift.
const MAX_KERNEL_EP_CHUNKS: usize = MAX_ENDPOINTS.div_ceil(KERNEL_EP_CHUNK_PAGES as usize);

/// The endpoint behind a name, or `None` if the name no longer resolves. Caller holds `SCHED`.
///
/// This used to panic on a miss, because endpoints could not be destroyed (their regions stayed
/// pinned), so a miss was kernel corruption. Object revocation made destruction real: a stale
/// `Endpoint` capability (its endpoint reclaimed out from under a holder) is now ordinary user
/// input, so this returns `None` and the callers turn that into a clean error rather than a panic.
///
/// The `'static` is the page's pinned-ness made into a lifetime: while the name resolves the page is
/// pinned and direct-mapped, and `SCHED` serializes every access to what it holds.
fn endpoint_of(sched: &Scheduler, ep: EpId) -> Option<&'static mut Endpoint> {
    let phys = *sched.endpoints.get(ep)?;
    // SAFETY: retyped exclusively for this endpoint, its region pinned while the name resolves,
    // direct-mapped, and serialized by SCHED, which every caller holds.
    Some(unsafe { &mut *(crate::arch::mmu::phys_to_virt(phys) as *mut Endpoint) })
}

/// Mark the current thread's blocking IPC as aborted (a stale endpoint, or one revoked while it
/// blocked): the syscall layer reads-and-clears this after the primitive returns and hands back an
/// error. A helper because several IPC paths set it. Caller holds `SCHED`.
fn set_ipc_aborted(sched: &mut Scheduler, tid: Tid) {
    if let Some(t) = sched.threads.get_mut(tid) {
        t.handshake.abort();
    }
}

/// **Read and clear the current thread's IPC-aborted flag** (object revocation). The syscall layer
/// calls this right after an endpoint IPC primitive returns: `true` means the endpoint was stale, or
/// revoked while the thread blocked on it, so the caller gets an error instead of the primitive's
/// placeholder result. Kernel-side IPC callers never set it (their endpoints are never revoked), so
/// they need not check it.
pub fn take_ipc_aborted() -> bool {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return false;
    };
    let tid = current_tid();
    sched
        .threads
        .get_mut(tid)
        .map(|t| t.handshake.take_aborted())
        .unwrap_or(false)
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

/// **Per-cpu ring of the last few scheduler events** (first-silicon diagnostics, 2026-08-14; the
/// module name is provisional). A boot-7 bench dump on the VisionFive 2 showed an end state no
/// legal transition sequence produces (a thread `Blocked` at a non-syscall pc; a receiver
/// `Running` as another core's current for ten seconds), and an end state alone cannot say which
/// transition wrote it. This keeps the last [`trace::DEPTH`] events each core performed, and
/// [`dump_threads`] prints them, so the *path* into the wedge is on the serial log.
///
/// Cost and honesty:
///
/// - One relaxed `fetch_add` and one relaxed store per event, on paths that already hold `SCHED`
///   or run in IRQ context with interrupts masked, so each ring has exactly one writer (its own
///   core) and no entry can tear (one `u64`).
/// - **Compiled out of `--features bench` builds**, so the benchmark numbers the tripwire watches
///   are not measuring the instrument. The board tour build carries no features and keeps it.
/// - A dump reads other cores' rings racily (drain/steal events are recorded outside `SCHED`);
///   an entry is atomic, so the worst case is an event missing from the tail, never a torn one.
///
/// The bench build gets a no-op twin of the same two-function surface (below), so the call sites
/// are identical in every configuration and nothing here needs a dead-code allow.
#[cfg(not(feature = "bench"))]
mod trace {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Events per core. 16 is enough to see the whole final approach to a wedge (a block, the
    /// wakes around it, the switch that stranded something) without turning the dump into a log.
    pub const DEPTH: usize = 16;

    /// What happened. The discriminant is packed into the entry's top byte.
    #[derive(Clone, Copy)]
    #[repr(u8)]
    pub enum Event {
        /// `schedule()` picked `tid` and marked it Running on this core.
        SwitchTo = 1,
        /// The thread running here marked itself Blocked (aux = low byte of the endpoint name).
        BlockSelf = 2,
        /// This core moved `tid` Blocked -> Ready onto a run queue (rendezvous, reply, or abort).
        Wake = 3,
        /// This core saw `tid` still on a cpu and parked the wake (`wake_pending`).
        WakeDeferred = 4,
        /// This core completed a parked wake in `finish_switch`.
        WakeCompleted = 5,
        /// This core pushed `tid` into cpu `aux`'s inbox (placement or load-aware wake).
        PlaceRemote = 6,
        /// This core served a steal: handed `tid` to requester cpu `aux`.
        StealServe = 7,
        /// This core drained its inbox; the tid field carries the count moved.
        InboxDrain = 8,
        /// This core REFUSED a wake of `tid`: the target was parked in IPC and the waker had
        /// delivered nothing (no message, no signal, no abort). The boot-8 gate firing; on a
        /// healthy boot this event never appears, so its presence in a bench dump is the finding.
        WakeRefused = 9,
        /// This core set `ipc_served` on `tid`: a delivery completed the thread's parked IPC.
        /// `aux` names the delivering site (1 send, 2 recv-collect, 3 `send_cap`, 4 `recv_cap`-collect,
        /// 5 call, 6 reply, 7 irq signal, 8 death message), so a bench dump answers "who served
        /// this thread" by reading the ring instead of inferring it from a frozen syscall count,
        /// which is the inference boots 7 through 9 got wrong (notes/visionfive2.md, fifth stop).
        Served = 10,
    }

    struct Ring {
        seq: AtomicU64,
        slots: [AtomicU64; DEPTH],
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY_RING: Ring = Ring {
        seq: AtomicU64::new(0),
        slots: [const { AtomicU64::new(0) }; DEPTH],
    };

    static RINGS: [Ring; crate::cpu::MAX_CPUS] = [EMPTY_RING; crate::cpu::MAX_CPUS];

    /// Record an event on the calling core's ring. Every call site runs with interrupts masked
    /// (under `SCHED` or in IRQ context), so the owning core cannot interleave with itself.
    #[inline]
    pub fn record(kind: Event, tid: u64, aux: u8) {
        let ring = &RINGS[crate::cpu::id()];
        let seq = ring.seq.fetch_add(1, Ordering::Relaxed);
        // kind in the top byte, aux below it, the tid's low 48 bits under that. A tid is
        // (generation << 32) | slot with slot < 256; 48 bits keeps 16 bits of generation,
        // plenty to disambiguate in a dump.
        let entry = ((kind as u64) << 56) | ((aux as u64) << 48) | (tid & 0x0000_FFFF_FFFF_FFFF);
        ring.slots[(seq as usize) % DEPTH].store(entry, Ordering::Relaxed);
    }

    /// Print core `cpu`'s ring, oldest first. Racy against that core's own writes, by design.
    pub fn dump(cpu: usize) {
        let ring = &RINGS[cpu];
        let seq = ring.seq.load(Ordering::Relaxed);
        if seq == 0 {
            return;
        }
        let start = seq.saturating_sub(DEPTH as u64);
        crate::print!("    core {cpu} events [{start}..{seq}):");
        for s in start..seq {
            let e = ring.slots[(s as usize) % DEPTH].load(Ordering::Relaxed);
            let (kind, aux, tid) = (e >> 56, (e >> 48) & 0xff, e & 0x0000_FFFF_FFFF_FFFF);
            let name = match kind {
                1 => "switch",
                2 => "block",
                3 => "wake",
                4 => "wake?",
                5 => "wake+",
                6 => "place",
                7 => "steal",
                8 => "drain",
                9 => "refuse",
                10 => "serve",
                _ => "?",
            };
            crate::print!(" {name}:{tid:#x}");
            if matches!(kind, 2 | 6 | 7 | 10) {
                crate::print!("/{aux}");
            }
        }
        crate::println!();
    }
}

/// The bench build's no-op twin of [`trace`]: same names, same signatures, nothing recorded, so
/// the benchmark numbers the tripwire watches never measure the instrument and the call sites
/// need no `cfg` of their own.
#[cfg(feature = "bench")]
mod trace {
    /// Mirrors the real module's [`Event`](super::trace::Event) variants; carried only so the
    /// call sites name the same paths in both configurations.
    #[derive(Clone, Copy)]
    pub enum Event {
        SwitchTo,
        BlockSelf,
        Wake,
        WakeDeferred,
        WakeCompleted,
        PlaceRemote,
        StealServe,
        InboxDrain,
        WakeRefused,
        Served,
    }

    #[inline]
    pub fn record(_kind: Event, _tid: u64, _aux: u8) {}

    pub fn dump(_cpu: usize) {}
}

/// **The boot tour's last-reached stage**, printed in every [`dump_threads`] header (first-silicon
/// diagnostics, 2026-08-15; name provisional).
///
/// Boots 7 through 9 on the VisionFive 2 were called a hang inside the initrd demo because the
/// tour's serial lines after "init : measured, built, started" never showed at the bench, while
/// the thread dumps kept printing. The dumps' own rows later proved the tour had in fact advanced
/// through the UART-driver step (notes/visionfive2.md, fifth stop), so "which step did the boot
/// thread reach" must not be inferable only from serial lines that can go missing: a breadcrumb
/// the periodic dump repeats survives a lossy or misread log. The riscv tour bumps this at each
/// step; the number-to-step table lives beside the tour in main.rs.
static BOOT_STAGE: AtomicU32 = AtomicU32::new(0);

/// Record that the boot tour reached `stage`. Monotonic by convention, not enforced.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // the riscv tour is the caller today
pub fn note_boot_stage(stage: u32) {
    BOOT_STAGE.store(stage, Ordering::Relaxed);
}

/// Read the tour stage back. The hang watcher uses it to fall silent once the tour has
/// finished (stage 10): boot 13 completed healthily and still printed five dumps of a
/// quiescent machine, which reads as a hang to anyone who has not memorized the watcher.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // the riscv tour is the caller today
pub fn boot_stage() -> u32 {
    BOOT_STAGE.load(Ordering::Relaxed)
}

/// **A corruption tripwire over the scheduler's registries** (first-silicon diagnostics,
/// 2026-08-15; module name provisional). Armed around the initrd demo on the board tour, it
/// re-reads the watched ranges on the timer tick and prints every byte that changed since the
/// last look: address, tick, before and after. A legal change (a spawn writing a fresh `TcbPtr`, a
/// reap bumping a generation) prints as a recognizable delta at a table offset; a stray write
/// prints as bytes nothing in the choreography explains. The instrument does not judge, it shows,
/// because boots 7 through 9 proved the judging is the part that goes wrong.
///
/// What it watches: the thread table and the endpoint registry (the `slots` arrays and their
/// generations), which are quiescent between spawns and creates. The per-cpu blocks are
/// deliberately NOT watched: `ticks`, `runnable`, `current` and the queues churn on every
/// scheduler entry by design, so a checksum there measures the scheduler working, not corruption.
///
/// Cost and honesty:
///
/// - Unarmed (every build's steady state), the tick-path cost is one relaxed load.
/// - Armed, the owner core re-reads ~13 KiB per tick. Diagnostic-build money, spent only inside
///   the demo window on the board tour.
/// - Compiled out of `--features bench` builds exactly as the event rings are, so the tripwire
///   benchmarks never measure the instrument.
/// - The watched memory is concurrently mutated under SCHED while the check reads it lock-free;
///   a torn read of an in-flight legal write can print as a divergence. That is a false alarm
///   only in the sense that the mutation was legal; the printed delta says so itself.
/// - The instrument's own state (the watch table, the shadow) is serialized by
///   `canary_gate::Gate`, a one-word state machine with loom-searched guards. Its first protocol
///   was two hand-written flags here, and that pair raced (2026-08-15): a `check` that lost the
///   single-flight slot returned silently having checked nothing, which the kernel test read as
///   a missed corruption (the thead-c906 flake, notes/cpu-models.md BUGS), and a re-arm could
///   rewrite the plan under a checker that had seen `ARMED` but not yet won the slot. See
///   `crates/canary_gate` for both holes and the harnesses that falsify the old spelling.
#[cfg(not(feature = "bench"))]
mod canary {
    use core::sync::atomic::{AtomicU64, Ordering};

    use canary_gate::Gate;

    /// One watched range and where its shadow copy lives.
    #[derive(Clone, Copy)]
    struct Watch {
        base: usize,
        len: usize,
        shadow_off: usize,
    }

    const MAX_RANGES: usize = 4;
    /// Both registries today total ~13 KiB (128 `Option<TcbPtr>` at 16 bytes, 512 `Option<u64>`
    /// at 16 bytes, plus generations); 24 KiB leaves room for growth and the test's scratch.
    const SHADOW_BYTES: usize = 24 * 1024;
    /// Print at most this many diverging bytes over an armed window, so a large legal rewrite
    /// cannot flood the serial log that the dump itself needs.
    const PRINT_CAP: u64 = 48;

    /// Interior-mutable statics whose one owner at a time is a live `canary_gate` guard:
    /// `arm` writes them holding an `ArmGuard`, `check` reads and writes them holding a
    /// `CheckGuard`, and the gate admits at most one guard of either kind (the exclusion is
    /// loom-checked in `crates/canary_gate`, where the previous hand-written spelling of this
    /// serialization is also falsified).
    struct Racy<T>(core::cell::UnsafeCell<T>);
    // SAFETY: access is serialized by the gate's guards; see the struct comment.
    unsafe impl<T> Sync for Racy<T> {}

    static GATE: Gate = Gate::new();
    static DIVERGED: AtomicU64 = AtomicU64::new(0);
    static PRINTED: AtomicU64 = AtomicU64::new(0);
    static WATCHES: Racy<([Watch; MAX_RANGES], usize)> = Racy(core::cell::UnsafeCell::new((
        [Watch {
            base: 0,
            len: 0,
            shadow_off: 0,
        }; MAX_RANGES],
        0,
    )));
    static SHADOW: Racy<[u8; SHADOW_BYTES]> = Racy(core::cell::UnsafeCell::new([0; SHADOW_BYTES]));

    /// Arm over `ranges`, snapshotting their current bytes. Caller guarantees the ranges stay
    /// readable while armed (ours are `'static` kernel tables). Serializes itself: any in-flight
    /// check finishes before the plan is touched, and the new plan is published whole. Spins, so
    /// call from thread context (both callers do); the tick side never spins, so a tick landing
    /// mid-arm skips rather than deadlocks.
    pub fn arm(ranges: &[(usize, usize)]) {
        let guard = GATE.arm();
        DIVERGED.store(0, Ordering::Relaxed);
        PRINTED.store(0, Ordering::Relaxed);
        // SAFETY: the ArmGuard is exclusive ownership of these statics (the gate's contract,
        // loom-checked in crates/canary_gate); no check pass can start until it drops.
        let (watches, count) = unsafe { &mut *WATCHES.0.get() };
        // SAFETY: as above.
        let shadow = unsafe { &mut *SHADOW.0.get() };
        let mut off = 0usize;
        let mut n = 0usize;
        for &(base, len) in ranges.iter().take(MAX_RANGES) {
            assert!(off + len <= SHADOW_BYTES, "canary shadow too small");
            for i in 0..len {
                // SAFETY: caller promises base..base+len readable; volatile because another
                // core may be mid-write under SCHED (a torn snapshot only costs a printed delta).
                shadow[off + i] = unsafe { core::ptr::read_volatile((base + i) as *const u8) };
            }
            watches[n] = Watch {
                base,
                len,
                shadow_off: off,
            };
            off += len;
            n += 1;
        }
        *count = n;
        drop(guard); // the release store that publishes the plan
    }

    /// Disarm, and QUIESCE: when this returns, no check pass is mid-flight and none can start,
    /// so the caller may repurpose the watched memory. (Today's watched ranges are `'static`, so
    /// the quiescence buys certainty rather than papers over a lifetime; it costs a bounded spin
    /// while at most one in-flight pass finishes.)
    pub fn disarm() {
        GATE.disarm();
    }

    /// How many bytes have diverged since arming. The test hook, and a bench-note number.
    #[cfg_attr(not(test), allow(dead_code))] // release builds read it off the serial print
    pub fn divergences() -> u64 {
        DIVERGED.load(Ordering::Relaxed)
    }

    /// Re-read every watched byte against the shadow; print and absorb what changed. Called from
    /// the timer tick (IRQ context, interrupts masked) and from the test. Single-flight, so a
    /// slow check on one core and the next tick on another cannot interleave shadow updates.
    ///
    /// Split in two so the disarmed tick costs no stack. A debug-build prologue reserves the
    /// WHOLE frame before the first instruction of the body runs, early return included, and the
    /// one-piece spelling of this function carried a 592-byte frame onto the interrupted thread's
    /// stack on every tick of every thread, disarmed or not. That frame was one of the middle
    /// frames of the 2026-08-15 thread-stack overflow (thread.rs, `STACK_PAGES`); the tick path
    /// pays ~16 bytes now, and only an armed pass pays for the real work.
    ///
    /// Returns whether a full pass RAN. `false` means disarmed, mid-arm, or another core's pass
    /// holds the slot. The tick ignores the answer (a sampling instrument may skip a beat); a
    /// caller that must observe a completed pass loops until it gets `true`. Returning the
    /// refusal instead of swallowing it is the fix for the c906 flake: the test's decisive check
    /// used to lose the slot to a tick's pass that had read the byte before the flip, and its
    /// silent no-op read as a missed corruption.
    pub fn check() -> bool {
        if !GATE.armed_hint() {
            return false; // one relaxed load: every unarmed tick's whole cost
        }
        check_armed()
    }

    /// The armed pass, outlined. `#[inline(never)]` is what keeps [`check`]'s frame from
    /// swallowing this one's; without it the split is cosmetic.
    #[inline(never)]
    fn check_armed() -> bool {
        let Some(guard) = GATE.try_check() else {
            return false;
        };
        // SAFETY: the CheckGuard is exclusive ownership of these statics (the gate's contract,
        // loom-checked in crates/canary_gate), and taking it saw the arm guard's release, so the
        // plan is whole, never torn.
        let (watches, count) = unsafe { &*WATCHES.0.get() };
        // SAFETY: as above, and mutation is confined to the guard's lifetime.
        let shadow = unsafe { &mut *SHADOW.0.get() };
        let tick = crate::arch::timer::ticks();
        for w in watches.iter().take(*count) {
            for i in 0..w.len {
                // SAFETY: the armed range is 'static kernel memory (arm's contract); volatile
                // because SCHED-holding writers mutate it concurrently and honestly.
                let now = unsafe { core::ptr::read_volatile((w.base + i) as *const u8) };
                let was = shadow[w.shadow_off + i];
                if now != was {
                    DIVERGED.fetch_add(1, Ordering::Relaxed);
                    if PRINTED.fetch_add(1, Ordering::Relaxed) < PRINT_CAP {
                        crate::println!(
                            "    canary: tick={tick} addr={:#x} (range {:#x}+{:#x} off {:#x}) {was:#04x} -> {now:#04x}",
                            w.base + i,
                            w.base,
                            w.len,
                            i,
                        );
                    }
                    shadow[w.shadow_off + i] = now;
                }
            }
        }
        drop(guard); // the release store the next pass's acquire pairs with
        true
    }
}

/// The bench build's no-op twin of [`canary`], so the call sites carry no `cfg` of their own.
#[cfg(feature = "bench")]
mod canary {
    pub fn arm(_ranges: &[(usize, usize)]) {}
    pub fn disarm() {}
    pub fn check() -> bool {
        false
    }
}

/// Arm the [`canary`] over the thread table and the endpoint registry, snapshotting under `SCHED`
/// so the baseline is a consistent cut. The riscv initrd demo arms before parking in its recv and
/// disarms when the recv returns; see notes/visionfive2.md (fifth stop) for what boot 11 does
/// with the output.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // the riscv tour is the caller today
pub fn canary_arm_registries() {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return;
    };
    let threads = (
        core::ptr::from_ref(&sched.threads.table) as usize,
        size_of_val(&sched.threads.table),
    );
    let endpoints = (
        core::ptr::from_ref(&sched.endpoints) as usize,
        size_of_val(&sched.endpoints),
    );
    canary::arm(&[threads, endpoints]);
}

/// Disarm the [`canary`]. The demo window's other bracket. Quiesces: when it returns, no check
/// pass is in flight on any core.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // the riscv tour is the caller today
pub fn canary_disarm() {
    canary::disarm();
}

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
        kernel_ep_chunks: 0,
    });
    drop(sched); // release before spawning, which takes the lock itself

    // This core (core 0) is running the boot thread.
    set_current_tid(boot_tid);

    // (The run queue and inbox used to have capacity reserved here, so a push from the timer IRQ
    // could never allocate. The queues are intrusive now: a push is two pointer writes and
    // *cannot* allocate, so there is nothing to reserve. §9's rule became structural.)

    // The idle thread. Its entire body is "wait for an interrupt, then let the scheduler look for
    // work." It is deliberately kept OUT of the ready queue (see cpu::PerCpu::idle): the scheduler picks it
    // only when nothing else is runnable, so it never steals a turn from real work.
    let idle = Thread::spawn(|| run_idle()).expect("could not create the idle thread");

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
///
/// aarch64 only in practice: RISC-V's twin path (`arch/riscv64/exceptions.rs`) recognises the IPI
/// from the SBI software-interrupt cause rather than from an interrupt id, so it needs no constant.
#[cfg_attr(target_arch = "riscv64", allow(dead_code))]
pub const RESCHED_SGI: u32 = 0;

/// Drain this core's migration inbox into its run queue, and request a reschedule.
///
/// Called from the reschedule-SGI handler: another core pushed one or more threads into our inbox
/// and poked us. We move them onto our own (single-owner) run queue and set `need_resched`, so the
/// handler's tail runs `schedule()` and picks them up. IRQ context, so interrupts are masked, which
/// is what `with_runq` needs; we hold nothing else, so taking the inbox is rank-safe (§11).
pub fn drain_inbox() {
    let mut moved = 0u64;
    let mut inbox = cpu::current().inbox.lock();
    while let Some(thread) = inbox.pop_front() {
        // SAFETY: the sender pushed a live Ready thread; popping it here is the only removal
        // path, so it is on no other queue. Nothing is dereferenced: the handoff is pure
        // pointer movement, which is why this needs no scheduler lock.
        cpu::current().with_runq(|q| unsafe { q.push_back(thread) });
        moved += 1;
    }
    // The inbox is empty now; mirror that under the lock (DECISIONS §28). The threads moved into the
    // run queue, whose own mirror `with_runq` just updated, so the total load is unchanged.
    cpu::current().note_inbox_len(inbox.len());
    // And count them: this is the only place a thread crosses from a remote core's hands into this
    // core's queue, which makes it the one honest observation point for "the placement arrived".
    cpu::current().note_adopted(moved);
    drop(inbox);
    if moved > 0 {
        trace::record(trace::Event::InboxDrain, moved, 0);
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
/// The queue-able pointer to a live thread.
///
/// Returns [`core::ptr::NonNull`] rather than `*mut`, and that is not decoration: the pointer is derived from a
/// `&mut Thread` handed out by the thread table, so **non-nullness is a fact of construction rather
/// than a promise the caller keeps**. Saying so in the type removes null from the intrusive queue's
/// safety contract entirely, which is one of the two things CodeQL's `rust/access-invalid-pointer`
/// alerts were pointing at (milestone 45). What the type still cannot express is that the pointee
/// outlives its time on the queue; that is the caller's rule 2, and no type available here can carry
/// it for an intrusive structure.
fn tcb_ptr(sched: &mut Scheduler, tid: Tid) -> core::ptr::NonNull<Thread> {
    core::ptr::NonNull::from(
        sched
            .threads
            .get_mut(tid)
            .expect("tcb_ptr of a dead thread"),
    )
}

/// Put an already-created thread onto core `target`'s run queue. Caller holds `SCHED`.
///
/// Local: straight onto our own queue (SCHED masks interrupts, which `with_runq` needs). Remote:
/// into the target's inbox, and the SGI (sent after SCHED is released, by the caller) makes it
/// drain. The inbox push under SCHED is rank-safe (INBOX < SCHED), and the inbox's own lock supplies
/// the release/acquire that orders our thread-table insert before the target's drain (§11).
fn place_on(target: usize, thread: core::ptr::NonNull<Thread>) {
    // A REMOTE parked cpu's inbox is drained by nothing, so placing there is a thread nothing
    // will ever run: the VisionFive 2 first-silicon hang (notes/visionfive2.md, third stop). The
    // online-set sweep removed every count-as-index chooser, and this is the audit lane's
    // tripwire for the next one: loud in debug/test builds, where every merge boots it. The
    // release board build compiles it out and keeps [`dump_threads`]'s parked-inbox line as the
    // field diagnostic.
    //
    // Placing onto ONESELF is exempt, and the exemption is load-bearing, found by this assertion
    // firing on the suite's own boot: a secondary's bring-up probe (`smp::secondary_main` step 6)
    // spawns onto its own core one step before that core sets its online bit, and a local
    // placement goes straight onto the running core's own run queue, which that core drains by
    // definition. The hazard this guards is exactly the remote case.
    debug_assert!(
        target == cpu::id() || {
            let mask = crate::smp::online_harts_mask();
            mask & (1 << target) != 0
        },
        "placement onto parked cpu {target} (online mask {:#b}): nothing drains a parked core's \
         inbox, so this thread would never run",
        crate::smp::online_harts_mask(),
    );
    if target == cpu::id() {
        // SAFETY: `thread` is a live Ready thread (see tcb_ptr), on no other queue.
        cpu::current().with_runq(|q| unsafe { q.push_back(thread) });
    } else {
        // SAFETY: as above; the inbox mutex serializes access to the link.
        let mut inbox = cpu::inbox_of(target).lock();
        // SAFETY: the live Ready thread named in the comment above, on no other queue, and the inbox lock is held across the push.
        unsafe { inbox.push_back(thread) };
        // Mirror the target's inbox depth so it counts as load (DECISIONS §28); under the lock, so
        // the store is serialised with any concurrent drain.
        cpu::of(target).note_inbox_len(inbox.len());
        // SAFETY: reading the id of a live thread we still hold exclusively (see above).
        let tid = unsafe { (*thread.as_ptr()).id };
        trace::record(trace::Event::PlaceRemote, tid, target as u8);
    }
}

/// Spawn a thread and place it on a **specific** core (SMP step 3c).
///
/// The cross-core placement primitive. `spawn` puts work on the calling core; this puts it on
/// `target`, which is what lets the machine actually spread load. A remote target is handed the
/// thread through its inbox and then poked with the reschedule SGI. (Wiring `spawn` itself to
/// round-robin over `target` is the trivial next step, once the mechanism is proven.)
pub fn spawn_on<F: FnOnce() + Send + 'static>(target: usize, f: F) -> Option<Tid> {
    let remote = target != cpu::id();

    let id = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut()?;
        // **The Thread is built on its own TCB page, not carried there** (milestone 124). The old
        // shape called `Thread::spawn(f)` for a value and moved it through this closure, and every
        // instantiation of this generic function carried 3888 to 4592 bytes of frame as a result:
        // over the 4096-byte guard page, which is the size at which one frame can step past the
        // guard in a single move and corrupt the neighbouring stack with no fault at all.
        let id = sched.threads.insert_in_place(|tid, dst| {
            // SAFETY: `dst` is the fresh, exclusively-ours TCB page `insert_in_place` claimed; it
            // is aligned for `Thread` and holds no live one, so `write` drops nothing.
            unsafe { Thread::spawn_into(f, tid, dst) }
        })?;
        place_on(target, tcb_ptr(sched, id));
        id
    }; // SCHED released here, before the SGI, so the target's schedule() can take it

    if remote {
        // Poke the target: its handler drains the inbox we just pushed to and reschedules.
        crate::arch::irq::send_reschedule(target);
    }
    Some(id)
}

pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> Option<Tid> {
    // Placement is the power of two choices (DECISIONS §28): the new thread lands on the lighter of
    // two randomly sampled cores, not always on the spawner's, so work spreads instead of piling on
    // one core beside idle ones (the FS-server starvation lesson). `spawn_on` carries the thread to
    // the chosen core over the §11 inbox/SGI path.
    spawn_on(pick_spawn_target(), f)
}

/// **Power of two choices: which core should a new thread run on** (DECISIONS §28.1). Sample two
/// random cores' runnable counters (relaxed, possibly stale, which §28 accepts) and return the
/// lighter. Near-optimal balancing that reads at most two remote counters no matter how many cores
/// there are, where a full least-loaded scan would contend on every counter and age badly. On a
/// single online core it is a no-op; the two samples may coincide, degrading to one choice harmlessly.
fn pick_spawn_target() -> usize {
    let n = crate::smp::online_count();
    if n <= 1 {
        return cpu::id();
    }
    // The k-th ONLINE cpu, not index k: the online set is not contiguous from zero on real boards
    // (first-silicon bench, 2026-08-14: {1,2,3} online, and modulo-count placed init into parked
    // slot 0's inbox forever). See smp::online_cpus.
    let a = crate::smp::nth_online(cpu::current().rng_next() as usize);
    let b = crate::smp::nth_online(cpu::current().rng_next() as usize);
    if cpu::of(a).runnable() <= cpu::of(b).runnable() {
        a
    } else {
        b
    }
}

/// **Serve a pending work-steal request** (DECISIONS §28.3), at a scheduler entry where interrupts
/// are masked (the reschedule-SGI handler). If an idle core asked this core for work, hand it one
/// thread from our run queue and poke it. We give from the *queue*, never the thread on the CPU, and
/// only if we have one to spare; an empty give leaves the requester to ask again next tick, the
/// bounded cost §28 accepts. Pull-based and lock-free between run queues: the only shared structure
/// touched is the requester's inbox.
pub fn serve_steal_request() {
    // Take the request, which also clears the slot so the next idle core can queue a fresh one. The
    // acquire/release pairing and the read-and-clear live in `steal_request`, where loom checks
    // them (notes/interleaving.md); this function spends its own code on the hand-off.
    let Some(requester) = cpu::current().steal_request.take() else {
        return;
    };
    let requester = requester as usize;
    // One thread off our own queue, if any. `with_runq` keeps the runnable mirror exact.
    let thread = cpu::current().with_runq(|q| q.pop_front());
    if let Some(t) = thread {
        // SAFETY: reading the id of the thread we just popped and hold exclusively.
        let tid = unsafe { (*t.as_ptr()).id };
        trace::record(trace::Event::StealServe, tid, requester as u8);
        // SAFETY: a live Ready thread we just popped, on no other queue; the inbox mutex serialises
        // the handoff and orders our pop before the requester's drain (the `place_on` discipline).
        let mut inbox = cpu::inbox_of(requester).lock();
        // SAFETY: the live Ready thread named in the comment above, on no other queue, and the inbox lock is held across the push.
        unsafe { inbox.push_back(t) };
        cpu::of(requester).note_inbox_len(inbox.len());
        crate::arch::irq::send_reschedule(requester);
    }
}

/// **An idle core asks a loaded core for work** (DECISIONS §28.3), from the idle loop. Pick the
/// most-loaded other core and, if it has a queued thread to spare, request one over its steal slot
/// and poke it with the reschedule SGI; the victim serves it at its next scheduler entry, the stolen
/// thread lands in our inbox, and that SGI's drain runs it. One outstanding request per victim
/// (`steal_request::Slot::claim` is a compare-exchange from empty), so a crowd of idle cores
/// collapses to one steal per victim per round.
fn try_initiate_steal() {
    // Do not steal if we have work of our own arriving: our run queue is empty (that is why the idle
    // thread is running), but the inbox may hold threads a remote just handed us that our own next
    // scheduler entry will drain. `runnable` counts those, so this guard defers to our own work.
    if cpu::current().runnable() > 0 {
        return;
    }
    let me = cpu::id();
    let mut victim = None;
    let mut best = 0usize;
    for c in crate::smp::online_cpus() {
        if c != me {
            // Steal only a run-queue backlog, never a victim's inbox in transit (see `runq_len`).
            let r = cpu::of(c).runq_len();
            if r > best {
                best = r;
                victim = Some(c);
            }
        }
    }
    if let Some(v) = victim
        && cpu::of(v).steal_request.claim(me as u32)
    {
        crate::arch::irq::send_reschedule(v);
    }
}

/// **The idle thread's body**, shared by the boot core's spawned idle thread and every secondary's
/// adopted idle context. Each pass: try to steal work from a loaded core (§28), park in `wfi` until
/// an interrupt (the stolen thread's SGI, a spawn's SGI, or the tick), then yield so the scheduler
/// runs whatever arrived. Never returns; it is the fallback the scheduler picks only when this
/// core's run queue is empty.
///
/// Before an idle thread existed, a moment where every thread was blocked waiting for I/O was a
/// kernel panic. It is never in the ready queue, so it never competes with real work, and it is
/// per-CPU as of §11 step 3b, so an idle core parks in its own `wfi`.
pub fn run_idle() -> ! {
    loop {
        try_initiate_steal();
        crate::arch::wait_for_interrupt();
        yield_now();
    }
}

/// Spawn a thread against a **quota**: at most `budget` of these may be alive at once.
///
/// Reserving a slot is an atomic decrement; the slot lives inside the spawned `Thread` as a
/// [`QuotaToken`] and comes back when the thread is reaped. Returns `None` if the budget is
/// exhausted (too many children already alive) OR the kernel is out of memory: the caller cannot
/// tell the two apart, and does not need to: either way it could not spawn, and it must degrade
/// rather than panic. This is the bound that stops a spawn flood or a leaked-thread pile-up from
/// exhausting kernel memory. See notes/quotas.md and notes/security.md.
///
/// # It has no caller today, and that is worth saying plainly
///
/// Its one caller was the kernel-wired `shell_service`'s spawn service, which DECISIONS §28 retired
/// and milestone 41 deleted. Nothing in the kernel spawns against a quota now, because **the bound
/// moved**: a userspace process spawns out of its own untyped budget (§10, §16), so the budget *is*
/// the quota and it is enforced by retyping rather than by a counter. This function is the bound for
/// *kernel* threads, and no kernel thread is currently spawned in a loop by anything untrusted.
///
/// Kept rather than deleted because removing a documented safety mechanism is a design decision, not
/// dead-code triage, and notes/quotas.md and notes/security.md both describe it. Allowed
/// unconditionally and on purpose (DECISIONS §38, disposition 3): there is no configuration in which
/// something calls it, and pretending otherwise with a `cfg` predicate would be the dishonest option.
#[allow(dead_code)]
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

    let Some(mut thread) = Thread::spawn(f) else {
        // Out of kernel memory. Give the reserved slot back, since no thread will hold it.
        budget.fetch_add(1, Ordering::Relaxed);
        return None;
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
    schedule();
}

/// The current thread exited cleanly (`SYS_EXIT`). Never returns.
pub fn exit() -> ! {
    depart(abi::fault::EVENT_EXIT, 0, 0)
}

/// The current thread faulted (a bad access, an illegal instruction) and is being killed. Never
/// returns. `pc` is the faulting instruction and `addr` the faulting address (0 if the fault class
/// carries none). The arch fault handlers call this from the faulting thread's kernel stack, the
/// same context `exit` runs in, so the departure below is identical bar the event code and words.
pub fn fault(pc: u64, addr: u64) -> ! {
    depart(abi::fault::EVENT_FAULT, pc, addr)
}

/// **A thread's last act: report its death, then leave the CPU forever** (milestone 22, §26).
///
/// Two outcomes, decided by whether the thread was spawned with a supervision endpoint (its
/// `fault_ep`, set at `START` from the reserved fault slot):
///
///   - **Unsupervised** (`fault_ep == None`): today's behaviour exactly. Mark `Finished` and
///     `schedule()` away; the next thread's `finish_switch` reaps it once it is off this stack.
///   - **Supervised**: build the five-word §26 message, retain it on the corpse for postmortem,
///     deliver it to the supervision endpoint (waking a waiting supervisor, or parking the corpse
///     on the endpoint so the message is not lost if none is waiting), and mark the thread `Dead`.
///     A `Dead` corpse is never reaped by `finish_switch`; it persists, registers and address
///     space intact, until the supervisor reaps it with §16 revocation.
///
/// Either way we are still running on the thread's own kernel stack, so we cannot free it here; we
/// only mark state and `schedule()`, exactly as `exit` always has.
fn depart(event: u64, pc: u64, addr: u64) -> ! {
    {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("depart before sched::init");
        let current = current_tid();

        let fault_ep = sched.threads.get(current).and_then(|t| t.fault_ep);

        match fault_ep {
            None => {
                if let Some(t) = sched.threads.get_mut(current) {
                    t.handshake.state = State::Finished;
                }
            }
            Some(ep) => {
                let msg = [event, current, pc, addr, 0];
                if let Some(t) = sched.threads.get_mut(current) {
                    // Retain the message on the corpse (postmortem) and stage it as the mailbox a
                    // parked-corpse delivery will hand the supervisor. Dead: never runs again.
                    t.fault_msg = Some(msg);
                    t.mailbox = msg;
                    t.handshake.state = State::Dead;
                }
                deliver_death(sched, current, ep, msg);
            }
        }
        // Not requeued and not removed: we are still on this stack. The switch below leaves it,
        // and for a Finished thread the next thread reaps it; a Dead one waits for the supervisor.
    }

    schedule();
    unreachable!("a departed thread was scheduled again");
}

/// Deliver a corpse's five-word death message to its supervision endpoint. Caller holds `SCHED`
/// and has already marked the corpse `Dead` with `msg` in its mailbox.
///
/// This is the ordinary synchronous-send rendezvous (`Endpoint::send`), reused: if a supervisor is
/// blocked in `RECV`, hand it the message and wake it; if none is, the corpse joins the endpoint's
/// sender queue with the message in its mailbox, so the notification waits there rather than being
/// lost (the same guarantee an ordinary blocked sender gets, and the reason a data-carrying death
/// uses the sender queue rather than the data-less IRQ signal count). The corpse is never woken:
/// `ipc_recv` recognises a `Dead` sender and leaves it dead after taking its message, the same way
/// it leaves a `CALL` caller blocked. If the endpoint itself is gone (the supervisor was torn down
/// first), the message is simply dropped, like an interrupt with no live endpoint.
fn deliver_death(sched: &mut Scheduler, corpse: Tid, ep: EpId, msg: [u64; 5]) {
    let me = tcb_ptr(sched, corpse);
    let Some(endpoint) = endpoint_of(sched, ep) else {
        return;
    };
    // SAFETY: `me` is the corpse, live in the table (Dead, not yet reaped) and on no queue; if it
    // joins the sender queue below it stays put, since nothing wakes or reaps a Dead thread until
    // the supervisor drains it and revokes. Same pointer discipline as ipc_send.
    match unsafe { endpoint.send(me) } {
        ipc::Send::Rendezvous(receiver) => {
            // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
            let receiver = unsafe { (*receiver.as_ptr()).id };
            let r = sched.threads.get_mut(receiver).unwrap();
            r.mailbox = msg;
            r.handshake.serve(); // delivered: this wake passes the boot-8 gate
            trace::record(trace::Event::Served, receiver, 8);
            wake(sched, receiver);
        }
        ipc::Send::Blocked => {
            // The corpse is parked on the sender queue now, its mailbox already holding `msg`.
            // Record the parking so a dump shows where the death message waits.
            if let Some(t) = sched.threads.get_mut(corpse) {
                t.handshake.wait_on = Some((ep, WaitRole::Sender));
            }
        }
    }
}

/// Called from the timer IRQ. **Records** that a switch is wanted; does not switch.
pub fn on_tick() {
    cpu::current().need_resched.store(true, Ordering::Relaxed);
    // The corruption tripwire, when armed (the board tour's initrd-demo window). One relaxed
    // load when it is not, which is every other tick everywhere. IRQ context is safe for its
    // println: the console's IrqSafeMutex masks interrupts while held, so the interrupted
    // context on this core cannot be mid-print (the irq_notify argument, one lock over).
    // The answer is ignored on purpose: a sampling instrument may skip a beat when another
    // core's pass (or an arm) holds the gate, and the tick must never spin in IRQ context.
    let _ = canary::check();
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
    // **Not from an interrupt stack** (milestone 124). A switch parks the running `sp` in the
    // outgoing thread's `Context` and resumes it there, arbitrarily later; a per-core interrupt
    // stack cannot promise those bytes will still be the thread's, so a thread parked there would
    // resume on whatever the next interrupt on that core had written. The interrupt path defers its
    // switch to `preempt_if_needed`, which its dispatcher calls one frame outside the trampoline,
    // back on the interrupted thread's own stack.
    //
    // Debug-only because it costs a `sp` read and a scan of `MAX_CPUS` spans on the hottest path in
    // the kernel, and because `script/stack-depth-check` proves the same property statically in CI,
    // on both architectures, by showing no context switch is reachable from the interrupt-stack
    // entry point. This is the runtime half of that pair, for the edges a call graph cannot see.
    debug_assert!(
        !crate::interrupt_stack::contains(crate::arch::current_sp()),
        "schedule() called on an interrupt stack: the outgoing thread would be parked on memory \
         that belongs to a core (see kernel/src/interrupt_stack.rs)",
    );

    // Rule 2: no interrupts across the decision *or* the switch. Between "I chose a thread" and
    // "I am running it" there must be no window for the timer to choose again.
    //
    // The saved state is a local, on **this thread's stack**, which is exactly what makes it
    // correct: when someone eventually switches back to us, `switch_to` returns here, and this
    // frame (with the right `was_enabled` in it) is still sitting where we left it.
    let was_enabled = crate::arch::interrupts::disable();

    // A labeled block, so every exit path leaves through the SAME point: the guard drops at the
    // block's end and interrupts are restored ONCE, AFTER it. The earlier version called
    // `interrupts::restore(was_enabled)` and `return` from *inside* this block, which re-enabled
    // interrupts while still holding the scheduler lock: a one-instruction window in which a
    // timer could fire, re-enter `schedule()`, and try to take a lock we already held. It was
    // intermittent and it was real; see the lock-rank violation it produced.
    let switch = 'decide: {
        let mut guard = SCHED.lock();
        let Some(sched) = guard.as_mut() else {
            break 'decide None;
        };

        let current = current_tid();

        // **Forcible teardown, before anything else** (DECISIONS §16 amendment; §28 made it
        // load-bearing). A thread `DESTROY` marked killed must never run again. Convert it to a
        // `Finished` corpse here, at the top of the decision, so *every* path below reaps it: not
        // only the switch path, but the "nothing else to run, keep current" path a runaway alone on
        // its core takes. Before §28's scattering, a killed runaway shared a core with other work, so
        // a switch always happened and the old requeue-time check sufficed; once a runaway can be the
        // only thread on its core, that check was unreachable and the runaway spun forever.
        if let Some(t) = sched.threads.get_mut(current)
            && t.killed
            && t.handshake.state == State::Running
        {
            t.handshake.state = State::Finished;
        }
        let state = sched.threads.get(current).map(|t| t.handshake.state);

        // **Only a still-Running thread goes back on the ready queue.** A thread that reached
        // here after marking itself `Blocked` (it is waiting for IPC), `Finished`, or `Dead` (a
        // supervised corpse) must not be rescheduled, and this one line is what makes blocking work:
        // `schedule()` can be called from the timer IRQ *while* a thread is mid-way through blocking
        // itself, and it must not undo that by helpfully requeueing it.
        let runnable = state == Some(State::Running);

        let idle_tid = cpu::current().idle.load(Ordering::Relaxed);

        let next = match cpu::current().with_runq(|q| q.pop_front()) {
            // SAFETY: only live Ready threads are ever queued; reading the id is the last thing
            // that happens before the pointer is dropped in favor of the (validated) Tid.
            Some(t) => unsafe { (*t.as_ptr()).id },
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

        // **The running thread must never come off its own run queue** (boot 8's downstream
        // catastrophe, guarded here on its own merits). The pop above precedes the requeue below,
        // so a legal schedule can never hand back `current`; if it ever does, something queued a
        // thread that was still running, and switching into it would restore `t.context`, a
        // pointer to a frame this very thread has already resumed and consumed: execution
        // time-travels to its previous switch-out point on a reused stack and spins there
        // forever, off every instrument. Debug builds fail loudly; the board build heals by
        // keeping the thread running, which is the only state that is still coherent.
        if next == current {
            if cfg!(debug_assertions) {
                panic!("schedule() popped its own current thread from the run queue");
            }
            sched.threads.get_mut(current).unwrap().handshake.state = State::Running;
            break 'decide None;
        }

        // Requeue the outgoing thread if it can still run, but never the idle thread, which lives
        // outside the ready queue. A killed thread is already `Finished` (handled at the top), so it
        // is not runnable here and is reaped by `finish_switch` after the switch, no queue surgery.
        if runnable && current != idle_tid {
            // `preempt` marks Ready and deliberately leaves `on_cpu` set: the thread is in a
            // queue AND still standing on this core until finish_switch runs, which is the one
            // legal overlap and the reason only this core may pop its own queue (the extracted
            // protocol's steal-vs-switch-out rule; see crates/wake_handshake).
            sched.threads.get_mut(current).unwrap().handshake.preempt();
            let ptr = tcb_ptr(sched, current);
            // SAFETY: just marked Ready, coming off the CPU, on no queue. Round robin: the back.
            cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
        }

        // Running, with on_cpu set until ITS successor's finish_switch, one switch from now.
        sched.threads.get_mut(next).unwrap().handshake.switch_in();
        set_current_tid(next);
        trace::record(trace::Event::SwitchTo, next, 0);

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
        // right, which is every switch between two kernel threads.)
        //
        // SAFETY: `next_root` is `reserved_root()`, or the composed value of the `AddressSpace`
        // owned by thread `next`, which the block above popped off this core's run queue and marked
        // `Running` with `on_cpu` set before releasing `SCHED`. No other core can pick it up in that
        // state and nothing reaps a thread that is on a CPU, so the root is still live here even
        // though the lock is not held. The lock is released on purpose (rule 1, above), which is
        // exactly why this obligation cannot be a borrow and has to be a sentence.
        unsafe { crate::arch::mmu::switch_user_root(next_root) };

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
    // The predecessor's context is saved now (we are running, so switch_to completed). What that
    // makes legal is `wake_handshake::Handshake::finish_switch`'s verdict: reap a Finished
    // predecessor, complete a wake that was deferred mid-switch-out, or simply clear `on_cpu` so
    // other cores may run it. The transition is the crate's (loom searches it; see
    // notes/interleaving.md); the reap and the queue push are ours.
    match t.handshake.finish_switch() {
        SwitchOutVerdict::Reap => {
            // Hoist the address space out BEFORE the in-place drop, to be torn down after the lock
            // is released: its teardown is untyped::destroy (milestone 14 phase B.4), whose §13
            // revocation sweep takes SCHED itself to delete stray Frame capabilities. Dropping it
            // here would deadlock on our own lock. The rest of the Thread (stack, quota) still
            // drops under SCHED, exactly as before.
            let space = t.space.take();
            sched.threads.remove(prev);
            drop(guard);
            drop(space);
        }
        SwitchOutVerdict::WakeCompleted => {
            trace::record(trace::Event::WakeCompleted, prev, 0);
            let ptr = tcb_ptr(sched, prev);
            // SAFETY: live, just made Ready, on no queue (a deferred wake was deferred precisely
            // because the waker did NOT queue it). IRQs are still masked on both callers' paths.
            cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
        }
        SwitchOutVerdict::Cleared => {}
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
    // A device-IRQ wake is LOAD-AWARE (DECISIONS §28.2), unlike a rendezvous wake, which stays
    // local. If the woken driver lands on a *remote* core, `wake_load_aware` returns that core so we
    // can poke it after SCHED is released (the `place_on` discipline: push under the lock, SGI
    // after). The SGI send from IRQ context is a plain controller write, safe here.
    let remote = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");

        // `signal` wakes a waiting receiver or counts the signal; it never blocks or joins a queue. A
        // stale name (the endpoint an interrupt was bound to has been revoked) is simply dropped: an
        // interrupt with no live endpoint has nowhere to go, which is not an error.
        let Some(endpoint) = endpoint_of(sched, ep) else {
            return;
        };
        if let Some(waiter) = endpoint.signal() {
            // SAFETY: only live Blocked threads sit on wait queues; reading the id revalidates it
            // through the table for everything after.
            let waiter = unsafe { (*waiter.as_ptr()).id };
            let t = sched.threads.get_mut(waiter).unwrap();
            t.mailbox = [1, 0, 0, 0, 0];
            t.handshake.serve(); // the signal is the delivery (the boot-8 gate)
            trace::record(trace::Event::Served, waiter, 7);
            wake_load_aware(sched, waiter)
        } else {
            None
        }
    };
    if let Some(target) = remote {
        crate::arch::irq::send_reschedule(target);
    }
}

/// Why creating an endpoint failed. The two causes need telling apart because they call for opposite
/// responses: a full region means carve more memory and retry, a full registry means give up.
///
/// They used to be one `None`, which is also why the registry-full case leaked a page: the caller
/// could not know the page it had just spent was about to be thrown away.
enum EpFail {
    /// The region has no page left to retype. Nothing was spent.
    RegionFull,
    /// The registry is at [`MAX_ENDPOINTS`]. Checked *before* spending a page, so nothing was spent.
    RegistryFull,
}

/// Create an endpoint **in `region`'s memory** (milestone 19a): one page retyped and pinned, the
/// endpoint at its start, a fresh generational name in the registry. The shared engine of the
/// `RETYPE_OBJ` syscall and the kernel's own [`create_endpoint`].
fn try_create_endpoint_from(region: u64) -> Result<EpId, EpFail> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(EpFail::RegistryFull)?;

    // Checked BEFORE the retype, which is a fix and not just tidiness: this used to retype a page and
    // then discover the registry was full, spending the page for nothing. The old comment called that
    // "a process-local loss on its own budget", which is true and is still a leak a caller cannot see
    // or recover. Asking first costs a compare.
    if sched.endpoints.len() >= MAX_ENDPOINTS {
        return Err(EpFail::RegistryFull);
    }

    // Rank: UNTYPED (58) under SCHED (60) is a legal descent; the pin rides in the same lock
    // hold as the carve, so no destroy can race the page away (see retype_object_page).
    let phys = crate::untyped::retype_object_page(region).ok_or(EpFail::RegionFull)?;

    // The page arrives zeroed, and an all-zero Endpoint happens to be valid; write it explicitly
    // anyway, because "happens to be" is the kind of truth that stops being one silently.
    // SAFETY: fresh page, exclusively ours, direct-mapped.
    unsafe { (crate::arch::mmu::phys_to_virt(phys) as *mut Endpoint).write(Endpoint::new()) };

    // Cannot fail: capacity was checked above under this same lock hold.
    sched
        .endpoints
        .insert_with(|_| phys)
        .ok_or(EpFail::RegistryFull)
}

/// Create an endpoint in `region`'s memory. `None` when the region is out of budget or the registry
/// is full. The `RETYPE_OBJ` syscall's engine; userspace gets one flat failure because a process
/// cannot act on the difference (it holds one region and cannot enlarge the kernel's registry).
pub fn create_endpoint_from(region: u64) -> Option<EpId> {
    try_create_endpoint_from(region).ok()
}

/// Create an IPC endpoint on the kernel's own budget. Returns the name that goes inside an
/// `Object::Endpoint`. Chunks are carved lazily and **grown on demand**, so this does not depend on
/// anyone having guessed the suite's eventual size.
///
/// Panics only on a genuinely unrecoverable condition: the registry at [`MAX_ENDPOINTS`], the chunk
/// bound reached (which cannot happen before the registry fills, by construction), or no memory left
/// to carve from. Every caller is the kernel or a test wiring a service, so there is no user to
/// return an error to.
pub fn create_endpoint() -> EpId {
    loop {
        // Take, or lazily carve, the current chunk.
        let region = {
            let mut guard = SCHED.lock();
            let sched = guard.as_mut().expect("no scheduler");
            match sched.kernel_ep_region {
                Some(r) => r,
                None => {
                    assert!(
                        sched.kernel_ep_chunks < MAX_KERNEL_EP_CHUNKS,
                        "the kernel carved all {MAX_KERNEL_EP_CHUNKS} endpoint chunks; \
                         with {MAX_ENDPOINTS} registry slots this should be unreachable",
                    );
                    let r = crate::untyped::create(KERNEL_EP_CHUNK_PAGES)
                        .expect("no memory for a kernel endpoint chunk");
                    sched.kernel_ep_chunks += 1;
                    sched.kernel_ep_region = Some(r);
                    r
                }
            }
        };

        match try_create_endpoint_from(region) {
            Ok(ep) => return ep,
            // The chunk is spent. Drop it and let the next pass carve a fresh one; the loop runs at
            // most twice per call, because a fresh chunk always has a page. Clearing the handle is
            // what "forgotten deliberately" means in the field's doc comment: the pages stay pinned
            // and the endpoints already in them stay live.
            Err(EpFail::RegionFull) => {
                let mut guard = SCHED.lock();
                let sched = guard.as_mut().expect("no scheduler");
                // Only clear the handle we just failed on. Another core may already have replaced it.
                if sched.kernel_ep_region == Some(region) {
                    sched.kernel_ep_region = None;
                }
            }
            Err(EpFail::RegistryFull) => {
                panic!("out of endpoints: {MAX_ENDPOINTS} live at once, raise MAX_ENDPOINTS")
            }
        }
    }
}

/// **Which core should a device-IRQ wake place its driver on** (DECISIONS §28.2). The least-loaded
/// online core, with the current (IRQ-handling) core winning ties: only a *strictly* less-loaded
/// core displaces it. That is what makes this load-aware without thrashing. A driver that takes a
/// completion interrupt every request (the block server through a RedoxFS mount) wakes on the same
/// affinity core each time and, since that core is no more loaded than any other while it is the
/// only work, stays there. When a core does pile up (the `std_net` RX path landing beside real work),
/// a strictly-lighter core pulls the driver off it, so the pipeline stops re-concentrating. A full
/// scan is fine: device IRQs are not the spawn hot path and `MAX_CPUS` is small.
fn pick_wake_target() -> usize {
    let here = cpu::id();
    let mut best = here;
    let mut best_load = cpu::current().runnable();
    // The online SET, never `0..count` (first-silicon sweep, 2026-08-14): on the VisionFive 2 the
    // set is {1,2,3}, and the count-as-index loop would read parked slot 0's zeroed `runnable()`,
    // which wins every comparison, so every device-IRQ wake would land in a dead core's inbox. It
    // also never considered online cpu 3. See smp::online_cpus.
    for c in crate::smp::online_cpus() {
        if c == here {
            continue;
        }
        let load = cpu::of(c).runnable();
        if load < best_load {
            best_load = load;
            best = c;
        }
    }
    best
}

/// A **device-interrupt** wake (DECISIONS §28.2): load-aware, not local. Where [`wake`] queues a
/// rendezvous partner on the waker's own core (message in registers, cache warm), an interrupt
/// carries no such locality, and pinning the driver to the IRQ core re-concentrates the pipeline
/// (the `std_net` lesson). So place it on [`pick_wake_target`]'s choice. Returns `Some(target)` when
/// that is a *remote* core, so the caller sends the reschedule SGI after releasing SCHED; `None`
/// when it stayed local or the wake was parked. Caller holds the lock.
fn wake_load_aware(sched: &mut Scheduler, tid: Tid) -> Option<usize> {
    let t = sched.threads.get_mut(tid)?;
    // The whole decision (not-blocked, the boot-8 undelivered-wake gate, the switch-out deferral)
    // is `wake_handshake::Handshake::try_wake`, the extracted protocol loom searches on the host
    // (notes/interleaving.md). This function keeps what the crate cannot see: the trace ring, the
    // progress heartbeat, and §28.2's placement policy on the one verdict that queues.
    match t.handshake.try_wake() {
        WakeVerdict::NotBlocked => None,
        WakeVerdict::Refused => {
            trace::record(trace::Event::WakeRefused, tid, 0);
            None
        }
        WakeVerdict::Deferred => {
            // A device-IRQ wake is forward progress too (test builds only). A deferral in this
            // window is rare, and one non-load-aware completion in `finish_switch` is not worth
            // teaching that path a placement policy.
            #[cfg(test)]
            crate::testing::note_progress();
            trace::record(trace::Event::WakeDeferred, tid, 0);
            None
        }
        WakeVerdict::Queue => {
            #[cfg(test)]
            crate::testing::note_progress();
            let ptr = core::ptr::NonNull::from(t);
            trace::record(trace::Event::Wake, tid, 0);
            let target = pick_wake_target();
            if target == cpu::id() {
                // SAFETY: just Blocked -> Ready, on no queue; SCHED masks interrupts, which
                // with_runq needs.
                cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
                None
            } else {
                // Into the target's inbox (place_on keeps the inbox-len mirror under the inbox
                // lock). The SGI that drains it goes out after SCHED drops, in irq_notify.
                place_on(target, ptr);
                Some(target)
            }
        }
    }
}

/// Move a blocked thread back to the ready queue. Caller holds the lock.
///
/// The decision lives in `wake_handshake::Handshake::try_wake`, extracted so loom can search its
/// interleavings on the host (notes/interleaving.md); this function is the kernel-side half, the
/// queue push and the trace ring. The two rules the verdicts carry, kept here in one breath
/// because this is where a reader meets them: **the undelivered-wake gate** (boot 8: a wake whose
/// critical section delivered nothing has not dequeued the thread from its endpoint and has
/// nothing for its recv to return, so it is refused and recorded as `refuse:tid` on the ring), and
/// **the wake-before-switch-out deferral** (a thread still on its CPU has a stale saved context,
/// so the wake parks in `wake_pending` and its own core's `finish_switch` completes it once the
/// context is provably saved; found by a 2-in-10 flake, notes/intrusive-queues.md).
fn wake(sched: &mut Scheduler, tid: Tid) {
    if let Some(t) = sched.threads.get_mut(tid) {
        match t.handshake.try_wake() {
            WakeVerdict::NotBlocked => {}
            WakeVerdict::Refused => {
                trace::record(trace::Event::WakeRefused, tid, 0);
            }
            WakeVerdict::Deferred => {
                // A completed rendezvous is forward progress even when its queueing is deferred:
                // keep the hang watchdog's heartbeat alive so a slow-but-live IPC pipeline
                // (std_net) is not read as a deadlock (test builds only).
                #[cfg(test)]
                crate::testing::note_progress();
                trace::record(trace::Event::WakeDeferred, tid, 0);
            }
            WakeVerdict::Queue => {
                #[cfg(test)]
                crate::testing::note_progress();
                let ptr = core::ptr::NonNull::from(t);
                trace::record(trace::Event::Wake, tid, 0);
                // Onto this core's queue: a rendezvous wake stays local on purpose (§28.2), the
                // message is in registers and the cache is warm. Every caller (ipc_*, irq_notify)
                // holds SCHED, so interrupts are masked.
                // SAFETY: just transitioned Blocked -> Ready, so it was on no queue and now joins
                // one.
                cpu::current().with_runq(|q| unsafe { q.push_back(ptr) });
            }
        }
    }
}

/// Widen an ordinary three-word IPC message into the five-word mailbox. Words 3 and 4 are zero;
/// only a fault/exit message (DECISIONS §26) ever fills them, and a `RECV` hands all five back, so
/// an ordinary receiver simply never reads the top two. Keeping the mailbox one width means the
/// fault path reuses the same rendezvous machinery rather than growing a parallel one.
fn wide(m: [u64; 3]) -> [u64; 5] {
    [m[0], m[1], m[2], 0, 0]
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
    // E3's footprint-perturbation experiment (milestone 134): reachable but never taken; see
    // `crate::fastpath_pad` for what this is and why it costs nothing when the feature is off.
    #[cfg(feature = "fastpath_pad")]
    crate::fastpath_pad::maybe_pad();
    let msg = wide(msg);
    let block = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // A stale endpoint (its region was revoked): mark this send aborted and do not block. The
        // kernel-side `ipc_send` wrapper never hits this (its endpoints are never revoked); the
        // syscall layer reads the flag and returns an error.
        let Some(endpoint) = endpoint_of(sched, ep) else {
            set_ipc_aborted(sched, current);
            return;
        };
        // SAFETY: `me` is the running thread (live, on no queue), and if queued it stays live:
        // a thread queued on an endpoint is Blocked, which the reaper never touches. See tcb_ptr.
        match unsafe { endpoint.send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver.as_ptr()).id };
                let r = sched.threads.get_mut(receiver).unwrap();
                r.mailbox = msg;
                r.handshake.serve(); // delivered: this wake passes the boot-8 gate
                trace::record(trace::Event::Served, receiver, 1);
                wake(sched, receiver);
                false
            }
            ipc::Send::Blocked => {
                // `send` has already queued `current` as a sender; we record why it is parked.
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = msg;
                me.handshake.park((ep, WaitRole::Sender)); // only a collecting receiver may wake us
                trace::record(trace::Event::BlockSelf, current, ep as u8);
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
pub fn ipc_recv(ep: EpId) -> [u64; 5] {
    let immediate = {
        let mut guard = SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        let current = current_tid();

        let me = tcb_ptr(sched, current);
        // A stale endpoint (revoked): mark aborted and return a placeholder; the syscall layer sees
        // the flag and errors. (A thread revoked *while blocked* below is handled the same way: the
        // reaper sets the flag and wakes it, and it returns its stale mailbox for the layer to drop.)
        let Some(endpoint) = endpoint_of(sched, ep) else {
            set_ipc_aborted(sched, current);
            return [0, 0, 0, 0, 0];
        };
        // SAFETY: as in ipc_send: the running thread, and Blocked-while-queued keeps it live.
        match unsafe { endpoint.recv(me) } {
            // An interrupt already fired while we were not waiting. Take it and do not block.
            ipc::Recv::Signal => Some([1, 0, 0, 0, 0]),
            ipc::Recv::FromSender(sender) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let sender = unsafe { (*sender.as_ptr()).id };
                let msg = sched.threads.get(sender).unwrap().mailbox;
                // A caller (its outgoing cap is the one-shot Reply the kernel minted for a CALL, §12)
                // is awaiting a *reply*, which a plain RECV cannot furnish: only RECV_CAP delivers the
                // reply capability. Deliver the words but leave the caller blocked rather than wake it
                // with its own request masquerading as a reply. Serve CALL endpoints with RECV_CAP; a
                // plain RECV here leaves the caller hung, the same no-timeout limitation as a reply
                // that never comes.
                //
                // A **dead sender** is a fault/exit corpse parked on its supervision endpoint
                // (DECISIONS §26): deliver its five-word message but never wake it, exactly as for a
                // caller, because it is dead-until-reaped and must not run again. `recv` already
                // popped it off the sender queue, so it is now a free-standing corpse the supervisor
                // reaps with revocation.
                let leave_blocked = matches!(
                    sched.threads.get(sender).unwrap().outgoing_cap,
                    Some(c) if matches!(c.object, crate::cap::Object::Reply(_))
                ) || sched.threads.get(sender).unwrap().handshake.state
                    == State::Dead;
                if !leave_blocked {
                    // Collected: the sender's rendezvous is complete, which is what lets its
                    // wake through the boot-8 gate.
                    sched.threads.get_mut(sender).unwrap().handshake.serve();
                    trace::record(trace::Event::Served, sender, 2);
                    wake(sched, sender);
                } else if sched.threads.get(sender).unwrap().handshake.state == State::Dead {
                    // The corpse's death message is collected and `recv` popped it off the sender
                    // queue; it waits on nothing now, it only awaits its reap.
                    sched.threads.get_mut(sender).unwrap().handshake.wait_on = None;
                }
                Some(msg)
            }
            ipc::Recv::Blocked => {
                // `recv` has already queued `current` as a receiver.
                let me = sched.threads.get_mut(current).unwrap();
                me.handshake.park((ep, WaitRole::Receiver)); // only a delivering sender may wake us
                trace::record(trace::Event::BlockSelf, current, ep as u8);
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
            let t = sched.threads.get(current_tid()).unwrap();
            // The boot-8 gate makes an undelivered resume unreachable; this is its tripwire,
            // loud in every QEMU test build, on the path where the strand was observed.
            debug_assert!(
                t.handshake.delivered(),
                "recv resumed with nothing delivered"
            );
            t.mailbox
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
        let Some(endpoint) = endpoint_of(sched, ep) else {
            set_ipc_aborted(sched, current);
            return; // stale endpoint: aborted, syscall layer errors
        };
        // SAFETY: as in ipc_send.
        match unsafe { endpoint.send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver.as_ptr()).id };
                let r = sched.threads.get_mut(receiver).unwrap();
                let slot = r.cspace.insert(cap).unwrap_or(NO_CAP);
                r.mailbox = [data, slot, 0, 0, 0];
                r.handshake.serve(); // delivered: this wake passes the boot-8 gate
                trace::record(trace::Event::Served, receiver, 3);
                wake(sched, receiver);
                false
            }
            ipc::Send::Blocked => {
                // `send` queued `current`; we park the data word and the capability to hand over.
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = [data, 0, 0, 0, 0];
                me.outgoing_cap = Some(cap);
                me.handshake.park((ep, WaitRole::Sender)); // only a collecting receiver may wake us
                trace::record(trace::Event::BlockSelf, current, ep as u8);
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
        let Some(endpoint) = endpoint_of(sched, ep) else {
            set_ipc_aborted(sched, current);
            return [0, 0, 0]; // stale endpoint: aborted, syscall layer errors
        };
        // SAFETY: as in ipc_send.
        match unsafe { endpoint.recv(me) } {
            // An interrupt signal is not a delegation; it carries no capability.
            ipc::Recv::Signal => Some([1, NO_CAP, 0]),
            ipc::Recv::FromSender(sender) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let sender = unsafe { (*sender.as_ptr()).id };
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
                    // Collected: the sender's rendezvous is complete (the boot-8 gate).
                    sched.threads.get_mut(sender).unwrap().handshake.serve();
                    trace::record(trace::Event::Served, sender, 4);
                    wake(sched, sender);
                }
                // x0 = word0, x1 = the delivered slot, x2 = word1 (a CALL's second word; 0 for a plain
                // SEND_CAP, whose sender parked mailbox[1] = 0).
                Some([msg[0], slot, msg[1]])
            }
            ipc::Recv::Blocked => {
                let me = sched.threads.get_mut(current).unwrap();
                me.handshake.park((ep, WaitRole::Receiver)); // only a delivering sender may wake us
                trace::record(trace::Event::BlockSelf, current, ep as u8);
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
            let t = sched.threads.get(current_tid()).unwrap();
            debug_assert!(
                t.handshake.delivered(),
                "recv_cap resumed with nothing delivered"
            );
            let m = t.mailbox;
            [m[0], m[1], m[2]] // RECV_CAP carries three words; the top two are the fault path's
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
        let Some(endpoint) = endpoint_of(sched, ep) else {
            set_ipc_aborted(sched, current);
            return [0, 0, 0]; // stale endpoint: aborted, syscall layer errors
        };
        // SAFETY: as in ipc_send; a caller queued here is Blocked until its Reply arrives.
        match unsafe { endpoint.send(me) } {
            ipc::Send::Rendezvous(receiver) => {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                let receiver = unsafe { (*receiver.as_ptr()).id };
                // A server is parked in RECV_CAP: hand it the reply cap and the two words now.
                let r = sched.threads.get_mut(receiver).unwrap();
                let slot = r.cspace.insert(reply).unwrap_or(NO_CAP);
                r.mailbox = [msg[0], slot, msg[1], 0, 0];
                r.handshake.serve(); // delivered: this wake passes the boot-8 gate
                trace::record(trace::Event::Served, receiver, 5);
                wake(sched, receiver);
            }
            ipc::Send::Blocked => {
                // No server yet; `send` queued us as a sender. Park the words and ride the reply cap
                // in `outgoing_cap` so the eventual RECV_CAP hands it over and, seeing a Reply, leaves
                // us blocked (see ipc_recv_cap).
                let me = sched.threads.get_mut(current).unwrap();
                me.mailbox = [msg[0], msg[1], 0, 0, 0];
                me.outgoing_cap = Some(reply);
            }
        }
        // Either way we block until the reply arrives. We are NOT queued as a receiver; the Reply
        // capability, which carries our tid, is the only thing that can wake us.
        let me = sched.threads.get_mut(current).unwrap();
        me.handshake.park((ep, WaitRole::Reply)); // only the reply (or an abort) may wake us
        trace::record(trace::Event::BlockSelf, current, ep as u8);
    }

    schedule(); // returns once ipc_reply has filled our mailbox and woken us

    let guard = SCHED.lock();
    let sched = guard.as_ref().expect("no scheduler");
    let t = sched.threads.get(current_tid()).unwrap();
    debug_assert!(
        t.handshake.delivered(),
        "call resumed with nothing delivered"
    );
    let m = t.mailbox;
    [m[0], m[1], m[2]] // a reply is two words plus the pad; the fault path owns the top two
}

/// **Reply: deliver two words to a blocked caller and wake it** (milestone 12). The other half of
/// [`ipc_call`], reached by invoking the one-shot Reply capability, which carries the caller's `tid`.
/// The caller is blocked awaiting exactly this. If it is already gone (it cannot be, while blocked,
/// but be defensive), the reply is simply dropped.
pub fn ipc_reply(caller: Tid, msg: [u64; 2]) {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().expect("no scheduler");
    if let Some(t) = sched.threads.get_mut(caller) {
        // **Only a caller that awaits a reply is touched** (boot 8's observe-and-strand guard).
        // A Reply names a tid, not a wait state, and this is the one wake site addressed by tid
        // rather than through an endpoint's wait queue. Delivered to a thread parked as an
        // ordinary receiver (a stale reply whose CALL was aborted long ago, its caller re-parked
        // elsewhere), it would clobber that thread's mailbox and wake it messageless while its
        // TCB is still linked on the endpoint's wait queue, a double-enqueue on the one intrusive
        // link. Anything not Reply-parked gets nothing, exactly as a reply to a dead caller.
        if !matches!(t.handshake.wait_on, Some((_, WaitRole::Reply))) {
            return;
        }
        t.mailbox = [msg[0], msg[1], 0, 0, 0];
        t.handshake.serve(); // delivered: this wake passes the boot-8 gate
        trace::record(trace::Event::Served, caller, 6);
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

/// Delete every `DeviceFrame` capability naming `phys` from every cspace **except the calling
/// thread's** (milestone 23, DECISIONS §41). The caller keeps its own, which is the difference
/// between reclaiming a page and taking a device back to hand on; [`crate::revoke::
/// revoke_device_from_others`] has the reasoning.
pub fn delete_device_frame_caps_from_others(phys: u64) {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return;
    };
    let keeper = current_tid();
    let target = crate::cap::Object::DeviceFrame(phys);
    for t in sched.threads.iter_mut() {
        if t.id == keeper {
            continue;
        }
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

/// Hand the current thread a capability **at an explicit slot**, leaving lower slots empty.
///
/// [`grant`] fills the first free slot, which is what an ordinary `Spawn` literal wants: slot 0 is
/// `grants[0]` and reading the literal tells you the whole authority. But some out-of-band
/// conventions (notes/abi.md §4) name a *fixed* slot that a program may hold without holding the
/// ones below it: a std program granted a directory capability but not the network holds slot 4
/// with 2 and 3 empty, and the emptiness is load-bearing (it is how `std::net` knows it has no
/// network). This is the same explicit-target move `Tcb::CAP_INSERT` already offers a userspace
/// loader, available to the kernel's own service wiring.
pub fn grant_at(slot: u64, cap: crate::cap::Cap) -> Result<u64, crate::cap::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(crate::cap::Error::NoFreeSlot)?;
    let current = current_tid();
    sched
        .threads
        .get_mut(current)
        .ok_or(crate::cap::Error::NoFreeSlot)?
        .cspace
        .insert_at(slot, cap)
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
        // Remember which region paid for this TCB. It is the region an endpoint reap reclaims
        // (DECISIONS §32), and here is the only point where the answer is known rather than
        // inferred: the caller named it, and nothing afterwards can tell us as reliably.
        t.tcb_region = Some(region);
        t
    });
    // On a full table the page stays the region's (spend-only); nothing to recycle. The region
    // is already pinned by retype_object_page, so its destroy is refused regardless.
    name
}

/// Tear down every kernel object whose backing page lies in `[base, end)`, so `untyped::destroy`
/// can reclaim the region (object revocation). `Err` if a **live** thread (`Ready`/`Running`/
/// `Blocked`) sits in the region, or if a dead one is still standing on its kernel stack
/// (`handshake.on_cpu`); the region stays pinned. But the refusal is no longer passive:
/// it **arms the kill** (DECISIONS §16 amendment), marking each live resident thread so the
/// scheduler tears it down at its next preemption, so an owner that retries (the shell's `^C`
/// escalation, §24) reclaims a runaway rather than being told forever to wait for it. `Embryo` and
/// `Finished` threads are removed here (dropped, and their generational names killed, so every
/// outstanding `Tcb` capability to them goes stale on its next use).
///
/// **The region's endpoints go first, on every pass, refusal or not**, and that ordering is
/// load-bearing rather than tidy: it is what wakes a resident blocked in `RECV` so the armed kill
/// can actually land on it. The long comment at the sweep says why, and notes/frames.md carries the
/// boot it fixed.
///
/// Takes `SCHED`, so it must run **outside** any teardown `Drop`: this is the caller-driven half of
/// revocation, and `untyped::destroy` is the `SCHED`-free half (which is why the reaper's
/// `Drop` -> `destroy` path cannot deadlock against it). See `untyped::unpin`.
/// What the refuse phase of [`reap_region_objects`] decides about one resident thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionReap {
    /// Free its pages: it can never run again and no core is standing on its stack.
    Reap,
    /// It can still be scheduled. Refuse, and arm DECISIONS §16's kill so the owner's retry
    /// reclaims a runaway rather than being told to wait forever.
    RefuseAndArm,
    /// It is dead but has not left its own kernel stack yet. Refuse, and arm **nothing**: there is
    /// nothing left to doom, and the condition clears on its own one context switch from now.
    RefuseStanding,
}

/// **The refuse phase's rule, lifted out so it can be stated and tested without staging a race.**
///
/// The `on_cpu` arm is the one that had to be learned the expensive way, and it is why this is a
/// named function rather than a condition inside the loop. The rule a reader must carry away:
/// **`state` says whether a thread can run again, and `on_cpu` says whether a core is standing on
/// its stack, and freeing a `Thread` unmaps that stack.** Those are different questions; this path
/// asked only the first for months, and the answer to the second is what four CI panics were.
/// See notes/stack.md, "a kernel stack freed under its owner".
fn region_reap_verdict(state: State, on_cpu: bool) -> RegionReap {
    if matches!(state, State::Ready | State::Running | State::Blocked) {
        RegionReap::RefuseAndArm
    } else if on_cpu {
        RegionReap::RefuseStanding
    } else {
        RegionReap::Reap
    }
}

fn reap_region_objects(base: u64, end: u64) -> Result<(), ()> {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return Err(());
    };
    // A TCB sits at the start of its page, so the page's physical address is the thread pointer
    // translated back. That is the whole test for "this object lives in the region".
    let page_of = |t: &Thread| crate::arch::mmu::virt_to_phys(t as *const Thread as u64);

    // --- Endpoint phase: the region's endpoints go FIRST, refusal or not. ---
    //
    // **This ordering is what lets `DESTROY` reclaim a region full of blocked servers**, and until
    // 2026-08-16 it could not. The sweep used to sit after the refusal below, so a region holding a
    // process parked in `RECV` was refused forever: the refusal armed §16's kill, the kill is spent
    // by `schedule()`, and a `Blocked` thread never reaches `schedule()`. The owner retried until it
    // gave up, and the memory stayed spoken for until the machine stopped. That is the whole reason
    // the aarch64 test boot ran out of frames: `userspace_init_brings_up_the_console_server` builds a
    // console server out of init's budget and that server blocks in its serve loop, so init's
    // 2048-frame region was unreclaimable by construction. See notes/frames.md.
    //
    // Sweeping first fixes it because **the wake is already here**: removing an endpoint drains its
    // wait queues, marks each waiter's IPC aborted and wakes it, which is precisely the transition a
    // blocked resident needs to become schedulable and so to spend the kill the refusal arms one
    // paragraph below. A server whose endpoints came out of the region being destroyed dies; one
    // blocked on somebody else's endpoint still does not, and `reclaim_region`'s caller is told so by
    // the refusal rather than by a hang (see `user::holding::Holding`'s BUGS).
    //
    // **A refused reclaim was already destructive** and says so in `reclaim_region`'s BUGS: it arms
    // kills on every live resident. This makes the same pass also end the region's endpoints, which
    // is the same commitment one object over: the caller has said this region is going away. What it
    // must not do is *surprise* anyone, which is why it is written here rather than assumed.
    //
    // **Rescan for one at a time rather than listing them all first.** The obvious shape is to walk
    // the table into a `[u64; MAX_ENDPOINTS]` and then walk that, because `remove` mutates the table
    // and you cannot remove while iterating it. That array is **4096 bytes of a 24 KiB kernel thread
    // stack** (`thread::STACK_PAGES`), and this function is already the deepest frame in the kernel;
    // measured with `-Z emit-stack-sizes` it was 6816 bytes, of which 6144 was three such scratch
    // arrays, against a measured thread-stack high-water of 11672 bytes: this one frame wanted 2104
    // bytes MORE than all the headroom there was. See notes/stack-high-water.md.
    //
    // Rescanning costs O(live endpoints) per removal instead of O(1). That is the right trade here
    // and nowhere near a hot path: this runs when a region is torn down, the table is 512 slots, and
    // a real teardown removes a handful. Stack is the scarce resource, not these comparisons.
    loop {
        let doomed = sched
            .endpoints
            .iter()
            .find(|&(_, &phys)| base <= phys && phys < end)
            .map(|(name, _)| name);
        let Some(name) = doomed else { break };

        // Drain the endpoint's waiters. `endpoint_of` returns a `'static` reference, so it does not
        // hold the `sched` borrow across the wakes below.
        let mut waiters = [0u64; MAX_THREADS];
        let mut nw = 0;
        if let Some(endpoint) = endpoint_of(sched, name) {
            endpoint.drain_waiters(|w| {
                // SAFETY: wait-queue entries are live Blocked threads; the id revalidates it.
                waiters[nw] = unsafe { (*w.as_ptr()).id };
                nw += 1;
            });
        }
        for &tid in &waiters[..nw] {
            set_ipc_aborted(sched, tid);
            wake(sched, tid); // the link is free (drained), so wake queues it onto a run queue
        }
        sched.endpoints.remove(name);
    }

    // --- Refuse phase: no thread in the region may still be able to run. ---

    // A live thread (Ready/Running/Blocked) in the region: freeing its page would pull the stack,
    // or the running address space, out from under a thread that can still be scheduled. We may not
    // reclaim under it this pass, but the forcible tier of `^C` (DECISIONS §24) needs `DESTROY` to
    // *tear a runaway down*, not merely refuse it. So arm the kill (§16 amendment): mark every live
    // resident thread `killed` and refuse. A killed thread never runs again; the scheduler converts
    // it to a corpse at its next preemption, with no queue surgery here and no core stopping another
    // (each core reaps its own on the timer). The region's owner retries `DESTROY` (the shell's
    // escalation loop already does), and once the runaway has torn down this pass finds it gone and
    // reclaims. A thread that only ever blocks, never scheduled to hit that preemption, is the
    // cooperative tier's job (send it its interrupt endpoint), not this one.
    let mut live = false;
    // **And a second refusal, which is not about being alive at all**: a thread whose
    // `handshake.on_cpu` is still set. That flag means "a core is standing on this thread's kernel
    // stack", it is cleared by that core's successor in `finish_switch`, and freeing the `Thread`
    // is what unmaps the stack. `finish_switch` is built around exactly this and says so; this
    // path was not, because it reasoned from `state`, and `Dead` genuinely does mean "never runs
    // again". **Never runs again is not the same as off its stack**, and the gap between them is a
    // real window: `depart` marks a supervised thread `Dead`, delivers its death message (waking
    // the supervisor, possibly on another core), releases `SCHED`, and only *then* calls
    // `schedule()`. A supervisor that reaps inside those few hundred instructions unmapped the
    // stack under the corpse, whose next store then walked the exception vector down to this
    // slot's base. Four CI runs over five days, always the same test, always the same slot, and
    // read as a stack overflow for three of them. See notes/stack.md, "a kernel stack freed under
    // its owner".
    //
    // No kill is armed for this one, deliberately: the thread is already dead, so there is nothing
    // to doom, and the refusal clears on its own one context switch from now. The caller retries,
    // which is `reclaim_region`'s existing contract.
    let mut standing = false;
    //
    // A live thread (Ready/Running/Blocked) in the region: freeing its page would pull the stack, or
    // the running address space, out from under a thread that can still be scheduled. A `Dead`
    // corpse (milestone 22) is *not* live: it never runs again, so it is reapable here exactly like
    // an `Embryo` or a `Finished` thread, which is precisely what "reaped with §16 revocation"
    // (DECISIONS §26) means.
    //
    // **The pin is not what makes dropping a resident's address space safe, and saying it was cost
    // us a double free.** This comment used to argue that the region stays pinned through the reap,
    // so a bound space dropping here is refused by `untyped::destroy`. That is true for a drop that
    // happens *inside* this function, under `SCHED`. It is false for the one that matters: the
    // reaper (`finish_switch`) hoists a dead thread's space out, releases `SCHED`, and drops it
    // afterwards, by which time `reclaim_region` may already have unpinned. What makes it safe is
    // that a space built from a region it does not own never frees that region at all
    // (`user::Backing`), which is a property of the space rather than of the timing.
    for t in sched.threads.iter_mut() {
        let phys = page_of(t);
        if !(base <= phys && phys < end) {
            continue;
        }
        match region_reap_verdict(t.handshake.state, t.handshake.on_cpu) {
            RegionReap::RefuseAndArm => {
                t.killed = true;
                live = true;
            }
            RegionReap::RefuseStanding => standing = true,
            RegionReap::Reap => {}
        }
    }
    if live || standing {
        return Err(());
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
        // **Unlink a corpse from its supervision endpoint first.** A supervised thread that died
        // with nobody in `RECV` is parked on that endpoint's *sender* queue holding its death
        // message (DECISIONS §26 implementation note 2), and that endpoint is the supervisor's, so
        // it is not in this region and the endpoint sweep above did not touch it. Freeing the TCB
        // while it is still linked there would leave a dangling pointer that the supervisor's next
        // `RECV` would follow into a recycled page. §16's `DESTROY` could already reach this (reap
        // before receiving); §32's endpoint reap makes it easy to reach, because a supervisor can be
        // told a tid by its builder and never collect the message at all.
        let parked = sched
            .threads
            .get(tid)
            .filter(|t| t.handshake.state == State::Dead)
            .and_then(|t| t.fault_ep);
        if let Some(ep) = parked {
            let ptr = tcb_ptr(sched, tid);
            if let Some(endpoint) = endpoint_of(sched, ep) {
                // SAFETY: `ptr` is compared by pointer, never dereferenced; the other queued
                // senders are re-pushed and are all still live (blocked threads or corpses).
                unsafe { endpoint.remove_sender(ptr) };
            }
        }
        sched.threads.remove(tid);
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
///
/// # BUGS
///
/// **`Err` is destructive, and it does not read that way at a call site.** A refusal caused by a
/// live thread arms DECISIONS §16's kill on *every* live thread in the region, so the first call
/// dooms them and the owner's retry reclaims. That is the point (§24's `^C` escalation is built on
/// it), but it makes `reclaim_region(r).is_err()` unusable as a question: asking it kills the
/// answer. A caller that wants to know whether a region is busy without ending what is in it has no
/// such call today. Milestone 72 traced an intermittent lost-wakeup hang to one line of test code
/// that used the refusal as a probe; see `user::tests::reclaim_frees_a_started_then_exited_childs_regions`.
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

/// **Collect a corpse a supervision endpoint supervises** (DECISIONS §32, `endpoint::REAP`).
///
/// The one thing a supervisor could not previously do without holding the authority to *build* a
/// process. `ep` is the endpoint the supervisor invoked; `tid` is the id the kernel stamped on the
/// death message. Authorization is the relationship the kernel already tracks (`Thread::fault_ep`,
/// §26 implementation note 1) rather than a new registry: the named thread's recorded supervision
/// endpoint must *be* the invoked one, which is why the tid needs no badge and no handle. The
/// decision itself is `capability::reap_decision`, proved for every input in that crate.
///
/// Then the reclaim is §16's, unchanged: `reclaim_region` on the region the TCB was retyped from,
/// which is exactly the region name the owner would have passed to `Untyped::DESTROY`. One teardown
/// path, so the two cannot drift. **The pages go back to the region's owner under §13**, which is
/// the builder, not the reaper: a supervisor frees a child's memory without ever being able to spend
/// it, because it never holds a capability to it.
///
/// Takes and releases `SCHED` before the reclaim, which takes it again: `reap_region_objects` must
/// run with the lock and cannot be called under it.
pub fn reap_supervised(ep: EpId, tid: Tid) -> Result<(), abi::Error> {
    let region = {
        let guard = SCHED.lock();
        let sched = guard.as_ref().ok_or(abi::Error::NotSupervised)?;
        // A stale or recycled tid resolves to `None` here (generational names, `crates/slots`), so
        // it presents exactly as an unsupervised thread and cannot alias a fresh one.
        let t = sched.threads.get(tid);
        let fault_ep = t.and_then(|t| t.fault_ep);
        let dead = t.is_some_and(|t| t.handshake.state == State::Dead);
        match capability::reap_decision(fault_ep, ep, dead) {
            capability::Reap::NotSupervised => return Err(abi::Error::NotSupervised),
            capability::Reap::StillAlive => return Err(abi::Error::StillAlive),
            capability::Reap::Permitted => {}
        }
        // A supervised thread is always one `create_tcb` built out of a region, so this is `Some`;
        // be honest rather than unwrap, and report "nothing here to collect" if it ever is not.
        t.and_then(|t| t.tcb_region)
            .ok_or(abi::Error::NotSupervised)?
    };
    // `NotPermitted` for the same reasons `DESTROY` gives it: the child `SPLIT` its own budget and a
    // child region still owns part of the run, or a racing reap got there first and the name is now
    // stale. A restart policy reads it as "not yet", which is what it means.
    reclaim_region(region).map_err(|_| abi::Error::NotPermitted)
}

/// **Read one entry of the domain a supervision endpoint supervises** (milestone 126,
/// `endpoint::SURVEY`). Returns `(next_cursor, tid, state)`; a `next_cursor` of
/// `abi::survey::DONE` means the walk is finished and the other two words are 0.
///
/// **The domain is the supervision subtree the kernel already maintains**, so there is no registry
/// to keep in step with reality and no way for the view to disagree with it. Membership is
/// `capability::survey_includes`, which is the same relationship `reap_supervised` authorizes with
/// and is proved in that crate; a thread appears here exactly when its `Thread::fault_ep` *is* the
/// invoked endpoint. Nothing about the caller's own identity is consulted, because holding the
/// endpoint with `READ` is the whole of the claim (the rights check is the syscall layer's).
///
/// **One entry per call, and the lock is given back between them.** A survey of a domain with a
/// hundred children would otherwise hold `SCHED` for a hundred children's worth of work at a
/// userspace program's discretion, which is a scheduler-latency hole a program could open on
/// purpose. The cost is that the survey is a sequence of snapshots rather than one; see the `BUGS`
/// section of notes/process-view.md, which states exactly what that does and does not promise.
pub fn survey_supervised(ep: EpId, cursor: u64) -> Result<(u64, u64, u64), abi::Error> {
    let guard = SCHED.lock();
    // Before the scheduler exists there is no domain to report, which is "nothing here", not a
    // refusal: the caller's authority was never in question.
    let Some(sched) = guard.as_ref() else {
        return Ok((abi::survey::DONE, 0, 0));
    };
    // A cursor past the table is empty rather than an error (`iter_from`'s contract), so a caller
    // that keeps feeding back what it was given cannot walk off the end into a refusal that would
    // read as "you may not look".
    let from = usize::try_from(cursor).unwrap_or(usize::MAX);
    for (slot, t) in sched.threads.iter_from(from) {
        if capability::survey_includes(t.fault_ep, ep) {
            return Ok((slot as u64 + 1, t.id, survey_state(t.handshake.state)));
        }
    }
    Ok((abi::survey::DONE, 0, 0))
}

/// The run state a survey reports, as an `abi::survey` code.
///
/// `Embryo` and `Finished` are unreachable for a supervised thread and are mapped anyway rather
/// than left to a panic or a wildcard: supervision is recorded at `START`, so an embryo has no
/// `fault_ep` to match, and a supervised death goes to `Dead` rather than `Finished`. Reporting
/// them as `READY` and `DEAD` is the honest nearest neighbour if either ever becomes reachable,
/// and the `match` is exhaustive so a seventh state cannot be added without meeting this.
const fn survey_state(state: State) -> u64 {
    match state {
        State::Embryo | State::Ready => abi::survey::READY,
        State::Running => abi::survey::RUNNING,
        State::Blocked => abi::survey::BLOCKED,
        State::Finished | State::Dead => abi::survey::DEAD,
    }
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
        if t.handshake.state != State::Embryo {
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
    if t.handshake.state != State::Embryo {
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
///
/// `target` is `None` for first-free placement (the original behaviour) or `Some(slot)` to place
/// the capability in a specific free slot, which a supervisor uses to put a child's supervision
/// endpoint in the reserved fault slot (milestone 22). A targeted insert into an occupied or
/// out-of-range slot is `OutOfMemory`, so the reservation cannot be quietly overwritten.
pub fn tcb_insert_cap(
    tid: Tid,
    cap: crate::cap::Cap,
    target: Option<u64>,
) -> Result<u64, abi::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(abi::Error::NoSuchSlot)?;
    let t = sched.threads.get_mut(tid).ok_or(abi::Error::NoSuchSlot)?;
    if t.handshake.state != State::Embryo {
        return Err(abi::Error::WrongObject);
    }
    match target {
        None => t.cspace.insert(cap).map_err(|_| abi::Error::OutOfMemory),
        Some(slot) => t
            .cspace
            .insert_at(slot, cap)
            .map_err(|_| abi::Error::OutOfMemory),
    }
}

/// **Start an embryo** (milestone 19c.3): the no-start-before-whole gate, then make it runnable.
/// Refuses a TCB that is not an embryo, or one with no bound address space or no entry set: a
/// half-built thread must never run. On success the thread gets its kernel stack and entry
/// context and joins this core's run queue.
pub fn start_tcb(tid: Tid, args: [u64; 3]) -> Result<(), abi::Error> {
    let mut guard = SCHED.lock();
    let sched = guard.as_mut().ok_or(abi::Error::NoSuchSlot)?;
    let t = sched.threads.get_mut(tid).ok_or(abi::Error::NoSuchSlot)?;

    if t.handshake.state != State::Embryo {
        return Err(abi::Error::WrongObject); // already started (or not a TCB)
    }
    // WHOLE, or refuse: a bound address space and an entry point. Either missing is a half-built
    // thread, and starting it would drop to EL0 with no low half or no code.
    if t.space.is_none() || t.entry.0 == 0 {
        return Err(abi::Error::NotPermitted); // configure it first
    }

    // **The spawn-slot convention** (milestone 22, DECISIONS §26). If the reserved fault slot holds
    // an Endpoint capability, this thread is supervised: record the endpoint as its fault target
    // and consume the slot, so the child cannot forge fault messages on it (the kernel stays the
    // only sender on this path, §26.5). Supervision is fixed here, at spawn, and never changes.
    if let Ok(fault_cap) = t.cspace.get(abi::fault::FAULT_EP_SLOT)
        && let crate::cap::Object::Endpoint(ep) = fault_cap.object
    {
        t.fault_ep = Some(ep);
        let _ = t.cspace.delete(abi::fault::FAULT_EP_SLOT);
    }

    t.start_args = args; // the child's x0, x1, x2 (19d/19e)
    if !t.arm_for_start() {
        return Err(abi::Error::OutOfMemory); // no kernel stack to be had
    }
    t.handshake.state = State::Ready;
    // Placement is the power of two choices (DECISIONS §28), the same as `spawn`: a freshly started
    // user thread lands on the lighter of two sampled cores rather than always the starter's, so a
    // process that spawns a pipeline does not pile it all onto one core. `place_on` enqueues locally
    // or hands the thread to the target's inbox; the SGI that makes a remote target pick it up goes
    // out after SCHED is released.
    let target = pick_spawn_target();
    let ptr = tcb_ptr(sched, tid);
    place_on(target, ptr);
    drop(guard);
    if target != cpu::id() {
        crate::arch::irq::send_reschedule(target);
    }
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

    // SAFETY: `ttbr` is the composed value of the `AddressSpace` the block above just moved into
    // the *current* thread's slot. The current thread is the one executing this line, so it is on a
    // CPU and cannot be reaped, and the space it now owns is live until that thread's `Drop` runs.
    unsafe { crate::arch::mmu::switch_user_root(ttbr) };
}

/// The top of the current thread's kernel stack: **where its `TrapFrame` belongs.**
///
/// `None` for the boot thread, which runs on the stack `boot.s` set up and does not own it.
///
/// A user thread's `TrapFrame` is not an ordinary local. It must sit at exactly the address the
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

/// **The user PC recorded in `tid`'s trap frame** (milestone 71, test support), read from the top of
/// its kernel stack, which is the one address the trap path and the user-entry path must agree on.
/// `None` if the name does not resolve or the thread has no kernel stack of its own.
///
/// This is [`dump_threads`]'s per-thread PC lookup, exposed so a test can assert the agreement
/// rather than only a human reading a hang dump. A thread that has reached user mode reads back a
/// user address here; a zero means nothing wrote a frame where the trap path will look for one.
#[cfg_attr(not(test), allow(dead_code))]
pub fn user_pc_of(tid: Tid) -> Option<u64> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    let t = sched.threads.get(tid)?;
    t.stack
        .as_ref()
        .map(|s| crate::arch::exceptions::user_pc(s.top()))
}

/// **Postmortem: read a corpse's retained fault/exit message** (milestone 22, test support). A
/// `Dead` thread keeps its five-word §26 message until the supervisor reaps it, so this proves the
/// corpse's TCB still holds its fault-time state after the notification was delivered. `None` if
/// the name does not resolve or the thread is not a corpse.
#[cfg_attr(not(test), allow(dead_code))]
pub fn corpse_fault_msg(tid: Tid) -> Option<[u64; 5]> {
    let guard = SCHED.lock();
    let sched = guard.as_ref()?;
    let t = sched.threads.get(tid)?;
    (t.handshake.state == State::Dead)
        .then_some(t.fault_msg)
        .flatten()
}

/// **Is `tid` still in the thread table?** (test support.)
///
/// The narrow question "was *this* thread reaped", which is what a test that spawned one actually
/// wants to know. [`thread_count`] answers a wider one, and the width is a defect when it is used
/// this way: the count is the size of the whole table, so an unrelated process finishing its
/// teardown moves it, and a test waiting for the count to come back to a baseline is really waiting
/// for the rest of the system to hold still. It need not.
///
/// The name is generational, so a reaped thread's `Tid` never resolves again even if its slot is
/// reused: `false` here means gone, not "gone or replaced".
#[cfg_attr(not(test), allow(dead_code))]
pub fn thread_present(tid: Tid) -> bool {
    SCHED
        .lock()
        .as_ref()
        .is_some_and(|s| s.threads.get(tid).is_some())
}

/// **Arm a kill on one thread by name** (test support), the single-thread form of what
/// [`reap_region_objects`] does to a whole region.
///
/// This exists because a test that spawns a **bare** user thread had no way to take it back. A
/// thread spawned into a reclaimable region is torn down by reclaiming the region; a thread spawned
/// with plain [`spawn`] around `user::run` belongs to no region, so there was no handle to end it
/// with. Two tests need a subject that never exits on its own (a spinner whose point is that it
/// never yields, and a child that must hold the free-frame count still while it is read), and both
/// therefore leaked a runnable thread for the rest of the suite. Two spinning threads on a four-hart
/// machine is a scheduling load the rest of the suite then runs under, which is how it presented:
/// `reclaim_frees_a_started_then_exited_childs_regions` starved and tripped its watchdog, on CI,
/// intermittently, far from the tests that caused it.
///
/// **No new syscall and no change to the user-visible surface.** DECISIONS §16's armed kill is the
/// whole mechanism: setting `killed` makes the scheduler convert the thread to a corpse at its next
/// preemption, which is exactly how `DESTROY` and §24's `^C` escalation already work. Rule 3 governs
/// the syscall boundary; this is an in-kernel function for in-kernel tests.
///
/// Returns whether a live thread was found and marked. A `false` means the `Tid` did not resolve,
/// which for a generational name means the thread is already gone rather than that the kill failed.
///
/// The kill is **armed, not immediate**: the thread dies at its next preemption, so a caller that
/// needs it actually gone waits for [`thread_present`] to go false rather than assuming.
#[cfg_attr(not(test), allow(dead_code))]
pub fn kill_thread(tid: Tid) -> bool {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return false;
    };
    match sched.threads.get_mut(tid) {
        Some(t) => {
            t.killed = true;
            true
        }
        None => false,
    }
}

pub fn thread_count() -> usize {
    SCHED.lock().as_ref().map_or(0, |s| s.threads.len())
}

/// **Count the runnable threads that are not the caller and not an idle thread** (test support).
///
/// A leaked one-shot driver that spins forever instead of exiting is `Ready`/`Running` for the rest
/// of the boot; a thread doing legitimate work is `Blocked` on an endpoint when the system is
/// quiescent. So, from a quiesced probe (yield until pending exits are reaped), this count is the
/// number of leaked spinners: the idle threads (one per core) and the probe itself are the only
/// runnable threads a clean system has. The regression proxy for the test-thread starvation that
/// made the RedoxFS mount overrun the hang watchdog.
#[cfg_attr(not(test), allow(dead_code))]
pub fn runnable_non_idle_count(&exclude: &Tid) -> usize {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return 0;
    };
    let mut idles = [u64::MAX; crate::cpu::MAX_CPUS];
    // Harvest each ONLINE core's idle tid by set membership, not `0..count` (first-silicon sweep,
    // 2026-08-14): with the VisionFive 2's {1,2,3} online, count-as-index misses cpu 3's idle tid,
    // and its idle thread would then be counted as a leaked spinner. The array slot order does not
    // matter; only membership in `idles` does.
    for (slot, c) in idles.iter_mut().zip(crate::smp::online_cpus()) {
        *slot = crate::cpu::of(c).idle.load(Ordering::Relaxed);
    }
    sched
        .threads
        .iter_mut()
        .filter(|t| {
            matches!(t.handshake.state, State::Ready | State::Running)
                && t.id != exclude
                && !idles.contains(&t.id)
        })
        .count()
}

/// Print every thread's scheduler state, for diagnosing a hang. A lost IPC wakeup leaves a thread
/// `Blocked` forever with nothing to wake it; this shows which thread, and the `on_cpu`/`wake_pending`
/// flags that would reveal a botched wake-before-switch-out handoff. Takes SCHED, which is free when
/// the hang is a blocked thread (not a lock deadlock). Used by the test watchdog.
/// Feed every live thread's stack into the high-water accounting (milestone 84). Long-lived
/// service threads (the FS server, the shape of the incident that motivated the measurement) are
/// never reaped, so their stacks are only visible here, not in `KernelStack`'s `Drop`. A thread may
/// be running on another core while its stack is scanned; the scan reads a snapshot, and a racing
/// deepening is at worst under-reported by this run (see `stack::high_water`).
#[cfg(test)]
pub fn scan_live_thread_stacks() {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return;
    };
    for t in sched.threads.iter_mut() {
        if let Some(s) = t.stack.as_ref() {
            // SAFETY: a `KernelStack` this thread still owns, so its pages are mapped until its
            // `Drop` runs, and `KernelStack::new` painted the whole span. `SCHED` is held, so the
            // thread cannot be reaped out from under the scan.
            let used = unsafe { crate::stack::high_water(s.bottom(), s.top()) };
            crate::stack::note_thread_stack_use(used);
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn dump_threads() {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        crate::println!("  dump_threads: no scheduler");
        return;
    };
    // What this dump can and cannot honestly claim (first-silicon audit, 2026-08-14):
    //
    //   - `state`/`on_cpu`/`wake_pending`/`wait`, and the endpoint counts, are a CONSISTENT
    //     snapshot: every writer holds SCHED, which this dump holds.
    //   - `pc` is the trap frame at the thread's stack top, which trap entry writes WITHOUT
    //     SCHED. For an off-cpu thread it is trustworthy (the frame write happened-before the
    //     state write on that core, and our lock acquire synchronises with its release). For a
    //     thread on a cpu it is a racing read of live state, printed with a `*`.
    //   - the per-core lines read other cores' atomics relaxed. `current` is written under
    //     SCHED, so it is quiescent while we hold the lock, except for a core mid-switch
    //     (between its SCHED release and its finish_switch): such a core's `current` names the
    //     incoming thread while the outgoing one still runs for a few more instructions.
    //   - `ticks` is that core's timer-interrupt count. A core whose ticks FREEZE across dumps
    //     is not taking traps at all: wedged with interrupts masked, or stuck inside an SBI call
    //     in M-mode (where delegated S-interrupts cannot preempt), which no other row can show.
    // The stage breadcrumb repeats here on purpose: a serial line printed once can go missing
    // from a bench log (boots 7 through 9 were convicted on exactly that absence), but a dump
    // that fires every couple of seconds re-states how far the tour got each time.
    crate::println!(
        "--- thread dump (hang diagnostic; pc* = on-cpu, racy; tour stage {}) ---",
        BOOT_STAGE.load(Ordering::Relaxed),
    );
    for t in sched.threads.iter_mut() {
        let pc = t
            .stack
            .as_ref()
            .map(|s| crate::arch::exceptions::user_pc(s.top()))
            .unwrap_or(0);
        // The address-space root, which is what tells threads of DIFFERENT PROCESSES apart. Without
        // it a `pc` in this dump is close to useless for a userspace hang: every user program is
        // linked at the same base (0x40_0000), so a bare PC resolves plausibly against several
        // binaries at once and invites exactly the wrong conclusion. That is not hypothetical, it
        // cost an hour on 2026-07-30: three spinning threads read equally well as three FS servers
        // in RedoxFS directory code or as one std client looping in `read_to_end`, and the PCs alone
        // could not say which. Threads sharing a root are one process; distinct roots are distinct
        // processes, and 0 is a kernel thread with no user address space.
        let root = t.space.as_ref().map(|s| s.root()).unwrap_or(0);
        crate::print!(
            "  tid={:#06x} state={:?} on_cpu={} wake_pending={} has_outgoing_cap={} pc={:#010x}{} aspace={:#010x}",
            t.id,
            t.handshake.state,
            t.handshake.on_cpu,
            t.handshake.wake_pending,
            t.outgoing_cap.is_some(),
            pc,
            if t.handshake.on_cpu { "*" } else { "" },
            root,
        );
        // The wait reason, written by the same SCHED-held statement that wrote `Blocked`. A
        // `Blocked` thread with `wait=-` here is the smoking gun for a state byte written outside
        // the block paths (corruption, or a block applied to the wrong TCB): every legal block
        // records what it waits on. See notes/visionfive2.md, fourth bench stop.
        match t.handshake.wait_on {
            Some((ep, role)) => crate::println!(" wait={ep:#x}/{role:?}"),
            None => crate::println!(" wait=-"),
        }
    }
    // Endpoint topology: which endpoint each blocked thread is queued on, so a deadlock shows as a
    // sender with no receiver. Diagnostic only.
    for (name, &phys) in sched.endpoints.iter() {
        // SAFETY: a live endpoint page, direct-mapped, under SCHED.
        let ep = unsafe { &*(crate::arch::mmu::phys_to_virt(phys) as *const Endpoint) };
        let (ns, nr, np) = ep.debug_counts();
        if ns != 0 || nr != 0 || np != 0 {
            crate::println!("  ep={name:#06x} senders={ns} receivers={nr} pending={np}");
        }
    }
    // The online set, not `0..count` (first-silicon sweep, 2026-08-14): on the VisionFive 2 the
    // count-as-index loop printed parked slot 0 as if it were a live core and hid online core 3.
    for c in crate::smp::online_cpus() {
        let pc = cpu::of(c);
        let inbox_len = pc.inbox.lock().len();
        crate::println!(
            "  core {c}: current={:#06x} idle={:#06x} switched_from={:#06x} need_resched={} inbox_len={} ticks={} steal_req={:?}",
            pc.current.load(Ordering::Relaxed),
            pc.idle.load(Ordering::Relaxed),
            pc.switched_from.load(Ordering::Relaxed),
            pc.need_resched.load(Ordering::Relaxed),
            inbox_len,
            // A core whose tick count holds still between dumps is taking no timer traps: it is
            // spinning with interrupts masked, or parked inside an SBI call in M-mode (a remote
            // fence that never completes looks exactly like this). The one field that tells a
            // wedged core from a scheduler that merely chose not to run somebody.
            crate::arch::timer::ticks_on(c),
            // A steal request that stays claimed dump after dump means the victim never reached
            // a scheduler entry to serve it: the same wedge, seen from a thief's side.
            pc.steal_request.peek(),
        );
        trace::dump(c);
    }
    // A parked slot with a non-empty inbox is a thread nothing will ever run: the exact shape of
    // the VisionFive 2 placement hang (init modulo-counted into slot 0's inbox; that dead inbox in
    // this dump was the clue). Since the online-set sweep no path should produce it, so if this
    // prints, something is picking cpus by count again.
    let online = crate::smp::online_harts_mask();
    for c in (0..crate::cpu::MAX_CPUS).filter(|c| online & (1 << c) == 0) {
        let inbox_len = cpu::of(c).inbox.lock().len();
        if inbox_len != 0 {
            crate::println!(
                "  core {c}: PARKED with inbox_len={inbox_len} (placed on a dead core)"
            );
        }
    }
    crate::println!("--- end thread dump ---");
}

/// **How many senders are parked on an endpoint.** Test support (milestone 22 phase B.2).
///
/// A negative assertion ("the supervisor sent nothing more") cannot be made with `RECV`, which would
/// block forever on a quiet endpoint. This is the non-blocking look that lets a test say "and then
/// nothing happened" instead of hanging when the code is right.
#[cfg(test)]
pub fn endpoint_waiting_senders(ep: EpId) -> usize {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return 0;
    };
    match endpoint_of(sched, ep) {
        Some(e) => e.debug_counts().0,
        None => 0,
    }
}

/// **How many receivers are parked on an endpoint.** The twin of [`endpoint_waiting_senders`], and
/// test support for the same reason (milestone 81).
///
/// A test that wants to act *on* a blocked waiter needs to know the waiter is blocked, and "I
/// yielded, so it must have run" is not that knowledge: since DECISIONS §28 the waiter is placed on
/// another core, and on the physical core under HVF a yield on this one returns in nanoseconds. So
/// the wait has to be on the queue itself, which is what this reads.
#[cfg(test)]
pub fn endpoint_waiting_receivers(ep: EpId) -> usize {
    let mut guard = SCHED.lock();
    let Some(sched) = guard.as_mut() else {
        return 0;
    };
    match endpoint_of(sched, ep) {
        Some(e) => e.debug_counts().1,
        None => 0,
    }
}

/// **Test support: a wake with nothing delivered** (the boot-8 injector, 2026-08-14).
///
/// Issues a bare `wake()` against `tid` under `SCHED`, through the same function every scheduler
/// wake site funnels into, with no message written, no signal counted, and no abort flagged: the
/// transition the VisionFive 2's boot-8 event ring recorded against the boot thread (`wake:0x0`
/// on a boot where no sender to its endpoint existed). This is deliberately not a hand-rolled
/// state poke: it exercises the real wake path, so whatever `wake()` does about an undelivered
/// wake is what this injects.
#[cfg(test)]
pub fn wake_without_delivery(tid: Tid) {
    let mut guard = SCHED.lock();
    if let Some(sched) = guard.as_mut() {
        wake(sched, tid);
    }
}

/// **The number that says preemption is real**, read by the preemption tests and printed by the
/// milestone tour.
///
/// The alternate boot modes (`shell`, `bench`, `initboot`) each compile the tour out and run no
/// tests, so in those three configurations this genuinely has no caller. That is a property of the
/// boot mode, not evidence the counter is dead, which is why the allow is conditioned on exactly
/// those features rather than written unconditionally.
#[cfg_attr(
    any(feature = "shell", feature = "bench", feature = "initboot"),
    allow(dead_code)
)]
pub fn preemptions() -> u64 {
    PREEMPTIONS.load(Ordering::Relaxed)
}

pub fn count_preemption() {
    PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
}

/// **The deferred half of preemption**: switch away if this core's tick asked for it.
///
/// Called by both ISAs' trap dispatchers *after* the handler has returned to the interrupted
/// thread's own stack, which is the whole reason it is a function rather than four lines at the
/// bottom of `handle_irq` where it lived until milestone 124. The handler may have run on this
/// core's interrupt stack, and `schedule()` may only be called on a stack the interrupted thread
/// owns; see `kernel/src/interrupt_stack.rs`.
///
/// Portable, and identical on both architectures, which is the other half of why it moved: the four
/// lines were written twice and had drifted in their comments already.
pub fn preempt_if_needed() {
    if take_need_resched() && is_running() {
        count_preemption();
        schedule();
    }
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

    /// Wait for `cond`, bounded by the CLOCK rather than by a yield count.
    ///
    /// These tests used to spin a fixed number of yields and then assert. A yield count is not a
    /// duration: on a loaded host, or once §28's placement scattered work across cores, this core can
    /// burn two hundred cheap yields long before the threads it is waiting on have been scheduled at
    /// all. That is not a hang, it is an impatient observer, and it fails an assertion that describes
    /// the system rather than the test.
    ///
    /// It has now bitten three times in one day, in three different files: the reap waits in
    /// `user.rs`, three spin counts in `smp.rs`, and here. The third time was a gate run failing with
    /// "finished threads were never reaped, left: 11, right: 5" while a leaked QEMU held 199% of the
    /// host, which is exactly the condition a yield count cannot survive and a clock can.
    ///
    /// Two seconds is far beyond any honest completion here (these are milliseconds when the machine
    /// is quiet) and well inside the harness's 90 s per-test ceiling, which remains the backstop for a
    /// genuine hang. Still a leak trap rather than a masked failure: work that never completes times
    /// out, and the caller's assertion then reports what was actually wrong.
    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if cond() {
                return true;
            }
            crate::sched::yield_now();
        }
        cond()
    }

    /// Wait for `cond` **without yielding**, budgeted in timer ticks delivered to this core rather
    /// than in wall-clock time.
    ///
    /// For `a_thread_that_never_yields_is_preempted_anyway`, which must not yield (that is the
    /// whole point of it) and is waiting for a *preemption*. A tick is when a preemption can
    /// happen, so the number of preemption opportunities that went by is what the claim is about.
    /// Why that unit survives a contended host where a `timer::now()` deadline does not is
    /// [`crate::testing::TickBudget`]'s argument, and the re-anchoring on migration is its
    /// mechanism; here the migration is additionally the news this test is waiting for.
    ///
    /// If ticks stop arriving altogether this does not return, and the harness's 90 s per-test
    /// ceiling is the backstop: a timer that is not delivering is the arch timer tests' failure to
    /// report, not this one's.
    fn within_ticks(budget: u64, mut cond: impl FnMut() -> bool) -> bool {
        let mut budget = crate::testing::TickBudget::new(budget);
        loop {
            if cond() {
                return true;
            }
            if budget.expired() {
                return cond();
            }
            core::hint::spin_loop();
        }
    }

    /// Spin the scheduler until `cond`, bounded by wall-clock, returning whether it happened. Since
    /// DECISIONS §28, work a test spawns runs on *other* cores, so this core is often idle and a
    /// yield returns at once: a fixed count of yields elapses in almost no real time and times out
    /// before the parallel result lands. A ~2 s deadline gives the other cores real time while
    /// staying far under the 60 s hang watchdog, so a genuine lost wakeup still fails.
    fn spin_until(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if cond() {
                return true;
            }
            super::yield_now();
        }
        cond()
    }

    // --- Raising an interrupt on purpose, on each ISA ---------------------------------------
    //
    // The two delivery tests below (`an_interrupt_becomes_a_message` and
    // `an_interrupt_that_arrives_before_the_wait_is_not_lost`) are portable: the property they check
    // is that the kernel turns an interrupt into a message and does not lose one that arrives early,
    // and neither of those is architectural. Only the *trigger* is, and it is genuinely asymmetric,
    // so it lives here in three small functions rather than in a comment claiming parity.
    //
    // aarch64 has a software-generated interrupt (an SGI), so it needs no device whatsoever.
    //
    // RISC-V has nothing of the kind. `sip.SEIP` is read-only to S-mode (only the PLIC, driven by a
    // real wire, sets it), the PLIC's pending block is read-only by specification, and the one
    // interrupt S-mode can raise on itself is the SBI's IPI, which arrives as a *software* interrupt
    // (`scause` = 1) down a different arm of `riscv_trap_body` than a device's, touching neither
    // `irq_route` nor `irq_notify`. Using it would have looked like parity and proved nothing.
    //
    // So RISC-V uses the smallest real interrupt line it can assert by hand: the console UART's own
    // transmit-empty interrupt, which a 16550 raises the instant it is enabled, because the
    // transmitter of a polling console is always empty. No transfer, no external stimulus, nothing
    // to read back, and it is 16550 architecture rather than a QEMU behaviour, so it should carry to
    // a real part. See `console::raise_uart_interrupt` and notes/interrupts.md.

    /// The interrupt `an_interrupt_becomes_a_message` raises.
    #[cfg(target_arch = "aarch64")]
    fn delivery_irq() -> u32 {
        1 // an SGI: software-triggerable, no hardware behind it
    }
    /// The interrupt `an_interrupt_that_arrives_before_the_wait_is_not_lost` raises. A different SGI
    /// from `delivery_irq` so the two tests cannot see each other's routes.
    #[cfg(target_arch = "aarch64")]
    fn pending_irq() -> u32 {
        2
    }

    /// The NS16550's PLIC source on QEMU `virt`. A board constant, hardcoded identically on
    /// main.rs's boot-tour and shell paths; another board would give its UART a different number,
    /// and this is one of the places that would have to learn it from the device tree.
    ///
    /// On the VisionFive 2 the boot summary reads `uart irq : source 32 (device tree)`, so
    /// `DELIVERY_IRQ = 10` causes `raise_uart_interrupt` on IRQ 10 to never reach the thread
    /// waiting on the route bound to the real IRQ 32. The test now reads the DTB-driven value the
    /// rest of the kernel already uses (`user::uart_irq_and_source`); on QEMU `virt` that gives 10
    /// (via the fallback `UART_RX_INTID`), on the VF2 it gives 32.
    #[cfg(target_arch = "riscv64")]
    fn delivery_irq() -> u32 {
        crate::user::uart_irq_and_source().0
    }
    /// **The same source, deliberately.** RISC-V has exactly one line these tests can assert by
    /// hand, so unlike aarch64's two SGIs the two tests share it. They do not collide: each rebinds
    /// the route to its own endpoint before raising, and each quiets the line before it returns.
    #[cfg(target_arch = "riscv64")]
    fn pending_irq() -> u32 {
        crate::user::uart_irq_and_source().0
    }

    /// Enable the test interrupt at the controller. Nothing is raised yet.
    #[cfg(target_arch = "aarch64")]
    fn arm_test_irq(intid: u32) {
        crate::drivers::gic::enable(intid, 0); // SGI: per-core, target ignored
    }

    #[cfg(target_arch = "riscv64")]
    fn arm_test_irq(intid: u32) {
        // The affinity policy picks (and remembers) which hart's PLIC context this source lands on,
        // exactly as it does for a real driver's line. The handler masks the source when it fires,
        // so this also re-enables it for the second of the two tests.
        crate::arch::irq::enable(intid);
    }

    /// Raise it.
    #[cfg(target_arch = "aarch64")]
    fn raise_test_irq(intid: u32) {
        // Self, by asking rather than by assuming core 0: the test thread runs wherever the
        // scheduler put it, and a fixed target is the count-as-index disease in miniature.
        crate::drivers::gic::send_sgi(intid, crate::cpu::id());
    }

    #[cfg(target_arch = "riscv64")]
    fn raise_test_irq(intid: u32) {
        // The console UART's line is the only one this ISA can assert by hand, so a caller naming
        // any other source would silently raise the wrong interrupt and then wait for one that
        // never came, which reads as a kernel bug rather than a test bug.
        debug_assert_eq!(
            intid,
            delivery_irq(),
            "riscv can only raise the console UART's own line by hand"
        );
        crate::console::raise_uart_interrupt();
    }

    /// Lower it again, so the next test starts from a quiet line.
    #[cfg(target_arch = "aarch64")]
    fn quiet_test_irq() {
        // An SGI is edge-triggered and one-shot: there is no line to lower.
    }

    #[cfg(target_arch = "riscv64")]
    fn quiet_test_irq() {
        crate::console::quiet_uart_interrupt();
    }

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

        // Wait until it has had a turn (on this or another core, since §28).
        spin_until(|| RAN.load(Ordering::SeqCst));

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

        let region = crate::untyped::create(2).expect("a fresh 2-page region");
        let tid = crate::sched::create_tcb(region).expect("retype a TCB from the region");

        // The embryo is named, not counted. This pair used to bracket `thread_count()` against a
        // baseline taken above (`threads_before + 1`, then `threads_before`), which is the reaper
        // count's defect in a different test: the headcount is the size of the whole table, so a
        // neighbouring thread exiting between the two reads lands the count BELOW what the
        // assertion demands and blames this embryo for it. `thread_present` on the Tid this test
        // created is immune by construction, and it is strictly the stronger claim: the old
        // "the TCB's table slot must be freed" could pass with the embryo still in the table, as
        // long as somebody else's thread left in the same window. Fourth appearance of this fix;
        // see notes/load-sensitive-assertions.md and `thread_present`'s own doc comment.
        assert!(
            crate::sched::thread_present(tid),
            "the embryo should be in the table before reclaim"
        );
        assert!(
            crate::memory::free_frames() < frames_before,
            "creating the region should have spent frames"
        );

        crate::sched::reclaim_region(region)
            .expect("reclaim a region whose only object is an unstarted TCB");

        assert!(
            !crate::sched::thread_present(tid),
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

    /// **Untyped SPLIT returns a child's pages to the parent on reclaim (LIFO), so a split parent is
    /// not committed for its lifetime.** Carve a region into two children; the parent refuses reclaim
    /// while they live. Reclaiming a child out of order (not the top of the watermark) leaves a hole;
    /// reclaiming the top child un-bumps the parent, so its budget is re-splittable. Either way a
    /// child's pages go back to the *parent*, not the allocator, so the free-frame count does not move
    /// until the parent itself, now childless, is destroyed.
    #[test_case]
    fn split_returns_child_pages_to_the_parent() {
        let frames_before = crate::memory::free_frames();
        let parent = crate::untyped::create(8).expect("parent region");
        assert_eq!(
            crate::memory::free_frames(),
            frames_before - 8,
            "create spent the parent's pages"
        );

        let child_a = crate::untyped::split(parent, 4).expect("split child a"); // [0,4)
        let child_b = crate::untyped::split(parent, 4).expect("split child b"); // [4,8), the top
        assert_ne!(child_a, child_b);
        assert!(crate::untyped::has_children(parent));
        assert!(
            crate::sched::reclaim_region(parent).is_err(),
            "a parent with live children refuses reclaim",
        );
        assert!(
            crate::untyped::split(parent, 1).is_none(),
            "a fully-carved parent cannot split further",
        );

        // Reclaim out of order (child_a is not the top): a hole, its pages returned to the parent,
        // nothing to the allocator.
        crate::sched::reclaim_region(child_a).expect("reclaim child a (leaves a hole)");
        assert_eq!(
            crate::memory::free_frames(),
            frames_before - 8,
            "a reclaimed child returns pages to the parent, not the allocator",
        );
        assert!(
            crate::untyped::has_children(parent),
            "one child still lives"
        );

        // Reclaim the top child: the parent un-bumps and is childless, and its budget re-splits.
        crate::sched::reclaim_region(child_b).expect("reclaim child b (the LIFO top)");
        assert!(!crate::untyped::has_children(parent), "no children remain");
        let child_c = crate::untyped::split(parent, 4).expect("the LIFO-returned pages re-split");
        crate::sched::reclaim_region(child_c).expect("reclaim child c");

        // Nothing reached the allocator until now: destroying the childless root parent frees the
        // whole run, the hole included, exactly once.
        crate::sched::reclaim_region(parent).expect("destroy the now-childless root parent");
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "the root parent's pages return to the allocator",
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

    /// **A thread blocked on an endpoint wakes with an error when the endpoint is revoked.** Rather
    /// than refuse the reclaim (the old safe subset) or strand the waiter, revocation drains the
    /// endpoint's wait queue, marks each waiter aborted, and wakes it: the reclaim *succeeds*, and the
    /// woken thread's blocking IPC reports the endpoint is gone (`take_ipc_aborted`) instead of
    /// returning a message it never received. This is the richer semantic, folded into the IPC core.
    #[test_case]
    fn a_blocked_waiter_wakes_with_an_error_when_its_endpoint_is_revoked() {
        static ABORTED: AtomicBool = AtomicBool::new(false);
        static WOKE: AtomicBool = AtomicBool::new(false);
        ABORTED.store(false, Ordering::SeqCst);
        WOKE.store(false, Ordering::SeqCst);

        let region = crate::untyped::create(2).expect("region");
        let ep = crate::sched::create_endpoint_from(region).expect("endpoint from region");

        // A thread that blocks receiving on the endpoint, then records whether it was aborted.
        crate::sched::spawn(move || {
            let _ = crate::sched::ipc_recv(ep);
            ABORTED.store(crate::sched::take_ipc_aborted(), Ordering::SeqCst);
            WOKE.store(true, Ordering::SeqCst);
        })
        .expect("spawn a waiter");

        // **The waiter must be queued on the endpoint before the reclaim**, or there is nothing to
        // wake and the test passes on a fiction. This used to be one `yield_now()`, on the premise
        // (written when the machine was single core, stale since DECISIONS §28 scattered placement)
        // that yielding hands this core to the waiter. It does not: the waiter is on another core,
        // and a yield here only says *this* core had nothing else to do.
        //
        // Milestone 81 is where that came due. Under TCG the round-robin between vCPUs made one
        // yield enough often enough to look deliberate; on the physical core under HVF the four
        // vCPUs are four host threads running at once, this core's yield returns in nanoseconds,
        // and the reclaim ran before the waiter had ever been scheduled ("the revoked waiter never
        // woke"). Same defect the milestone-78 family had, found by a *faster* machine rather than
        // a loaded one: a yield count is not a duration in either direction.
        assert!(
            wait_for(|| crate::sched::endpoint_waiting_receivers(ep) == 1),
            "the waiter never blocked on the endpoint, so the reclaim had nothing to wake",
        );

        // Reclaiming the endpoint's region now succeeds: the waiter is woken with an error, not left
        // to strand the reclaim.
        crate::sched::reclaim_region(region)
            .expect("reclaim wakes the blocked waiter rather than refusing");

        // Clock-bounded, not yield-bounded: see `wait_for`. Since §28 the waiter is on another
        // core, so this core's fifty yields can elapse before it has been scheduled at all.
        assert!(
            wait_for(|| WOKE.load(Ordering::SeqCst)),
            "the revoked waiter never woke"
        );
        assert!(
            ABORTED.load(Ordering::SeqCst),
            "the woken waiter did not see its IPC aborted",
        );
    }

    /// **A wake with nothing delivered must not complete a parked receiver's `RECV`** (boot 8,
    /// VisionFive 2, 2026-08-14). The bench dump's shape: the boot thread, parked in `ipc_recv`
    /// on the report endpoint, took a `wake:0x0` on a boot where no sender to that endpoint
    /// existed, and its recv neither completed with a message nor re-parked. The recv tail reads
    /// the mailbox unconditionally after `schedule()` returns, so an undelivered wake completes
    /// the recv with whatever the mailbox happened to hold, and the receiver's TCB is still
    /// linked on the endpoint's wait queue (the waker that owns the unlink never ran), which is
    /// the intrusive one-link invariant broken in kernel memory.
    ///
    /// The claim: a `Blocked` IPC thread may only become `Ready` by the hand that completed its
    /// rendezvous (message staged, signal counted, or abort flagged). An undelivered wake is
    /// refused, the receiver stays parked, and a real sender still reaches it afterwards.
    /// The canary tripwire's contract, both halves: an unchanged watched range reports nothing,
    /// and a byte flipped behind its back is counted (and printed) on the next check. The scratch
    /// is this test's own static, so the live registries are never poked; arming over the real
    /// tables is `canary_arm_registries`, which is plain plumbing over the same `arm`.
    ///
    /// Both checks LOOP until a pass actually runs, and the loop is the fix for a real flake
    /// (thead-c906, 2026-08-15; notes/cpu-models.md BUGS): `check()` is single-flight, timer
    /// ticks on other cores call it too (secondaries are online here), and this test's decisive
    /// call used to lose the slot to a tick's pass that had read the scratch byte *before* the
    /// flip. The old `check()` swallowed that refusal and the flip went uncounted; now it says
    /// `false` and the test insists on a pass of its own.
    #[test_case]
    fn the_canary_reports_a_byte_that_changed_behind_its_back() {
        use core::sync::atomic::AtomicU8;
        static SCRATCH: [AtomicU8; 32] = [const { AtomicU8::new(0xA5) }; 32];
        let base = SCRATCH.as_ptr() as usize;
        super::canary::arm(&[(base, 32)]);
        while !super::canary::check() {
            core::hint::spin_loop();
        }
        assert_eq!(
            super::canary::divergences(),
            0,
            "an unchanged range must not diverge"
        );
        SCRATCH[7].store(0x5A, Ordering::Relaxed);
        // The timer's own check may absorb the flip before this completed pass; either way the
        // count is visible here (this core ran a full pass after the store, and taking the gate
        // acquires whatever an earlier pass counted before releasing it).
        while !super::canary::check() {
            core::hint::spin_loop();
        }
        assert!(
            super::canary::divergences() >= 1,
            "a flipped watched byte must be reported"
        );
        super::canary::disarm();
    }

    #[test_case]
    fn a_wake_without_delivery_cannot_complete_a_parked_recv() {
        static GOT: AtomicU64 = AtomicU64::new(u64::MAX);
        static DONE: AtomicBool = AtomicBool::new(false);
        GOT.store(u64::MAX, Ordering::SeqCst);
        DONE.store(false, Ordering::SeqCst);

        let ep = crate::sched::create_endpoint();
        let tid = crate::sched::spawn(move || {
            let m = crate::sched::ipc_recv(ep);
            GOT.store(m[0], Ordering::SeqCst);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn receiver");

        // Queued on the endpoint, not "probably scheduled by now" (the milestone-81 lesson).
        assert!(
            wait_for(|| crate::sched::endpoint_waiting_receivers(ep) == 1),
            "the receiver never parked on the endpoint"
        );

        // The injection: the real wake path, nothing delivered.
        crate::sched::wake_without_delivery(tid);

        // The receiver must stay parked. Held for half a second of yields rather than one look,
        // because the spurious completion needs the receiver to be scheduled first.
        let deadline = crate::arch::timer::now() + crate::arch::timer::frequency() / 2;
        while crate::arch::timer::now() < deadline {
            assert!(
                !DONE.load(Ordering::SeqCst),
                "a wake with nothing delivered completed the recv (it returned {:#x})",
                GOT.load(Ordering::SeqCst),
            );
            crate::sched::yield_now();
        }
        assert_eq!(
            crate::sched::endpoint_waiting_receivers(ep),
            1,
            "the undelivered wake took the receiver off the endpoint"
        );

        // And the rendezvous still works: a real sender completes the same recv with its message.
        crate::sched::ipc_send(ep, [81, 0, 0]);
        assert!(
            wait_for(|| DONE.load(Ordering::SeqCst)),
            "the real message never arrived after the refused wake"
        );
        assert_eq!(
            GOT.load(Ordering::SeqCst),
            81,
            "the recv completed with something other than the real message"
        );
    }

    /// **A reply only wakes a caller that awaits one** (boot 8's observe-and-strand guard). A
    /// `Reply` capability names a tid, not a wait state. `ipc_reply` used to deliver to any
    /// `Blocked` thread with that tid: invoked against a thread parked as an ordinary endpoint
    /// receiver (a stale reply whose CALL was long since aborted, with the caller re-parked
    /// elsewhere), it clobbered the mailbox and woke the thread messageless while its TCB was
    /// still linked on the endpoint's wait queue. Same strand as the test above, reached through
    /// the one wake site addressed by tid rather than by rendezvous.
    #[test_case]
    fn a_reply_to_a_thread_parked_as_a_receiver_is_dropped() {
        static GOT: AtomicU64 = AtomicU64::new(u64::MAX);
        static DONE: AtomicBool = AtomicBool::new(false);
        GOT.store(u64::MAX, Ordering::SeqCst);
        DONE.store(false, Ordering::SeqCst);

        let ep = crate::sched::create_endpoint();
        let tid = crate::sched::spawn(move || {
            let m = crate::sched::ipc_recv(ep);
            GOT.store(m[0], Ordering::SeqCst);
            DONE.store(true, Ordering::SeqCst);
        })
        .expect("spawn receiver");

        assert!(
            wait_for(|| crate::sched::endpoint_waiting_receivers(ep) == 1),
            "the receiver never parked on the endpoint"
        );

        // A reply aimed at a thread that is not awaiting a reply: dropped, like a reply to a
        // dead caller.
        crate::sched::ipc_reply(tid, [0xDEAD, 0]);

        let deadline = crate::arch::timer::now() + crate::arch::timer::frequency() / 2;
        while crate::arch::timer::now() < deadline {
            assert!(
                !DONE.load(Ordering::SeqCst),
                "a stray reply completed a receiver's recv (it returned {:#x})",
                GOT.load(Ordering::SeqCst),
            );
            crate::sched::yield_now();
        }

        // The mailbox was not clobbered and the rendezvous still works.
        crate::sched::ipc_send(ep, [81, 0, 0]);
        assert!(
            wait_for(|| DONE.load(Ordering::SeqCst)),
            "the real message never arrived after the dropped reply"
        );
        assert_eq!(
            GOT.load(Ordering::SeqCst),
            81,
            "the recv completed with the stray reply's words, not the real message"
        );
    }

    /// Several threads take turns.
    #[test_case]
    fn threads_round_robin() {
        static COUNTS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
        static STOP: AtomicBool = AtomicBool::new(false);

        let mut tids = [0 as crate::thread::Tid; 3];
        for (t, c) in tids.iter_mut().zip(&COUNTS) {
            *t = crate::sched::spawn(move || {
                while !STOP.load(Ordering::SeqCst) {
                    c.fetch_add(1, Ordering::SeqCst);
                    crate::sched::yield_now();
                }
            })
            .expect("spawn failed");
        }

        // Wait ON the property (every thread has run), clock-bounded, rather than asserting after
        // a fixed 300 yields. A yield count is not a duration: §28 scatters these threads across
        // cores, and on a contended host this core can burn its yields before a starved vCPU has
        // run its thread at all, which failed a gate run once as "thread {i} never ran" and passed
        // on the re-run. Widening a wait on the property itself only delays noticing (the smp.rs
        // wait_for argument); the watchdog stays the backstop for a genuine starvation.
        let all_ran = || COUNTS.iter().all(|c| c.load(Ordering::SeqCst) > 0);
        let ran = wait_for(all_ran);
        STOP.store(true, Ordering::SeqCst);
        assert!(ran, "a spawned thread never ran");

        // Wait for the exits, so three threads mid-teardown are not what a later test's frame or
        // thread accounting finds in flight.
        assert!(
            wait_for(|| tids.iter().all(|&t| !crate::sched::thread_present(t))),
            "the round-robin threads were never reaped"
        );
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
    /// function call**: nothing a cooperative scheduler could possibly hook.
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

        // Pin both threads to THIS core, the one the test thread busy-waits on, so this stays a
        // *same-core* preemption test after DECISIONS §28 made the default `spawn` scatter work
        // across cores. The claim under test is that a never-yielding thread cannot monopolize the
        // core it is on; if the spinner ran on some other idle core the timer would never have to
        // preempt anything here, and the test would prove nothing. `spawn_on(cpu::id())` keeps the
        // spinner, the polite thread, and the waiter contending for one core, as they always did.
        let here = crate::cpu::id();

        // The hostile thread. This is the arbitrary ELF binary, in miniature.
        crate::sched::spawn_on(here, || {
            while !STOP.load(Ordering::Relaxed) {
                SPINNING.fetch_add(1, Ordering::Relaxed);
                // Deliberately nothing else. No yield. No call. Nothing to cooperate with.
            }
        })
        .expect("spawn failed");

        // **Wait for the hostile thread to reach a CPU before the polite one exists.**
        //
        // The order used to be: spawn both, wait for the polite thread, set STOP, and only then
        // sample `SPINNING > 0`. That sample is a race the test creates for itself: if the polite
        // thread gets its turn first, STOP is set before the spinner has ever been scheduled, the
        // spinner exits its loop without incrementing anything, and the run goes red with "the
        // spinner never ran at all" while the kernel did nothing wrong. It failed CI exactly that
        // way on 2026-08-04 (`sifive-u54`), on a pull request that changed no kernel code.
        //
        // The spinner running is a **precondition** of the claim, not the claim, so it is waited
        // on rather than sampled. Waiting for it here also strengthens what follows: the polite
        // thread's turn can now only have come from preempting a thread that was genuinely running.
        assert!(
            within_ticks(200, || SPINNING.load(Ordering::Relaxed) > 0),
            "the spinner never reached a CPU in 200 tick periods: it was placed on a run queue \
             and never scheduled, which is a placement or wake failure rather than a preemption one"
        );

        // Counted from here, so the preemptions this test claims are the ones that gave the polite
        // thread its turn, not the ones that started the spinner.
        let preemptions_before = crate::sched::preemptions();

        // A well-behaved thread that just wants a turn.
        crate::sched::spawn_on(here, || {
            OTHER_RAN.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        // And now we wait, WITHOUT yielding either. If preemption does not work, nobody moves and
        // the budget runs out. The budget is 200 *delivered ticks* rather than one second of wall
        // clock: preemption opportunities are what this claim is counted in, and a host that
        // deschedules the emulator produces fewer of them, never more. See `within_ticks`.
        assert!(
            within_ticks(200, || OTHER_RAN.load(Ordering::SeqCst)),
            "TWO HUNDRED TICKS AND THE POLITE THREAD NEVER RAN. The spinner still owns the CPU, \
             which means preemption is not working and a single bad program can hang this \
             machine. This is precisely the failure DECISIONS.md §5 predicted for \
             cooperative scheduling."
        );

        // **And the spinner has to have actually spun before we stop it**, or the test is vacuous:
        // a polite thread running on a core nobody was monopolizing says nothing about preemption.
        // Stopping it the instant the polite thread reported is a race the two orderings decide,
        // and on the physical core under HVF (milestone 81) it came out the other way: the polite
        // thread ran first, `STOP` was set, and the spinner was killed before its first increment
        // ("the spinner never ran at all"). Its own second, since the wait above may have spent all
        // of the first.
        let spin_deadline = crate::arch::timer::now() + crate::arch::timer::frequency();
        while SPINNING.load(Ordering::Relaxed) == 0 {
            assert!(
                crate::arch::timer::now() < spin_deadline,
                "the spinner never ran at all, so nothing was monopolizing this core and the \
                 polite thread's turn proves nothing about preemption"
            );
            core::hint::spin_loop();
        }

        STOP.store(true, Ordering::Relaxed);

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
    /// The reaping cannot happen in `exit()`: a thread cannot unmap the stack it is standing
    /// on. It happens in `schedule()`, from the *next* thread, once we are safely off it. Every
    /// kernel has something called a reaper, and this is why.
    #[test_case]
    fn a_finished_thread_is_reaped_and_its_memory_returned() {
        // Reaping is proven per thread, by `thread_present` on the Tids THIS test spawned, not by
        // the global table headcount returning to a baseline. `thread_count()` is the size of the
        // whole table, so a neighbour's teardown finishing late moves it: it failed on CI as
        // "left: 5, right: 6", a count BELOW its baseline, which eight reaped threads cannot
        // produce but one baseline-counted thread exiting mid-test does. Same shape as the
        // `reclaim_frees_a_started_then_exited_childs_regions` fix; see
        // notes/load-sensitive-assertions.md.
        //
        // Where the reuse probe below found its own stack, **reported by the thread itself**. A test
        // cannot read the stack out of a thread it spawned, because by the time it looks the thread
        // may already have been reaped, which is the very thing this test waits for. A thread taking
        // the address of one of its own locals has no such race.
        static PROBE_SP: AtomicU64 = AtomicU64::new(0);

        fn batch_of_eight() {
            let mut tids = [0 as crate::thread::Tid; 8];
            for t in &mut tids {
                *t = crate::sched::spawn(|| {}).expect("spawn failed");
            }
            // Let them all run and exit, and let the reaper catch up. Clock-bounded, not yield-bounded:
            // §28 can place these on other cores, and a Finished thread is only removed when its own
            // core switches away from it, so no number of yields *here* can make that happen.
            assert!(
                wait_for(|| tids.iter().all(|&t| !crate::sched::thread_present(t))),
                "finished threads were never reaped"
            );
        }

        // The FIRST batch legitimately costs a couple of frames: the stack area is a fresh
        // region of virtual address space, so `map_page` has to build an L2 and an L3 page
        // table for it. Those are a one-time cost, not a leak: `unmap_page` frees the leaf
        // mapping but leaves the intermediate tables standing (see the TODO on `paging::unmap`).
        batch_of_eight();

        // **Reuse is asserted directly, because the frame count below cannot do it.** Measured
        // 2026-08-17: with the `FREE_STACK_VAS` push deleted from `KernelStack::drop`, which IS the
        // milestone-6 bug this test is named for, the entire aarch64 leg passed, this test included.
        // The reason is arithmetic rather than luck. A slot is `STACK_SLOT_SPAN`, 28 KiB, so eight
        // of them consume 224 KiB of fresh address space, and a leaked page table costs a *frame*
        // only when the bump crosses a 2 MiB L3 boundary. 224 KiB is 11% of one table's span, so
        // the frame count can see the defect only when the batch happens to straddle a boundary,
        // and that is worse than 11% random: where `NEXT_STACK_VA` stands here is a function of how
        // many threads the tests BEFORE this one spawned, which is fixed for a given tree. So for
        // any given tree the frame count either always catches the defect or always misses it, and
        // which one is decided by unrelated code upstream. The frame assertion below is the outcome;
        // this is the mechanism, and at that batch size only the mechanism is observable.
        //
        // The claim: a thread spawned after the first batch has been reaped lands BELOW the
        // watermark that stood before it, which is what "it reused a dead thread's range" means.
        //
        // **ONE thread, and the count is the whole argument.** The first version of this asserted it
        // for all eight of a batch and failed on a clean kernel, on thread 1, two slots above the
        // watermark. That was correct behaviour and a wrong assertion: the watermark is the
        // high-water mark of *concurrent* live threads, so a batch whose threads happen to be reaped
        // later relative to spawning legitimately needs more slots than the previous batch did, and
        // bumps it. Asserting over a batch conflated reuse with concurrency, which is this
        // milestone's own defect ("a wait written against something wider than the property")
        // committed while fixing it. Recorded in notes/load-sensitive-assertions.md rather than
        // quietly corrected, because reproducing the family from the inside is the useful part.
        //
        // One thread cannot exceed a high-water mark that eight just set. `thread_present` going
        // false already implies the push happened (`Threads::remove` runs `KernelStack::drop` before
        // it removes the table entry), so the free list holds up to eight of the first batch's slots
        // when this spawns, and a single pop cannot drain it. A neighbour spawning here can only
        // RAISE the watermark, which makes the claim easier: the failure direction is one-way, which
        // is the discipline the rest of this test was rebuilt for.
        //
        // It runs BEFORE the frame baseline below so that the settle loop absorbs its own stack
        // frees, rather than leaving them in flight inside the window the frame assertion measures.
        let watermark = crate::thread::stack_area_span().1;
        PROBE_SP.store(0, Ordering::SeqCst);
        let probe = crate::sched::spawn(|| {
            let local = 0u64;
            PROBE_SP.store(&local as *const u64 as u64, Ordering::SeqCst);
        })
        .expect("spawn failed");
        assert!(
            wait_for(|| !crate::sched::thread_present(probe)),
            "the stack-reuse probe was never reaped"
        );
        let probe_sp = PROBE_SP.load(Ordering::SeqCst);
        assert!(
            probe_sp != 0,
            "the stack-reuse probe never reported which stack it got"
        );
        assert!(
            probe_sp < watermark,
            "a thread spawned after eight were reaped was given FRESH stack address space: its sp \
             {probe_sp:#x} is at or above the {watermark:#x} watermark that stood before it, so a \
             dead thread's range was not reused and an L2 plus an L3 page table leak per 2 MiB \
             consumed, forever"
        );

        // Sample the frame baseline only once it has STOPPED MOVING: a reaped thread's stack
        // frames are freed by `finish_switch` on whatever core reaps it, a beat after the thread
        // leaves the table, so reading `used` the instant the Tids are gone races the first
        // batch's own frees. Two agreeing samples a yield apart mean nothing is in flight, and
        // `wait_for`'s deadline keeps a genuinely unstable allocator a failure rather than a spin.
        let used = || crate::memory::stats().unwrap().used;
        let mut last = used();
        assert!(
            wait_for(|| {
                crate::sched::yield_now();
                let prev = core::mem::replace(&mut last, used());
                prev == last
            }),
            "frame accounting never settled after the first batch"
        );
        let before = last;

        // The SECOND batch must allocate NOTHING it keeps. The page tables exist, and the dead
        // threads' virtual address ranges went back on the free list, so eight new threads land
        // in the same addresses with the same tables.
        //
        // If this ever regresses, the kernel leaks two frames of page tables per 2 MiB of stack
        // address space consumed, forever, and threads come and go.
        batch_of_eight();

        // `<=`, not `==`, and the direction is the argument: a leak leaves `used` ABOVE `before`
        // and never comes back, so the wait times out and fails. A neighbour's late teardown
        // landing in this window can only FREE frames, pushing `used` below `before`, and holding
        // still is not a property this test can demand of the rest of the machine.
        //
        // **This comment used to end "sensitivity to the milestone-6 bug is unchanged: every
        // leaked frame keeps `used() <= before` false", and that was true of the arithmetic and
        // false about the bug.** It is unchanged from the `==` form, which is what it was written
        // to defend, and both forms are near-blind: the defect leaks page tables per 2 MiB of
        // address space and eight threads consume 224 KiB, so there is usually no leaked frame for
        // either form to see. Corrected 2026-08-17 by deleting the VA push and watching the leg go
        // green. What this assertion is genuinely responsible for is the leak that *does* show at
        // this batch size, a per-thread frame the reaper failed to return; the reuse claim above is
        // what covers the defect in the test's name.
        //
        // The number in the message is the one the wait DECIDED on, not a fresh sample. Re-reading
        // `used()` to format the panic races the frames still arriving, so a genuine timeout could
        // report a delta of zero: `saturating_sub` clamped the negative case away instead of making
        // it impossible, and "leaked 0 frames" is the same unreadable diagnostic as the "-52" this
        // milestone is named for, minus the sign that gave it away. `wait_for` re-evaluates the
        // predicate once after its deadline, so a `false` return leaves `seen > before` and the
        // count below cannot be zero. See notes/load-sensitive-assertions.md.
        let mut seen = before;
        let came_back = wait_for(|| {
            seen = used();
            seen <= before
        });
        assert!(
            came_back,
            "a second batch of eight threads leaked {} frames: stack address ranges are not \
             being reused, so page tables accumulate forever",
            seen - before
        );
    }

    /// Every thread stack has a guard page.
    ///
    /// A thread stack is 24 KiB (under half the boot stack's), and threads are where deep
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

    /// **A fatal fault can name the stack it fell off**, for all three kinds of kernel stack.
    ///
    /// Milestone 78. The guard pages already worked; nothing *said* so. Two `cpu matrix` runs died
    /// with `unexpected RISC-V trap: scause=0xf stval=0xffffffd0001fe000 from_user=false`, which
    /// reads as a memory-system fault, and it took hand arithmetic against `thread.rs`'s slot span
    /// to discover that the address was the base of a thread stack's guard page. `guard_page_at` is
    /// that arithmetic, in the kernel, so the machine says it instead.
    ///
    /// **That `stval` is one of only two addresses this project's guard-page faults ever used**,
    /// and the other is aarch64's `0xffff0010001b3000`. An earlier version of this paragraph read
    /// that as proof of a fixed-site writer, on the argument that "a depth-driven overflow does not
    /// repeat an address". **It does, and exactly this one** (2026-08-17): a fault that reaches the
    /// exception vector's own frame store walks `sp` down a frame at a time and stores upward in
    /// aligned steps, so the terminal store lands on the guard base exactly, whatever `sp` was
    /// doing. The address carried no information; the *slot number* did, and it survived a change
    /// to the slot span. See notes/stack.md, "a kernel stack freed under its owner".
    ///
    /// The three kinds are allocated three different ways (a linker symbol, a `.bss` array, a slot
    /// in the virtual area 64 GiB up), so this checks one of each rather than trusting one to stand
    /// for the others. It also checks the *negatives*, because a classifier that answered
    /// `Thread(0)` for every address would pass the positive half and be worse than nothing.
    #[test_case]
    fn a_guard_page_fault_names_its_stack() {
        use crate::arch::mmu;
        use crate::stack::{GuardPage, guard_page_at};
        use crate::thread::{KernelStack, STACK_SLOT_SPAN};

        assert_eq!(guard_page_at(mmu::stack_guard()), Some(GuardPage::Boot));
        assert_eq!(
            guard_page_at(mmu::stack_guard() + 4095),
            Some(GuardPage::Boot),
            "the last byte of the guard page is still the guard page"
        );
        assert_eq!(
            guard_page_at(mmu::stack_bottom()),
            None,
            "the first usable byte of the stack is not a guard page"
        );

        // Slot 1's guard address is a static layout fact, not a runtime one: the classifier does
        // range math over addresses that exist for every slot, online or parked, so this needs no
        // core 1 at all. (An earlier comment here justified the index by core 1 being online,
        // which was the wrong reason and read as an online-set assumption.)
        let g = crate::smp::secondary_stack_guard(1);
        assert_eq!(guard_page_at(g), Some(GuardPage::Secondary(1)));

        // A live thread stack, so the watermark provably covers it.
        let stack = KernelStack::new().expect("could not allocate a thread stack");
        let slot = (stack.guard() - crate::thread::stack_area_span().0) / STACK_SLOT_SPAN;
        assert_eq!(guard_page_at(stack.guard()), Some(GuardPage::Thread(slot)));
        assert_eq!(
            guard_page_at(stack.bottom()),
            None,
            "the stack's own first page reported as its guard page"
        );
        assert_eq!(
            guard_page_at(stack.top() - 8),
            None,
            "the top of the stack reported as a guard page"
        );

        // Kernel text is not a stack of any kind.
        assert_eq!(guard_page_at(guard_page_at as *const () as u64), None);
    }

    /// **A dead thread that has not left its own kernel stack is not reapable**, and that one
    /// clause is the whole of the bug that produced four `*** KERNEL STACK OVERFLOW ***` panics in
    /// CI over five days without any stack ever overflowing.
    ///
    /// `depart` publishes a supervised thread as `Dead` and delivers its death message, waking the
    /// supervisor, *before* it reaches `switch_to`. A supervisor that reaps in that window used to
    /// free the `Thread`, and `KernelStack::drop` unmaps six pages with a real `tlbi` under a core
    /// that is still running on them. The corpse's next store faulted, the exception vector's own
    /// frame store faulted on the same dead stack, and the vector walked `sp` down one 272-byte
    /// frame at a time until it landed in the mapped stack below, which is why the reported address
    /// was the slot base every single time and never moved.
    ///
    /// The rule is stated over `(state, on_cpu)` rather than over a live thread table because the
    /// window is a few hundred instructions wide on two cores, which is not a thing a test can
    /// stage. It reproduced on a desk only with a deliberate spin loop inserted in `depart`; see
    /// notes/stack.md, "a kernel stack freed under its owner". What this pins is the claim, so the
    /// next person to edit that loop meets `on_cpu` as a requirement rather than as a detail.
    #[test_case]
    fn a_dead_thread_still_standing_on_its_stack_is_not_reapable() {
        use super::{RegionReap, State, region_reap_verdict};

        // Off its stack: the states that mean "never runs again" really are reapable, which is
        // what makes region teardown work at all.
        for state in [State::Dead, State::Finished, State::Embryo] {
            assert_eq!(
                region_reap_verdict(state, false),
                RegionReap::Reap,
                "{state:?} with no core on its stack must be reapable",
            );
        }

        // Still on its stack: refused, and refused WITHOUT arming a kill, because there is nothing
        // left to kill and the condition clears itself one context switch from now.
        for state in [State::Dead, State::Finished] {
            assert_eq!(
                region_reap_verdict(state, true),
                RegionReap::RefuseStanding,
                "{state:?} still standing on its kernel stack must not have that stack unmapped",
            );
        }

        // A thread that can still be scheduled is the older refusal, and it still arms the kill.
        for state in [State::Ready, State::Running, State::Blocked] {
            assert_eq!(
                region_reap_verdict(state, false),
                RegionReap::RefuseAndArm,
                "a live {state:?} thread must be refused and armed",
            );
            assert_eq!(
                region_reap_verdict(state, true),
                RegionReap::RefuseAndArm,
                "being on a cpu must not downgrade a live thread's refusal to the passive one",
            );
        }
    }

    /// **The slots are contiguous, so one stack's guard page begins where the previous stack
    /// ends**, and a fault report has to be able to say which of the two it is looking at.
    ///
    /// This is the geometry that made two 2026-08-16 guard-page faults ambiguous. Both landed on a
    /// slot's guard page at offset 0 and 8, and the report said "sp went 4096 bytes past the
    /// bottom" without ever reading `sp`. Those same two addresses are also the first two words
    /// **above the top of the stack in the slot below**, which is a completely different bug with
    /// a completely different fix, and nothing printed could tell them apart.
    ///
    /// `thread_stack_site` is what lets the report place `sp` in the same units as the faulting
    /// address. The assertions below are the arithmetic that claim rests on: a slot's usable span
    /// measured from its own bottom, its guard page as negative offsets, and the join, where slot
    /// `n`'s guard base is exactly one past slot `n-1`'s last usable byte.
    #[test_case]
    fn a_slots_guard_page_begins_where_the_slot_below_it_ends() {
        use crate::stack::thread_stack_site;
        use crate::thread::{KernelStack, STACK_PAGES, STACK_SLOT_SPAN};

        let stack = KernelStack::new().expect("could not allocate a thread stack");
        let (area, _) = crate::thread::stack_area_span();
        let slot = (stack.guard() - area) / STACK_SLOT_SPAN;

        assert_eq!(
            thread_stack_site(stack.bottom()),
            Some((slot, 0)),
            "the lowest usable byte is zero bytes above the bottom"
        );
        assert_eq!(
            thread_stack_site(stack.top() - 8),
            Some((slot, (STACK_PAGES * 4096) as i64 - 8)),
            "the last usable word is one word short of the stack's size above the bottom"
        );
        assert_eq!(
            thread_stack_site(stack.guard()),
            Some((slot, -4096)),
            "the guard page's base is a whole page below the bottom"
        );

        // The join. `top` is exclusive, so it is the next slot's guard base, and the site function
        // must report it as *that* slot's guard rather than as this one's stack.
        assert!(
            slot >= 1,
            "the first stack allocated in the suite is not slot 0"
        );
        assert_eq!(
            thread_stack_site(area + slot * STACK_SLOT_SPAN),
            Some((slot, -4096)),
        );
        assert_eq!(
            thread_stack_site(area + slot * STACK_SLOT_SPAN - 8),
            Some((slot - 1, (STACK_PAGES * 4096) as i64 - 8)),
            "the word below a slot's guard base belongs to the previous slot's stack, and a report \
             that cannot say so cannot tell an overflow from a store past a neighbour's top",
        );

        // Outside the area entirely: kernel text is not a thread stack.
        assert_eq!(
            thread_stack_site(thread_stack_site as *const () as u64),
            None
        );
    }

    /// **The rendezvous, receiver-first.** A thread blocks on an empty endpoint, and stays
    /// blocked, and a *later* sender is what frees it: carrying the message.
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

        // Clock-bounded, not yield-bounded: see `wait_for`.
        assert!(
            wait_for(|| RECEIVED.load(Ordering::SeqCst)),
            "the receiver never woke"
        );
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
        // Five words now (the top two are the fault path's, DECISIONS §26); an ordinary send fills
        // the first three and leaves the rest zero.
        assert_eq!(
            msg,
            [0x1234, 0x5678, 0x9abc, 0, 0],
            "wrong message received"
        );

        // Clock-bounded, not yield-bounded: see `wait_for`. This one has evidence rather than a
        // theory behind it: under eight spinning host processes it failed here on `rv64` on
        // 2026-08-04, because fifty yields on an idle core are microseconds and the sender was on
        // a vCPU the host had descheduled.
        assert!(
            wait_for(|| SENT_RETURNED.load(Ordering::SeqCst)),
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

        assert!(
            spin_until(|| DONE.load(Ordering::SeqCst)),
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
        // Each spawn is followed to its own reap by name. This used to take `thread_count()` as a
        // baseline and spin `while thread_count() > baseline { yield_now() }`, which is the whole
        // family in three lines: the headcount is moved by every other test's teardown, so the
        // loop could exit at once (a neighbour reaping first) and leave this batch's stacks in
        // flight, or never exit at all (a neighbour's thread outliving the batch) with no clock to
        // stop it, spinning until the harness's 90 s ceiling with a message about kernel stacks.
        // `thread_present` on the Tid each spawn returned asks the narrow question, and `wait_for`
        // supplies the bound the yield loop never had.
        let settle = |tid| {
            assert!(
                wait_for(|| !crate::sched::thread_present(tid)),
                "a spawned thread was never reaped, so the frame count below would be read \
                 mid-teardown"
            );
        };

        // Warm up: reach steady state (first spawn after boot may still be settling VAs).
        for _ in 0..2 {
            settle(super::spawn(|| {}).expect("warmup spawn"));
        }

        let free_before = crate::memory::stats().unwrap().free();
        for _ in 0..6 {
            settle(super::spawn(|| {}).expect("spawn failed"));
        }

        // `>=`, not `==`, and the direction is the argument, the same one the reaper test's frame
        // half carries. The defect this guards spends allocator frames on kernel stacks, which
        // drives `free` DOWN and keeps it there, so the wait times out and fails exactly as
        // before. A neighbour's late teardown landing in this window can only FREE frames, pushing
        // `free` ABOVE the baseline, and equality additionally demanded that the rest of the
        // machine hold still for the duration, which is not a property this test is responsible
        // for. See notes/load-sensitive-assertions.md.
        //
        // And the number in the message is the one the wait decided on, for the reason the reaper
        // test's frame half carries at length: a re-sampled `saturating_sub` reports zero when the
        // frames land between the wait giving up and the panic being formatted, which is a red run
        // whose message denies there is anything wrong with it.
        let mut seen = free_before;
        let recovered = wait_for(|| {
            seen = crate::memory::stats().unwrap().free();
            seen >= free_before
        });
        assert!(
            recovered,
            "six threads came and went and the frame allocator lost {} frames: a kernel stack is \
             still drawing from the allocator instead of the kernel budget",
            free_before - seen,
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
        spin_until(|| GOT.load(Ordering::SeqCst) != 0);
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

        assert!(
            spin_until(|| DONE.load(Ordering::SeqCst)),
            "the call never returned"
        );
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

        spin_until(|| GOT_A.load(Ordering::SeqCst) != 0 && GOT_B.load(Ordering::SeqCst) != 0);
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
    /// If `Blocked` were not respected in `schedule()`, if a blocked thread were helpfully
    /// requeued: this would still pass, so it is not the whole story (the two rendezvous tests
    /// above are). But it is the cheap, direct statement of what blocking is *for*: a waiting
    /// thread must not burn the CPU.
    #[test_case]
    fn other_threads_run_while_one_is_blocked() {
        static PROGRESS: AtomicU64 = AtomicU64::new(0);
        static STOP: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();

        PROGRESS.store(0, Ordering::SeqCst);
        STOP.store(false, Ordering::SeqCst);

        let blocked = super::spawn(move || {
            super::ipc_recv(ep); // blocks forever (nobody sends); must not starve the worker
        })
        .expect("spawn failed");

        let worker = super::spawn(|| {
            while !STOP.load(Ordering::SeqCst) {
                PROGRESS.fetch_add(1, Ordering::SeqCst);
                super::yield_now();
            }
        })
        .expect("spawn failed");

        // Clock-bounded, not yield-bounded: see `wait_for`. It failed here on `sifive-u54` and
        // `rva22s64` under eight spinning host processes on 2026-08-04, which is the same lesson
        // `threads_round_robin` learned three tests up: a hundred yields is not a duration, and on
        // a contended host this core burns them before the worker's vCPU has run at all.
        let progressed = wait_for(|| PROGRESS.load(Ordering::SeqCst) > 0);
        STOP.store(true, Ordering::SeqCst);

        assert!(
            progressed,
            "a worker made no progress while another thread was blocked on IPC",
        );

        // Free the blocked receiver so it does not sit in the endpoint queue forever, and wait for
        // BOTH threads to actually be gone. Twenty yields used to be the wait, which is the same
        // "count is not a duration" defect one level down: this test's teardown landing late is
        // precisely the neighbouring state that made other tests' frame and thread accounting fail
        // (notes/load-sensitive-assertions.md).
        super::ipc_send(ep, [0, 0, 0]);
        assert!(
            wait_for(
                || !crate::sched::thread_present(blocked) && !crate::sched::thread_present(worker)
            ),
            "this test's own threads had not finished when it returned",
        );
    }

    /// **An interrupt becomes a message.** DECISIONS §10 and notes/interrupts.md, executed.
    ///
    /// A thread blocks waiting on an interrupt it can only name through an endpoint. We raise the
    /// interrupt from software, the kernel's handler turns it into a notification, and the blocked
    /// thread wakes. This is the exact path a userspace driver takes when a real device interrupts.
    ///
    /// **The two ISAs raise it differently, and they are not twins in what they cost to raise.** See
    /// [`raise_test_irq`]: aarch64 sends itself a GIC SGI, which needs no device at all; RISC-V has
    /// no SGI, so it makes the console UART assert its own line into the PLIC with one register
    /// write. The kernel path under test is the same on both (the handler routes the interrupt to an
    /// endpoint and signals it), and RISC-V's leg additionally covers the PLIC claim/mask/complete
    /// handshake that an SGI on aarch64 does not reach. What RISC-V gives up is aarch64's "minus the
    /// device" property. The alternative there was the SBI's IPI, which arrives as a *software*
    /// interrupt down a different arm of the trap dispatcher and would not have touched
    /// `irq_route`/`irq_notify` at all, so it would have proved less while looking like more.
    #[test_case]
    fn an_interrupt_becomes_a_message() {
        static WOKE: AtomicBool = AtomicBool::new(false);

        let ep = super::create_endpoint();
        super::bind_irq(delivery_irq(), ep);
        arm_test_irq(delivery_irq());

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

        // Fire it. The controller delivers it, the handler routes it to `ep`, the waiter wakes.
        raise_test_irq(delivery_irq());

        let woke = spin_until(|| WOKE.load(Ordering::SeqCst));
        quiet_test_irq();
        assert!(
            woke,
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
        // and its QuotaToken drops, returning the slot. Clock-bounded (milestone 81): the 100
        // yields this used to spend are microseconds on the physical core, well before another
        // core has run the woken child to completion.
        super::ipc_send(ep, [0, 0, 0]);
        // Clock-bounded, not yield-bounded: see `wait_for`. The slot comes back when the child is
        // *reaped*, which happens on whichever core it ran on, so a yield count here measures this
        // core's idleness rather than that child's teardown. Waiting on the budget itself is also
        // the exact property: the assertion below is the confirmation, not the wait.
        assert!(
            wait_for(|| BUDGET.load(Ordering::Relaxed) > 0),
            "a child exited but its quota slot was never returned to the budget",
        );

        // A slot is free again.
        assert!(
            wait_for(|| super::spawn_with_quota(&BUDGET, || {}).is_some()),
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
    ///
    /// Raised the same two ways as `an_interrupt_becomes_a_message`, with the same caveat about
    /// what each ISA's raise does and does not cost (see [`raise_test_irq`]).
    #[test_case]
    fn an_interrupt_that_arrives_before_the_wait_is_not_lost() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let ep = super::create_endpoint();
        super::bind_irq(pending_irq(), ep);
        arm_test_irq(pending_irq());

        // Fire it with NOBODY waiting. The signal must be counted.
        let routed = ROUTED_IRQS.load(Ordering::Relaxed);
        raise_test_irq(pending_irq());
        // Wait for the handler to have actually run, rather than for a fixed number of yields: a
        // yield elapses in no real time on an idle core (DECISIONS §28), and under SMP the interrupt
        // may be taken on another core entirely, so counting yields here would be counting nothing.
        let delivered = spin_until(|| ROUTED_IRQS.load(Ordering::Relaxed) > routed);
        quiet_test_irq();
        assert!(
            delivered,
            "the interrupt was raised but the handler never routed it, so this test could not \
             reach the question it exists to ask",
        );

        static SAW: AtomicBool = AtomicBool::new(false);
        super::spawn(move || {
            super::ipc_recv(ep); // must return immediately: the signal is pending
            SAW.store(true, Ordering::SeqCst);
        })
        .expect("spawn failed");

        assert!(
            spin_until(|| SAW.load(Ordering::SeqCst)),
            "an interrupt that fired before the WAIT was lost",
        );
    }

    /// The kernel's endpoint supply grows past one chunk, and a retired chunk's endpoints keep working.
    ///
    /// This exists because `KERNEL_EP_PAGES` used to be a ceiling that grew with the *test suite*
    /// rather than the system, so every few merges someone hit a panic telling them to raise a
    /// constant for a reason no single branch had caused. Growth on demand retires that, and this test
    /// is what keeps it retired.
    ///
    /// Creating `KERNEL_EP_CHUNK_PAGES + 1` endpoints crosses a chunk boundary wherever in the current
    /// chunk we happen to start, so the carve-a-new-chunk path is exercised rather than assumed. The
    /// second assertion is the one that matters more: an endpoint minted *before* the transition must
    /// still resolve afterwards, which is what proves that forgetting a filled chunk's handle
    /// (deliberate, see the field's doc comment) does not orphan the endpoints living in it.
    #[test_case]
    fn the_kernels_endpoint_supply_grows_past_one_chunk() {
        let mut names = [0u64; super::KERNEL_EP_CHUNK_PAGES as usize + 1];
        for slot in names.iter_mut() {
            *slot = super::create_endpoint();
        }

        // Distinct names: a chunk transition that handed back the same page twice would show here.
        for (i, &a) in names.iter().enumerate() {
            for &b in &names[i + 1..] {
                assert_ne!(a, b, "two endpoints share a name across a chunk transition");
            }
        }

        // Every one still resolves, including the earliest, which is in a chunk we have since retired.
        let mut guard = super::SCHED.lock();
        let sched = guard.as_mut().expect("no scheduler");
        for (i, &ep) in names.iter().enumerate() {
            assert!(
                super::endpoint_of(sched, ep).is_some(),
                "endpoint {i} stopped resolving after the supply grew",
            );
        }
    }
}
