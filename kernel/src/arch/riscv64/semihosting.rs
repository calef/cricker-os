//! **Asking the host to terminate, RISC-V.** The test harness's exit path.
//!
//! The module keeps the aarch64 name (`arch::semihosting::exit`) because it is the arch contract the
//! test harness calls, but the mechanism is different: not ARM semihosting, but QEMU virt's
//! `sifive_test` finisher, an MMIO word at 0x10_0000 that exits QEMU. Writing `PASS` exits 0;
//! writing `FAIL | (code << 16)` exits non-zero. That maps exactly onto our success/failure codes,
//! so the harness works unchanged. The finisher is reached through the kernel's direct map (paging is
//! on by the time any test runs; `mmu::map_everything` maps this page device-typed). See
//! notes/riscv-port.md.

use core::arch::asm;

/// The `sifive_test` finisher's **physical** address on QEMU's `virt` machine. Reached through the
/// direct map at run time, since paging is on (bare-mode identity is long gone).
const SIFIVE_TEST_PHYS: u64 = 0x10_0000;
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
    // SAFETY: an MMIO store to the finisher register through the direct map (mapped device-typed by
    // mmu::map_everything). The write exits QEMU, so nothing after it runs.
    let reg = super::mmu::phys_to_virt(SIFIVE_TEST_PHYS) as *mut u32;
    unsafe { core::ptr::write_volatile(reg, word) };

    // The finisher terminates the guest; if it somehow does not, stop rather than run on.
    loop {
        // SAFETY: wait-for-interrupt is always safe.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
