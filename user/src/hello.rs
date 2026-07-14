//! The first program cricker-os runs that it did not compile into itself.
//!
//! A separate crate, a separate linker script, a separate ELF. It arrives in the **initrd**,
//! the way Linux gets its initramfs: QEMU loads the file into RAM and writes the address into
//! the device tree, and the kernel finds it at `/chosen/linux,initrd-start`. Nothing about this
//! binary is known to the kernel at build time.
//!
//! # It has no syscalls, and that is deliberate
//!
//! There is no ABI yet (DECISIONS §10: the syscall surface gets designed at 7d, against a
//! capability table). So this program cannot *tell* the kernel anything.
//!
//! Instead it **checks its own image** and speaks with the only two words it has:
//!
//!   - `svc` — everything I expected about my own memory is true.
//!   - `brk` — it is not. (Which the kernel treats as a fault, and kills us.)
//!
//! **No data crosses the boundary.** The kernel counts `svc`s and faults and learns whether its
//! loader is correct, without either side agreeing on the meaning of a single register.

#![no_std]
#![no_main]

/// In `.rodata`. Proves the read-only segment was mapped, and mapped *readable*.
#[unsafe(no_mangle)]
static RODATA_MARKER: [u8; 4] = [0xc0, 0xff, 0xee, 0xd0];

/// In `.data`. Proves the loader copied file contents, not just zeroes.
#[unsafe(no_mangle)]
static mut DATA_MARKER: u64 = 0x0000_c0ff_ee00_d0d0;

/// In `.bss`, because it is zero. **Proves `memsz > filesz` was honoured.**
///
/// The bytes for this are NOT in the ELF. If the loader copies `p_filesz` and stops, this holds
/// whatever the previous owner of that frame left behind, and the check below fails.
#[unsafe(no_mangle)]
static mut BSS_MARKER: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // .text ran, or we would not be here.

    // .rodata is mapped and readable.
    if RODATA_MARKER != [0xc0, 0xff, 0xee, 0xd0] {
        fail();
    }

    // .data was loaded from the file.
    // SAFETY: single-threaded, and nobody else has this address space.
    if unsafe { core::ptr::read_volatile(&raw const DATA_MARKER) } != 0x0000_c0ff_ee00_d0d0 {
        fail();
    }

    // .bss was zeroed. The file does not contain these eight bytes.
    // SAFETY: as above.
    if unsafe { core::ptr::read_volatile(&raw const BSS_MARKER) } != 0 {
        fail();
    }

    // And .data is actually writable, which .text had better not be.
    // SAFETY: as above.
    unsafe {
        core::ptr::write_volatile(&raw mut BSS_MARKER, 1);
        if core::ptr::read_volatile(&raw const BSS_MARKER) != 1 {
            fail();
        }
    }

    // The stack works: this call has a frame, and it returns.
    if !stack_works(7) {
        fail();
    }

    ok();

    // And now spin, with no syscall, no yield, and not one function call, so that the only
    // thing in the universe that can take the CPU back from us is a timer interrupt landing
    // between two of these instructions. See DECISIONS.md §5.
    loop {
        core::hint::spin_loop();
    }
}

/// Recurse a little, to prove `SP_EL0` points at real, writable memory.
#[inline(never)]
fn stack_works(n: u64) -> bool {
    let local = [n; 8];
    if n == 0 {
        return local[0] == 0;
    }
    core::hint::black_box(&local);
    stack_works(n - 1)
}

/// "Everything I expected about myself is true."
fn ok() {
    // SAFETY: `svc` is the one instruction EL0 has for talking to EL1, and at 7c the kernel
    // does nothing with it but count it.
    unsafe { core::arch::asm!("svc #0", options(nostack, nomem)) };
}

/// "It is not." The kernel treats a `brk` from EL0 as a fault and kills us, which is exactly
/// the signal we want: a failed check must be indistinguishable from a broken program.
fn fail() -> ! {
    // SAFETY: this deliberately traps.
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
