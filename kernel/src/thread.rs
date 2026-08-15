//! Kernel threads.
//!
//! # What a thread actually is
//!
//! From notes/registers.md, milestone 1, before any of this existed:
//!
//! > **A thread is a stack plus a set of register values.** That is not a metaphor. It is the
//! > complete and literal definition.
//!
//! And that is exactly what a [`Thread`] is here: a [`KernelStack`], and a single **stack
//! pointer** naming the place on that stack where its registers are saved. Nothing else. The
//! `context` field is 8 bytes, and it is the whole of a suspended thread's CPU state, because
//! everything else is sitting on the stack it points at.
//!
//! # Every thread gets a guard page
//!
//! Milestone 3 blew the boot stack, wrote through `.bss` and `.data` into `.text`, and hung
//! the machine for 150 seconds with no output. Milestone 4 gave the *boot* stack a guard page,
//! and the same bug became an instant, precise fault naming the exact byte that went too far.
//!
//! Thread stacks get one too, and it is not decoration: **a thread stack is 16 KiB**, an eighth
//! of the boot stack's, and threads are where deep recursion actually happens. This is the
//! first non-test user of `mmu::map_page` / `mmu::unmap_page`, which we built at milestone 4
//! ahead of any caller precisely so the discipline (break-before-make, an un-ignorable TLB
//! flush) would be right the first time.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use frames::FRAME_SIZE;
use paging::Flags;

use crate::arch::mmu::{self, KERNEL_VA_BASE};
use crate::sync::{IrqSafeMutex, rank};

pub type Tid = u64;

/// 16 KiB. Four pages.
///
/// Linux uses 16 KiB per kernel thread on arm64, for the same reason: you have thousands of
/// them and each one costs real memory. If that sounds tight, remember the guard page below
/// it turns "too small" from a silent corruption into a legible fault.
pub const STACK_PAGES: usize = 4;

/// Where kernel thread stacks live, virtually.
///
/// Deliberately far above the direct map (which occupies `KERNEL_VA_BASE | pa` for every real
/// physical address), so a stack address can never collide with the virtual *name* of a
/// physical one. 64 GiB up, and RAM will not reach there for a while.
const STACK_AREA: u64 = KERNEL_VA_BASE | 0x0000_0010_0000_0000;

/// One thread's slot in [`STACK_AREA`]: the guard page, then [`STACK_PAGES`] of stack. Every slot
/// is this wide and every base is a multiple of it from `STACK_AREA`, including the reused ones
/// (`FREE_STACK_VAS` hands back the slot base, never an interior address), which is what lets a
/// fault handler turn an address back into "slot N, this far into its guard page". See
/// [`crate::stack::guard_page_at`].
pub const STACK_SLOT_SPAN: u64 = (STACK_PAGES as u64 + 1) * FRAME_SIZE;

static NEXT_STACK_VA: AtomicU64 = AtomicU64::new(STACK_AREA);

/// The thread-stack area as `(base, watermark)`: every stack slot ever handed out lies below the
/// watermark, and nothing else in the kernel map lies in the span at all.
///
/// Reads one relaxed atomic and nothing else, deliberately: the caller is a fault handler that has
/// already lost the machine, so it may not take a lock. A slot allocated concurrently on another
/// core can be missing from the range, which costs a diagnosis and never a wrong one.
pub fn stack_area_span() -> (u64, u64) {
    (STACK_AREA, NEXT_STACK_VA.load(Ordering::Relaxed))
}

/// The `id` a constructor writes before the scheduler's table has named the thread. Deliberately
/// `u64::MAX` (= `cpu::NO_TID`), which the generational table can never mint, so a thread that
/// somehow escaped naming resolves to nothing instead of to slot 0. Every insert path overwrites
/// it via `Table::insert_with` (milestone 14 phase A; design/kernel-objects-from-untyped.md).
pub const UNNAMED: Tid = u64::MAX;

/// Stack address ranges from threads that have exited.
///
/// **Reusing these is not a micro-optimization.** Bump-allocating virtual addresses forever
/// means every 2 MiB of address space consumed permanently costs an L2 and an L3 page table,
/// because `unmap_page` frees the leaf mapping but leaves the intermediate tables standing (see
/// the TODO on `paging::unmap`). Threads come and go; the tables would only ever accumulate.
///
/// Handing the address range back means a new thread lands in page tables that already exist,
/// and the whole system reaches a steady state. A test asserts that a second batch of threads
/// costs **exactly zero** additional frames.
static FREE_STACK_VAS: IrqSafeMutex<FreeVas> = IrqSafeMutex::new(rank::STACK_VA, FreeVas::new());

