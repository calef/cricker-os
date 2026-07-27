//! **Asking the host to terminate, RISC-V.** The test harness's exit path.
//!
//! The module keeps the aarch64 name (`arch::semihosting::exit`) because it is the arch contract the
//! test harness calls, but the mechanism is different: not ARM semihosting, but QEMU virt's
//! `sifive_test` finisher, an MMIO word at 0x10_0000 that exits QEMU. Writing `PASS` exits 0;
//! writing `FAIL | (code << 16)` exits non-zero. That maps exactly onto our success/failure codes,
//! so the harness works unchanged. Real and correct in bare mode (virtual == physical). See
//! notes/riscv-port.md.

use core::arch::asm;

/// The `sifive_test` finisher MMIO register on QEMU's `virt` machine.
const SIFIVE_TEST: *mut u32 = 0x10_0000 as *mut u32;
/// Write this to exit QEMU with status 0.
const FINISHER_PASS: u32 = 0x5555;
/// Base value for a failing exit; the caller's code is packed into the high half.
const FINISHER_FAIL: u32 = 0x3333;

/// The harness's success code (a passing exit).
pub const EXIT_SUCCESS: u32 = 0;
/// The harness's failure code (any non-zero exit).
pub const EXIT_FAILURE: u32 = 1;

/// Terminate the QEMU guest with `code` (0 = success). Drives the `sifive_test` finisher: `PASS` for
/// a clean exit, `FAIL` with the code in the high bits otherwise.
pub fn exit(code: u32) -> ! {
    let word = if code == 0 {
        FINISHER_PASS
    } else {
        FINISHER_FAIL | (code << 16)
    };
    // SAFETY: an MMIO store to the finisher register. In bare mode its physical address is directly
    // addressable. The write exits QEMU, so nothing after it runs.
    unsafe { core::ptr::write_volatile(SIFIVE_TEST, word) };

    // The finisher terminates the guest; if it somehow does not, stop rather than run on.
    loop {
        // SAFETY: wait-for-interrupt is always safe.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
