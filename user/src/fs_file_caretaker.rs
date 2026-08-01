//! **The file caretaker: a directory capability attenuated to one file** (milestone 31 phase 2,
//! DECISIONS §27, notes/grant-expression.md).
//!
//! Milestone 31's headline is that naming a resource in a command *is* granting it: `run wc
//! report.txt` hands `wc` one readable file and nothing else. The FS service's unit of authority is
//! a *directory* (§27: the endpoint a client holds IS the directory capability), which is more than
//! that command said. This process is the difference.
//!
//! It is a **caretaker** in Mark Miller's sense: it holds the wider capability, exports a narrower
//! one, and is the only path between them. It opens the granted name once at startup, then serves
//! the same [`fs_proto::fs`] protocol its own client speaks, with a namespace of exactly one name
//! and a direction it cannot widen.
//!
//! # Why this is a separate process and not a check inside the FS server
//!
//! The FS server serves one endpoint and binds it to one directory. Serving a second, narrower
//! endpoint from the same process would need the server to receive on a *set* of endpoints, which
//! this kernel does not offer and which would be a design fork to add (a badge, seL4's answer, is
//! the shape it would take). Putting the attenuation in its own process needs nothing new: it is an
//! ordinary FS client above and an ordinary FS server below.
//!
//! And it makes the claim checkable rather than asserted. The confined program holds an endpoint to
//! *this* process and nothing that names the FS server, so it cannot route around the narrowing even
//! in principle; the boundary is an address space, not a branch. That is the same reason milestone
//! 36's checker lives outside the component it checks.
//!
//! # Capability contract (kernel/src/user.rs `fs_service::start_granted`)
//!
//! - **slot 0**: the FS-service endpoint, `WRITE`. The directory capability being attenuated. This
//!   is the whole of what the caretaker holds that its client does not.
//! - **slot 1**: the narrowed endpoint, `READ`. The confined program `CALL`s here; this endpoint IS
//!   the per-file capability.
//! - **slot 2**: a report endpoint, `WRITE`. Readiness, sent once the granted name is open.
//! - **[`PAGE_VA`]**: the page shared with the FS server *and* with the client. One frame, three
//!   parties, and that is sound because every request on both sides is a blocking `CALL`: the client
//!   is parked inside its call for the whole time the caretaker is using the page, so the two uses
//!   cannot interleave. A second page would buy nothing but a copy.
//!
//! The granted name and direction arrive in the three `START` argument words
//! ([`fs_proto::grant`]), so a per-file grant costs no extra frame and the caretaker needs nothing
//! mapped before it runs.

#![no_std]
#![no_main]

use fs_proto::{fs, grant, op, reply_err, reply_errno};
use user_rt::{call, invoke, recv_cap, send};

/// The FS-service endpoint: the directory capability this process attenuates.
const FS: u64 = 0;
/// The narrowed endpoint: where the confined program calls. Holding it is holding the file.
const CLIENT: u64 = 1;
/// Where readiness goes, once.
const REPORT: u64 = 2;

/// The one page, shared with the FS server above and the client below. See the module note on why
/// one frame is enough.
const PAGE_VA: u64 = 0x0000_0000_0060_0000;
/// Its size, the contract's transfer unit.
const PAGE: usize = fs_proto::PAGE;

/// Copy `bytes` into the shared page.
fn put(bytes: &[u8]) {
    for (i, &b) in bytes.iter().take(PAGE).enumerate() {
        // SAFETY: PAGE_VA is a mapped, writable page of PAGE bytes; the loop is clamped to it.
        unsafe { core::ptr::write_volatile((PAGE_VA + i as u64) as *mut u8, b) };
    }
}

/// Read `out.len()` bytes out of the shared page.
fn get(out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        // SAFETY: as above; callers clamp `out` to the page.
        *b = unsafe { core::ptr::read_volatile((PAGE_VA + i as u64) as *const u8) };
    }
}

/// One request to the FS server, forwarded verbatim. The caretaker does not reinterpret a request
/// it has decided to allow: it passes the words through, so the file behaves exactly as it would
/// through the directory capability. Attenuation is about *which* requests pass, not about changing
/// what they mean.
fn forward(w0: u64, w1: u64) -> i64 {
    call(FS, w0, w1).0 as i64
}

/// Answer the blocked caller through the one-shot Reply the kernel minted.
fn reply(slot: u64, r0: i64) {
    // SAFETY: the kernel minted this Reply naming the blocked caller; REPLY consumes it.
    unsafe { invoke(slot, abi::reply::REPLY, r0 as u64, 0, 0) };
}