/// A fixed stack of reusable stack-VA ranges (milestone 14 phase B.1). Bounded by construction:
/// a range is pushed only when a thread dies and popped when one spawns, so the free count can
/// never exceed the most threads that ever lived at once, which the scheduler caps at
/// `MAX_THREADS` (= 128; sched.rs). The array is sized to that bound, and the debug assert is the
/// cross-check.
struct FreeVas {
    vas: [u64; 128],
    len: usize,
}

impl FreeVas {
    const fn new() -> Self {
        Self {
            vas: [0; 128],
            len: 0,
        }
    }

    fn pop(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.vas[self.len])
    }

    fn push(&mut self, va: u64) {
        debug_assert!(
            self.len < self.vas.len(),
            "more dead stack ranges than MAX_THREADS"
        );
        if self.len < self.vas.len() {
            self.vas[self.len] = va;
            self.len += 1;
        } // else: leak the VA range rather than corrupt; unreachable per the bound above
    }
}

/// The saved thread context (`arch::Context`) and the context switch (`arch::switch_to`) are
/// arch-specific by nature: a context *is* a particular CPU's callee-saved register set. `thread.rs`
/// treats a `Context` as opaque, it only stores one and hands it to `switch_to`, and builds a fresh
/// one through the two `for_*_thread` constructors in `arch`. Re-exported here so the thread
/// subsystem's callers (`sched`) keep naming them through `crate::thread`. See notes/riscv-port.md.
pub use crate::arch::{Context, switch_to};

/// A stack, with an unmapped page beneath it.
///
/// The frame list is a fixed array (milestone 14 phase B.1): a kernel stack is always exactly
/// [`STACK_PAGES`] frames, so there was never anything dynamic about it but the container.
pub struct KernelStack {
    guard: u64,
    bottom: u64,
    top: u64,
    /// The physical pages backing the stack, from the kernel's own budget (`kmem`, milestone
    /// 19c.1). Physical addresses, not `Frame`s, because they belong to the kernel object
    /// region and return to it (recycled) rather than to the frame allocator: the kernel's
    /// stack spending is bounded by a boot carve now, not open-ended. `0` marks a page that was
    /// never mapped (a partial-build failure path).
    pages: [u64; STACK_PAGES],
}

impl KernelStack {
    pub fn new() -> Option<Self> {
        // One page of virtual address space for the guard, plus the stack itself. The guard's
        // VA is simply never mapped, which is the entire mechanism.
        let span = STACK_SLOT_SPAN;

        // Reuse a dead thread's address range if there is one, so the page tables covering it
        // are already built. Only bump into fresh address space when there isn't.
        let base = FREE_STACK_VAS
            .lock()
            .pop()
            .unwrap_or_else(|| NEXT_STACK_VA.fetch_add(span, Ordering::Relaxed));

        let guard = base;
        let bottom = base + FRAME_SIZE;
        let top = bottom + STACK_PAGES as u64 * FRAME_SIZE;

        let mut pages = [0u64; STACK_PAGES];
        for (i, slot) in pages.iter_mut().enumerate() {
            // From the kernel's own budget (19c.1), recycled from dead stacks, not the frame
            // allocator. This is what makes "the kernel cannot spend beyond its boot carve"
            // true of stacks, the last open-ended kernel draw milestone 14 had not closed.
            let Some(phys) = crate::kmem::page() else {
                return None; // `pages` so far are recorded; Drop recycles what we did map
            };
            let va = bottom + i as u64 * FRAME_SIZE;

            if mmu::map_page(va, phys, Flags::kernel_data()).is_err() {
                crate::kmem::recycle(phys); // never mapped: straight back to the budget
                return None; // Drop handles the earlier, mapped pages
            }
            *slot = phys;
        }

        // Paint the whole stack for the high-water report (milestone 84): every page is mapped and
        // no thread has run on it, so there is no live portion to skip.
        //
        // SAFETY: the loop above mapped every page of `[bottom, top)` and returned early on any
        // failure, and this `KernelStack` has not been handed to a thread yet, so nothing is on it.
        #[cfg(test)]
        unsafe {
            crate::stack::paint(bottom, top);
        };

        Some(KernelStack {
            guard,
            bottom,
            top,
            pages,
        })
    }

