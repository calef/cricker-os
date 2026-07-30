//! **The syscall boundary, as a single artifact.**
//!
//! The kernel and every user program depend on this crate, so the ABI is *one thing* rather than
//! two files that agree by luck. If it changes, both sides fail to compile, which is the entire
//! point: a boundary that can drift silently is not a boundary.
//!
//! # The surface is three calls, and that is deliberate
//!
//! DECISIONS.md §4 rule 3: *the syscall surface stays narrow and explicit. It is a boundary, not
//! a habit.* And §10 chose capabilities, which is what makes three enough:
//!
//! ```text
//!   exit(code)                              you always have authority over yourself
//!   yield()                                 likewise
//!   invoke(cap, method, a0, a1, a2)         EVERYTHING ELSE
//! ```
//!
//! There is no `open`. No `read`. No `write`. No `fork`. **A process can only act on things it
//! was handed**, and `invoke` is how it acts on them.
//!
//! `exit` and `yield` are plain syscalls rather than invocations on a TCB capability, and the
//! reason is worth stating: **a capability is authority over something *else*.** You do not need
//! to be granted the right to stop running.
//!
//! # The register convention
//!
//! ```text
//!   x8  syscall number
//!   x0  capability slot        (for invoke)
//!   x1  method
//!   x2  arg0
//!   x3  arg1
//!   x4  arg2
//!
//!   x0  return: >= 0 is a result, < 0 is an `Error`
//! ```
//!
//! `x8` for the number is Linux's aarch64 convention, and there is no reason to be different.

#![no_std]

pub const SYS_EXIT: u64 = 0;
pub const SYS_YIELD: u64 = 1;
pub const SYS_INVOKE: u64 = 2;

/// `cap_delete(slot)`: drop a capability from the caller's own cspace, freeing the slot
/// (milestone 19d). The one capability-management operation the surface needs beyond `invoke`:
/// a loader (init) retypes hundreds of frames through a 16-slot cspace and must recycle slots.
/// Dropping a capability is authority over your *own* table, so like `exit` and `yield` it is a
/// bare syscall, not an invocation on some object. Deleting an empty slot is a harmless no-op.
pub const SYS_CAP_DELETE: u64 = 3;

/// An index into the calling thread's capability table. **Not a pointer, not a handle you can
/// guess.** The kernel looks in *your* table, and if the slot is empty you get `NoSuchSlot`.
pub type CapSlot = u64;

/// The number of capability slots in a thread's cspace. Mirrors the kernel's `cap::CSPACE_SLOTS`;
/// lives here too so userspace can name the reserved [`fault::FAULT_EP_SLOT`] without reaching into
/// the kernel. The two must agree, and a mismatch would put the fault slot in different places on
/// the two sides of the boundary, which is exactly the drift this crate exists to prevent.
pub const CSPACE_SLOTS: u64 = 16;

/// Methods on a `Console` capability. **Historical: no longer wired up.**
///
/// Milestone 8 removed the kernel-served `Console` object (the console became a userspace server
/// reached by an `Endpoint`). This constant is kept so the ABI's history is legible, but nothing
/// in the kernel dispatches it any more.
#[allow(dead_code)]
pub mod console {
    /// `invoke(cap, WRITE, ptr, len, _)` -> bytes written.
    ///
    /// `ptr` is a **user** pointer, and the kernel will refuse it unless *the user itself* could
    /// read it. See `syscall::user_slice`, and the confused deputy in notes/capabilities.md.
    pub const WRITE: u64 = 0;
}

/// Methods on an `Endpoint` capability. **This is IPC.**
///
/// An endpoint is a rendezvous point, and the two methods are the two sides of it. Which one you
/// may call is a matter of *rights*, not of the endpoint: a capability with `WRITE` can `SEND`,
/// one with `READ` can `RECV`. So the same object, handed out with different rights, is a
/// one-way pipe in whichever direction each holder was trusted with. Neither side can do the
/// other's job, and neither had to be told which end it is.
pub mod endpoint {
    /// `invoke(cap, SEND, w0, w1, w2)` -> 0. **Blocks until a receiver takes the message.**
    ///
    /// The three words travel in registers and never touch memory. That is the whole of the
    /// fastpath, and it is DECISIONS §10's rule made real: *IPC carries control.* Bulk data will
    /// move later by handing over a frame capability, not by copying bytes into a message.
    pub const SEND: u64 = 0;

