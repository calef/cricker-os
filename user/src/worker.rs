//! The worker: a whole program in one job.
//!
//! Milestone 19f.2, the first program that is its **own binary** rather than a role of `hello`.
//! init loads it out of the initrd archive by the name `"worker"` (crickerfs), builds a child
//! address space and TCB at this ELF's own entry, and `START`s it with the input `n` in `x1`. The
//! worker squares `n`, `SEND`s the answer on the one endpoint init granted it (slot 0), and exits.
//! That is the entire program: no role byte to dispatch on, no capabilities beyond the single one
//! it needs. Least authority made real, because the program *is* its authority.
//!
//! It shares the `user` crate's `link.ld` (linked at `0x40_0000`, in its own address space, so the
//! shared load address is not a conflict) but not one line of hello's code: a distinct ELF with its
//! own `_start` and panic handler. The tiny runtime below (`invoke`/`send`/`exit`) is duplicated
//! from hello on purpose. When 19f.3 splits the next binary, that second copy is the signal to lift
//! a shared user-runtime crate, with the requirements finally known rather than guessed.

#![no_std]
#![no_main]

/// The endpoint init grants the worker as its only capability (slot 0). Its one `SEND` goes here,
/// straight to whoever is waiting (the kernel test, or the shell behind init's spawn service).
const RESULT: u64 = 0;

/// The worker's entry. `START` (milestone 19e) hands it three registers: `x0` is unused here (a
/// standalone binary needs no role selector), the input `n` arrives in `x1`, `x2` is unused.
#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, n: u64, _x2: u64) -> ! {
    let result = n.wrapping_mul(n);
    send(RESULT, result, 0, 0);
    // Exit rather than spin: this is a whole process lifecycle (spawn, run, report, exit). The
    // kernel reaps the thread and frees the address space.
    exit();
}

/// `SEND` three words on the endpoint capability in `slot`.
fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    // SAFETY: `svc` traps to EL1, which validates the capability named by `slot` before acting.
    unsafe { invoke(slot, abi::endpoint::SEND, w0, w1, w2) }
}

/// Terminate this process.
fn exit() -> ! {
    // SAFETY: `svc`; SYS_EXIT never returns.
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_EXIT, in("x0") 0u64, options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}

/// # Safety
/// `svc` traps to EL1. The kernel validates the capability and method; that is its whole job.
unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") cap => ret,
            in("x1") method,
            in("x2") a0,
            in("x3") a1,
            in("x4") a2,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // No console and no report channel to trust: the only honest way to signal "something is wrong"
    // is a fault the kernel turns into a kill. A worker bug is a dead worker, nothing more.
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {
        core::hint::spin_loop();
    }
}
