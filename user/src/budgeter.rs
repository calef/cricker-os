//! The budgeter: a program that spends exactly the memory it was granted, and reports how much.
//!
//! Milestone 31 phase 1. This is what makes `run --mem N budgeter` *real* rather than
//! parsed-and-ignored. The shell splits `N` pages off its own untyped budget and delegates that
//! untyped to this program (init inserts it at slot 1); the program then maps pages out of it, one
//! at a time, until the budget is exhausted, and reports the count on its result endpoint (slot 0).
//!
//! The number it reports *is* the authority the command line handed it. Grant more, it maps more;
//! grant nothing, and it holds no untyped at slot 1 at all, so its very first `MAP` returns
//! `NoSuchSlot` and it reports zero. There is no ambient pool to fall back on: a nife process
//! spends the budget it was given and not one page more.
//!
//! One honest detail, recorded rather than hidden: an `N`-page untyped does not yield `N` mapped
//! data pages. Reaching a fresh virtual address needs page-table pages, and those come from the
//! same untyped (the kernel allocates nothing on a process's behalf, DECISIONS §10). So the
//! reported count is `N` minus the handful of pages the tables cost. That the count tracks `N`, and
//! collapses to zero with no grant, is the proof the budget is real.
//!
//! # The budgeter's world
//!
//! - slot 0: the result endpoint (SEND: report the page count).
//! - slot 1: an untyped budget (the delegated `--mem` grant). Absent when `--mem` was 0.
//!
//! Name: unrecorded. Introduced 2026-07-28 with the grant expression. It is an agent noun in the
//! `broker`/`spawner`/`painter` family milestone 63 later named as a family, but nothing records
//! the choice.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{exit, invoke, send};

/// The result endpoint init grants every spawned program (slot 0).
const RESULT: u64 = 0;
/// The delegated `--mem` untyped (slot 1). Absent (empty slot) when no budget was granted.
const BUDGET: u64 = 1;

const PAGE: u64 = 4096;
/// Where we start mapping. Well clear of our own segments (linked at `0x40_0000`) and stack
/// (`0x50_0000)`: a fresh, page-aligned, low-half window with room to grow.
const SPEND_BASE: u64 = 0x1000_0000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    let mut mapped: u64 = 0;
    let mut va = SPEND_BASE;
    loop {
        // Map one more page out of the granted untyped at `va`, writable. The page and any page
        // table it needs both come from the budget; when the budget is spent the kernel returns a
        // negative error (OutOfMemory), and a program with no budget at all gets NoSuchSlot on the
        // first call. Either way, we stop and report what we got.
        // SAFETY: `svc`/`ecall`; the kernel validates the untyped capability and the address.
        let r = unsafe { invoke(BUDGET, abi::untyped::MAP, va, 0, 0) };
        if r < 0 {
            break;
        }
        // Touch the page so the mapping is not just nominal: a byte written proves it is real,
        // writable memory the process now owns.
        // SAFETY: we just mapped `va` read/write in our own address space.
        unsafe { core::ptr::write_volatile(va as *mut u8, 0xA5) };
        mapped += 1;
        va += PAGE;
    }
    send(RESULT, mapped, 0, 0);
    exit();
}

user_rt::panic_handler!();