    /// `invoke(cap, RECV, _, _, _)` -> w0, with w1 in x1 and w2 in x2. **Blocks until a message
    /// arrives.**
    pub const RECV: u64 = 1;

    /// `invoke(cap, SEND_CAP, cap_slot, rights, w0)` -> 0. **Delegate a capability.** Passes the
    /// capability in the sender's `cap_slot`, narrowed to `rights` (see [`rights`]), plus one data
    /// word, over this endpoint; blocks until a receiver takes it. The endpoint capability needs
    /// `WRITE` (you may send here), and the *delegated* capability needs `GRANT` (you were trusted
    /// to pass it on). `rights` may only narrow what the sender holds, never widen it. This is the
    /// operation that makes cricker-os a capability system a process can actually compose in:
    /// authority moves between processes at runtime instead of being wired by the kernel at spawn.
    pub const SEND_CAP: u64 = 2;

    /// `invoke(cap, RECV_CAP, _, _, _)` -> w0, with the received capability's new slot in x1 and a
    /// second data word in x2, or [`NO_CAP`] in x1 if the message carried no capability. **Blocks
    /// until a message arrives.** The received capability lands in a free slot of the receiver's own
    /// cspace, chosen by the kernel; x1 is where. This is also how a server receives a [`CALL`]: the
    /// slot in x1 holds a one-shot [`reply`] capability naming the caller. Needs `READ`.
    pub const RECV_CAP: u64 = 3;

    /// `invoke(cap, CALL, w0, w1, _)` -> r0, with r1 in x1. **Send two words and block until
    /// replied.** The atomic send-and-wait a server can answer safely: at the rendezvous the kernel
    /// mints a one-shot [`reply`] capability naming *this* caller and hands it to the server (through
    /// [`RECV_CAP`]), so the server can answer a caller it was never wired to, exactly once, and only
    /// that caller. Needs `WRITE`. Milestone 12; see notes/ipc-naming.md.
    pub const CALL: u64 = 4;

    /// `invoke(cap, REAP, tid, _, _)` -> 0. **Collect a corpse this endpoint supervises**
    /// (DECISIONS §32). `tid` is the thread id the kernel stamped on the death message
    /// ([`fault`](super::fault)); the kernel reclaims that thread's TCB, its address space, and the
    /// region behind them, exactly what [`untyped::DESTROY`](super::untyped::DESTROY) would have
    /// reclaimed. Needs `READ` on this endpoint: the authority to collect is the authority to
    /// *receive* deaths here, which is what a supervisor holds.
    ///
    /// **Authorization is the supervision relationship, not a rights bit and not a registry.** The
    /// kernel checks that the named thread's recorded fault endpoint *is this endpoint*, so a tid
    /// means something only relative to the capability it arrived through. A supervisor therefore
    /// needs no capability to the child's memory, and gains none: the reclaimed region returns to
    /// its owner under §13 region ownership, which is the **builder**. A supervisor can free a
    /// child's memory; it cannot spend it.
    ///
    /// Three distinct refusals, because a restart policy wants to tell them apart:
    ///
    /// - [`Error::StillAlive`] the thread is not dead. Collecting a corpse is not killing; killing a
    ///   live child is the stronger act and stays with `Untyped::DESTROY` (§24's forcible `^C`).
    /// - [`Error::NotSupervised`] no thread by that tid supervised by *this* endpoint: another
    ///   supervisor's child, an already-collected one, or a stale tid whose generational name no
    ///   longer resolves. The two are deliberately one error, so a supervisor cannot probe the tid
    ///   space of children it does not supervise.
    /// - [`Error::NotPermitted`] the corpse's region cannot be reclaimed yet (the child `SPLIT` its
    ///   own budget and those children are still live), the same refusal `DESTROY` gives.
    pub const REAP: u64 = 5;

    /// The x1 value from [`RECV_CAP`] when the message carried no capability.
    pub const NO_CAP: u64 = u64::MAX;
}

/// Methods on a `Reply` capability. **A one-shot answer to a specific caller.**
///
/// The kernel mints one on [`endpoint::CALL`] and hands it to the server through
/// [`endpoint::RECV_CAP`]. It names the exact blocked caller, carries `WRITE` and no `GRANT` (so it
/// cannot be passed on), and is consumed the instant it is used, so a server cannot reply twice,
/// reply to the wrong caller, or hoard it. Those are kernel guarantees, not server discipline.
pub mod reply {
    /// `invoke(reply_cap, REPLY, r0, r1, _)` -> 0. Deliver `{r0, r1}` to the caller, wake it, and
    /// consume this capability (a second use is [`Error::NoSuchSlot`]).
    pub const REPLY: u64 = 0;
}

