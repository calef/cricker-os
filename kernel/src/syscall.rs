//! The syscall boundary. **Three calls.**
//!
//! DECISIONS.md §4 rule 3 said the syscall surface stays narrow and explicit, *"a boundary, not a
//! habit."* §8 said milestone 7 was a hard decision point and that hacking one in without the
//! conversation meant the plan had failed. §10 had the conversation and chose capabilities.
//!
//! This is what that buys:
//!
//! ```text
//!   exit(code)                          authority over yourself
//!   yield()                             likewise
//!   invoke(cap, method, a0, a1, a2)     EVERYTHING ELSE
//! ```
//!
//! No `open`. No `read`. No `write`. No `fork`. **A process can only act on things it was
//! handed.** The ABI lives in `crates/abi`, which both the kernel and every user program depend
//! on, so the boundary is *one artifact* rather than two files that agree by luck.
//!
//! # No pointer ever crosses this boundary
//!
//! There used to be a `user_slice` here: the console `write` syscall took a `(ptr, len)` from
//! userspace and the kernel read the user's memory, which is why it needed the `AT S1E0R`
//! confused-deputy defence. Milestone 8 moved the console to a userspace server and deleted that
//! path. Today every argument is a scalar in a register (a capability slot, a method, a `va`, a
//! word), so the kernel follows no user pointer and there is no deputy to confuse. The primitive
//! that made the old check possible, `mmu::user_can_read`, is kept for the next syscall that does
//! take a user pointer.

use crate::arch::exceptions::TrapFrame;
use crate::arch::mmu;
use crate::cap::{Object, Rights};
use crate::sched;
use abi::Error;

/// Called from the `svc` arm of `exception_dispatch`.
pub fn dispatch(frame: &mut TrapFrame) {
    let nr = frame.x[8];

    // `exit` never comes back, so it is not part of the result-writing path below.
    if nr == abi::SYS_EXIT {
        sched::exit();
    }

    let result: Result<i64, Error> = match nr {
        abi::SYS_YIELD => {
            sched::yield_now();
            Ok(0)
        }
        abi::SYS_INVOKE => invoke(frame, frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4]),
        _ => Err(Error::BadSyscall),
    };

    // The return value goes back in x0, which `exception_restore` will pop into the register the
    // user is waiting on. Writing to the trap frame IS writing to the user's registers.
    frame.x[0] = match result {
        Ok(v) => v as u64,
        Err(e) => (e as i64) as u64,
    };
}

/// Act on a capability.
///
/// **The lookup is the security mechanism, and it is a bounds check.** `slot` is an index into
/// *this thread's* table, which lives in kernel memory. An empty slot is `NoSuchSlot`: not
/// "permission denied", but *there is nothing there*. That difference is what no-ambient-authority
/// feels like from the inside.
fn invoke(
    frame: &mut TrapFrame,
    slot: u64,
    method: u64,
    a0: u64,
    a1: u64,
    a2: u64,
) -> Result<i64, Error> {
    let cap = sched::current_cap(slot).map_err(|_| Error::NoSuchSlot)?;

    match cap.object {
        Object::Endpoint(ep) => match method {
            // SEND takes WRITE, RECV takes READ. The *same* endpoint, handed out with different
            // rights, is a one-way pipe in whichever direction each holder was trusted with.
            abi::endpoint::SEND => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // The three words are already in registers. **Nothing is read from user memory**,
                // so there is no pointer to validate and no confused-deputy question to ask. That
                // is the fastpath, and it is why IPC carries control and not bulk data (§10).
                sched::ipc_send(ep, [a0, a1, a2]);
                Ok(0)
            }
            abi::endpoint::RECV => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                let msg = sched::ipc_recv(ep);
                // Word 0 goes back the way every syscall result does, in x0 (dispatch writes it
                // from our return value). Words 1 and 2 we place directly, because a syscall
                // return is one register and a message is three.
                frame.x[1] = msg[1];
                frame.x[2] = msg[2];
                Ok(msg[0] as i64)
            }
            _ => Err(Error::BadMethod),
        },

        Object::Untyped(region) => match method {
            abi::untyped::MAP => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // Retype a page out of the untyped and map it, writable, at `a0` in the caller's
                // own address space. Both the page and any page tables come from the untyped, so
                // the KERNEL ALLOCATES NOTHING: `mmu::map_current_user_page`'s only source of
                // memory is the closure below, which bumps the untyped's watermark.
                let va = a0;
                // Reject the cheap failures BEFORE retyping a page for them: a non-page-aligned
                // or non-low-half address can never be mapped, and without this pre-check each
                // such attempt would silently spend a page of the process's own untyped (a
                // self-inflicted budget leak the audit noted). An already-mapped `va` still costs
                // one page, which is process-local and bounded by the untyped.
                if va & 0xfff != 0 || (va >> 48) != 0 {
                    return Err(Error::BadPointer);
                }
                match mmu::map_current_user_page(va, paging::Flags::user_data(), || {
                    crate::untyped::retype_page(region)
                }) {
                    Ok(()) => Ok(0),
                    Err(paging::MapError::OutOfFrames) => Err(Error::OutOfMemory),
                    Err(_) => Err(Error::BadPointer), // misaligned, already mapped, or wrong half
                }
            }
            _ => Err(Error::BadMethod),
        },

        Object::Irq(intid) => match method {
            // WAIT blocks on the endpoint the kernel routed this interrupt to. The interrupt
            // arrives as a message (sched::irq_notify), exactly like any other.
            abi::irq::WAIT => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                let ep = sched::irq_route(intid).ok_or(Error::WrongObject)?;
                let m = sched::ipc_recv(ep);
                Ok(m[0] as i64)
            }
            // ACK re-enables the interrupt at the GIC. The kernel masked it when it fired; now
            // that the driver has serviced the device, it is safe to let it fire again.
            abi::irq::ACK => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                crate::drivers::gic::enable(intid);
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },
    }
}
