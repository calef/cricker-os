//! **The Rust shell around a C component** (milestone 36, DECISIONS §31).
//!
//! This is the load-bearing piece of the seam, and what it does *not* do is the design. It holds
//! every capability, it makes every syscall, and it hands the C nothing but a pointer and a length.
//!
//! ```text
//!   kernel  <--- svc/ecall --->  c_shim (Rust)  <--- C ABI: (u8*, usize) --->  c_seam.c
//!                                ^^^^^^^^^^^^
//!                                every capability and every syscall stops here
//! ```
//!
//! So the foreign component cannot widen the kernel's syscall surface (rule 3 / DECISIONS §4): not
//! because we asked it not to, but because a syscall needs a capability slot and it has never seen
//! one. And the confinement claim is exactly as narrow as it should be: **the C can corrupt memory
//! inside a grant this shell already had, and nothing else.**
//!
//! # What this shell is endowed with
//!
//! | slot | capability | why |
//! |---|---|---|
//! | 0 | report endpoint, WRITE | so the run is legible from the outside |
//! | 1 | this instance's untyped region, WRITE | pays for `malloc`; a `DESTROY` of it is the reap |
//! | 15 | the supervision endpoint | consumed by the kernel at `START` (§26) |
//!
//! plus two mappings the confiner made: the grant at [`c_seam::GRANT_VA`], read/write, and the
//! read-only witness page immediately after it. The C is told about the first and not the second.
//!
//! # Where `malloc` comes from
//!
//! Milestone 27's untyped-backed [`UntypedHeap`], wired to slot 1: the
//! region this instance was built in. So the C heap is the process's own budget, a C leak exhausts
//! this instance and touches no other process's memory, and the single `Untyped::DESTROY` that reaps
//! the corpse reclaims the heap along with everything else.
//!
//! Name: ratified 2026-08-01 (calef, milestone 61), replacing `cshim`. The `c_` prefix means
//! "written in C" (DECISIONS §31), spelled with the separator the tenet requires; the run-together
//! form was the same defect `cseam` had.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// A source file shared by several binaries through `#[path]`, and each uses a different slice of it,
// so the unused halves are expected. This is the one shape where a blanket allow is the honest one:
// the module is compiled once per binary and no single binary is meant to use all of it (§38).

use core::alloc::{GlobalAlloc, Layout};

use user_rt::heap::UntypedHeap;
use user_rt::send;

/// What the confiner endowed us with, in order.
const REPORT: u64 = 0; // WRITE: say what happened
const HEAP_UT: u64 = 1; // WRITE: the region we were built in, which also pays for malloc

/// The heap's ceiling. Eight pages is one growth step (`user_rt::heap::MIN_GROW_PAGES`) and far more
/// than the component's single scratch allocation needs; small on purpose, so a runaway C `malloc`
/// loop hits a bounded wall inside its own budget.
const HEAP_MAX: u64 = 8 * c_seam::PAGE;

/// The heap behind `malloc` and `free`. Not registered as `#[global_allocator]`: nothing in this
/// program uses Rust's `alloc`, and the two C entry points below are the only callers.
static HEAP: UntypedHeap = UntypedHeap::new();