    /// Where `sp` starts. The stack grows **down** from here (notes/stack.md).
    pub fn top(&self) -> u64 {
        self.top
    }

    /// The unmapped page below the stack. Test support.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn guard(&self) -> u64 {
        self.guard
    }

    /// The lowest usable byte. Test support.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bottom(&self) -> u64 {
        self.bottom
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        // Measure before unmapping (milestone 84). The reaper runs this on the successor's stack,
        // never the one being scanned. Skip a partial build (some pages never mapped, so a scan
        // would fault) and note it was never painted or used anyway.
        #[cfg(test)]
        if self.pages.iter().all(|&p| p != 0) {
            // SAFETY: every page is mapped (the `all` above is exactly that check), still mapped
            // because the unmap loop below has not run, and `new` painted the whole span.
            let used = unsafe { crate::stack::high_water(self.bottom, self.top) };
            crate::stack::note_thread_stack_use(used);
        }

        for (i, &phys) in self.pages.iter().enumerate() {
            if phys == 0 {
                continue; // a page a failed build never mapped
            }
            let va = self.bottom + i as u64 * FRAME_SIZE;

            // `unmap_page` discharges the TLB obligation with a real `tlbi`. It has to, and the
            // reason is right here: this virtual address is about to be handed to a **different
            // thread's stack**. A stale translation would let the new thread read (and write)
            // the dead thread's saved registers. See notes/page-tables.md.
            if mmu::unmap_page(va).is_ok() {
                crate::kmem::recycle(phys); // home to the kernel budget, not the frame allocator
            }
        }

        // Hand the address range back, so the next thread lands in page tables that already
        // exist. The physical pages were recycled above; this returns the *names*.
        FREE_STACK_VAS.lock().push(self.guard);
    }
}

/// **Where a thread is in its life.** The enum itself lives in `crates/wake_handshake` now,
/// because the block/wake transitions that read and write it were lifted there for loom to search
/// (the fourth bench stop's retrofit; see that crate's header and notes/interleaving.md). The
/// kernel keeps its vocabulary: `State` here is exactly `wake_handshake::RunState`, and nothing in
/// `sched.rs` reads any differently than it did.
pub use wake_handshake::RunState as State;

/// **Which side of a rendezvous a blocked thread is waiting as.** The role half of
/// [`Thread::wait_on`], recorded at the same instant `state` goes `Blocked` and by the same code,
/// so a hang dump can say *what kind* of wait a thread is in rather than only that it waits.
///
/// `Reply` is a `CALL` caller: it is waiting for its one-shot Reply capability to be invoked, and
/// (in the rendezvous-met case) it sits on **no** endpoint queue at all, which is exactly the wait
/// a dump could previously not distinguish from a lost wakeup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitRole {
    /// Parked on an endpoint's sender queue (a `SEND`/`SEND_CAP` with no receiver, or a corpse
    /// holding its death message).
    Sender,
    /// Parked on an endpoint's receiver queue (a `RECV`/`RECV_CAP` with nothing to take).
    Receiver,
    /// A `CALL` caller blocked until `REPLY`; queued as a sender only if no server was waiting.
    Reply,
}