/// The rights bits, matching `caps::Rights`, so userspace can name the rights to narrow a
/// delegated capability to (the `rights` argument to [`endpoint::SEND_CAP`]) without depending on
/// the kernel's `caps` crate.
/// Object types for [`untyped::RETYPE_OBJ`]: what a page of untyped becomes.
pub mod objtype {
    /// An IPC endpoint, page-resident, owned by the caller's budget (milestone 19a).
    pub const ENDPOINT: u64 = 1;

    /// An address space (milestone 19b): the retyped page **is the L0 root table**, and the
    /// untyped it came from becomes the space's backing region, paying for every intermediate
    /// page table and revocation record the space ever needs, exactly as an exec-built space's
    /// region does (milestone 14 B.4). One budget model, decided on challenge: the seL4
    /// principle (the user pays, the kernel allocates nothing) in this kernel's idiom, rather
    /// than seL4's per-call paging-structure objects, which belong to the explicitness this
    /// project deliberately did not copy.
    pub const ASPACE: u64 = 2;

    /// A thread (milestone 19c.3): the retyped page holds the TCB, and the object is born an
    /// **embryo**, in no queue and not runnable. It becomes a running thread only through
    /// [`tcb::CONFIGURE`] (bind an address space, set entry and stack) and [`tcb::START`], with
    /// [`tcb::CAP_INSERT`] granting its initial authority in between. A half-built TCB can never
    /// run: `START` refuses one with no bound space or no entry.
    pub const TCB: u64 = 3;
}

/// Methods on a `Tcb` capability (milestone 19c.3): **another thread, under construction.**
/// Created by [`untyped::RETYPE_OBJ`] with [`objtype::TCB`].
pub mod tcb {
    /// `invoke(cap, CONFIGURE, entry, user_sp, aspace_slot)` -> 0. Bind the address space named
    /// by the capability in `aspace_slot` (which is **consumed**: it becomes the thread's, and
    /// dies with it), and set where EL0 execution begins and on what user stack. Needs `WRITE`
    /// on the TCB cap and `WRITE` on the aspace cap. Only an unstarted (embryo) TCB.
    pub const CONFIGURE: u64 = 0;

    /// `invoke(cap, CAP_INSERT, cap_slot, rights, target)` -> child_slot. Copy the capability in
    /// the caller's `cap_slot`, narrowed to `rights`, into the child's cspace, returning the slot
    /// it landed in. The child's whole initial authority is built this way, one grant at a time,
    /// before it runs. Needs `WRITE` on the TCB cap and `GRANT` on the inserted capability.
    ///
    /// `target` chooses where the capability lands: **0 places it in the first free slot** (the
    /// original behaviour, so every existing caller is unchanged), and `n` places it in slot
    /// `n - 1`. The explicit target exists for the supervision endpoint, which a supervisor puts
    /// in the reserved [`fault::FAULT_EP_SLOT`](super::fault::FAULT_EP_SLOT) rather than wherever
    /// first-free happened to fall. `OutOfMemory` if the chosen slot is occupied or out of range.
    pub const CAP_INSERT: u64 = 1;

    /// `invoke(cap, START, _, _, _)` -> 0. Make the thread runnable: it gets a kernel stack and
    /// an entry context and joins the run queue. **Refuses a half-built thread** (no bound
    /// address space, or no entry): a TCB must be whole before it runs. Needs `WRITE`.
    pub const START: u64 = 2;
}

/// Methods on an `Aspace` capability (milestone 19b): **another process's memory, under
/// construction.** Created by [`untyped::RETYPE_OBJ`] with [`objtype::ASPACE`]; nothing can run
/// in it until TCBs arrive (19c), so today it is a structure you build and revocation can reach.
pub mod aspace {
    /// `invoke(cap, MAP_INTO, va, frame_slot, writable)` -> 0. Map the frame in `frame_slot`
    /// into THIS address space at `va`, read-only or read/write. Needs `WRITE` on the aspace
    /// capability; the frame capability needs `READ` for a read-only mapping, `WRITE` for a
    /// writable one, the `frame::MAP` rule verbatim. Page tables and the revocation record are
    /// paid from the space's own backing region; a mapping whose record cannot be afforded is
    /// refused and unmapped (`OutOfMemory`), never left invisible to revocation.
    pub const MAP_INTO: u64 = 0;

