//! **The directory warden: a directory capability attenuated to one subtree** (milestone 47,
//! notes/dir-capability.md).
//!
//! `fwarden` narrows a directory capability to one *file*. This narrows it to one *directory*, which
//! is the shape `cd`, a per-process namespace, and every "here is somewhere to write your logs"
//! grant actually want. It is the same caretaker pattern (Mark Miller's term): it holds the wider
//! capability, exports a narrower one, and is the only path between them.
//!
//! # It performs no checks, and that is the design
//!
//! `fwarden` has to inspect requests, because a file capability and a directory capability speak
//! different protocols and it is translating between them. This one does not. At startup it sends
//! **one** [`fs::OPENDIR`], which asks the FS server for a handle to the granted name carrying the
//! granted rights; the server intersects those rights with its own and refuses if the answer came up
//! short. Everything the client can reach afterwards is reached *through that handle*, so the
//! attenuation lives in the handle the server minted and not in any branch here.
//!
//! What this process actually does is **translate a namespace**. The client numbers handles in its
//! own space starting at [`fs_proto::fs::ROOT`], which is the granted directory; this maps each one
//! to the FS server's number and forwards. A client that guesses a handle is guessing in a table
//! with a handful of inhabitants, none of which it chose, and a handle this process never minted is
//! `EBADF` from one check.
//!
//! Two consequences worth stating, because they are what the confinement rests on:
//!
//! - **The confined program holds an endpoint to this process and nothing that names the FS server**,
//!   so "it cannot reach the parent directory" is a property of its cspace rather than of a branch
//!   it is trusted to take. The boundary is an address space, which is why `fwarden`'s tests are
//!   witnesses and not assertions, and why this one's are too.
//! - **A rights-carrying handle alone would not confine it.** The FS server's handle table is per
//!   server, not per client, so a program holding the FS-service endpoint could always name
//!   [`fs_proto::fs::ROOT`] and be back at the image root. The handle is the authority; the endpoint
//!   is the boundary. That is the whole argument for this process existing.
//!
//! # Capability contract (kernel/src/user.rs `fs_service::start_granted_dir`)
//!
//! - **slot 0**: the FS-service endpoint, `WRITE`. The directory capability being attenuated.
//! - **slot 1**: the narrowed endpoint, `READ`. The confined program `CALL`s here; this endpoint IS
//!   the subtree capability.
//! - **slot 2**: a report endpoint, `WRITE`. Readiness, sent once the descent has succeeded.
//! - **[`PAGE_VA`]**: the page shared with the FS server *and* with the client, one frame for all
//!   three, sound for the reason `fwarden`'s note gives: every request on both hops is a blocking
//!   `CALL`, so the client is parked inside its own call for the whole time this process is using
//!   the page.
//!
//! The granted name and the requested rights arrive in the three `START` argument words
//! ([`fs_proto::grant`], whose spec word carries a rights mask of any width), so a subtree grant
//! costs no extra frame.

#![no_std]
#![no_main]

use fs_proto::{fs, grant, op, reply_err, reply_errno};
use user_rt::{call, invoke, recv_cap, send};

/// The FS-service endpoint: the directory capability this process attenuates.
const FS: u64 = 0;
/// The narrowed endpoint: where the confined program calls. Holding it is holding the subtree.
const CLIENT: u64 = 1;
/// Where readiness goes, once.
const REPORT: u64 = 2;

/// The one page, shared with the FS server above and the client below.
const PAGE_VA: u64 = 0x0000_0000_0060_0000;
/// Its size, the contract's transfer unit.
const PAGE: usize = fs_proto::PAGE;

/// How many handles a confined program may hold open at once through this warden, including the
/// granted directory itself at index 0.
///
/// Fixed and small because this process has no heap and runs on the single stack page `run` maps: a
/// growable table would need an allocator and a 4 KiB local would overflow the stack on the first
/// request. Sixteen is well past what the attacker and any `cd`/`ls` sequence needs, and running out
/// is `EMFILE`, which is a real POSIX answer rather than a silently reused slot.
const SLOTS: usize = 16;

/// `EMFILE`: the client has as many handles open through this warden as it may.
const EMFILE: i32 = 24;
/// `EINVAL`, for the verbs this contract does not carry and for closing the root of a namespace.
const EINVAL: i32 = 22;
/// `EBADF`, for a handle this process never minted. One refusal site, so a forged handle and a
/// stale one are refused by the same check.
const EBADF: i32 = 9;

/// Copy `bytes` into the shared page.
fn put(bytes: &[u8]) {
    for (i, &b) in bytes.iter().take(PAGE).enumerate() {
        // SAFETY: PAGE_VA is a mapped, writable page of PAGE bytes; the loop is clamped to it.
        unsafe { core::ptr::write_volatile((PAGE_VA + i as u64) as *mut u8, b) };
    }
}

/// One request to the FS server, forwarded verbatim except for the handle this process substituted.
/// The bytes ride in the page all three parties share, so a forward copies nothing.
fn forward(w0: u64, w1: u64) -> i64 {
    call(FS, w0, w1).0 as i64
}

/// Answer the blocked caller through the one-shot Reply the kernel minted.
fn reply(slot: u64, r0: i64) {
    // SAFETY: the kernel minted this Reply naming the blocked caller; REPLY consumes it.
    unsafe { invoke(slot, abi::reply::REPLY, r0 as u64, 0, 0) };
}

