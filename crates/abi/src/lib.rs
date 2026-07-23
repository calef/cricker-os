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

/// An index into the calling thread's capability table. **Not a pointer, not a handle you can
/// guess.** The kernel looks in *your* table, and if the slot is empty you get `NoSuchSlot`.
pub type CapSlot = u64;

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
pub mod rights {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const GRANT: u64 = 1 << 2;
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
    /// `invoke(cap, SETUP_QUEUE, num, _, _)` -> 0. The kernel programs queue 0's ring addresses to
    /// the fixed offsets in the driver's DMA region, so the driver never chooses them.
    pub const SETUP_QUEUE: u64 = 2;
    /// `invoke(cap, NOTIFY, _, _, _)` -> 0, or `DmaRefused` if a newly-published descriptor points
    /// outside the driver's DMA region. On refusal the device is NOT told to go.
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
            _ => return None,
        })
    }
}
