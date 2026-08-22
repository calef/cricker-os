//! **The confiner: the process that confines a C component and proves it** (milestone 36,
//! DECISIONS §31).
//!
//! It is three roles in one program, and the fusion is deliberate rather than lazy:
//!
//! 1. **Builder.** It splits a region off its budget per instance and lays `c_shim` out in it, so a
//!    single `Untyped::DESTROY` reaps a whole instance (§16 object revocation).
//! 2. **Supervisor.** It holds the C component's supervision endpoint (§26), so a fault becomes a
//!    five-word message it receives rather than a silence it has to guess at, and it decides whether
//!    to restart. The kernel relaunches nothing.
//! 3. **Checker.** It holds the witness pages in *its own* address space, so "nothing outside the
//!    grant changed" is asserted from the other side of an MMU boundary rather than from inside the
//!    process that just misbehaved.
//!
//! **Why roles 1 and 2 are one process here, and what that no longer costs.** This program is the
//! measurement that produced DECISIONS §32: reaping used to need `WRITE` on the region the corpse
//! lives in, which is the same right that *builds* a process out of it, so a supervisor that
//! restarted its child either held construction authority or proxied the reap through something that
//! did. §32 put the reap on the supervision endpoint instead, and role 2 here now uses it: the
//! corpse is collected with `user_rt::reap`, and the per-instance region capability is deleted as
//! soon as the child is started rather than held for the instance's whole life.
//!
//! **What that did not remove, which is itself a finding about §32.** This program still holds a full
//! construction budget, because it is *also* role 1, the builder: it splits a region per instance and
//! lays `c_shim` out in it, and §32 does not touch construction. What §32 removed is the reason a
//! *supervisor* had to hold one. Read as a measurement: the bundling §31 recorded was two things, and
//! only one of them was the reap. Split roles 1 and 2 into separate processes and the supervisor half
//! would now hold nothing but endpoints, which is exactly what milestone 22's `sub_server_supervisor` does since
//! §32. Keeping them fused here is still deliberate (§31: the requirement is visible in one program
//! instead of hidden behind an IPC hop). See DECISIONS §31's "what the supervisor had to hold".
//!
//! Role 3 is genuinely separate from the C component and must be: a checker inside the faulting
//! address space could only report what that address space could see, which is exactly the thing
//! under suspicion.
//!
//! Name: ratified 2026-08-01 (calef, milestone 61), replacing `cwarden`. Refused `cwarden` and the
//! whole `warden` family, a synonym this project invented for a pattern that already has a name:
//! DECISIONS §50 settled that using the literature's word claims "this is that", where a synonym
//! asserts novelty there is none. Refused the caretaker noun for this one specifically: it holds a
//! region and confines foreign code rather than attenuating a directory capability to a narrower
//! one, so it is deliberately outside that family.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// A source file shared by several binaries through `#[path]`, and each uses a different slice of it,
// so the unused halves are expected. This is the one shape where a blanket allow is the honest one:
// the module is compiled once per binary and no single binary is meant to use all of it (§38).