    /// `MAP_INTO`'s third argument: how to map the frame. Read-only, read/write, or executable
    /// code (milestone 19d, so a loader can lay down a child's `.text`). Code is W^X: mapped RX,
    /// never writable, and the kernel makes the I-cache coherent with the loader's data writes.
    pub const MAP_RO: u64 = 0;
    pub const MAP_RW: u64 = 1;
    pub const MAP_CODE: u64 = 2;
}

pub mod rights {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const GRANT: u64 = 1 << 2;
}

/// **The fault endpoint: thread death becomes a message a supervisor holds** (milestone 22,
/// DECISIONS §26). When a thread faults or exits, the kernel delivers one message to the
/// supervision endpoint its spawner designated, and the thread's corpse persists (dead until the
/// supervisor reaps it with §16 revocation). Restart policy lives in userspace; the kernel never
/// relaunches anything. This module is the two conventions §26 said it would add: a message-format
/// convention and a spawn-slot convention. No new syscall and no new method (§26).
pub mod fault {
    /// **The spawn-slot convention.** A supervised child is spawned with its supervision endpoint
    /// in this reserved cspace slot (via [`tcb::CAP_INSERT`] with an explicit target slot, or a
    /// [`Spawn`](../user/struct.Spawn.html) grant). At `START` the kernel reads this slot: if it
    /// holds an `Endpoint` capability the thread is supervised, and the kernel records the endpoint
    /// as the thread's fault target and clears the slot (so the child cannot forge fault messages
    /// on it, keeping §26's "the kernel is the only sender" property). An empty slot means the
    /// thread is unsupervised and gets today's behaviour: it dies and is reaped immediately.
    ///
    /// It is the **last** cspace slot, deliberately out of the way of the low slots a child's
    /// ordinary grants fill from zero upward, so an unsupervised child never accidentally lands a
    /// working endpoint here and gets mistaken for a supervised one.
    pub const FAULT_EP_SLOT: u64 = super::CSPACE_SLOTS - 1;

    /// **The message-format convention.** A fault/exit notification is five words, delivered to the
    /// supervision endpoint's holder through a plain `RECV`:
    ///
    /// ```text
    ///   w0  event    FAULT or EXIT
    ///   w1  tid      the dead thread's id (kernel-stamped, trustworthy: the kernel is the
    ///                only sender on this path, so the supervisor need not badge it)
    ///   w2  pc       the faulting instruction (0 for a clean exit)
    ///   w3  addr     the faulting address (0 for a clean exit, or a fault with no address)
    ///   w4  reserved 0 today; a fault-reply / resume protocol arrives here additively (§26.4)
    /// ```
    ///
    /// `RECV` returns w0 in the syscall's result register and w1..w4 in the next four argument
    /// registers. Ordinary three-word IPC leaves w3 and w4 zero, so a supervisor is the only
    /// receiver that reads them.
    pub const EVENT_FAULT: u64 = 1;
    pub const EVENT_EXIT: u64 = 2;
}

/// Methods on an `Irq` capability. **How a userspace driver owns an interrupt.**
pub mod irq {
    /// `invoke(cap, WAIT, _, _, _)` -> 1. **Blocks until the interrupt fires.** The kernel masks
    /// the interrupt when it fires and hands it to us as a message; nothing device-specific
    /// happens in the kernel.
    pub const WAIT: u64 = 0;

    /// `invoke(cap, ACK, _, _, _)` -> 0. Re-enable the interrupt at the GIC, once we have quieted
    /// the device. Until we call this, the interrupt stays masked and cannot storm.
    pub const ACK: u64 = 1;
}

