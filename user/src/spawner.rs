//! **The construction sub-server: process building, and nothing else** (milestone 22 phase B.2).
//!
//! Where init's process-construction authority went. It holds one untyped budget (WRITE only, so it
//! may spend memory but never lend it), a request channel, and **one program image** copied into its
//! address space by rootsup. It does not hold the initrd, so "build me program X" is not a thing that
//! can be asked of it: the only program it can name is the one it was handed.
//!
//! Each instance is built in its **own region** split off the budget, which is what makes reaping a
//! single `Untyped::DESTROY` (§16 object revocation) and keeps one instance's corpse from pinning
//! another's memory. A LIFO reap returns the pages to the budget (§16's return-of-pages), so a
//! restart loop is not a leak. The spawner keeps that region capability so it can reap on request,
//! and it is the only process in the tree that can: the supervisor decides *whether* to reap and
//! rebuild, the spawner is what *can*. Policy and authority, split.
//!
//! It is not the supervisor. It never reads a fault message and it holds no policy; it answers
//! requests in the order they arrive. See notes/trusted-init.md.

#![no_std]
#![no_main]

use user_rt::{cap_delete, recv, send};

// Each binary in the tree compiles the shared module but uses a different slice of it (the sub-server
// builds nothing, the supervisor holds no memory), so the unused halves are expected, not dead.
#[path = "suptree.rs"]
#[allow(dead_code)]
mod suptree;

use suptree::{Endow, REP_BUILT, REP_FAILED, REP_REAPED, REQ_BUILD, REQ_REAP};

/// The capabilities rootsup endowed us with, in order.
const REQ: u64 = 0; // READ: build/reap requests arrive here
const REP: u64 = 1; // WRITE: answers go back here
const BUDGET: u64 = 2; // WRITE: the memory every instance is made of
const REPORT: u64 = 3; // WRITE|GRANT: so each instance can report for itself
const CHILDFAULT: u64 = 4; // READ|GRANT: so each instance is born supervised

/// Where rootsup copied the one program image we may build. Must match rootsup.rs.
const IMAGE_VA: u64 = 0x3000_0000;

/// Pages per instance region: enough for the sub-server's segments, its stack, its address-space
/// tables, and its TCB. A debug build of a tiny program is a handful of pages.
const INSTANCE_PAGES: u64 = 48;

/// How many instances we track at once. The tree runs one sub-server and a restart reaps the corpse
/// before building the replacement, so three is already slack. It is also a cspace budget: each live
/// instance costs one slot for its region capability.
const MAX_INSTANCES: usize = 3;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, image_len: u64, _a2: u64) -> ! {
    // SAFETY: rootsup mapped `image_len` bytes of program image read-only at IMAGE_VA.
    let image = unsafe { core::slice::from_raw_parts(IMAGE_VA as *const u8, image_len as usize) };
    let Ok(elf) = elf::Elf::parse(image) else {
        suptree::fail()
    };

    // Instance id (1-based, so 0 means "no instance") -> the region capability it was built in. The
    // region is what `DESTROY` reaps; the id is the handle the supervisor names it by. The kernel's
    // fault message carries a *tid*, and no method turns a tid into anything the spawner holds, so the
    // handle is ours to issue. See the note in notes/trusted-init.md on the tid-to-instance gap.
    let mut region: [u64; MAX_INSTANCES] = [0; MAX_INSTANCES];

    loop {
        let (op, arg, _w2) = recv(REQ);
        match op {
            REQ_BUILD => {
                match build(&elf, arg, &mut region) {
                    Some(instance) => send(REP, REP_BUILT, instance, 0),
                    None => send(REP, REP_FAILED, 0, 0),
                };
            }
            REQ_REAP => {
                let idx = arg.wrapping_sub(1) as usize;
                let ok = match region.get(idx) {
                    Some(&r) if arg != 0 && r != 0 => {
                        let reaped = suptree::untyped_destroy(r);
                        if reaped {
                            cap_delete(r);
                            region[idx] = 0;
                        }
                        reaped
                    }
                    _ => false,
                };
                send(REP, if ok { REP_REAPED } else { REP_FAILED }, 0, 0);
            }
            _ => {
                send(REP, REP_FAILED, 0, 0);
            }
        }
    }
}

/// Build one instance in its own region, endowed with a report endpoint and its supervision endpoint,
/// started with `attempt` in its second argument register. Returns the instance handle.
fn build(elf: &elf::Elf, attempt: u64, region: &mut [u64; MAX_INSTANCES]) -> Option<u64> {
    let free = region.iter().position(|&r| r == 0)?;
    let r = suptree::untyped_split(BUDGET, INSTANCE_PAGES).ok()?;
    let tcb = suptree::build_child(
        BUDGET,
        r,
        elf,
        &Endow {
            caps: &[(REPORT, abi::rights::WRITE)],
            maps: &[],
            blobs: &[],
            fault: Some(CHILDFAULT),
        },
    )
    .ok()?;
    if !suptree::tcb_start(tcb, 0, attempt, 0) {
        return None;
    }
    // The TCB capability is not the thread: dropping it leaves the thread running, and the region
    // capability is what still reaches the corpse later.
    cap_delete(tcb);
    region[free] = r;
    Some(free as u64 + 1)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    suptree::fail()
}