// The loader, shared with the milestone-22 supervision tree. `ChildEndowment.maps` and `ChildEndowment.fault` are the
// two parts this milestone leans on: a child born with shared pages and born supervised.
use c_seam::checks;
use supervision_proto::ChildEndowment;
use user_rt::{cap_delete, invoke, recv_fault, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// What the kernel grants us, and nothing else.
const ROOT_UT: u64 = 0; // the construction budget: what we build each instance out of
const REPORT: u64 = 1; // WRITE|GRANT, so each instance gets its own narrowed view

/// Pages per instance region. A debug-build `c_shim` is a couple of dozen pages of segments plus
/// its stack, its page tables, its TCB, its address space, and the eight-page heap `malloc` grows
/// into. Each instance is reaped before the next is built, so this is a peak, not a total.
const INSTANCE_PAGES: u64 = 96;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, initrd_len: u64, _a2: u64) -> ! {
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM read-only at INITRD_VA.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    let Ok(fs) = nifefs::Fs::parse(archive) else {
        bail(1)
    };
    let Some(shim) = fs.read("c_shim").and_then(|b| elf::Elf::parse(b).ok()) else {
        bail(2)
    };

    // The three pages the whole proof rests on. All from our own budget, all ours to start with; the
    // C component's process will see two of them and never the third.
    let Ok(grant) = supervision_proto::retype_frame_from(ROOT_UT) else {
        bail(3)
    };
    let Ok(wit_ro) = supervision_proto::retype_frame_from(ROOT_UT) else {
        bail(4)
    };
    let Ok(wit_far) = supervision_proto::retype_frame_from(ROOT_UT) else {
        bail(5)
    };

    // Map all three read/write **into ourselves**, at the same virtual addresses the C component will
    // use. Same numbers on both sides is what lets the wild-store address be compared directly
    // against the address the kernel reports, with no translation step to get wrong.
    for (frame, va) in [
        (grant, c_seam::GRANT_VA),
        (wit_ro, c_seam::WITNESS_RO_VA),
        (wit_far, c_seam::WITNESS_FAR_VA),
    ] {
        // SAFETY: plain syscall; the kernel validates the frame, the va, and the budget.
        if unsafe { invoke(frame, abi::frame::MAP, va, 1, ROOT_UT) } != 0 {
            bail(6)
        }
    }
    for i in 0..c_seam::PAGE as usize {
        witness_ro()[i] = c_seam::pattern_ro(i);
        witness_far()[i] = c_seam::pattern_far(i);
    }

    // Our children's deaths arrive here. We are the only holder with READ, and the kernel is the only
    // sender (§26.5), so both the tid and the fault address are trustworthy without a badge.
    let Ok(faultep) = supervision_proto::retype_obj_from(ROOT_UT, abi::objtype::ENDPOINT) else {
        bail(7)
    };

    let mut attempt = 0u64;
    while attempt < c_seam::ATTEMPTS {
        // Reset the grant before every attempt. Necessary, not hygiene: the misbehaving attempts
        // scribble a marker over the input's first byte, so without this the honest run would be
        // transforming the wreckage of the previous one.
        let g = grant_page();
        g.fill(0);
        g[c_seam::IN_OFF..c_seam::IN_OFF + c_seam::INPUT.len()].copy_from_slice(c_seam::INPUT);

        let Ok(region) = supervision_proto::untyped_split(ROOT_UT, INSTANCE_PAGES) else {
            bail(10)
        };
        let Ok(tcb) = supervision_proto::build_child(
            ROOT_UT,
            region,
            &shim,
            &ChildEndowment {
                // The shell's whole authority: say what happened, and spend the region it lives in
                // (which is what pays for `malloc`). No GRANT on either, so it can neither lend the
                // report endpoint on nor hand its budget to anyone.
                caps: &[(REPORT, abi::rights::WRITE), (region, abi::rights::WRITE)],
                // The grant is read/write, which is the authority the C code abuses. The witness page
                // right behind it is read-only, which is what turns an off-by-one into a fault
                // instead of a corruption. Nothing maps WITNESS_FAR here, deliberately.
                maps: &[
                    (c_seam::GRANT_VA, grant, abi::aspace::MAP_RW),
                    (c_seam::WITNESS_RO_VA, wit_ro, abi::aspace::MAP_RO),
                ],
                blobs: &[],
                fault: Some(faultep),
                ..ChildEndowment::new()
            },
        ) else {
            bail(11)
        };
        if !supervision_proto::tcb_start(tcb, 0, attempt, 0) {
            bail(12)
        }
        // Neither capability is the thing itself, and neither is needed any more. The TCB capability
        // is not the thread (dropping it leaves the thread running), and since §32 the region
        // capability is not the reap: the corpse is collected through the supervision endpoint. So we
        // hold nothing that reaches a live instance's memory, and the child keeps the narrowed copy of
        // the region it was endowed with, which is what `malloc` spends.
        cap_delete(tcb);
        cap_delete(region);

        // Block until the child dies, one way or the other. All five words, because the fourth is the
        // faulting address and this is the program that cares where the C code pointed.
        let (event, tid, pc, addr, _reserved) = recv_fault(faultep);
        send(REPORT, c_seam::RPT_DEATH, tid, event);
        send(REPORT, c_seam::RPT_SITE, pc, addr);
        send(
            REPORT,
            c_seam::RPT_VERDICT,
            attempt,
            verdict(attempt, event, pc, addr),
        );

        // Reap, through the supervision endpoint the death arrived on, naming the tid the kernel
        // stamped on it (§32). The corpse is dead-until-reaped (§26.4), so its region stayed pinned
        // until now and the verdict above was computed while it was still there to inspect. We hold no
        // capability to that region: the authority for this is the supervision relationship, and the
        // pages go back to ROOT_UT, which is where they came from.
        if user_rt::reap(faultep, tid) != 0 {
            bail(13)
        }

        if event != abi::fault::EVENT_FAULT {
            // A clean exit is "finished", not "crashed", and that distinction is why §26 delivers both
            // events. It is also what ends this run: the honest attempt is the last one.
            break;
        }
        attempt += 1;
    }

    // Park rather than exit, so we do not become a death of our own for somebody else to handle.
    loop {
        let (event, tid, pc, addr, _) = recv_fault(faultep);
        send(REPORT, c_seam::RPT_DEATH, tid, event);
        send(REPORT, c_seam::RPT_SITE, pc, addr);
    }
}