/// Methods on a `Virtio` capability. **How a driver operates a device it cannot point out of its
/// own DMA region.** The kernel owns the queue addresses and the notify; the driver builds
/// requests in its DMA region and submits through here.
pub mod virtio {
    /// `invoke(cap, READ_REG, off, _, _)` -> register value. Reads are DMA-safe, so any register.
    pub const READ_REG: u64 = 0;
    /// `invoke(cap, WRITE_REG, off, val, _)` -> 0. Only DMA-*safe* registers (status, features,
    /// interrupt-ack); the queue-address and notify registers are refused (they go through the
    /// validated paths below).
    pub const WRITE_REG: u64 = 1;
    /// `invoke(cap, SETUP_QUEUE, num, queue, _)` -> 0. The kernel programs the given queue's ring
    /// addresses to the fixed offsets of that queue's ring block in the driver's DMA region, so the
    /// driver never chooses them. `queue` selects the virtqueue (a virtio-net device uses receive =
    /// 0, transmit = 1; the disk uses only queue 0, and passing 0 keeps its ABI byte-identical).
    /// `BadQueue`/`WrongObject` if `queue` is out of range or the block does not fit the region.
    pub const SETUP_QUEUE: u64 = 2;
    /// `invoke(cap, NOTIFY, queue, _, _)` -> 0, or `DeviceRefused` if a newly-published descriptor on
    /// that queue points outside the driver's DMA region. On refusal the device is NOT told to go.
    /// `queue` selects the virtqueue, as for `SETUP_QUEUE`; each queue keeps its own validated
    /// high-water mark, so receive and transmit submits never interfere.
    pub const NOTIFY: u64 = 3;
}

/// Methods on an `Untyped` capability. **How a process spends its own memory.**
pub mod untyped {
    /// `invoke(cap, MAP, va, _, _)` -> 0. Retype one page out of the untyped and map it, writable,
    /// at `va` in the caller's own address space. The page and any page tables it needs both come
    /// from the untyped; the kernel allocates nothing. Returns `OutOfMemory` when the untyped is
    /// exhausted (the *process* is out of budget, not the kernel).
    pub const MAP: u64 = 0;

    /// `invoke(cap, RETYPE, _, _, _)` -> slot. Retype one page out of the untyped into a **`Frame`
    /// capability** the caller now holds, and return the slot it landed in. Nothing is mapped: the
    /// caller decides where to map it, and may delegate it first. This is the split that makes a
    /// page a first-class, delegatable object rather than something mapped in one shot. `OutOfMemory`
    /// when the untyped is exhausted or the caller's cspace is full.
    pub const RETYPE: u64 = 1;

    /// `invoke(cap, RETYPE_OBJ, objtype, _, _)` -> slot. Retype one page out of the untyped into
    /// a **kernel object** (milestone 19a; design/init-and-granular-spawn.md): the object lives
    /// in that page, in the caller's own memory, and the returned slot holds a full-rights
    /// capability to it. One object per page, deliberately (one memory rule for the whole object
    /// family; packing is a later placement optimization).
    ///
    /// `objtype` names what to make (see [`objtype`](super::objtype)); 19a implements
    /// `ENDPOINT`. A region that has produced a kernel object is **pinned**: [`DESTROY`] reclaims
    /// it (object revocation) once the objects are torn down. `BadMethod` for an unknown objtype;
    /// `OutOfMemory` when the untyped is exhausted, the object registry is full, or the cspace is.
    pub const RETYPE_OBJ: u64 = 2;

    /// `invoke(cap, SPLIT, pages, _, _)` -> slot. Carve `pages` off this untyped's unspent budget
    /// into a **new child untyped**, and return the slot holding a full-rights capability to it.
    /// seL4's untyped-retype-into-untyped: a spawner splits a child its own region so it can be
    /// reclaimed independently. This untyped is then marked as having children and can no longer be
    /// `DESTROY`ed (its pages are committed; the child frees them at its own `DESTROY`). `OutOfMemory`
    /// when the budget or region table is exhausted or the cspace is full; `NotPermitted` without
    /// `WRITE`.
    pub const SPLIT: u64 = 3;

    /// `invoke(cap, DESTROY, _, _, _)` -> 0. **Reclaim this region and every object retyped from
    /// it** (object revocation, the region-owner's half). The objects are torn down and their pages
    /// returned; every capability to them goes stale on next use (generational names, no derivation
    /// tree to walk). `NotPermitted` while a live thread still occupies the region, or if it has
    /// been `SPLIT` into children (destroy the children first), or without `WRITE`.
    pub const DESTROY: u64 = 4;
}

/// Methods on a `Frame` capability. **A physical page a process holds, maps, and shares.**
pub mod frame {
    /// `invoke(cap, MAP, va, writable, untyped_slot)` -> 0. Map this frame at `va` in the caller's
    /// own address space. `writable` != 0 maps it read/write (needs `WRITE` on the frame); `0` maps
    /// it read-only (needs `READ`). Page tables to reach `va` come from the untyped named by
    /// `untyped_slot`, so the kernel allocates nothing. `BadPointer` for a misaligned or high `va`,
    /// `OutOfMemory` when that untyped is exhausted.
    pub const MAP: u64 = 0;