/// The client's handle namespace: `table[i]` is the FS server's handle for the client's handle `i`,
/// or `None` for a slot the client does not hold. Slot 0 is the granted directory and is never
/// freed, which is what makes [`fs_proto::fs::ROOT`] mean "the root of *your* namespace" on this
/// endpoint exactly as it does on the FS server's.
struct Table([Option<u64>; SLOTS]);

impl Table {
    /// The FS server's handle for a client handle, or `None` if the client never got it.
    fn get(&self, client: u64) -> Option<u64> {
        self.0.get(client as usize).copied().flatten()
    }

    /// Record a handle the FS server just minted and return the client's number for it, or `None`
    /// if the client is already holding as many as it may.
    fn install(&mut self, server: u64) -> Option<u64> {
        // Never slot 0: that is the granted directory, and handing it out again would let a CLOSE
        // of the new number take the whole namespace away.
        let i = self.0.iter().skip(1).position(|s| s.is_none())? + 1;
        self.0[i] = Some(server);
        Some(i as u64)
    }
}

/// **The serve loop: a namespace translation and nothing else.**
///
/// Every arm does the same three things, which is the point: map the client's handle to the FS
/// server's, forward the request unchanged, and (for the verbs that mint a handle) map the answer
/// back. There is no rights check anywhere in here, because the rights are on `dir`, the handle the
/// FS server minted at startup, and everything the client can reach it reached through that handle.
fn serve(dir: u64) -> ! {
    let mut table = Table([None; SLOTS]);
    table.0[0] = Some(dir);

    loop {
        let (w0, reply_slot, w1) = recv_cap(CLIENT);
        let len = fs::req_len(w0).min(PAGE) as u64;
        let verb = op(w0);
        let Some(server_handle) = table.get(fs::req_handle(w0)) else {
            reply(reply_slot, reply_err(EBADF));
            continue;
        };

        let r: i64 = match verb {
            // The verbs that mint a handle: forward, then give the client a number of its own. The
            // client never learns the FS server's numbering, so a handle it guesses is a guess in a
            // space it did not choose.
            fs::OPEN | fs::CREATE | fs::OPENDIR | fs::MKDIR => {
                let r = forward(fs::req(verb, server_handle, len), w1);
                if r < 0 {
                    r
                } else {
                    match table.install(r as u64) {
                        Some(client) => client as i64,
                        None => {
                            // Give the handle straight back rather than leaking it in the server:
                            // this process holds the only reference to it and would otherwise pin a
                            // node for the rest of the boot.
                            forward(fs::req(fs::CLOSE, r as u64, 0), 0);
                            reply_err(EMFILE)
                        }
                    }
                }
            }
            // **The one verb whose second word is not opaque**, because it names a second directory.
            // Both handles are the client's, so both are translated, and this is still translation
            // and not a check: the FS server decides whether the rights on those two handles allow
            // the move, and this process does not know or ask what they are.
            fs::RENAME => match table.get(fs::dst_handle(w1)) {
                Some(dst) => forward(
                    fs::req(verb, server_handle, len),
                    fs::rename_dst(dst, fs::dst_len(w1).min(PAGE) as u64),
                ),
                None => reply_err(EBADF),
            },
            // The verbs that operate on a handle. `w1` passes through untouched: it is an offset for
            // READ/WRITE, a size for TRUNCATE and a cursor for READDIR, and none of them means
            // anything to this process.
            //
            // `UNLINK` is here rather than with the handle-minting verbs above because it hands
            // nothing back: a name goes away and no capability appears, so there is nothing to
            // install in the table. It needs no check here for the same reason nothing else does,
            // and the reason is worth stating for the verb that *destroys* something: the FS server
            // resolves the name under the handle this process substituted, and that handle carries
            // whatever `REMOVE` the grant carried.
            fs::READ | fs::WRITE | fs::UNLINK => forward(fs::req(verb, server_handle, len), w1),
            fs::TRUNCATE | fs::READDIR => forward(fs::req(verb, server_handle, 0), w1),
            fs::FSTAT => forward(fs::req(fs::FSTAT, server_handle, 0), 0),
            // Closing the granted directory is refused for the same reason the FS server refuses to
            // close its own root: it is not something the client opened, and a client that could
            // close it could make every later request in its own session fail.
            fs::CLOSE => {
                let client = fs::req_handle(w0);
                if client == fs::ROOT {
                    reply_err(EINVAL)
                } else {
                    table.0[client as usize] = None;
                    forward(fs::req(fs::CLOSE, server_handle, 0), 0)
                }
            }
            _ => reply_err(EINVAL),
        };
        reply(reply_slot, r);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(name_lo: u64, name_hi: u64, spec: u64) -> ! {
    let mut buf = [0u8; grant::MAX_NAME];
    let n = grant::unpack_name(name_lo, name_hi, grant::spec_len(spec), &mut buf);

    // **The whole grant, in one request.** Descend into the named directory asking for exactly the
    // rights this process was started with. The FS server intersects them with its own and refuses
    // if the intersection is smaller, so a wiring that asked for more than the parent had fails
    // here, loudly, rather than coming up serving a capability nobody meant to hand out.
    put(&buf[..n]);
    let (r0, _) = call(
        FS,
        fs::req(fs::OPENDIR, fs::ROOT, n as u64),
        grant::spec_rights(spec),
    );
    if reply_errno(r0 as i64).is_some() {
        panic!();
    }

    send(REPORT, fs_proto::fixture::READY, 0, 0);
    serve(r0);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A warden that could not descend is a warden with nothing to attenuate: trap, and the kernel
    // reaps it legibly. Its client then blocks on a call nobody answers, which notes/fs-server.md
    // records as the cost of a dead server on a blocking contract.
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
