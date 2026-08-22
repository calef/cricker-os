//! **The root supervisor: an init that gives its authority away** (milestone 22 phase B.2).
//!
//! init is the unverified component that builds every other process, and phase B.1 closed the
//! question of *what bytes it is*. This closes the other half: **what a broken one can still do.**
//!
//! The pre-B.2 init (`system_initializer`, `hello`'s init role) holds a large untyped budget for its entire life,
//! because it stays the system's process builder. Every process in the system is therefore one bug in
//! init away from being built wrong. This program instead holds full construction authority for
//! exactly as long as it takes to build two servers, and then **deletes it**:
//!
//! ```text
//!   root_supervisor   root untyped + the initrd + a report endpoint      (briefly)
//!     |
//!     +-- spawner   one program image, one budget, no initrd     (can build only that program)
//!     |
//!     +-- sub_server_supervisor    a request channel + a fault endpoint         (holds NO memory at all)
//!             |
//!             +-- flaky   a report endpoint                      (the supervised sub-server)
//!
//!   then: root_supervisor deletes its untyped and becomes a RECV loop on its own supervision endpoint.
//! ```
//!
//! After the handoff `root_supervisor` cannot build a process, cannot endow anyone with memory, and cannot
//! reach the sub-servers' internals. What is left of it is a supervisor so small it essentially
//! cannot fail: receive a death, report it, stop. A compromised `root_supervisor` at that point can lie about
//! deaths on one endpoint. That is the entire blast radius.
//!
//! The restart policy is `sub_server_supervisor`'s, in userspace, and the kernel never relaunches anything
//! (DECISIONS §26). See notes/trusted-init.md and notes/supervision.md.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `rootsup`. Refused `rootsup` and
//! `root_sup` (`sup` was the abbreviation, so putting a separator in it would have relocated the
//! problem rather than fixed it).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// Each binary in the tree compiles the shared module but uses a different slice of it (the sub-server
// builds nothing, the supervisor holds no memory), so the unused halves are expected, not dead.
use supervision_proto::{ChildEndowment, REPORT_FAILED, REPORT_INIT_DROPPED, REPORT_SUP_SAW_DEATH};
use user_rt::{cap_delete, invoke, recv, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// What the kernel grants us, and nothing else.
const ROOT_UT: u64 = 0; // the construction budget: ours briefly, then deleted
const REPORT: u64 = 1; // WRITE|GRANT, so we can endow the servers with a narrowed view

/// The budget we carve off for the spawner. It builds every instance of the sub-server out of this,
/// each in its own reclaimable region, so this is the only memory the tree ever spends after boot.
const SPAWNER_BUDGET_PAGES: u64 = 384;

/// Where the spawner finds the one program image it is allowed to build. Must match spawner.rs.
const SPAWNER_IMAGE_VA: u64 = 0x3000_0000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, initrd_len: u64, _a2: u64) -> ! {
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM read-only at INITRD_VA.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    let Ok(fs) = nifefs::Fs::parse(archive) else {
        bail(1)
    };
    let Some(spawner) = fs.read("spawner").and_then(|b| elf::Elf::parse(b).ok()) else {
        bail(2)
    };
    let Some(sub_server_supervisor) = fs
        .read("sub_server_supervisor")
        .and_then(|b| elf::Elf::parse(b).ok())
    else {
        bail(3)
    };
    // The sub-server's image travels as *bytes*, not as an archive: the spawner gets exactly the one
    // program it may build. We hold the initrd; it never does.
    let Some(flaky) = fs.read("flaky") else {
        bail(4)
    };

    // The channels. The spawner serves `req` and answers on `rep`; `childfault` is where the
    // sub-server's deaths go (sub_server_supervisor receives them); `rootfault` is where OUR children's deaths go.
    let Ok(req) = supervision_proto::retype_obj_from(ROOT_UT, abi::objtype::ENDPOINT) else {
        bail(5)
    };
    let Ok(rep) = supervision_proto::retype_obj_from(ROOT_UT, abi::objtype::ENDPOINT) else {
        bail(6)
    };
    let Ok(childfault) = supervision_proto::retype_obj_from(ROOT_UT, abi::objtype::ENDPOINT) else {
        bail(7)
    };
    let Ok(rootfault) = supervision_proto::retype_obj_from(ROOT_UT, abi::objtype::ENDPOINT) else {
        bail(8)
    };
    let Ok(sp_ut) = supervision_proto::untyped_split(ROOT_UT, SPAWNER_BUDGET_PAGES) else {
        bail(9)
    };

    // 1. The construction sub-server. It holds a budget (WRITE only: it may *spend* memory, never
    //    lend it), the request pair, a grantable report endpoint so it can endow the children it
    //    builds, a grantable view of the child fault endpoint so children are born supervised, and
    //    the one program image. It cannot build anything else, because it has nothing else to build.
    let Ok(tcb) = supervision_proto::build_child(
        ROOT_UT,
        ROOT_UT,
        &spawner,
        &ChildEndowment {
            caps: &[
                (req, abi::rights::READ),
                (rep, abi::rights::WRITE),
                (sp_ut, abi::rights::WRITE),
                (REPORT, abi::rights::WRITE | abi::rights::GRANT),
                (childfault, abi::rights::READ | abi::rights::GRANT),
            ],
            maps: &[],
            blobs: &[(SPAWNER_IMAGE_VA, flaky)],
            fault: Some(rootfault),
            ..ChildEndowment::new()
        },
    ) else {
        bail(10)
    };
    if !supervision_proto::tcb_start(tcb, 0, flaky.len() as u64, 0) {
        bail(11)
    }
    cap_delete(tcb);

    // 2. The supervisor. **It holds no memory at all**: a request channel, a death channel, and a way
    //    to report. Its restart policy is code, not authority, so a compromised sub_server_supervisor can ask for
    //    rebuilds of one program and nothing more.
    let Ok(tcb) = supervision_proto::build_child(
        ROOT_UT,
        ROOT_UT,
        &sub_server_supervisor,
        &ChildEndowment {
            caps: &[
                (req, abi::rights::WRITE),
                (rep, abi::rights::READ),
                (childfault, abi::rights::READ),
                (REPORT, abi::rights::WRITE),
            ],
            maps: &[],
            blobs: &[],
            fault: Some(rootfault),
            ..ChildEndowment::new()
        },
    ) else {
        bail(12)
    };
    if !supervision_proto::tcb_start(tcb, 0, 0, 0) {
        bail(13)
    }
    cap_delete(tcb);

    // 3. Give it all away. The wiring caps first (the servers hold their own narrowed copies), then
    //    the spawner's budget, then the construction budget itself. From here we cannot make a page,
    //    an address space, a thread, or an endpoint.
    for c in [req, rep, childfault, sp_ut] {
        cap_delete(c);
    }
    cap_delete(ROOT_UT);

    // And prove it, from the inside, on the two primitives that build things: retype a page, and
    // retype a kernel object. Both must now fail with `NoSuchSlot` (-1), which is what
    // no-ambient-authority feels like: there is nothing there, and no way to name what used to be.
    // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
    // before acting (user_rt's contract). A caller cannot break an invariant by passing a
    // bad slot or method; it gets an error back.
    let frame = unsafe { invoke(ROOT_UT, abi::untyped::RETYPE, 0, 0, 0) };
    // SAFETY: as above: the kernel validates the capability and the method.
    let object = unsafe { invoke(ROOT_UT, abi::untyped::RETYPE_OBJ, abi::objtype::TCB, 0, 0) };
    send(
        REPORT,
        REPORT_INIT_DROPPED,
        (frame < 0 && object < 0) as u64,
        (-frame) as u64,
    );

    // 4. What is left: the irreducible root. Its whole policy is "hear it, say it, stop." It cannot
    //    restart a dead tier-one server, because reaping and rebuilding both need WRITE on a region,
    //    which is the construction authority we just gave up on purpose. That tradeoff is recorded in
    //    DECISIONS §26's phase B block: a root that can restart is a root that can build, and the
    //    fail-closed floor is the more valuable of the two.
    loop {
        let (event, tid, _pc) = recv(rootfault);
        send(REPORT, REPORT_SUP_SAW_DEATH, tid, event);
    }
}

/// Report which stage failed, then trap. A half-built tree is not worth limping along, and the stage
/// code turns "nothing happened" into a legible failure.
fn bail(stage: u64) -> ! {
    send(REPORT, REPORT_FAILED, stage, 0);
    supervision_proto::fail()
}

user_rt::panic_handler!();
