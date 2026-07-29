//! The spinner: a runaway that ignores the interrupt entirely (milestone 24).
//!
//! A tight loop that touches nothing and checks nothing. It is the case the cooperative tier cannot
//! reach: the shell can set the interrupt flag all it likes, and the spinner never reads it. Only
//! the forcible tier ends it, the shell tearing the spinner's region down with object revocation
//! (DECISIONS §24, §16). That is the whole reason the second `^C` exists.
//!
//! It is deliberately a pure `loop`, accessing no memory the shell holds, so it is the honest worst
//! case: it cannot be killed by revoking a frame it depends on (it depends on none), only by the
//! region owner's `DESTROY` force-killing the resident thread. That kernel behavior is the §16
//! amendment landing alongside this milestone; until it merges, the shell's teardown of a spinner is
//! refused and the prompt returns having said so.
//!
//! It holds nothing: no capabilities, and it does not even map the shared job frame it was granted.

#![no_std]
#![no_main]

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
