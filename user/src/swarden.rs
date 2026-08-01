//! **The set warden: a directory capability attenuated to the names a pattern matched** (milestone
//! 47's globbing lane, notes/glob-grant.md).
//!
//! `fwarden` narrows a directory capability to one **file**. `dwarden` narrows it to one
//! **subtree**. This narrows it to a **set of names in one directory**, which is what `rm *.txt`
//! designates and what neither of the other two can express.
//!
//! # Why this is a third warden and not a mode on the second
//!
//! It was a real fork, and the reasons are three, in the order they mattered:
//!
//! 1. **`dwarden` performs no checks at all, and that is its design** (notes/dir-capability.md).
//!    Every attenuation it provides lives in the handle the FS server minted for it at startup, so
//!    there is no branch in it that could be wrong. A name filter is a check, and one that has to be
//!    consulted on **every name-taking verb** (`OPEN`, `CREATE`, `OPENDIR`, `MKDIR`, `UNLINK`,
//!    `RMDIR`, and both halves of `RENAME`). Adding a mode would trade that program's one strong
//!    property for a switch, and it would put a forget-a-verb surface in the warden that most
//!    deliberately has none.
//! 2. **`fwarden` cannot be generalized into this**, though the roadmap's phrasing invites the
//!    thought. It serves the *file* protocol: its client `OPEN`s a fixed handle and the warden
//!    answers `ENOTDIR` to `CREATE` and refuses every directory verb. Teaching it a set would mean
//!    teaching it `READDIR`, `UNLINK` and `RMDIR`, which is not generalizing a file warden, it is
//!    writing a directory warden and calling it one.
//! 3. **The grants have different shapes.** A single name rides in two `START` argument words; a set
//!    does not fit in any number of registers, so this program is started with a **frame** as well
//!    (`fs_proto::nameset`). Bolting that onto `dwarden` would make every subtree grant carry
//!    machinery it does not use.
//!
//! What *is* the roadmap's finding, and holds exactly: this is the same caretaker, speaking the same
//! `fs_proto` protocol above and below, over a wider namespace. **Nothing new in the kernel.**
//!
//! # The rule, and the fact that there is only one
//!
//! **A name that is not in the set does not exist here.** One sentence covers every verb: reading
//! it, writing it, creating it, removing it and renaming onto it are all `ENOENT`, because in this
//! scope there is no such name and nothing consulted a permission. That is `fwarden`'s sentence
//! (DECISIONS §27) over a set instead of over one name, and it is why there is no per-verb policy
//! here to get wrong.
//!
//! The filter applies at the **granted directory** and nowhere else. A handle minted below it (by
//! descending into a matched directory, which needs a `-r` grant's `DESCEND`) is unfiltered, and
//! that is right rather than a gap: the pattern selected top-level names, and what is under a
//! directory it selected was never a question the pattern asked.
//!
//! # Capability contract (kernel/src/user.rs `fs_service::start_granted_set`)
//!
//! - **slot 0**: the FS-service endpoint, `WRITE`. The directory capability being attenuated.
//! - **slot 1**: the narrowed endpoint, `READ`. Holding it is holding the set.
//! - **slot 2**: a report endpoint, `WRITE`. Readiness, sent once the descent has succeeded.
//! - **[`PAGE_VA`]**: the page shared with the FS server *and* the client, for `fwarden`'s reason.
//! - **[`SET_VA`]**: a **read-only** frame holding the encoded set, copied into a local at startup
//!   and never read again, so nothing that happens to that page afterwards can widen the namespace.
//!
//! The granted directory's name and rights ride in the three `START` argument words exactly as
//! `dwarden`'s do.

#![no_std]
#![no_main]

use fs_proto::{dirent, fs, grant, nameset, op, reply_err, reply_errno};
use user_rt::{call, invoke, recv_cap, send};

/// The FS-service endpoint: the directory capability this process attenuates.
const FS: u64 = 0;
/// The narrowed endpoint: where the confined program calls. Holding it is holding the set.
const CLIENT: u64 = 1;
/// Where readiness goes, once.
const REPORT: u64 = 2;

/// The one page, shared with the FS server above and the client below.
const PAGE_VA: u64 = 0x0000_0000_0060_0000;
/// The read-only page the encoded name set arrives in. A second frame, and the only place in the
/// grant machinery that needs one: a set is the first thing a grant carries that registers cannot.
const SET_VA: u64 = 0x0000_0000_0070_0000;
/// The shared page's size, the contract's transfer unit.
const PAGE: usize = fs_proto::PAGE;

/// How many handles a confined program may hold open at once, including the granted directory at
/// index 0. `dwarden`'s number, for `dwarden`'s reason: no heap, one stack page.
const SLOTS: usize = 16;

/// `EMFILE`: as many handles open through this warden as the client may have.
const EMFILE: i32 = 24;
/// `EINVAL`, for the verbs this contract does not carry and for closing the root of a namespace.
const EINVAL: i32 = 22;
/// `EBADF`, for a handle this process never minted.
const EBADF: i32 = 9;
/// `ENOENT`: **in this scope there is no such name.** The only refusal the set itself ever produces.
const ENOENT: i32 = 2;

