//! The first program cricker-os runs that it did not compile, talking to it through the first
//! syscall interface cricker-os ever had.
//!
//! It arrives in the **initrd**, the way Linux gets its initramfs. Nothing about it is known to
//! the kernel at build time.
//!
//! # It holds exactly one capability, and it can name nothing else
//!
//! Slot 0 is a `Console` it may `WRITE` to. That is the whole of its world. There is no path it
//! can say, no uid it can be, no `open()` it can call. See DECISIONS.md §10.
//!
//! So the program's job is to go and find the edges of that world, and check they are where the
//! kernel says they are:
//!
//!   1. Its own image is intact (`.text`, `.rodata`, `.data`, `.bss`, and a working stack).
//!   2. It **can** print, through slot 0.
//!   3. It **cannot** print through slot 1, which is empty. Not denied: *empty*.
//!   4. It cannot talk the kernel into printing the kernel's own memory. **The confused deputy.**
//!
//! Any surprise and it executes `brk`, which the kernel treats as a fault and kills it. So "no
//! fault" is a machine-checkable claim that every one of those held.

#![no_std]
#![no_main]

use abi::{Error, console};

/// In `.rodata`. Proves the read-only segment was mapped, and mapped *readable*.
#[unsafe(no_mangle)]
static RODATA_MARKER: [u8; 4] = [0xc0, 0xff, 0xee, 0xd0];

/// In `.data`. Proves the loader copied file contents, not just zeroes.
#[unsafe(no_mangle)]
static mut DATA_MARKER: u64 = 0x0000_c0ff_ee00_d0d0;

/// In `.bss`, because it is zero. **Proves `memsz > filesz` was honoured.** The bytes for this
/// are not in the ELF at all.
#[unsafe(no_mangle)]
static mut BSS_MARKER: u64 = 0;

/// The capability we were handed. Slot 0, by convention, because somebody put it there.
const CONSOLE: u64 = 0;

/// A slot nobody put anything in.
const EMPTY: u64 = 1;

/// The kernel's own `.text`, in the direct map. We know the address. **Knowing an address is not
/// authority**, and this program is about to demonstrate that at some length.
const KERNEL_TEXT: u64 = 0xffff_0000_4008_0000;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // --- 1. our own image ---
    // .text ran, or we would not be here.
    check(RODATA_MARKER == [0xc0, 0xff, 0xee, 0xd0]);

    // SAFETY: single-threaded, and nobody else has this address space.
    unsafe {
        check(core::ptr::read_volatile(&raw const DATA_MARKER) == 0x0000_c0ff_ee00_d0d0);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 0);

        core::ptr::write_volatile(&raw mut BSS_MARKER, 1);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 1);
    }
    check(stack_works(7));

    // --- 2. we can print, because we were handed the right to ---
    let msg = b"      hello from EL0, through a capability in slot 0.\n";
    check(write(CONSOLE, msg) == Ok(msg.len() as i64));

    // --- 3. and we cannot print through a slot nobody filled ---
    //
    // The answer is NoSuchSlot, and the word matters. It is not "permission denied", which would
    // mean the console is over there and we are not allowed to touch it. **There is nothing
    // there.** We cannot name the thing we did not get.
    check(write(EMPTY, b"this should never appear") == Err(Error::NoSuchSlot));

    // --- 4. the confused deputy ---
    //
    // We know exactly where the kernel's text is. We cannot read one byte of it: the MMU stops
    // us, and we have already watched a sibling program die trying.
    //
    // So we do not try. **We ask the kernel to read it for us.** We hold a genuine console
    // capability. Printing is a thing we are entitled to do. All we are doing is choosing the
    // bytes, and the kernel can certainly read them.
    //
    // If it does, it will have leaked its own memory, on our behalf, using its own authority,
    // and every check it performed will have passed. That is the confused deputy, and it is why
    // capabilities alone are not enough: the kernel has to refuse to be *our* deputy for an
    // address *we* could not touch.
    let stolen = unsafe { core::slice::from_raw_parts(KERNEL_TEXT as *const u8, 64) };
    check(write(CONSOLE, stolen) == Err(Error::BadPointer));

    // ...and the same trick with a length instead of an address, in case the check was lazy
    // about ranges rather than about pages.
    let straddle = unsafe { core::slice::from_raw_parts((KERNEL_TEXT - 8) as *const u8, 64) };
    check(write(CONSOLE, straddle) == Err(Error::BadPointer));

    // And a wholly unmapped low address, which is our own half but not ours.
    let unmapped = unsafe { core::slice::from_raw_parts(0x7000_0000u64 as *const u8, 16) };
    check(write(CONSOLE, unmapped) == Err(Error::BadPointer));

    let done = b"      and it refused to read the kernel's memory on my behalf.\n";
    check(write(CONSOLE, done) == Ok(done.len() as i64));

    // Everything held. Now spin, with no syscall, no yield and not one function call, so that the
    // only thing in the universe that can take the CPU back is a timer interrupt landing between
    // two of these instructions. DECISIONS.md §5.
    loop {
        core::hint::spin_loop();
    }
}

/// Print, through a capability. `Ok(n)` or an `Error`.
fn write(cap: u64, bytes: &[u8]) -> Result<i64, Error> {
    let r = unsafe {
        invoke(
            cap,
            console::WRITE,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            0,
        )
    };
    match Error::from_ret(r) {
        Some(e) => Err(e),
        None => Ok(r),
    }
}

/// The only way this program can act on anything outside itself.
///
/// # Safety
/// `svc` traps to EL1. The kernel validates everything; that is its job and the whole point.
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

/// **The only way this program can say "no".**
///
/// A `brk` from EL0 is a fault, and the kernel kills us for it. Which is exactly the signal we
/// want: a failed expectation must be indistinguishable from a broken program, because it *is*
/// one. The kernel needs no ABI to hear it, and no data crosses the boundary.
fn check(ok: bool) {
    if !ok {
        fail();
    }
}

fn fail() -> ! {
    // SAFETY: this deliberately traps.
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
