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
//! It also cannot reap a **hung** job: a live thread is refused on purpose (§32), and nothing here
//! escalates. That is §32's recorded watchdog case and it belongs to milestone 23, not here. At the
//! prompt the forcible tier already exists for the case a person can see: `^C` twice tears the job's
//! region down through the shell (milestone 24), and those jobs are built from the shell's own
//! untyped rather than init's, so they never reach this program at all.

#![no_std]
#![no_main]

use user_rt::{reap, recv_fault};

/// The supervision endpoint init endows every job with, held `READ`: the right to receive deaths
/// here, which is the same right §32 makes the right to collect them.
const DEATHS: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    loop {
        // The kernel is the only sender on this endpoint (§26 clears the child's fault slot at
        // `START`), so the tid is trustworthy without a badge.
        let (_event, tid, _pc, _addr, _rsvd) = recv_fault(DEATHS);
        if reap(DEATHS, tid) != 0 {
            // The tid arrived on a death message from this very endpoint, so both refusals §32
            // defines are unreachable: it cannot be `StillAlive` and it cannot be another
            // supervisor's child. Getting one anyway means the corpse is leaking, which is silent,
            // so trap instead: the kernel prints the pc and this process dies where the mistake is.
            fail()
        }
    }
}

fn fail() -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    user_rt::exit()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