/// Copy `bytes` into the shared page at `off`.
fn put_at(off: usize, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        if off + i >= PAGE {
            return;
        }
        // SAFETY: PAGE_VA is a mapped, writable page of PAGE bytes; the loop is clamped to it.
        unsafe { core::ptr::write_volatile((PAGE_VA + (off + i) as u64) as *mut u8, b) };
    }
}

/// Read `out.len()` bytes out of the shared page starting at `off`.
fn get_at(off: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        // SAFETY: as above; every caller clamps `out` and `off` to the page.
        *b = unsafe { core::ptr::read_volatile((PAGE_VA + (off + i) as u64) as *const u8) };
    }
}

/// One request to the FS server, forwarded verbatim except for the handle this process substituted.
fn forward(w0: u64, w1: u64) -> i64 {
    call(FS, w0, w1).0 as i64
}

/// Answer the blocked caller through the one-shot Reply the kernel minted.
fn reply(slot: u64, r0: i64) {
    // SAFETY: the kernel minted this Reply naming the blocked caller; REPLY consumes it.
    unsafe { invoke(slot, abi::reply::REPLY, r0 as u64, 0, 0) };
}

/// The client's handle namespace, `dwarden`'s table verbatim: `table[i]` is the FS server's handle
/// for the client's handle `i`. Slot 0 is the granted directory and is never freed.
struct Table([Option<u64>; SLOTS]);

impl Table {
    fn get(&self, client: u64) -> Option<u64> {
        self.0.get(client as usize).copied().flatten()
    }

    fn install(&mut self, server: u64) -> Option<u64> {
        let i = self.0.iter().skip(1).position(|s| s.is_none())? + 1;
        self.0[i] = Some(server);
        Some(i as u64)
    }
}

/// Read a name of `len` bytes out of the shared page at `off`.
///
/// A name longer than a grant can carry is truncated to nothing rather than clamped, because the set
/// holds no such name and a clamped name is a *different* name: comparing a prefix against the set
/// is how a filter accidentally admits `gl-one.txtXXX`.
fn name_at(off: usize, len: usize, buf: &mut [u8; grant::MAX_NAME]) -> usize {
    if len == 0 || len > grant::MAX_NAME || off + len > PAGE {
        return 0;
    }
    get_at(off, &mut buf[..len]);
    len
}

/// **The serve loop.** Every arm is `dwarden`'s, plus one question asked in one place: when a
/// request names something **in the granted directory**, is that name in the set?
fn serve(dir: u64, set: &[u8]) -> ! {
    let mut table = Table([None; SLOTS]);
    table.0[0] = Some(dir);

    loop {
        let (w0, reply_slot, w1) = recv_cap(CLIENT);
        let len = fs::req_len(w0).min(PAGE);
        let verb = op(w0);
        let asked = fs::req_handle(w0);
        let Some(server_handle) = table.get(asked) else {
            reply(reply_slot, reply_err(EBADF));
            continue;
        };
        // The filter is at the granted directory and nowhere else: what is under a directory the
        // pattern matched was never a question the pattern asked.
        let filtered = asked == fs::ROOT;

        let r: i64 = match verb {
            // The verbs that mint a handle. A name outside the set is refused **before** anything is
            // forwarded, so the FS server is never asked a question about a name this capability
            // cannot designate, and a refusal cannot be told from a name that is not on the disk.
            fs::OPEN | fs::CREATE | fs::OPENDIR | fs::MKDIR => {
                let mut buf = [0u8; grant::MAX_NAME];
                let n = name_at(0, len, &mut buf);
                if filtered && !nameset::contains(set, &buf[..n]) {
                    reply_err(ENOENT)
                } else {
                    let r = forward(fs::req(verb, server_handle, len as u64), w1);
                    if r < 0 {
                        r
                    } else {
                        match table.install(r as u64) {
                            Some(client) => client as i64,
                            None => {
                                forward(fs::req(fs::CLOSE, r as u64, 0), 0);
                                reply_err(EMFILE)
                            }
                        }
                    }
                }
            }
            // The removals. Same question, and it is the one that matters most: a set capability's
            // whole job is that `rm *.txt` cannot reach `notes.md`.
            fs::UNLINK | fs::RMDIR => {
                let mut buf = [0u8; grant::MAX_NAME];
                let n = name_at(0, len, &mut buf);
                if filtered && !nameset::contains(set, &buf[..n]) {
                    reply_err(ENOENT)
                } else {
                    forward(fs::req(verb, server_handle, len as u64), w1)
                }
            }
            // **`RENAME` needs both names in the set**, and the destination is the load-bearing
            // half: renaming a matched name onto an unmatched one would destroy a name this
            // capability was never granted, which is an escape even though nothing was opened.
            //
            // The consequence is real and is declared rather than worked around: a set is a *fixed*
            // namespace, so a set capability cannot move a name out of it. `mv *.txt` is not
            // something this shape can express.
            fs::RENAME => {
                let dst_len = fs::dst_len(w1).min(PAGE);
                let mut src = [0u8; grant::MAX_NAME];
                let mut dst = [0u8; grant::MAX_NAME];
                let sn = name_at(0, len, &mut src);
                let dn = name_at(len, dst_len, &mut dst);
                let dst_handle = table.get(fs::dst_handle(w1));
                let src_ok = !filtered || nameset::contains(set, &src[..sn]);
                let dst_ok = dst_handle != Some(dir) || nameset::contains(set, &dst[..dn]);
                match dst_handle {
                    _ if !(src_ok && dst_ok) => reply_err(ENOENT),
                    Some(d) => forward(
                        fs::req(verb, server_handle, len as u64),
                        fs::rename_dst(d, dst_len as u64),
                    ),
                    None => reply_err(EBADF),
                }
            }
            // **`READDIR` at the granted directory is answered from the set itself**, with no round
            // trip and no cursor translation, because the set *is* the namespace: there is nothing
            // to filter out of a server's listing that is not already absent from this one.
            //
            // It is deliberately not gated on the `ENUMERATE` the grant carries, and that is not a
            // widening: what a listing here reveals is exactly the set, which the command line
            // already printed before this process existed. Below the granted directory the server's
            // own listing is forwarded, and its rights apply as usual.
            fs::READDIR if filtered => list(set, w1 as usize),
            fs::READ | fs::WRITE => forward(fs::req(verb, server_handle, len as u64), w1),
            fs::TRUNCATE | fs::READDIR => forward(fs::req(verb, server_handle, 0), w1),
            fs::FSTAT => forward(fs::req(fs::FSTAT, server_handle, 0), 0),
            fs::CLOSE => {
                if asked == fs::ROOT {
                    reply_err(EINVAL)
                } else {
                    table.0[asked as usize] = None;
                    forward(fs::req(fs::CLOSE, server_handle, 0), 0)
                }
            }
            // **Extended attributes are not forwarded, and the refusal says exactly that**
            // (milestone 57, DECISIONS §42). See `fwarden` for the argument. All three wardens
            // answer the same word so that behaviour does not depend on which caretaker a program
            // happens to be behind.
            fs::GETXATTR | fs::SETXATTR | fs::LISTXATTR | fs::REMOVEXATTR => {
                reply_err(fs_proto::xattr::ENOTSUP)
            }
            _ => reply_err(EINVAL),
        };
        reply(reply_slot, r);
    }
}