/// **A reserved slot in a spawner's resource budget, returned when this thread dies.**
///
/// A process (the shell, say) that spawns children can be given a quota: at most N children alive
/// at once. Reserving a slot is an atomic decrement; a `QuotaToken` holds that reservation, and
/// its `Drop` gives it back. Because the token lives inside the `Thread`, the slot is returned at
/// exactly the moment the reaper drops the thread: a well-behaved child that exits frees its slot,
/// and a child that blocks forever keeps holding it, which is correct: it is still consuming a
/// thread, a stack, and an address space. This is what bounds kernel memory against a spawn flood
/// or a leaked-thread accumulation without any per-tick bookkeeping. See notes/quotas.md.
pub struct QuotaToken(&'static AtomicU32);

impl QuotaToken {
    /// Called only by `sched::spawn_with_quota`, which has no caller of its own today.
    #[allow(dead_code)]
    pub fn new(budget: &'static AtomicU32) -> Self {
        QuotaToken(budget)
    }
}

impl Drop for QuotaToken {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a blocked thread waits on: the endpoint and the side of the rendezvous it waits as. The
/// payload of [`wake_handshake::Handshake::wait_on`], opaque to that crate, matched by the kernel
/// (`ipc_reply`'s reply-role check, the hang dump's wait column).
pub type Wait = (crate::sched::EpId, WaitRole);

pub struct Thread {
    pub id: Tid,

    /// **The block/wake handshake**: `state` plus the `on_cpu`/`wake_pending`/`wait_on`/
    /// `ipc_served`/`ipc_aborted` protocol that used to be five loose fields here. Lifted into
    /// `crates/wake_handshake` so loom can search its interleavings on the host, and embedded so
    /// the kernel **calls** the checked transitions rather than mirroring them (the regions-crate
    /// precedent). Every access is under `SCHED`, exactly as before; the crate's header carries
    /// the protocol's rules, its races and its BUGS.
    pub handshake: wake_handshake::Handshake<Wait>,

    /// **The entire saved CPU state of this thread**: one stack pointer.
    ///
    /// Everything else lives on the stack it points at, pushed there by `switch_to`. Eight
    /// bytes. That is what "a thread is a stack plus a set of register values" means when you
    /// write it down.
    pub context: *mut Context,

    /// `None` for the boot thread, which runs on the stack `boot.s` set up and does not own it.
    ///
    /// Never *read*, and that is the point: it exists to be **dropped**. When the reaper removes
    /// a finished `Thread` from the map, this field's `Drop` unmaps four pages, discharges the
    /// TLB obligation, frees four frames, and hands the address range back. Ownership doing the
    /// work, exactly as notes/heap.md described it: the compiler proving the free happens once,
    /// at the right moment.
    ///
    /// So this one stays allowed unconditionally and on purpose: there is no configuration in which
    /// anything reads it, and that is the design rather than a gap (DECISIONS §38, disposition 3).
    #[allow(dead_code)]
    pub stack: Option<KernelStack>,

    /// The low half of memory, as far as this thread is concerned. `None` for a kernel thread,
    /// which has no business at a low address at all.
    ///
    /// **`TTBR0_EL1` is one register and it is global; threads are not.** So the context switch
    /// installs this on the way in, exactly as it installs a stack and a register file. A user
    /// thread that kept running while another thread swapped `TTBR0` would find its own code
    /// replaced by a stranger's, which is not a hypothetical: see notes/userspace.md.
    ///
    /// Owned, so the reaper's `drop` unmaps and frees the entire address space when the thread
    /// dies. Same mechanism as `stack` above, and for the same reason.
    pub space: Option<crate::user::AddressSpace>,

    /// **Everything this thread can name.**
    ///
    /// It starts **empty**, and that is the whole of DECISIONS §10 expressed as a field
    /// initializer. Under Unix a fresh process inherits every file descriptor its parent held,
    /// and can `open()` anything its uid permits. Here it can name *nothing at all* until
    /// somebody hands it something.
    ///
    /// It lives in kernel memory and userspace never sees a byte of it. Userspace sees an
    /// integer. That is the entire unforgeability mechanism, and it is a bounds check.
    pub cspace: crate::cap::CSpace,

    /// **The IPC message this thread most recently sent or received.** Five words.
    ///
    /// A sender parks its message here before blocking; a receiver reads it here after being
    /// woken. It is a `Thread` field rather than a stack local precisely because the rendezvous
    /// happens across two threads at two different times: the sender deposits it and blocks, and
    /// the receiver, running later, reaches into the sender's `Thread` to collect it. See
    /// sched.rs.
    ///
    /// Three words carry ordinary IPC; the extra two exist for the five-word fault/exit message a
    /// dead thread's corpse delivers to its supervisor (DECISIONS §26, abi's `fault` module).
    /// Ordinary sends leave words 3 and 4 zero, and `RECV` hands all five back, so only a
    /// supervisor ever reads the top two.
    pub mailbox: [u64; 5],

    /// **A slot in a spawner's quota, or `None` for a thread nobody bounded.** Reaped with the
    /// thread, which is how the slot comes back. See [`QuotaToken`].
    ///
    /// Never read, like [`Self::stack`]: it exists to be dropped, and its `Drop` returns the slot.
    /// It is `None` on every thread today, because `sched::spawn_with_quota` has had no caller since
    /// §28 retired the kernel-wired shell; see that function for why the mechanism stays.
    #[allow(dead_code)]
    pub quota: Option<QuotaToken>,

    /// **A capability parked here mid-delegation.** When a thread does a capability-carrying send
    /// (`SEND_CAP`) and no receiver is waiting, it blocks with the capability stashed here, exactly
    /// as `mailbox` stashes the data words. The receiver, running later, reaches in, `take()`s it,
    /// and inserts it into its own cspace. `None` for every ordinary send. See sched.rs.
    pub outgoing_cap: Option<crate::cap::Cap>,

    /// **The intrusive queue link** (milestone 14 phases A.2/A.3; notes/intrusive-queues.md).
    /// When this thread is on a run queue, a migration inbox, or an endpoint wait queue, this
    /// points at the next thread in it; `None` otherwise. One link, so a thread can be on at most
    /// one queue, which is not a limitation but the scheduler's state machine made physical:
    /// Ready threads are on exactly one run queue or inbox, Blocked threads on at most one
    /// endpoint queue, Running/Finished threads on none. Touched only by the queue that holds
    /// the thread, under that queue's synchronization.
    pub(crate) next: Option<core::ptr::NonNull<Thread>>,

    /// **Where this thread's EL0 execution begins** (milestone 19c.3), set by `Tcb::CONFIGURE`
    /// on an embryo, consumed by `START` to build the entry context. `(0, 0)` for a kernel
    /// thread, which never drops to EL0 and runs its closure instead.
    pub(crate) entry: (u64, u64), // (entry_va, user_sp)

    /// **The child's initial `x0`, `x1`, `x2`** (milestone 19d/19e): the words `START` hands the
    /// new EL0 thread in its first registers, so a loader can pass a child its role plus data (a
    /// worker's input, a driver's DMA address). All zero for a kernel thread.
    pub(crate) start_args: [u64; 3],

    /// **Did this thread's TCB page come from `kmem`** (recycle it on death) or from a user
    /// process's own region (leave it; the region reclaims it at destroy)? True for every
    /// kernel-created thread; false for a user-retyped TCB (19c.3). The page-origin half of the
    /// same owned-vs-borrowed question kernel stacks answered with "one owner" (notes/tcb.md).
    pub(crate) tcb_kmem: bool,

    /// **Marked for forcible teardown** (DECISIONS §16 amendment): a region's owner called
    /// `Untyped::DESTROY` while this thread was still live in it, so the thread is doomed. The
    /// scheduler converts a killed thread to a corpse at its next preemption instead of requeueing
    /// it (see `schedule`), so a runaway that never checks its endpoint is torn down without
    /// yanking it out of a queue or stopping another core: each core reaps its own on the timer.
    /// This is the forcible tier of `^C` (§24), where the shell's escalation retries `DESTROY`
    /// until the killed thread has self-terminated and the region is object-free.
    pub(crate) killed: bool,
    /// **Where this thread's fault/exit is reported** (milestone 22, DECISIONS §26), or `None` for
    /// an unsupervised thread. Set once at `START` from the child's reserved fault slot (abi's
    /// `FAULT_EP_SLOT`) and never afterward: supervision is granted at spawn only, so the
    /// relationship is fixed and visible in how the thread was built (§26.2). When the thread
    /// faults or exits, the kernel delivers a five-word message here and the corpse goes `Dead`
    /// until reaped; a thread with `None` dies and is reaped immediately, today's behaviour.
    pub(crate) fault_ep: Option<crate::sched::EpId>,

    /// **The untyped region this TCB's page was retyped out of** (DECISIONS §32), or `None` for a
    /// kernel-created thread whose page came from `kmem`. Recorded at `create_tcb`, which is the one
    /// place the answer is known for certain, and it is what an endpoint reap reclaims: the same
    /// region name the region's owner would have passed to `Untyped::DESTROY`, so there is one
    /// teardown path and not two. A supervisor never sees this number and holds no capability to it;
    /// naming the region is the kernel's job precisely because the supervisor cannot.
    ///
    /// It goes stale like any other region name (the slot's generation bumps at destroy), so a
    /// second reap of the same corpse finds nothing to reclaim rather than somebody else's region.
    pub(crate) tcb_region: Option<u64>,

    /// **The fault/exit message this thread's corpse carries** (milestone 22), retained after death
    /// so a test can prove a `Dead` TCB still holds its fault-time state. Set when the thread dies
    /// with a `fault_ep`; `None` while it lives. The words are the §26 format
    /// `[event, tid, pc, addr, reserved]`, the same five the supervisor received.
    pub(crate) fault_msg: Option<[u64; 5]>,
}

// SAFETY: plain storage of the link, nothing else, which is all the queue's contract asks.
unsafe impl intrusive::Node for Thread {
    fn next(&self) -> Option<core::ptr::NonNull<Self>> {
        self.next
    }
    fn set_next(&mut self, next: Option<core::ptr::NonNull<Self>>) {
        self.next = next;
    }
}

// SAFETY: a Thread is only ever touched under the scheduler's lock.
unsafe impl Send for Thread {}

impl Thread {
    /// The thread we are already running on, at `sched::init`.
    ///
    /// It has no stack of its own (it uses the boot stack) and no saved context yet: the first
    /// `switch_to` *away* from it is what fills that in. Which is the neat part: a thread's
    /// context is written by the act of leaving it, so the boot thread needs no special case
    /// beyond a null placeholder.
    pub fn boot() -> Self {
        Thread {
            id: UNNAMED, // named 0 by the table's first insert (see slots::Table)
            handshake: wake_handshake::Handshake::on_cpu_now(), // adopted mid-run: standing on its CPU
            context: core::ptr::null_mut(),
            stack: None,
            space: None,
            cspace: crate::cap::CSpace::new(),
            mailbox: [0; 5],
            quota: None,
            outgoing_cap: None,
            next: None,
            entry: (0, 0), // a kernel thread; never enters EL0 by this path
            start_args: [0; 3],
            tcb_kmem: true,
            killed: false,
            fault_ep: None,
            tcb_region: None,
            fault_msg: None,
        }
    }

    /// Adopt the context a **secondary core** is already running on as a thread, the way
    /// [`boot`](Self::boot) does for core 0.
    ///
    /// Same shape as `boot`: no stack of its own (it runs on the core's `smp` boot stack), a null
    /// context filled by the first `switch_to` away from it, `Running`. This becomes that core's
    /// idle thread, so it is never in a run queue; the scheduler falls back to it when the core's
    /// queue is empty. See smp.rs and `sched::adopt_secondary_idle`.
    pub fn adopt_current() -> Self {
        Thread {
            id: UNNAMED,                                        // named at insert, like every thread
            handshake: wake_handshake::Handshake::on_cpu_now(), // adopted mid-run: standing on its CPU
            context: core::ptr::null_mut(),
            stack: None,
            space: None,
            cspace: crate::cap::CSpace::new(),
            mailbox: [0; 5],
            quota: None,
            outgoing_cap: None,
            next: None,
            entry: (0, 0), // a kernel thread; never enters EL0 by this path
            start_args: [0; 3],
            tcb_kmem: true,
            killed: false,
            fault_ep: None,
            tcb_region: None,
            fault_msg: None,
        }
    }

    /// A new thread, ready to run `f` the first time it is scheduled.
    ///
    /// **The closure lives on the new thread's own stack** (milestone 14 phase B.3): `spawn` is
    /// generic, so `f` is moved at its concrete type into the top of the fresh stack, above the
    /// faked switch frame. No heap, no vtable: `x19` carries the closure's address and `x20` a
    /// monomorphized [`call_closure::<F>`] that knows how to call it. The old shape boxed the
    /// closure twice (a `dyn` fat pointer does not fit one register); both allocations are gone,
    /// and the memory is freed by being the thread's stack.
    /// **Build a thread directly into `dst`, rather than returning one by value** (milestone 124).
    ///
    /// A `Thread` is a large value: `CSpace<Object, 16>` alone is 384 bytes, and a debug build
    /// copies rather than elides at every move. Returning one travelled through
    /// `Thread::spawn`'s frame, `spawn_on`'s local, a closure capture, that closure's return, and
    /// finally `ptr.write`, and each hop was a real memcpy through a stack temporary. The
    /// instantiations of `sched::spawn_on` measured 3888 to 4592 bytes, **over the 4096-byte guard
    /// page on both ISAs**, which is the size at which a frame can step past the guard in one move
    /// and corrupt the neighbouring stack without ever faulting (notes/stack.md).
    ///
    /// Writing through a pointer the caller already has removes the hops. The destination is the
    /// TCB page `Threads::insert_at` claimed, which is where the thread was always going to live.
    ///
    /// Returns `false` and writes nothing if the kernel stack could not be allocated, which is the
    /// same failure `spawn` reported as `None`.
    ///
    /// # Safety
    ///
    /// `dst` must be writable, aligned for `Thread`, and hold no live `Thread`: this *writes*
    /// rather than assigns, so nothing is dropped. `Threads::insert_at` satisfies all three with a
    /// fresh page it exclusively owns.
    pub unsafe fn spawn_into<F: FnOnce() + Send + 'static>(
        f: F,
        id: Tid,
        dst: *mut Thread,
    ) -> bool {
        // Bounds at compile time, per monomorphization: a capture that does not comfortably fit
        // the stack is refused at build, not at runtime. 1 KiB is generous (captures here are a
        // few words) while leaving the 16 KiB stack its headroom.
        const {
            assert!(
                size_of::<F>() <= 1024,
                "spawn closure captures more than 1 KiB; pass a reference to static state instead"
            );
        };
        const {
            assert!(
                align_of::<F>() <= 16,
                "spawn closure over-aligned for a stack slot"
            );
        };

        // `false` rather than `?`: this returns a bool now, and the failure is the one `spawn`
        // used to report as `None`. Nothing has been written to `dst` at this point.
        let Some(stack) = KernelStack::new() else {
            return false;
        };

        // The closure's slot: at the very top of the stack, aligned down to 16 so the switch
        // frame below it keeps `sp` 16-aligned (notes/stack.md). Bytes above the initial `sp`
        // are never touched by the thread's own execution, so the value is safe there until
        // `call_closure` moves it out.
        let closure_at = (stack.top() - size_of::<F>() as u64) & !15;

        // SAFETY: inside the just-mapped stack; `write` moves `f` (no drop of the original).
        unsafe { (closure_at as *mut F).write(f) };

        // Fake a `switch_to` frame just below the closure, so that the very same `ret` that
        // resumes an existing thread also *starts* a new one. There is no separate "first
        // run" path: the trampoline just happens to be what `x30` points at.
        let context = (closure_at - size_of::<Context>() as u64) as *mut Context;

        // The closure lives on the new stack (`closure_at`); its concrete type was erased, so we
        // also hand over the monomorphized shim that knows how to call it. `arch` owns which
        // registers carry them and which trampoline `switch_to`'s first `ret` lands in.
        let call_shim = (call_closure::<F> as extern "C" fn(*mut ())) as usize as u64;
        // SAFETY: the stack was just mapped read/write, and this is inside it.
        unsafe {
            context.write(Context::for_kernel_thread(closure_at, call_shim));
        }

        // SAFETY: the caller's contract: `dst` is writable, aligned, and holds no live Thread.
        unsafe {
            dst.write(Thread {
                id,
                handshake: wake_handshake::Handshake::ready(),
                context,
                stack: Some(stack),
                space: None, // a kernel thread until it calls `user::exec`
                cspace: crate::cap::CSpace::new(), // and it can name nothing until it is handed something
                mailbox: [0; 5],
                quota: None,
                outgoing_cap: None,
                next: None,
                entry: (0, 0), // a kernel thread; becomes a user process via exec, not this path
                start_args: [0; 3],
                tcb_kmem: true,
                killed: false,
                fault_ep: None,
                tcb_region: None,
                fault_msg: None,
            });
        }
        true
    }

    /// The by-value form, for the callers that have nowhere to write into yet.
    ///
    /// `sched::init`'s idle thread and `spawn_blocked` still take this path; they hold no TCB page
    /// at the point they build. It costs the copies `spawn_into` exists to avoid, which is why
    /// `spawn_on` does not use it (milestone 124).
    pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> Option<Self> {
        let mut slot: core::mem::MaybeUninit<Thread> = core::mem::MaybeUninit::uninit();
        // SAFETY: `slot` is writable, aligned and holds no live Thread.
        if !unsafe { Self::spawn_into(f, UNNAMED, slot.as_mut_ptr()) } {
            return None;
        }
        // SAFETY: `spawn_into` returned true, so it initialised every field.
        Some(unsafe { slot.assume_init() })
    }

    /// **A TCB object, retyped but not started** (milestone 19c.3). No stack, no saved context,
    /// no address space, no entry: `Embryo`. `CONFIGURE` fills in the space and entry, `START`
    /// builds the stack and context and makes it `Ready`. `tcb_kmem` records that this TCB's
    /// page is a user region's, not `kmem`'s, so the reaper leaves it for the region.
    pub fn embryo() -> Self {
        Thread {
            id: UNNAMED,
            handshake: wake_handshake::Handshake::embryo(),
            context: core::ptr::null_mut(),
            stack: None,
            space: None,
            cspace: crate::cap::CSpace::new(),
            mailbox: [0; 5],
            quota: None,
            outgoing_cap: None,
            next: None,
            entry: (0, 0),
            start_args: [0; 3],
            tcb_kmem: false, // a user-retyped TCB page; the region owns it
            killed: false,
            fault_ep: None,
            tcb_region: None,
            fault_msg: None,
        }
    }

    /// Build this embryo's kernel stack and entry context, making it ready to first run at EL0
    /// (milestone 19c.3, the guts of `START`). The stack is kernel-owned (19c.1: a kernel stack
    /// is kernel infrastructure whoever the thread serves); the context is a faked `switch_to`
    /// frame whose trampoline drops to EL0 at `entry` on `user_sp`, exactly as `thread_trampoline`
    /// starts a kernel thread's closure. `false` if no kernel stack could be built.
    pub fn arm_for_start(&mut self) -> bool {
        let Some(stack) = KernelStack::new() else {
            return false;
        };
        let (entry, user_sp) = self.entry;
        let context = (stack.top() - size_of::<Context>() as u64) as *mut Context;
        // SAFETY: the stack was just mapped read/write, and this is inside it. `arch` owns the
        // mapping from (entry, user sp, args) onto registers and the EL0 first-run trampoline.
        unsafe {
            context.write(Context::for_user_thread(entry, user_sp, self.start_args));
        }
        self.stack = Some(stack);
        self.context = context;
        true
    }
}

/// Where a **user** thread starts, in Rust (milestone 19c.3): the EL0 mirror of `thread_entry`.
/// Reaps whoever we switched away from (a new thread skips `schedule`'s post-switch point), then
/// drops to EL0 at `entry` on `user_sp`. The address space was installed by the context switch
/// that scheduled us in (from our `space` field), so `TTBR0` already names it.
#[unsafe(no_mangle)]
extern "C" fn user_thread_entry(entry: u64, user_sp: u64, arg0: u64, arg1: u64, arg2: u64) -> ! {
    crate::sched::finish_switch();
    crate::user::enter_at_on_current(entry, user_sp, arg0, arg1, arg2)
}

/// The monomorphized bridge between "an address on a stack" and "a closure of type `F`".
///
/// `Thread::spawn` erases the closure's type when it parks it on the new stack; this function,
/// instantiated per closure type and passed through `x20`, is where the type comes back. It
/// moves the closure out of its stack slot and calls it; the captures drop normally when the
/// call returns.
extern "C" fn call_closure<F: FnOnce()>(closure: *mut ()) {
    // SAFETY: `closure` is the `F` that `Thread::spawn` placed on this very stack, and this is
    // the single read of it: the slot is dead bytes afterward, above `sp`, touched by nobody.
    let f = unsafe { closure.cast::<F>().read() };
    f();
}

/// Where a new thread actually begins, in Rust.
///
/// Called by `thread_trampoline` with the closure's stack address in `x0` and its monomorphized
/// caller in `x1`. If the thread is torn down before it ever runs, the closure's destructors do
/// not run (its captures are leaked in place); true of the boxed version before it, and fine for
/// what kernel threads capture (ids, statics), but a real constraint worth knowing about.
#[unsafe(no_mangle)]
extern "C" fn thread_entry(closure: *mut (), call: extern "C" fn(*mut ())) -> ! {
    // We are a brand-new thread, resuming for the first time. The thread this core switched away
    // from to start us may have finished; reap it now, off its stack, exactly as a resuming thread
    // does after `switch_to`. A new thread does not pass through `schedule()`'s post-switch point,
    // so this is the only place that reap happens for it. See sched::finish_switch.
    //
    // We arrive with IRQs masked (the trampoline no longer unmasks early: doing so before this
    // call stranded the predecessor when a timer IRQ overwrote `switched_from`; see context.s).
    // `finish_switch` therefore runs masked, as it must. Only now, once it has completed, do we
    // unmask, so this kernel thread's closure is preemptible.
    crate::sched::finish_switch();
    crate::arch::interrupts::enable();

    call(closure);

    crate::sched::exit();
}
