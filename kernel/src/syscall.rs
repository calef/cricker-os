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
//! # The interesting code in this file is not the dispatch. It is `user_slice`.

use crate::arch::exceptions::TrapFrame;
use crate::arch::mmu;
use crate::cap::{Object, Rights};
use crate::sched;
use abi::Error;
use frames::FRAME_SIZE;

/// The most bytes a process may ask us to print in one go.
///
/// Not a performance limit. `len` is a number the *user chose*, and without a cap it can name
/// the whole address space, and we would sit in a loop at EL1 with interrupts on, doing its
/// bidding, for a very long time.
const MAX_WRITE: u64 = 4096;

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
        Object::Console => match method {
            abi::console::WRITE => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }

                let bytes = user_slice(a0, a1)?;
                crate::console::write_bytes(bytes);
                Ok(bytes.len() as i64)
            }
            _ => Err(Error::BadMethod),
        },

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
    }
}

/// **Turn a user pointer into bytes the kernel is willing to look at.**
///
/// # This function is the confused deputy, refused
///
/// The user hands us `ptr` and `len`. Both are numbers *it chose*. It passes
/// `0xffff_0000_4008_0000`, which is our own `.text`.
///
/// The kernel can read that. It reads it all day. So a loader that simply dereferences the
/// pointer prints the kernel's memory **on the user's behalf, using the kernel's own authority**,
/// and the program that could not read one byte of it receives all of it.
///
/// **And no capability check catches this.** The console capability is perfectly genuine; the
/// program is entitled to print. The authority that leaked was never the console's. It was ours.
/// That is exactly the compiler service and the billing log (notes/capabilities.md), and it is
/// why the confused deputy is a *deputy*: it had the right to do the thing, and it did the thing
/// for the wrong principal.
///
/// The refusal is one question, asked of the hardware rather than re-derived in software:
///
/// > **Could EL0 have read this address itself?**
///
/// `AT S1E0R` answers it in one instruction (`mmu::user_can_read`). If the user could not, then
/// neither may we, **on its behalf**.
fn user_slice(ptr: u64, len: u64) -> Result<&'static [u8], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if len > MAX_WRITE {
        return Err(Error::BadPointer);
    }

    let end = ptr.checked_add(len).ok_or(Error::BadPointer)?;

    // Every page the range touches. Permissions are per-page, so page granularity is exactly
    // right, and a range that straddles a mapped page into an unmapped one is caught.
    let mut page = ptr & !(FRAME_SIZE - 1);
    while page < end {
        if !mmu::user_can_read(page) {
            return Err(Error::BadPointer);
        }
        page += FRAME_SIZE;
    }

    // TOCTOU, and why it is not a hole *yet*: between the check above and the read below, the
    // mapping could change. Nothing can currently do that. An address space belongs to exactly
    // one thread (kernel/src/thread.rs), so there is no second thread to unmap it, and we do not
    // have shared address spaces or demand paging. **When either of those arrives, this comment
    // becomes a bug**, and the fix is to copy the bytes under a lock rather than borrow them.
    //
    // SAFETY: every page is mapped and EL0-readable, which the hardware just confirmed. At EL1
    // we may read anything EL0 may read.
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}