// The C component. Three functions, one ABI: a pointer, a length, and an integer result. No
// structs, no callbacks, no ownership transfer, nothing that needs a header file to agree on.
unsafe extern "C" {
    fn c_seam_transform(grant: *mut u8, len: usize) -> u32;
    fn c_seam_overrun(grant: *mut u8, len: usize);
    fn c_seam_wild(grant: *mut u8, len: usize);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, attempt: u64, _a2: u64) -> ! {
    HEAP.init(HEAP_UT, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    // Report *before* the call, because two of the three attempts never return from it. This is how
    // the test knows a restarted instance reached its work at all.
    send(REPORT, c_seam::RPT_RAN, attempt, 0);

    let grant = c_seam::GRANT_VA as *mut u8;
    let len = c_seam::PAGE as usize;

    match attempt {
        // SAFETY: none of this is safe, and that is the milestone. `grant` is a page the confiner
        // mapped read/write for us, and `len` is its true length; the C is handed the truth and
        // misbehaves anyway. The kernel is what makes that survivable.
        c_seam::ATTEMPT_OVERRUN => unsafe { c_seam_overrun(grant, len) },
        // SAFETY: as above: the same true grant and true length, handed to the C that deliberately runs wild. The kernel is what makes that survivable.
        c_seam::ATTEMPT_WILD => unsafe { c_seam_wild(grant, len) },
        // The honest path. The result also lands in the grant, so the checker reads it out of shared
        // memory rather than trusting a number we forwarded.
        _ => {
            // SAFETY: same grant, same length; this call is the well-behaved one.
            unsafe { c_seam_transform(grant, len) };
        }
    }

    // Only the honest attempt gets here. A clean exit is what the supervisor must read as "finished"
    // rather than "crashed" (DECISIONS §26.3), and it is what ends the run.
    user_rt::exit()
}

// ===========================================================================================
// The libc, in full: TWO symbols.
//
// Tier two of the three tiers in design/roadmap/36-foreign-component.md (freestanding / a handful of
// symbols / full POSIX).
// The C references five symbols; this file defines two of them. Both halves of that sentence were
// discovered by letting the build fail and reading the error, which is the only honest way to learn
// what a foreign object file actually demands.
//
// **What the object demands.** `llvm-nm --undefined-only` on the compiled `c_seam.o`, identically
// on both ISAs and at every optimization level: `malloc`, `free`, `memcpy`, `memset`, `strlen`.
// Nothing else. No compiler-rt helper, no `memmove` conjured out of a struct copy, and no
// `__stack_chk_fail` (this component has no stack arrays, but `-fno-stack-protector` in build.rs
// makes that a decision rather than luck).
//
// **What the linker demands, which is less.** Delete all five and `rust-lld` reports exactly two
// undefined symbols: `malloc` and `free`. `memcpy`, `memset`, and `strlen` are already there, weakly,
// from Rust's own `compiler_builtins` (that is what makes a `no_std` Rust binary link at all). The C
// component's needs and the Rust runtime's overlap almost exactly, which is a real and reusable
// finding: the "handful of symbols" tier is smaller than it looks.
//
// **And why we do not define them anyway, which cost a debugging session.** The obvious Rust `memcpy`
// shim is `core::ptr::copy_nonoverlapping`, and `copy_nonoverlapping` *lowers to a call to `memcpy`*.
// So the shim calls itself, forever, and the symptom is not a stack overflow at a plausible depth but
// a store fault exactly at `sp` sixteen bytes below whatever stack you gave the process. (Found here
// with a 16 KiB stack, reproduced with 64 KiB, then understood.) `compiler_builtins` avoids the trap
// by compiling with `#[no_builtins]`; a program crate cannot. So the right answer is the one that is
// also the smaller one: let the runtime keep the three symbols it already owns, and shim only the two
// it does not. Recorded in notes/c-seam.md, because the next person to shim a libc symbol in Rust
// will reach for `copy_nonoverlapping` too.
// ===========================================================================================

/// `malloc`'s header size. **This is the one place the C ABI costs us something real.** `free(p)`
/// carries no size, but `GlobalAlloc::dealloc` requires the original `Layout`, so every allocation
/// stores its size in front of the pointer it returns. Sixteen bytes because that is also the
/// alignment `malloc` must guarantee for any C type, so the payload stays aligned for free.
const MALLOC_HDR: usize = 16;

/// The layout `malloc(n)` really allocates: `n` bytes plus the header, 16-byte aligned.
fn malloc_layout(n: usize) -> Option<Layout> {
    Layout::from_size_align(n.checked_add(MALLOC_HDR)?, MALLOC_HDR).ok()
}

/// # Safety
/// The C ABI's contract: the caller must eventually pass the returned pointer to [`free`] exactly
/// once, or never.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(n: usize) -> *mut u8 {
    let Some(layout) = malloc_layout(n) else {
        return core::ptr::null_mut();
    };
    // SAFETY: a non-zero-size layout, and the heap is wired (see `_start`).
    let base = unsafe { HEAP.alloc(layout) };
    if base.is_null() {
        return core::ptr::null_mut();
    }
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "malloc_layout requests MALLOC_HDR (16) alignment, which exceeds align_of::<usize>()"
    )]
    // SAFETY: `base` owns `MALLOC_HDR + n` bytes, 16-aligned, so the header write is in bounds and
    // aligned for a `usize`.
    unsafe {
        base.cast::<usize>().write(n);
        base.add(MALLOC_HDR)
    }
}

/// # Safety
/// `p` must be null or a pointer [`malloc`] returned and that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(p: *mut u8) {
    if p.is_null() {
        return;
    }
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "base is the 16-aligned pointer malloc returned, so it is aligned for a usize"
    )]
    // SAFETY: `p` came from `malloc`, so `p - MALLOC_HDR` is the base and holds the size.
    unsafe {
        let base = p.sub(MALLOC_HDR);
        let n = base.cast::<usize>().read();
        if let Some(layout) = malloc_layout(n) {
            HEAP.dealloc(base, layout);
        }
    }
}

// A panic here is a bug in the shell, not in the C. Trap, so the kernel prints the pc and this
// process's supervisor sees a fault rather than a silent hang.
user_rt::panic_handler!();
