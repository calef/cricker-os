//! What happens when the kernel panics.
//!
//! `std` provides a panic handler, so you have never had to think about this.
//! Without `std` you must write one, and it forces a real question: there is no
//! process to kill, no stderr, no shell to return to. What *should* happen?
//!
//! Our answer: say what went wrong, then stop the machine. Under `cargo test`, a
//! panic is a failing test, so we exit QEMU with a failure status instead.

use crate::arch;
use crate::println;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!();
    println!("[PANIC] {info}");
    crate::stack::warn_if_smashed();

    #[cfg(test)]
    arch::semihosting::exit(arch::semihosting::EXIT_FAILURE);

    #[cfg(not(test))]
    arch::halt()
}
