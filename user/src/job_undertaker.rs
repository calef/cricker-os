//! **The interactive boot's undertaker** (milestone 22 phase B.2, the interactive increment).
//!
//! The prompt's init builds a fresh process for every command a person runs. Before this program
//! existed, each of those processes stayed dead-but-uncollected forever and its region stayed spent,
//! so init's budget only ever went one way: a long session ran out of memory and the shell started
//! answering "could not spawn (init is out of memory)". Init could not collect them itself, because
//! there is no non-blocking receive and init is parked in `RECV` on the shell's spawn channel for its
//! whole life. So the collecting is a second process, and this is it.
//!
//! **Its entire authority is one endpoint capability with `READ`.** No untyped, no frame, no TCB, no
//! address space, and nothing it could report on. It cannot build a process, allocate a page, or
//! reach any child's memory; `Endpoint::REAP` (DECISIONS §32) is authorized by the supervision
//! relationship the kernel already records, not by holding the region. The pages therefore go back to
//! **init's** job budget, because init split the region and §13 says a region belongs to whoever owns
//! it. This process can free a job's memory and can never spend it.
//!
//! It is deliberately not a supervisor in the `sub_server_supervisor` sense: it holds no restart
//! policy, because a command a person typed has no business being restarted when it ends. Collecting
//! is the whole job.
//!
//! # BUGS
//!
//! It has no way to say anything. Init holds no channel it could report on, so a refused reap is
//! taken as the kernel contradicting itself (it named a thread dead and then refused to collect it)
//! and trapped on, which at least dies loudly at the pc of the mistake. The visible symptom of this
//! process dying is that init's job budget stops coming back and the prompt eventually answers
//! "could not spawn".
//!
//! **A refused reclaim is retried rather than trapped on** (milestone 31 phase 3, 2026-08-17), and
//! that is the one thing this program does that is not a single call. A job behind a directory grant
//! shares its region with the caretaker carrying the grant, so the first reclaim is refused by
//! construction and the retry is what collects it; see [`MAX_ATTEMPTS`], which explains the whole
//! mechanism and why running out is still a trap.
//!
//! It also cannot reap a **hung** job: a live thread is refused on purpose (§32), and nothing here
//! escalates. That is §32's recorded watchdog case and it belongs to milestone 23, not here. At the
//! prompt the forcible tier already exists for the case a person can see: `^C` twice tears the job's
//! region down through the shell (DECISIONS §24), and those jobs are built from the shell's own
//! untyped rather than init's, so they never reach this program at all.
//!
//! Name: ratified 2026-08-03 (calef), replacing `job_reaper`. Refused `job_reaper` (named for what
//! it does to jobs, while its siblings are named for what they serve, and "reaper" carries a faint
//! sense of taking a life) and, on 2026-08-04, `job_killer` (it claims an authority this program is
//! specifically denied). An undertaker arrives strictly after death and never causes it, which is
//! exactly the constraint DECISIONS §32 enforces by refusing to reap a live thread, and it joins
//! the register its siblings already speak: supervisor, caretaker, undertaker.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{reap, recv_fault, yield_now};

/// The supervision endpoint init endows every job with, held `READ`: the right to receive deaths
/// here, which is the same right §32 makes the right to collect them.
const DEATHS: u64 = 0;

/// **How many times one corpse may be refused before this program treats it as a broken kernel**
/// (milestone 31 phase 3).
///
/// A refusal is no longer always a bug, and that is what this constant is for. A job behind a
/// directory grant shares its region with the `fs_subtree_caretaker` that carries the grant
/// (DECISIONS §92), and the caretaker's serve loop only ends when that region is torn down. So the
/// **first** reclaim of such a region is refused by construction: `reap_region_objects` sweeps the
/// region's endpoints, which wakes the caretaker out of `RECV`, and a thread that can be scheduled
/// is `RefuseAndArm`. The same pass arms §16's kill, the scheduler turns the caretaker into a corpse
/// at its next preemption, and the retry finds nothing left alive and reclaims. That is exactly the
/// contract `reclaim_region` documents for the shell's `^C` escalation, met here for the first time
/// by an ordinary command.
///
/// Two properties make the number a safety net rather than a policy. It is far past what the
/// mechanism needs, because one preemption is enough and this yields between attempts. And running
/// out is still [`fail`], so the failure this program was written to make loud is still loud: a
/// corpse that never becomes collectable is a leak, and a leak here ends the prompt's memory a few
/// commands later with nothing to point at.
const MAX_ATTEMPTS: usize = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    loop {
        // The kernel is the only sender on this endpoint (§26 clears the child's fault slot at
        // `START`), so the tid is trustworthy without a badge.
        let (_event, tid, _pc, _addr, _rsvd) = recv_fault(DEATHS);
        collect(tid);
    }
}

/// Collect one corpse, retrying while the region still holds something that can run.
///
/// The tid arrived on a death message from this very endpoint, so two of §32's refusals are
/// unreachable: it cannot be `StillAlive` and it cannot be another supervisor's child. The third,
/// `NotPermitted`, means the *region* is not yet reclaimable, which is a fact about the region's
/// other residents rather than about this corpse; see [`MAX_ATTEMPTS`]. Yielding between attempts
/// is what lets the armed kill be spent: the thread standing in the way has to reach a preemption,
/// and spinning without giving up the core would be racing it on a single-core machine.
fn collect(tid: u64) {
    for _ in 0..MAX_ATTEMPTS {
        if reap(DEATHS, tid) == 0 {
            return;
        }
        yield_now();
    }
    fail()
}

fn fail() -> ! {
    user_rt::trap()
}

user_rt::panic_handler!();
