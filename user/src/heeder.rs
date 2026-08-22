//! The heeder: a long-running job that heeds the cooperative interrupt (DECISIONS §24).
//!
//! It works forever, one small unit at a time, and between units it reads a single word in a page
//! it shares with the shell: the interrupt flag. When the shell sets it (on the first `^C`), the
//! heeder cleans up, records that it stopped gracefully, and exits. This is the cooperative tier of
//! DECISIONS §24 made visible: the first `^C` asks the job to stop, and a job that listens does.
//!
//! Why a shared word and not an endpoint: a running computation cannot poll an endpoint (there is
//! no non-blocking receive) and cannot block on one without stalling the work the user wants to
//! interrupt. So the signal is memory the heeder reads with a plain load between units. See
//! `grant_plan::jobframe` and notes/grant-expression.md.
//!
//! # The heeder's world
//!
//! - the shared job frame, mapped read/write at [`JOB_FRAME_VA`] (init maps it; the shell holds the
//!   other view). The heeder reads [`jobframe::INTERRUPT`] and writes the rest. No capabilities: it
//!   touches only this page and exits. Its whole authority is one shared page.
//!
//! Name: unrecorded. Introduced 2026-07-28 with the cooperative interrupt tier: the program that
//! heeds an interrupt request rather than being killed by one. An agent noun in a family milestone
//! 63 later named, with no record of the choice.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use grant_plan::jobframe;
use user_rt::exit;

/// Where init maps the shared job frame in the child's address space. Must match the shell/init
/// wiring (below the ELF load address `0x40_0000` and the stack).
const JOB_FRAME_VA: usize = 0x0030_0000;

fn load(off: usize) -> u64 {
    // SAFETY: init mapped the job frame read/write at JOB_FRAME_VA; `off` is a valid word offset.
    unsafe { core::ptr::read_volatile((JOB_FRAME_VA + off) as *const u64) }
}

fn store(off: usize, v: u64) {
    // SAFETY: as above; this word is the child's to write (one writer per word, see jobframe).
    unsafe { core::ptr::write_volatile((JOB_FRAME_VA + off) as *mut u64, v) }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    let mut beats: u64 = 0;
    loop {
        // One unit of work. The heartbeat is liveness the shell can show; the spin makes a unit
        // take long enough that the interrupt is noticed promptly, not after a million iterations.
        beats = beats.wrapping_add(1);
        store(jobframe::HEARTBEAT, beats);
        for _ in 0..4096 {
            core::hint::spin_loop();
        }
        // Notice the cooperative interrupt between units.
        if load(jobframe::INTERRUPT) != 0 {
            store(jobframe::STATUS, jobframe::STATUS_INTERRUPTED);
            store(jobframe::DONE, 1); // tell the shell we are stopping cleanly, then leave
            exit();
        }
    }
}

user_rt::panic_handler!();
