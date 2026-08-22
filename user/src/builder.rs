//! **A minimal init: the system builder** (milestone 20, the richer-initrd step).
//!
//! This is the RISC-V counterpart of the init role in `hello` (which is aarch64-wired: PL011,
//! `svc`, the shell/console/input system). It is deliberately small and fully portable: it does the
//! one thing that matters for the demonstrator's thesis on a second ISA, which is that **userspace,
//! not the kernel, composes the system.**
//!
//! The kernel loads this program from the initrd's `init` entry, maps the whole nifefs archive
//! read-only, and grants it two capabilities: a large untyped budget (slot 0) and a report endpoint
//! (slot 1). From those, and nothing else, this program:
//!
//! 1. parses the archive (nifefs) and reads the `worker` program out of it by name,
//! 2. parses that ELF (the `elf` crate, linked into userspace),
//! 3. builds a child process out of its own budget through the granular capability verbs (retype an
//!    address space, copy each segment into retyped frames and map them, retype a TCB, endow it,
//!    configure, start), wiring the child's one endpoint to the report line, and
//! 4. starts the child with an input.
//!
//! The child (the worker) squares the input and SENDs the answer straight to the report endpoint,
//! which the kernel is waiting on. The kernel never touches the worker's bytes: this program loaded
//! it, built its address space, and started it. That is the init-as-system-builder model, proven on
//! RISC-V. It shares the `user` crate's `link.ld` and the `user_rt` syscall runtime; every syscall
//! it makes (retype, map, configure, start) crosses the same `ecall` ABI the worker uses.
//!
//! Name: unrecorded, and load-bearing anyway. `builder.rs`'s own header calls it "a minimal init:
//! the system builder", and that phrase is why `system_builder` was refused for a crate twice, on
//! 2026-08-01 and again on 2026-08-04. Its archive entry is `init`, the one deliberate exception to
//! "the binary, the source file and the archive entry are the same string" (notes/naming.md),
//! because `init` is the entry the kernel loads by name.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{cap_delete, exit, invoke, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's `riscv_initrd_demo`.
const INITRD_VA: u64 = 0x2000_0000;

/// The capabilities the kernel grants this program before it runs.
const UNTYPED: u64 = 0; // a budget to retype the child's aspace, frames, and TCB from
const REPORT: u64 = 1; // the report endpoint; we hand the child a narrowed WRITE view of it

/// The input we hand the worker. It returns `n * n` on the report endpoint.
const INPUT: u64 = 9;

/// The child's layout, in its own address space (it links at `0x40_0000`, like every user program).
const PAGE: u64 = 4096;
const CHILD_STACK_TOP: u64 = 0x0050_0000; // one page of stack is plenty for the worker

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, initrd_len: u64, _x2: u64) -> ! {
    // The archive the kernel mapped read-only at INITRD_VA; its length arrived in a1.
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM, read-only, at INITRD_VA.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };

    let Ok(fs) = nifefs::Fs::parse(archive) else {
        fail(0xE1);
    };
    let Some(worker_bytes) = fs.read("worker") else {
        fail(0xE2);
    };
    let Ok(elf) = elf::Elf::parse(worker_bytes) else {
        fail(0xE3);
    };

    // Build the worker and start it with INPUT. It SENDs its answer straight to REPORT (which we
    // grant it, narrowed to WRITE, as its slot 0), and the kernel receives it. We built the pipe.
    if build_and_start(&elf, INPUT).is_err() {
        fail(0xE4);
    }

    // Our job is done: the worker runs on its own. Leave, and the kernel reaps us.
    exit();
}