/// **The verdict on one attempt**: a bitmap of the confinement claims that held.
///
/// Every bit is a separate question asked of memory, not of the component. The component is not
/// consulted at all: it is dead by the time this runs.
fn verdict(attempt: u64, event: u64, pc: u64, addr: u64) -> u64 {
    let mut bits = 0u64;
    let g = grant_page();

    // Claim 1: the component's stores *inside* its grant landed. Without this every other bit could
    // be satisfied by a process whose stores never worked at all, which would prove nothing.
    let in_grant_landed = if attempt == c_seam::ATTEMPT_HONEST {
        // The honest path writes a checksum, so a non-zero one is the evidence its stores worked.
        g[c_seam::OUT_OFF..c_seam::OUT_OFF + 4]
            .iter()
            .any(|&b| b != 0)
    } else {
        g[0] == c_seam::MARK
    };
    if in_grant_landed {
        bits |= checks::IN_GRANT_WRITE_LANDED;
    }

    // Claim 2 and 3: the witnesses. Read through OUR mappings, in OUR address space, after the
    // component is dead. Every byte, because a partial overwrite is still an escape.
    if (0..c_seam::PAGE as usize).all(|i| witness_ro()[i] == c_seam::pattern_ro(i)) {
        bits |= checks::WITNESS_RO_INTACT;
    }
    if (0..c_seam::PAGE as usize).all(|i| witness_far()[i] == c_seam::pattern_far(i)) {
        bits |= checks::WITNESS_FAR_INTACT;
    }

    // Claim 4: the kernel's fault site is the site the C code computed. This is what rules out "it
    // crashed for some other reason on the way", which would make the witness checks vacuous.
    let expect_addr = match attempt {
        c_seam::ATTEMPT_OVERRUN => Some(c_seam::WITNESS_RO_VA),
        c_seam::ATTEMPT_WILD => Some(c_seam::WITNESS_FAR_VA),
        _ => None,
    };
    let site_ok = match expect_addr {
        Some(want) => event == abi::fault::EVENT_FAULT && addr == want && pc != 0,
        // A clean exit reports no site at all, and §26 says both words are zero. Asserting that is
        // asserting the kernel does not leak a stale register into an EXIT message.
        None => event == abi::fault::EVENT_EXIT && addr == 0 && pc == 0,
    };
    if site_ok {
        bits |= checks::FAULT_ADDR_AS_EXPECTED;
    }

    // Claim 5, the honest attempt only: the C actually computed the right answer, checked against an
    // independent Rust implementation of the same definition. A restart that produced a corpse that
    // "ran" but computed nothing would otherwise pass.
    if attempt == c_seam::ATTEMPT_HONEST && output_correct(g) {
        bits |= checks::OUTPUT_CORRECT;
    }

    bits
}

/// Is the honest attempt's output in the grant, and right? The checksum against a Rust
/// recomputation, and the transformed string byte for byte including its terminator.
fn output_correct(g: &mut [u8]) -> bool {
    let text = &c_seam::INPUT[..c_seam::INPUT.len() - 1]; // without the NUL the C stops at
    let want = c_seam::expected_checksum(text);
    let got = u32::from_le_bytes([
        g[c_seam::OUT_OFF],
        g[c_seam::OUT_OFF + 1],
        g[c_seam::OUT_OFF + 2],
        g[c_seam::OUT_OFF + 3],
    ]);
    if got != want {
        return false;
    }
    let out = &g[c_seam::OUT_OFF + 4..];
    c_seam::INPUT.iter().enumerate().all(|(i, &b)| {
        let up = if b.is_ascii_lowercase() { b - 32 } else { b };
        out[i] == up
    })
}

/// The three pages, as slices. Separate functions rather than one static, because each is a distinct
/// claim and mixing them up in the checker would be the most embarrassing possible bug.
fn grant_page() -> &'static mut [u8] {
    // SAFETY: a page we mapped read/write into our own address space at `_start`, and never unmap.
    unsafe { core::slice::from_raw_parts_mut(c_seam::GRANT_VA as *mut u8, c_seam::PAGE as usize) }
}
fn witness_ro() -> &'static mut [u8] {
    // SAFETY: ours, read/write, mapped at `_start`. The C component holds a read-only view of the
    // same physical frame, which is the point.
    unsafe {
        core::slice::from_raw_parts_mut(c_seam::WITNESS_RO_VA as *mut u8, c_seam::PAGE as usize)
    }
}
fn witness_far() -> &'static mut [u8] {
    // SAFETY: ours, read/write, mapped at `_start`. No other address space maps this frame at all.
    unsafe {
        core::slice::from_raw_parts_mut(c_seam::WITNESS_FAR_VA as *mut u8, c_seam::PAGE as usize)
    }
}

/// Report which stage failed, then trap. A half-built harness is not worth limping along, and the
/// stage code turns "nothing happened" into a legible failure.
fn bail(stage: u64) -> ! {
    send(REPORT, c_seam::RPT_FAILED, stage, 0);
    supervision_proto::fail()
}

user_rt::panic_handler!();
