//! **The construction sub-server: process building, and nothing else** (milestone 22 phase B.2).
//!
//! Where init's process-construction authority went. It holds one untyped budget (WRITE only, so it
//! may spend memory but never lend it), a request channel, and **one program image** copied into its
//! address space by `root_supervisor`. It does not hold the initrd, so "build me program X" is not a thing that
//! can be asked of it: the only program it can name is the one it was handed.
//!
//! Each instance is built in its **own region** split off the budget, which keeps one instance's
//! corpse from pinning another's memory. A LIFO reap returns the pages to this budget (§16's
//! return-of-pages), so a restart loop is not a leak.
//!
//! **It does not hold the instance regions** (DECISIONS §32). It used to keep each region capability
//! for its whole life, because reaping meant `Untyped::DESTROY` and the supervisor could not do that
//! without construction authority, so the spawner had to reap on request. §32 put the reap on the
//! supervision endpoint, so the capability's only remaining purpose was gone and it is deleted as
//! soon as the child is started. Nothing in the tree now holds a capability to a *live* instance's
//! memory, and the pages still come home when the supervisor collects the corpse.
//!
//! The honest note on what that gives up: nothing, today. A hung instance (livelocked, never dying)
//! was already unreclaimable, because the reap only ever happened on a supervisor's request and a
//! supervisor's requests only ever followed a death message. §32 records that case as the watchdog
//! case and deliberately leaves it open.
//!
//! It is not the supervisor. It never reads a fault message and it holds no policy; it answers
//! requests in the order they arrive. See notes/trusted-init.md.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// Each binary in the tree compiles the shared module but uses a different slice of it (the sub-server
// builds nothing, the supervisor holds no memory), so the unused halves are expected, not dead. This
// is the one shape where a blanket allow is the honest one: no single binary uses all of it (§38).
use supervision_proto::{ChildEndowment, REP_BUILT, REP_FAILED, REQ_BUILD};
use user_rt::{cap_delete, recv, send};

/// The capabilities `root_supervisor` endowed us with, in order.
const REQ: u64 = 0; // READ: build/reap requests arrive here
const REP: u64 = 1; // WRITE: answers go back here
const BUDGET: u64 = 2; // WRITE: the memory every instance is made of
const REPORT: u64 = 3; // WRITE|GRANT: so each instance can report for itself
const CHILDFAULT: u64 = 4; // READ|GRANT: so each instance is born supervised

/// Where `root_supervisor` copied the one program image we may build. Must match `root_supervisor.rs`.
const IMAGE_VA: u64 = 0x3000_0000;

/// Pages per instance region: enough for the sub-server's segments, its stack, its address-space
/// tables, and its TCB. A debug build of a tiny program is a handful of pages.
const INSTANCE_PAGES: u64 = 48;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, image_len: u64, _a2: u64) -> ! {
    // SAFETY: root_supervisor mapped `image_len` bytes of program image read-only at IMAGE_VA.
    let image = unsafe { core::slice::from_raw_parts(IMAGE_VA as *const u8, image_len as usize) };
    let Ok(elf) = elf::Elf::parse(image) else {
        supervision_proto::fail()
    };

    loop {
        let (op, arg, _w2) = recv(REQ);
        match op {
            REQ_BUILD => {
                let ok = build(&elf, arg);
                send(REP, if ok { REP_BUILT } else { REP_FAILED }, 0, 0);
            }
            _ => {
                send(REP, REP_FAILED, 0, 0);
            }
        }
    }
}

/// Build one instance in its own region, endowed with a report endpoint and its supervision endpoint,
/// started with `attempt` in its second argument register.
///
/// We keep nothing afterwards, and there is nothing left for us to keep: the TCB capability is not
/// the thread (dropping it leaves the thread running), and since §32 the region capability is not the
/// reap either. The supervisor collects the corpse through its supervision endpoint, and these pages
/// come back to this budget when it does.
fn build(elf: &elf::Elf, attempt: u64) -> bool {
    let Ok(region) = supervision_proto::untyped_split(BUDGET, INSTANCE_PAGES) else {
        return false;
    };
    let Ok(tcb) = supervision_proto::build_child(
        BUDGET,
        region,
        elf,
        &ChildEndowment {
            caps: &[(REPORT, abi::rights::WRITE)],
            maps: &[],
            blobs: &[],
            fault: Some(CHILDFAULT),
            ..ChildEndowment::new()
        },
    ) else {
        return false;
    };
    if !supervision_proto::tcb_start(tcb, 0, attempt, 0) {
        return false;
    }
    cap_delete(tcb);
    cap_delete(region);
    true
}

user_rt::panic_handler!();
