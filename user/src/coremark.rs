//! The CoreMark workload, at EL0 (milestone 19e: "run a real workload").
//!
//! The first program that isn't a demo or a driver: it does substantial, standard compute. It runs
//! the CoreMark-derived kernel (`crates/coremark`: a linked-list sort, a small-matrix multiply, and
//! a state machine, the three work items of a CoreMark iteration) for a fixed iteration count, then
//! SENDs the run's CRC home on the one endpoint init granted it (slot 0). The CRC is the run's
//! self-validation: for a fixed iteration count it is a fixed value (`coremark::PINNED_CRC_64`), so
//! init (and the kernel test) can check the computation came out right. A whole real program spawned
//! against the native ABI (notes/abi.md): given a capability, it computes and reports.
//!
//! The compute lives in a portable crate on purpose, so the *identical* code later compiles for
//! macOS and Linux and the compute comparison is one source on three OSs (notes/benchmarks.md).
//!
//! It **self-times**: it reads the virtual counter (`user_rt::now`) around the run, so it reports a
//! real score, not just correctness. Under HVF those ticks are real nanoseconds; under TCG they are
//! icount fiction (magnitudes are meaningless, the CRC is not). The report is three words:
//! `[crc, ticks, freq]`, so the receiver can compute iterations per second without a second syscall.

#![no_std]
#![no_main]

use user_rt::{cntfrq, exit, now, send};

/// The endpoint init grants us (slot 0): we SEND `[crc, ticks, freq]` here, then exit.
const RESULT: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    let start = now();
    let crc = coremark::run(coremark::PINNED_ITERS);
    let ticks = now().wrapping_sub(start);
    send(RESULT, crc as u64, ticks, cntfrq());
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A compute bug is a dead workload: fault, and let the kernel turn it into a kill.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