/// Build a child process from `elf` out of our untyped budget, grant it the report endpoint as its
/// slot 0, and start it with `n` in its second argument register. Returns `Err(())` on any failure.
///
/// This is a userspace ELF loader, the same shape as the kernel's `map_segments` but driven entirely
/// through the capability verbs: nothing here is privileged, it is all `invoke` on capabilities we
/// hold. It mirrors `hello`'s `build_child`, trimmed to exactly what this one child needs.
fn build_and_start(elf: &elf::Elf, n: u64) -> Result<(), ()> {
    // A fresh address space, retyped from our budget.
    let aspace = retype_obj(abi::objtype::ASPACE)?;

    // Lay each loadable segment into the child at the VA it names, with its own permissions. Each
    // page: retype a frame, map it in our OWN space to fill it, copy this page's slice of the
    // segment, then map it into the child. SCRATCH advances so we never reuse a filled VA.
    let mut scratch = 0x1000_0000u64;
    for seg in elf.segments() {
        let mode = if seg.is_executable() {
            abi::aspace::MAP_CODE
        } else if seg.is_writable() {
            abi::aspace::MAP_RW
        } else {
            abi::aspace::MAP_RO
        };
        let (start, end) = seg.page_range(PAGE);
        let mut va = start;
        while va < end {
            let frame = retype_frame()?;
            // Map the frame writable in our own space at `scratch`, fill it, then hand it over.
            // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
            // before acting (user_rt's contract). A caller cannot break an invariant by passing a
            // bad slot or method; it gets an error back.
            if unsafe { invoke(frame, abi::frame::MAP, scratch, 1, UNTYPED) } != 0 {
                return Err(());
            }
            // SAFETY: `scratch` is a page we just mapped read/write in our own space.
            let dst = unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE as usize) };
            dst.fill(0); // zero first, so a segment's .bss tail is clean
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + PAGE).min(file_hi);
            if lo < hi {
                let d = (lo - va) as usize;
                let s = (lo - file_lo) as usize;
                let len = (hi - lo) as usize;
                dst[d..d + len].copy_from_slice(&seg.data[s..s + len]);
            }
            // Into the child at the segment's own VA, with the segment's permissions.
            // SAFETY: as above: the kernel validates the capability and the method.
            if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, frame, mode) } != 0 {
                return Err(());
            }
            cap_delete(frame); // done with this frame's cap; free the slot for reuse
            scratch += PAGE;
            va += PAGE;
        }
    }

    // A one-page stack for the child, mapped just below CHILD_STACK_TOP.
    let stack_frame = retype_frame()?;
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe {
        invoke(
            aspace,
            abi::aspace::MAP_INTO,
            CHILD_STACK_TOP - PAGE,
            stack_frame,
            abi::aspace::MAP_RW,
        )
    } != 0
    {
        return Err(());
    }
    cap_delete(stack_frame);

    // The thread, its one capability (the report endpoint, narrowed to WRITE, lands as slot 0),
    // configured at the ELF's entry with the stack top, then started with `n` in its second arg.
    let tcb = retype_obj(abi::objtype::TCB)?;
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, REPORT, abi::rights::WRITE, 0) } < 0 {
        return Err(());
    }
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe {
        invoke(
            tcb,
            abi::tcb::CONFIGURE,
            elf.entry(),
            CHILD_STACK_TOP,
            aspace,
        )
    } != 0
    {
        return Err(());
    }
    // START's arguments become the child's a0/a1/a2; the worker reads its input from a1.
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe { invoke(tcb, abi::tcb::START, 0, n, 0) } != 0 {
        return Err(());
    }
    cap_delete(tcb); // our TCB cap; the worker keeps running until it exits
    Ok(())
}

/// Retype a kernel object out of our untyped budget; returns the slot its capability landed in.
fn retype_obj(objtype: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(UNTYPED, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Retype a page of our budget into a Frame capability; returns its cap slot.
fn retype_frame() -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Report a build failure to the kernel (a nonzero code the worker's real answer can never be, since
/// the worker only ever sends a perfect square) and exit. The kernel's `recv` sees it and says so.
fn fail(code: u64) -> ! {
    let _ = send(REPORT, code, 0, 0);
    exit();
}

user_rt::panic_handler!();