/// **The serve loop: the whole of the narrowing, in one place.**
///
/// `handle` is the FS server's handle for the granted file, which the client never learns; it sends
/// [`grant::HANDLE`] and this substitutes. That indirection is not decoration. It means a client
/// that guesses handle numbers is guessing in a space with exactly one inhabitant, and every guess
/// that misses gets `EBADF` from the same check.
fn serve(handle: u64, name: &[u8], writable: bool) -> ! {
    loop {
        let (w0, reply_slot, w1) = recv_cap(CLIENT);
        let len = fs::req_len(w0).min(PAGE);
        let asked = fs::req_handle(w0);

        let r: i64 = match op(w0) {
            // The one name this capability designates. Anything else is ENOENT: in *this* scope
            // there is no such name, and nothing consulted a permission. A holder cannot enumerate
            // and cannot learn what else the directory holds, which is the difference between a
            // narrowed capability and a permission check over a wider one.
            //
            // Only a name of exactly the granted length is ever copied off the page: the buffer is
            // [`grant::MAX_NAME`] bytes, not a page, because this process runs on the single stack
            // page `run` maps and a 4 KiB local would overflow it on the first request. A length
            // mismatch is refused before any copy, which is both the cheap check and the safe one.
            fs::OPEN => {
                let mut asked_name = [0u8; grant::MAX_NAME];
                if len == name.len() {
                    get(&mut asked_name[..len]);
                }
                if len == name.len() && &asked_name[..len] == name {
                    grant::HANDLE as i64
                } else {
                    reply_err(2) // ENOENT
                }
            }
            // A file capability is not a directory, so "make a name in it" is not a request that
            // means anything here. ENOTDIR says that; EACCES would imply a policy that could have
            // said yes.
            fs::CREATE => reply_err(grant::ENOTDIR),
            fs::READ if asked == grant::HANDLE => {
                forward(fs::req(fs::READ, handle, len as u64), w1)
            }
            // Write and truncate are the direction the grant either carries or does not. There is
            // nothing to consult and no way to widen it from in here.
            fs::WRITE if asked == grant::HANDLE => {
                if writable {
                    forward(fs::req(fs::WRITE, handle, len as u64), w1)
                } else {
                    reply_err(grant::EROFS)
                }
            }
            fs::TRUNCATE if asked == grant::HANDLE => {
                if writable {
                    forward(fs::req(fs::TRUNCATE, handle, 0), w1)
                } else {
                    reply_err(grant::EROFS)
                }
            }
            fs::FSTAT if asked == grant::HANDLE => forward(fs::req(fs::FSTAT, handle, 0), 0),
            // CLOSE is answered, not forwarded: the caretaker owns the underlying handle for its
            // whole life, and a client closing "its" handle must not be able to make the *next*
            // request fail. Re-opening the granted name is always allowed, so this is a no-op with
            // a truthful success rather than a lie.
            fs::CLOSE if asked == grant::HANDLE => 0,
            // **Extended attributes are not forwarded, and the refusal says exactly that**
            // (milestone 57, DECISIONS §42). This caretaker does not pass attribute requests
            // through, so the honest statement is "this capability does not carry them", not
            // `EBADF` ("no such handle") and not `EINVAL` ("you sent nonsense"). A verb that is not
            // offered fails loudly and the caller decides; what it must never receive is a wrong
            // answer wearing a right answer's shape. The refusal is uniform across all three
            // caretakers on purpose: a layer that worked through one narrowing and not another
            // would make behaviour depend on which caretaker happened to be in the chain, which is
            // the variation §42 exists to forbid.
            fs::GETXATTR | fs::SETXATTR | fs::LISTXATTR | fs::REMOVEXATTR => {
                reply_err(fs_proto::xattr::ENOTSUP)
            }
            // Every other handle number, and every verb this contract does not carry. One refusal
            // site, so a forged handle is refused by the same check as a stale one.
            _ => reply_err(9), // EBADF
        };
        reply(reply_slot, r);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(name_lo: u64, name_hi: u64, spec: u64) -> ! {
    let mut buf = [0u8; grant::MAX_NAME];
    let n = grant::unpack_name(name_lo, name_hi, grant::spec_len(spec), &mut buf);
    let name = &buf[..n];

    // Open the granted name once, through the directory capability, and never again. If it is not
    // there, this process has nothing to serve: die rather than come up serving a hole, so the
    // failure lands at the grant rather than at the first read.
    put(name);
    let (r0, _) = call(FS, fs::req(fs::OPEN, 0, n as u64), 0);
    if reply_errno(r0 as i64).is_some() {
        panic!();
    }

    send(REPORT, fs_proto::fixture::READY, 0, 0);
    serve(r0, name, grant::writable(spec));
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A caretaker that cannot open its file is a caretaker with nothing to attenuate: trap, and the
    // kernel reaps it legibly. Its client then blocks on a call nobody answers, which
    // notes/fs-server.md records as the cost of a dead server on a blocking contract.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
