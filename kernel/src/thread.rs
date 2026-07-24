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

use crate::arch::mmu::{self, KERNEL_VA_BASE};
use crate::memory;
use crate::sync::{IrqSafeMutex, rank};
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use frames::{FRAME_SIZE, Frame};
use paging::Flags;

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

static NEXT_STACK_VA: AtomicU64 = AtomicU64::new(STACK_AREA);

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
/// MAX_THREADS (= 128; sched.rs). The array is sized to that bound, and the debug assert is the
/// cross-check.
struct FreeVas {
    vas: [u64; 128],
    len: usize,
}

impl FreeVas {
    const fn new() -> Self {
        Self { vas: [0; 128], len: 0 }
    }

    fn pop(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.vas[self.len])
    }

    fn push(&mut self, va: u64) {
        debug_assert!(self.len < self.vas.len(), "more dead stack ranges than MAX_THREADS");
        if self.len < self.vas.len() {
            self.vas[self.len] = va;
            self.len += 1;
        } // else: leak the VA range rather than corrupt; unreachable per the bound above
    }
}

/// The callee-saved registers, as `switch_to` pushes them.
///
/// **This layout is a contract with `context.s`.** Twelve `u64`s, 96 bytes, in exactly this
/// order. Reorder a field here and the assembly restores the wrong register into the wrong
/// place, and the thread resumes with a frame pointer where its return address should be.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// `x19`. **Doubles as the argument to a brand-new thread**: `thread_trampoline` reads the
    /// boxed closure out of it. A callee-saved register, chosen exactly because `switch_to`
    /// restores it for us on the way in.
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    /// `x29`, the frame pointer. Zero for a new thread: it is the bottom of the backtrace.
    pub x29: u64,
    /// `x30`, the link register. **Where `switch_to`'s `ret` jumps.** For a new thread this is
    /// `thread_trampoline`, which is how a thread that has never run gets started by the same
    /// instruction that resumes one that has.
    pub x30: u64,
}

const _: () = assert!(size_of::<Context>() == 96);

unsafe extern "C" {
    /// Save our callee-saved registers, swap `sp`, restore theirs, and `ret` into **their**
    /// `x30`. See context.s: the last instruction returns to a different thread.
    pub fn switch_to(prev_context: *mut *mut Context, next_context: *mut Context);

    fn thread_trampoline();
}

/// A stack, with an unmapped page beneath it.
///
/// The frame list is a fixed array (milestone 14 phase B.1): a kernel stack is always exactly
/// [`STACK_PAGES`] frames, so there was never anything dynamic about it but the container.
pub struct KernelStack {
    guard: u64,
    bottom: u64,
    top: u64,
    frames: [Option<Frame>; STACK_PAGES],
}

impl KernelStack {
    pub fn new() -> Option<Self> {
        // One page of virtual address space for the guard, plus the stack itself. The guard's
        // VA is simply never mapped, which is the entire mechanism.
        let span = (STACK_PAGES as u64 + 1) * FRAME_SIZE;

        // Reuse a dead thread's address range if there is one, so the page tables covering it
        // are already built. Only bump into fresh address space when there isn't.
        let base = FREE_STACK_VAS
            .lock()
            .pop()
            .unwrap_or_else(|| NEXT_STACK_VA.fetch_add(span, Ordering::Relaxed));

        let guard = base;
        let bottom = base + FRAME_SIZE;
        let top = bottom + STACK_PAGES as u64 * FRAME_SIZE;

        let mut frames = [const { None }; STACK_PAGES];
        for (i, slot) in frames.iter_mut().enumerate() {
            let frame = memory::alloc()?;
            let va = bottom + i as u64 * FRAME_SIZE;

            if mmu::map_page(va, frame.addr(), Flags::kernel_data()).is_err() {
                memory::free(frame);
                return None; // `frames` drops, and Drop below unmaps what we did map
            }
            *slot = Some(frame);
        }

        Some(KernelStack {
            guard,
            bottom,
            top,
            frames,
        })
    }

    /// Where `sp` starts. The stack grows **down** from here (notes/stack.md).
    pub fn top(&self) -> u64 {
        self.top
    }

    /// The unmapped page below the stack. Test support.
    #[allow(dead_code)]
    pub fn guard(&self) -> u64 {
        self.guard
    }

