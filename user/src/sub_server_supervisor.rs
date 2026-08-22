//! **The sub-server's supervisor: restart policy, in userspace, holding nothing** (milestone 22
//! phase B.2).
//!
//! The kernel turns a death into a message (DECISIONS §26) and stops there. This is the other half:
//! what to *do* about it. Bounded retries, a clean exit treated as "finished" rather than "crashed",
//! and a give-up. All of it ordinary code in an unprivileged process, and none of it in the kernel.
//!
//! **What it holds is the point.** A request channel to the spawner, the death channel for its child,
//! and a WRITE view of a report endpoint. **No untyped at all**, so it cannot build a process, cannot
//! make an endpoint, cannot allocate a page. Its entire power is to ask the spawner for a rebuild of
//! the one program the spawner can build. A compromised supervisor is a restart loop, not a foothold.
//!
//! **It reaps its own children now** (DECISIONS §32). It used to ask the spawner to, because reaping
//! meant `Untyped::DESTROY` and only the spawner held the region capability; the reap is a method on
//! the supervision endpoint this program already holds, so the proxy hop is gone. Nothing about what
//! it holds has grown: still no memory, still nothing it can build with. The reclaimed pages go back
//! to the *spawner's* budget, because that is who owns the region (§13). The instance handle the
//! spawner used to issue is gone too: the kernel stamps a tid on the death message, and §32
//! authorizes that tid relative to the endpoint it arrived on.
//!
//! init is not involved in any of this, and cannot be: by the time the first death arrives, `root_supervisor`
//! has deleted the construction budget it would need. See notes/trusted-init.md.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `subsup`. Refused `subsup` and
//! `sub_supervisor`, which is ambiguous in the way that matters: this supervises **a sub-server**,
//! rather than being a supervisor beneath another one. "Sub-server" was already established
//! vocabulary here, 44 occurrences across the decisions, `supervision_proto`, the kernel and the
//! notes, so the name is built from a word the reader has met.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// Each binary in the tree compiles the shared module but uses a different slice of it (the sub-server
// builds nothing, the supervisor holds no memory), so the unused halves are expected, not dead.
use supervision_proto::{
    REP_BUILT, REPORT_FAILED, REPORT_SUP_GAVE_UP, REPORT_SUP_SAW_DEATH, REQ_BUILD,
};
use user_rt::{recv, send};

/// What `root_supervisor` endowed us with, in order. Notice what is missing: memory.
const REQ: u64 = 0; // WRITE: ask the spawner to build or reap
const REP: u64 = 1; // READ: its answers
const FAULT: u64 = 2; // READ: our child's deaths, kernel-stamped
const REPORT: u64 = 3; // WRITE: what we saw and what we decided

/// **The policy.** How many times we will rebuild a crashing sub-server before we stop. Bounded,
/// because a deterministic fault restarts into the same fault forever; this is the crash-loop guard
/// that §26 refused to put in the kernel. Two is enough to prove the shape and small enough that a
/// broken child does not spin the machine.
const MAX_RESTARTS: u64 = 2;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    let mut attempt = 0u64;
    let mut restarts = 0u64;
    if !build(attempt) {
        send(REPORT, REPORT_FAILED, 20, 0);
        supervision_proto::fail()
    }

    loop {
        // The kernel is the only sender here, so the tid is trustworthy without a badge (§26.5).
        let (event, tid, _pc) = recv(FAULT);
        send(REPORT, REPORT_SUP_SAW_DEATH, tid, event);

        // Reap first, either way: the corpse is dead-until-reaped, so its region stays pinned until we
        // say so. Reaping a finished child matters as much as reaping a crashed one.
        if !reap(tid) {
            send(REPORT, REPORT_FAILED, 22, 0);
            supervision_proto::fail()
        }

        if event != abi::fault::EVENT_FAULT {
            // A clean exit is not a reason to restart. This distinction is exactly why §26 delivers
            // both events instead of only faults, and it lives here, in userspace, where it belongs.
            break;
        }

        restarts += 1;
        if restarts > MAX_RESTARTS {
            send(REPORT, REPORT_SUP_GAVE_UP, restarts, 0);
            break;
        }
        attempt += 1;
        if !build(attempt) {
            send(REPORT, REPORT_FAILED, 21, 0);
            break;
        }
    }

    // Park rather than exit. Exiting would make *us* a death our own supervisor has to handle, and a
    // supervisor whose last act is to create work for its parent is a poor one.
    loop {
        let (event, tid, _pc) = recv(FAULT);
        send(REPORT, REPORT_SUP_SAW_DEATH, tid, event);
    }
}

/// Ask the spawner to build the next instance. We name the attempt; it builds, and tells us nothing
/// but whether it worked. It has nothing else to tell us: the kernel names the child, on the death
/// message, when the time comes.
fn build(attempt: u64) -> bool {
    send(REQ, REQ_BUILD, attempt, 0);
    let (verdict, _, _) = recv(REP);
    verdict == REP_BUILT
}

/// **Collect the corpse** (§32): a method on the supervision endpoint we already hold, naming the
/// tid the kernel stamped on the death message we just received. We hold no memory and cannot build
/// anything, and this does not change that: the pages go back to the spawner's budget, because the
/// spawner owns the region (§13). We can free our child's memory; we cannot spend it.
///
/// A `StillAlive` refusal cannot happen here, because we only ever call this on a tid that arrived
/// on a death message. It is reported rather than ignored anyway: if it ever did happen it would mean
/// the kernel had told us a thread was dead and then said otherwise, which is worth failing loudly
/// over rather than silently leaking a corpse.
fn reap(tid: u64) -> bool {
    user_rt::reap(FAULT, tid) == 0
}

user_rt::panic_handler!();