/// Write the set into the shared page as directory entries, from `cursor`, and return the bytes.
///
/// The type each name carries is what the directory said **when the grant was planned**, which is
/// the same resolve-at-grant-time rule the rest of a grant follows. A file that became a directory
/// afterwards is listed as it was designated, and the verb that acts on it answers for what it is.
fn list(set: &[u8], cursor: usize) -> i64 {
    let mut off = 0usize;
    let mut rec = [0u8; dirent::record_len(grant::MAX_NAME)];
    for (name, is_dir) in nameset::iter(set).skip(cursor) {
        let Some(n) = dirent::encode(&mut rec, name, is_dir) else {
            break;
        };
        if off + n > PAGE {
            break;
        }
        put_at(off, &rec[..n]);
        off += n;
    }
    off as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(name_lo: u64, name_hi: u64, spec: u64) -> ! {
    // **Copy the set before anything else runs.** It arrives in a read-only page, and this process
    // then works from a local for the rest of its life, so the namespace it serves cannot be changed
    // by anything that touches that page later. It is 273 bytes on the one stack page `run` maps.
    let mut set = [0u8; nameset::BYTES];
    for (i, b) in set.iter_mut().enumerate() {
        // SAFETY: SET_VA is a mapped, read-only page of at least nameset::BYTES bytes.
        *b = unsafe { core::ptr::read_volatile((SET_VA + i as u64) as *const u8) };
    }

    let mut buf = [0u8; grant::MAX_NAME];
    let n = grant::unpack_name(name_lo, name_hi, grant::spec_len(spec), &mut buf);

    // The descent, exactly `dwarden`'s: ask for the granted directory with the granted rights, and
    // die rather than come up serving a hole. Everything the client reaches, it reaches through the
    // handle this one request minted.
    put_at(0, &buf[..n]);
    let (r0, _) = call(
        FS,
        fs::req(fs::OPENDIR, fs::ROOT, n as u64),
        grant::spec_rights(spec),
    );
    if reply_errno(r0 as i64).is_some() {
        panic!();
    }
    // A set with nothing in it is not a capability anybody meant to hand out, and the shell refuses
    // to plan one (`capsh::Refusal::NoMatch`). If one arrives anyway the wiring is wrong, and dying
    // here is better than serving a namespace with no names in it.
    if nameset::count(&set) == 0 {
        panic!();
    }

    send(REPORT, fs_proto::fixture::READY, 0, 0);
    serve(r0, &set);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A warden that could not descend, or was handed an empty set, has nothing to attenuate: trap,
    // and the kernel reaps it legibly. Its client then blocks on a call nobody answers, which
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