    /// The lowest usable byte. Test support.
    #[allow(dead_code)]
    pub fn bottom(&self) -> u64 {
        self.bottom
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        for (i, frame) in self.frames.iter().enumerate() {
            let Some(frame) = frame else { continue };
            let va = self.bottom + i as u64 * FRAME_SIZE;

            // `unmap_page` discharges the TLB obligation with a real `tlbi`. It has to, and the
            // reason is right here: this virtual address is about to be handed to a **different
            // thread's stack**. A stale translation would let the new thread read — and write —
            // the dead thread's saved registers. See notes/page-tables.md.
            if mmu::unmap_page(va).is_ok() {
                memory::free(*frame);
            }
        }

        // Hand the address range back, so the next thread lands in page tables that already
        // exist. The frames are freed above; this returns the *names*.
        FREE_STACK_VAS.lock().push(self.guard);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    /// **Waiting for an IPC rendezvous** that has not happened yet.
    ///
    /// A blocked thread is in no run queue and will not be scheduled. It sits in an endpoint's
    /// wait queue instead, and the thread on the other side of that endpoint is the only thing
    /// that will ever move it back to `Ready`. This is the state that makes "block until a
    /// message arrives" a real thing a thread can do rather than a spin loop.
    Blocked,
    Finished,
}

/// **A reserved slot in a spawner's resource budget, returned when this thread dies.**
///
/// A process (the shell, say) that spawns children can be given a quota: at most N children alive
/// at once. Reserving a slot is an atomic decrement; a `QuotaToken` holds that reservation, and
/// its `Drop` gives it back. Because the token lives inside the `Thread`, the slot is returned at
/// exactly the moment the reaper drops the thread — a well-behaved child that exits frees its slot,
/// and a child that blocks forever keeps holding it, which is correct: it is still consuming a
/// thread, a stack, and an address space. This is what bounds kernel memory against a spawn flood
/// or a leaked-thread accumulation without any per-tick bookkeeping. See notes/quotas.md.
pub struct QuotaToken(&'static AtomicU32);

impl QuotaToken {
    pub fn new(budget: &'static AtomicU32) -> Self {
        QuotaToken(budget)
    }
}

impl Drop for QuotaToken {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

pub struct Thread {
    pub id: Tid,
    pub state: State,

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

    /// **The IPC message this thread most recently sent or received.** Three words.
    ///
    /// A sender parks its message here before blocking; a receiver reads it here after being
    /// woken. It is a `Thread` field rather than a stack local precisely because the rendezvous
    /// happens across two threads at two different times: the sender deposits it and blocks, and
    /// the receiver, running later, reaches into the sender's `Thread` to collect it. See
    /// sched.rs.
    pub mailbox: [u64; 3],

    /// **A slot in a spawner's quota, or `None` for a thread nobody bounded.** Reaped with the
    /// thread, which is how the slot comes back. See [`QuotaToken`].
    pub quota: Option<QuotaToken>,

    /// **A capability parked here mid-delegation.** When a thread does a capability-carrying send
    /// (`SEND_CAP`) and no receiver is waiting, it blocks with the capability stashed here, exactly
    /// as `mailbox` stashes the data words. The receiver, running later, reaches in, `take()`s it,
    /// and inserts it into its own cspace. `None` for every ordinary send. See sched.rs.
    pub outgoing_cap: Option<crate::cap::Cap>,

    /// **The intrusive queue link** (milestone 14 phases A.2/A.3; notes/intrusive-queues.md).
    /// When this thread is on a run queue, a migration inbox, or an endpoint wait queue, this
    /// points at the next thread in it; null otherwise. One link, so a thread can be on at most
    /// one queue, which is not a limitation but the scheduler's state machine made physical:
    /// Ready threads are on exactly one run queue or inbox, Blocked threads on at most one
    /// endpoint queue, Running/Finished threads on none. Touched only by the queue that holds
    /// the thread, under that queue's synchronization.
    pub(crate) next: *mut Thread,

    /// **Still standing on a CPU.** Set (under `SCHED`) when a core schedules this thread in;
    /// cleared by that core's *successor* thread in `finish_switch`, after the switch away has
    /// saved this thread's context. In the window between "marked itself Blocked and released
    /// the lock" and "its core actually switched off its stack", this is what tells a waker the
    /// thread's saved context is still stale. See [`wake_pending`](Self::wake_pending) and
    /// `cpu::PerCpu::switched_from`.
    pub(crate) on_cpu: bool,

    /// **A wake arrived while this thread was still switching out** (`on_cpu` was set). The
    /// waker parks the wake here instead of queueing a thread whose context is stale; the
    /// thread's own core completes it in `finish_switch`, when the context is provably saved.
    /// Both touched only under `SCHED`.
    pub(crate) wake_pending: bool,
}

// SAFETY: plain storage of the link, nothing else, which is all the queue's contract asks.
unsafe impl intrusive::Node for Thread {
    fn next(&self) -> *mut Self {
        self.next
    }
    fn set_next(&mut self, next: *mut Self) {
        self.next = next;
    }
}

// SAFETY: a Thread is only ever touched under the scheduler's lock.
unsafe impl Send for Thread {}

impl Thread {
    /// The thread we are already running on, at `sched::init`.
    ///
    /// It has no stack of its own (it uses the boot stack) and no saved context yet: the first
    /// `switch_to` *away* from it is what fills that in. Which is the neat part — a thread's
    /// context is written by the act of leaving it, so the boot thread needs no special case
    /// beyond a null placeholder.
    pub fn boot() -> Self {
        Thread {
            id: UNNAMED, // named 0 by the table's first insert (see slots::Table)
            state: State::Running,
            context: core::ptr::null_mut(),
            stack: None,
            space: None,
            cspace: crate::cap::CSpace::new(),
            mailbox: [0; 3],
            quota: None,
            outgoing_cap: None,
            next: core::ptr::null_mut(),
            on_cpu: true, // adopted mid-run: this thread is standing on its CPU right now
            wake_pending: false,
        }
    }

    /// Adopt the context a **secondary core** is already running on as a thread, the way
    /// [`boot`](Self::boot) does for core 0.
    ///
    /// Same shape as `boot`: no stack of its own (it runs on the core's `smp` boot stack), a null
    /// context filled by the first `switch_to` away from it, `Running`. This becomes that core's
    /// idle thread, so it is never in a run queue; the scheduler falls back to it when the core's
    /// queue is empty. See smp.rs and sched::adopt_secondary_idle.
    pub fn adopt_current() -> Self {
        Thread {
            id: UNNAMED, // named at insert, like every thread
            state: State::Running,
            context: core::ptr::null_mut(),
            stack: None,
            space: None,
            cspace: crate::cap::CSpace::new(),
            mailbox: [0; 3],
            quota: None,
            outgoing_cap: None,
            next: core::ptr::null_mut(),
            on_cpu: true, // adopted mid-run: this thread is standing on its CPU right now
            wake_pending: false,
        }
    }

    /// A new thread, ready to run `f` the first time it is scheduled.
    pub fn spawn(f: Box<dyn FnOnce() + Send + 'static>) -> Option<Self> {
        let stack = KernelStack::new()?;

        // `Box<dyn FnOnce()>` is a **fat** pointer (data + vtable), two words, and we have one
        // register to smuggle it through. So box it again: `Box<Box<dyn FnOnce()>>` is a thin
        // pointer to a fat one, and fits in `x19`.
        let boxed: Box<Box<dyn FnOnce() + Send>> = Box::new(f);
        let arg = Box::into_raw(boxed) as u64;

        // Fake a `switch_to` frame at the top of the fresh stack, so that the very same `ret`
        // that resumes an existing thread also *starts* a new one. There is no separate "first
        // run" path: the trampoline just happens to be what `x30` points at.
        let context = (stack.top() - size_of::<Context>() as u64) as *mut Context;

        // SAFETY: the stack was just mapped read/write, and this is inside it.
        unsafe {
            context.write(Context {
                x19: arg, // the closure, for the trampoline
                x20: 0,
                x21: 0,
                x22: 0,
                x23: 0,
                x24: 0,
                x25: 0,
                x26: 0,
                x27: 0,
                x28: 0,
                x29: 0, // no caller: the backtrace ends here
                x30: thread_trampoline as *const () as u64, // <- where `ret` will go
            });
        }

        Some(Thread {
            id: UNNAMED, // named at insert, like every thread
            state: State::Ready,
            context,
            stack: Some(stack),
            space: None, // a kernel thread until it calls `user::exec`
            cspace: crate::cap::CSpace::new(), // and it can name nothing until it is handed something
            mailbox: [0; 3],
            quota: None,
            outgoing_cap: None,
            next: core::ptr::null_mut(),
            on_cpu: false,
            wake_pending: false,
        })
    }
}

/// Where a new thread actually begins, in Rust.
///
/// Called by `thread_trampoline` with the boxed closure in `x0`.
#[unsafe(no_mangle)]
extern "C" fn thread_entry(arg: *mut ()) -> ! {
    // We are a brand-new thread, resuming for the first time. The thread this core switched away
    // from to start us may have finished; reap it now, off its stack, exactly as a resuming thread
    // does after `switch_to`. A new thread does not pass through `schedule()`'s post-switch point,
    // so this is the only place that reap happens for it. See sched::finish_switch.
    crate::sched::finish_switch();

    // SAFETY: `Thread::spawn` leaked exactly this box, and we are the only one who will ever
    // claim it. Reconstructing it here is what makes the closure's memory get freed when the
    // thread finishes, rather than leaking one per thread forever.
    let f: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(arg as *mut _) };

    f();

    crate::sched::exit();
}