    /// `invoke(cap, REVOKE, _, _, _)` -> 0. **Un-share this page** (milestone 13). Unmap it from every
    /// address space that mapped it and delete every capability to it, including the caller's own, so
    /// no holder can reach or re-map it. Needs `GRANT` (you were trusted to lend the frame, so you may
    /// take it back; a read-only consumer handed it without `GRANT` cannot revoke the owner). It does
    /// **not** reclaim the page: the untyped is spend-only, and `untyped::destroy` reclaims a whole
    /// region. See DECISIONS §13 and notes/capability-lifecycle.md.
    pub const REVOKE: u64 = 1;
}

/// What went wrong. Returned as a **negative** `x0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    /// **The slot is empty.** Not "permission denied": there is nothing there, and there is no
    /// way to name the thing you wanted. This is what no-ambient-authority *feels like*.
    NoSuchSlot = -1,

    /// The capability is real, but it is not that kind of object.
    WrongObject = -2,

    /// You hold the capability, but not with those rights. Rights only ever narrow on
    /// delegation, so somebody upstream chose this.
    NotPermitted = -3,

    /// The pointer you passed is not memory **you** could have touched yourself.
    ///
    /// The most interesting error here. See notes/capabilities.md: a kernel that follows a user
    /// pointer using its own authority is the confused deputy, and this is the refusal.
    BadPointer = -4,

    /// No such method on that object.
    BadMethod = -5,

    /// The syscall number is not one of the three.
    BadSyscall = -6,

    /// **The untyped region is exhausted.** The process ran out of the memory it was handed. The
    /// kernel is untouched: this is a budget, not a failure of the machine.
    OutOfMemory = -7,

    /// **A device operation was refused.** For virtio `NOTIFY`, this means a descriptor pointed
    /// outside the driver's DMA region and the device was not allowed to touch it.
    DeviceRefused = -8,

    /// **The thread is still running.** [`endpoint::REAP`] collects a corpse, and this one is not
    /// one yet. Distinct from `NotPermitted` on purpose (DECISIONS §32): "you may not kill" and "no
    /// such child" are different facts, and a restart policy branches on them differently (wait, or
    /// escalate to the owner's `Untyped::DESTROY`, versus give up on that tid).
    StillAlive = -9,

    /// **This endpoint does not supervise a thread by that tid.** Another supervisor's child, one
    /// already collected, or a tid whose generational name is stale. One error for all three
    /// deliberately: distinguishing "gone" from "not yours" would let a supervisor probe the tid
    /// space of children it has no relationship with.
    NotSupervised = -10,
}

impl Error {
    pub fn from_ret(v: i64) -> Option<Error> {
        Some(match v {
            -1 => Error::NoSuchSlot,
            -2 => Error::WrongObject,
            -3 => Error::NotPermitted,
            -4 => Error::BadPointer,
            -5 => Error::BadMethod,
            -6 => Error::BadSyscall,
            -7 => Error::OutOfMemory,
            -8 => Error::DeviceRefused,
            -9 => Error::StillAlive,
            -10 => Error::NotSupervised,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    /// Every variant round-trips through `from_ret`. The enum's `#[repr(i64)]` discriminants and
    /// `from_ret`'s match arms are two lists that must agree, and this crate is the one place a
    /// drift between them would not be a compile error: add a variant, forget the match arm, and
    /// userspace decodes a real kernel error as `None`. (The new variant must be added to `ALL`
    /// for this to guard it; that edit is at least in the same file.)
    #[test]
    fn every_error_round_trips_through_from_ret() {
        const ALL: &[Error] = &[
            Error::NoSuchSlot,
            Error::WrongObject,
            Error::NotPermitted,
            Error::BadPointer,
            Error::BadMethod,
            Error::BadSyscall,
            Error::OutOfMemory,
            Error::DeviceRefused,
            Error::StillAlive,
            Error::NotSupervised,
        ];
        for &e in ALL {
            assert_eq!(Error::from_ret(e as i64), Some(e));
        }
    }

    /// Success values and out-of-range negatives are not errors. `>= 0` is a result by the ABI's
    /// own rule, and an unknown negative must surface as "not an error I know" rather than being
    /// silently mapped onto the nearest variant.
    #[test]
    fn non_errors_decode_to_none() {
        assert_eq!(Error::from_ret(0), None);
        assert_eq!(Error::from_ret(1), None);
        assert_eq!(Error::from_ret(-11), None);
        assert_eq!(Error::from_ret(i64::MIN), None);
    }
}
