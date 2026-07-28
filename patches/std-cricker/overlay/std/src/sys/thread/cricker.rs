//! Threads on cricker-os, phase one: `spawn` is honestly `Unsupported`, `sleep` is real.
//!
//! The kernel has everything a `thread::spawn` needs (retype a TCB from the process's own
//! untyped, CONFIGURE it into this address space, START it); what does not exist yet is the
//! std-side plumbing that makes the result safe (a TLS story, park/unpark on a kernel
//! primitive, join). Milestone 27 phase one ships without it rather than shipping it wrong,
//! so `spawn` returns `Unsupported` (the roadmap's named fallback) and the sync primitives are
//! std's single-threaded `no_threads` implementations.
//!
//! `sleep` does not get the unsupported treatment (`panic!("can't sleep")`) because the ABI can
//! do it honestly today: read the virtual counter, and `SYS_YIELD` until the deadline passes.
//! Yielding, not spinning: each iteration gives the CPU away, so a sleeping std program costs
//! one run-queue pass per wakeup instead of a hot core. A blocking kernel timer (sleep as an
//! endpoint the timer signals) is better and belongs to the same future as park/unpark.

use crate::ffi::CStr;
use crate::io;
use crate::num::NonZero;
use crate::thread::ThreadInit;
use crate::time::Duration;

// Silence dead code warnings for the otherwise unused ThreadInit::init() call.
#[expect(dead_code)]
fn dummy_init_call(init: Box<ThreadInit>) {
    drop(init.init());
}

pub struct Thread(!);

pub const DEFAULT_MIN_STACK_SIZE: usize = 64 * 1024;

impl Thread {
    // unsafe: see thread::Builder::spawn_unchecked for safety requirements
    pub unsafe fn new(_stack: usize, _init: Box<ThreadInit>) -> io::Result<Thread> {
        Err(io::Error::UNSUPPORTED_PLATFORM)
    }

    pub fn join(self) {
        self.0
    }
}

pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    // The process model is one thread today, and that is an answer, not an error.
    Ok(NonZero::new(1).unwrap())
}

pub fn current_os_id() -> Option<u64> {
    None
}

pub fn yield_now() {
    crate::sys::pal::cricker::rt::yield_now();
}

pub fn set_name(_name: &CStr) {
    // No kernel-side thread names yet.
}

pub fn sleep(dur: Duration) {
    use crate::sys::pal::cricker::rt;
    let freq = rt::cntfrq() as u128;
    let ticks = (dur.as_nanos().saturating_mul(freq) / 1_000_000_000).min(u64::MAX as u128) as u64;
    let deadline = rt::now().saturating_add(ticks);
    while rt::now() < deadline {
        rt::yield_now();
    }
}
