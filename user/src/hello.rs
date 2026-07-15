//! The initrd program. **One binary, two roles**, chosen by the argument the kernel puts in
//! `x0` at `_start`, the way a real kernel hands a new process its argc.
//!
//! - **Role `CLIENT`**: an ordinary program that wants to print. It does not own a UART and
//!   cannot reach one. It writes its text into a page it *shares* with the console server, and
//!   sends the length over an endpoint. That is the whole of "printing" now.
//!
//! - **Role `CONSOLE_SERVER`**: the console driver, **at EL0.** It owns a mapping of the PL011's
//!   registers and a read-only view of the shared page. It loops: receive a length, copy that
//!   many bytes from the shared page to the UART, acknowledge. This code used to be in the
//!   kernel. Milestone 8 is the milestone where it left.
//!
//! # Why the bytes travel in shared memory and the length travels in a message
//!
//! DECISIONS §10: **IPC carries control, shared memory carries data.** The kernel is not in the
//! data path at all. It never sees the bytes, never copies them, never validates a pointer into
//! them. The confused-deputy problem that 7d had to defend against **cannot arise here**, because
//! the thing that could be confused (a kernel doing I/O for a user) no longer exists. The
//! architecture dissolved the bug.

#![no_std]
#![no_main]

mod input;
mod shell;
mod virtio;

use abi::{Error, endpoint};

/// Roles, as passed in `x0` by the kernel.
///
/// One binary, several behaviours. The kernel chooses by the argument it puts in `x0`, the way a
/// real kernel hands a new process its argv. A `SELF_CHECK` client needs no capabilities and no
/// shared memory (it only inspects its own image), which is why the milestone-7 tests can spawn
/// it bare; a `PRINTING` client needs the console endpoints and the shared page.
const SELF_CHECK: u64 = 0;
const CONSOLE_SERVER: u64 = 1;
const PRINTING: u64 = 2;
const VIRTIO_BLK: u64 = 3;
const INPUT: u64 = 4;
const SHELL: u64 = 5;
const WORKER: u64 = 6;
const UNTYPED_DEMO: u64 = 7;
const VIRTIO_ATTACK: u64 = 8;

// --- the shared layout, known to both roles because they are the same binary ---

/// The page shared between a client and the console server. The client writes text here; the
/// server reads it. Mapped read/write in the client, read-only in the server.
const SHARED_VA: u64 = 0x0000_0000_0060_0000;

/// The PL011's registers, mapped into the **server only**, as device memory.
const UART_VA: u64 = 0x0000_0000_0070_0000;
const UART_DR: u64 = 0x00; // data register: write a byte to transmit it
const UART_FR: u64 = 0x18; // flag register
const UART_FR_TXFF: u32 = 1 << 5; // transmit FIFO full

// --- capability slots, by convention (the kernel granted them in this order) ---

/// Client: slot 0 sends the print request. Server: slot 0 receives it.
const REQUEST: u64 = 0;
/// Client: slot 1 receives the ack. Server: slot 1 sends it.
const REPLY: u64 = 1;

// --- markers, so the client can check its own image was loaded correctly ---

#[unsafe(no_mangle)]
static RODATA_MARKER: [u8; 4] = [0xc0, 0xff, 0xee, 0xd0];
#[unsafe(no_mangle)]
static mut DATA_MARKER: u64 = 0x0000_c0ff_ee00_d0d0;
#[unsafe(no_mangle)]
static mut BSS_MARKER: u64 = 0;

/// The worker process's argument (n), delivered in `x1` at entry.
pub(crate) static mut WORKER_ARG: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, _arg2: u64) -> ! {
    // The worker receives its argument in x1 (dma_phys is reused as a generic scalar here).
    unsafe { WORKER_ARG = dma_phys };

    match role {
        CONSOLE_SERVER => console_server(),
        PRINTING => printing_client(),
        VIRTIO_BLK => virtio::run(dma_phys),
        INPUT => input::run(),
        SHELL => shell::run(),
        WORKER => shell::worker(),
        UNTYPED_DEMO => untyped_demo(),
        VIRTIO_ATTACK => virtio::run_attack(dma_phys),
        SELF_CHECK => self_check_client(),
        _ => self_check_client(),
    }
}

/// The console driver, running at EL0, owning the UART.
fn console_server() -> ! {
    loop {
        // Wait for a client to hand us a length. This BLOCKS until one sends.
        let (len, _, _) = recv(REQUEST);

        // Copy that many bytes from the shared page to the UART, one at a time, exactly as the
        // kernel's PL011 driver used to. The difference is only where this code runs.
        let shared = SHARED_VA as *const u8;
        for i in 0..len {
            // SAFETY: the shared page is mapped read-only in our address space, and `len` came
            // from a client we then verify by writing at most one page. A malicious length is a
            // read out of our OWN mapping, which faults US, not the kernel: a driver bug is a
            // crashed process. (7c/§10.)
            let byte = unsafe { core::ptr::read_volatile(shared.add(i as usize)) };
            uart_put(byte);
        }

        // Acknowledge, so the client knows the buffer is free to reuse.
        send(REPLY, len, 0, 0);
    }
}

/// Prove our own image is intact. None of this needs a capability: it is all our own memory. A
/// mismatch means the loader is broken, and we say so the only way we can, with a `brk` that the
/// kernel turns into a fault.
fn self_check() {
    check(RODATA_MARKER == [0xc0, 0xff, 0xee, 0xd0]);
    // SAFETY: single-threaded, sole owner of this address space.
    unsafe {
        check(core::ptr::read_volatile(&raw const DATA_MARKER) == 0x0000_c0ff_ee00_d0d0);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 0); // .bss was zeroed
        core::ptr::write_volatile(&raw mut BSS_MARKER, 1);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 1); // .data is writable
    }
    check(stack_works(7));
}

/// A program that checks its own image and then does nothing but exist. Needs no capabilities.
/// This is the "a real ELF ran and verified itself" program the milestone-7 tests spawn bare.
fn self_check_client() -> ! {
    self_check();

    // Make one syscall that needs no capability at all, to prove we reached EL0 and can trap
    // back in. Yield is authority over ourselves; nobody has to grant it.
    // SAFETY: `svc` traps to EL1; SYS_YIELD takes no arguments and cannot fail.
    unsafe { core::arch::asm!("svc #0", in("x8") abi::SYS_YIELD, options(nostack, nomem)) };

    loop {
        core::hint::spin_loop();
    }
}

/// A program that checks its own image and then prints, through the console server, using the
/// endpoints and shared page the kernel handed it.
fn printing_client() -> ! {
    self_check();

    // These cannot fail: this role is only ever spawned WITH the console, so `print` holds its
    // capabilities. A failure would be a `brk`, which is what we want if the wiring is wrong.
    check(print(b"      hello from EL0, printed by a driver that also runs at EL0.\n").is_ok());
    check(print(b"      the kernel never saw these bytes.\n").is_ok());

    // Done. Spin, so the timer can prove it still preempts us. No syscall, no yield, no call.
    loop {
        core::hint::spin_loop();
    }
}

/// Print `bytes` by handing them to the console server through shared memory.
///
/// Returns `Ok` if we hold the endpoints to reach the server, `Err(NoSuchSlot)` if we were not
/// given them. The bytes go in the shared page; only the length crosses the endpoint.
fn print(bytes: &[u8]) -> Result<(), Error> {
    let n = bytes.len().min(4096);

    // SAFETY: the shared page is mapped read/write in our address space. We own it between an
    // ack and the next send, which the reply below is what guarantees.
    let shared = SHARED_VA as *mut u8;
    for (i, &b) in bytes[..n].iter().enumerate() {
        unsafe { core::ptr::write_volatile(shared.add(i), b) };
    }

    // The length is the message. The data is already in place, shared, uncopied.
    let r = unsafe { invoke(REQUEST, endpoint::SEND, n as u64, 0, 0) };
    if let Some(e) = Error::from_ret(r) {
        return Err(e); // e.g. NoSuchSlot: we were not handed a console
    }

    // Wait for the server to finish reading the buffer before we touch it again.
    let (_ack, _, _) = recv(REPLY);
    Ok(())
}

/// Write one byte to the UART we own, spinning while the transmit FIFO is full. **This is the
/// driver.** It used to be `Pl011::write_byte` in the kernel.
fn uart_put(byte: u8) {
    // SAFETY: UART_VA is our device mapping of the PL011, established at spawn. The kernel
    // configured the device at boot; we only transmit.
    unsafe {
        let fr = (UART_VA + UART_FR) as *const u32;
        while core::ptr::read_volatile(fr) & UART_FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        core::ptr::write_volatile((UART_VA + UART_DR) as *mut u32, byte as u32);
    }
}

// --- the two IPC primitives, over `svc` ---

fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    // SAFETY: `svc` traps to EL1, which validates the capability in `slot`.
    unsafe { invoke(slot, endpoint::SEND, w0, w1, w2) }
}

fn recv(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: as above. RECV returns three words in x0/x1/x2.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => w0,
            in("x1") endpoint::RECV,
            lateout("x1") w1,
            lateout("x2") w2,
            in("x3") 0u64,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// # Safety
/// `svc` traps to EL1. The kernel validates everything, which is its whole job.
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

#[inline(never)]
fn stack_works(n: u64) -> bool {
    let local = [n; 8];
    if n == 0 {
        return local[0] == 0;
    }
    core::hint::black_box(&local);
    stack_works(n - 1)
}

/// The only way this program can say "no": a `brk`, which the kernel treats as a fault and kills
/// us for. A failed check must be indistinguishable from a broken program, because it is one.
fn check(ok: bool) {
    if !ok {
        fail();
    }
}

fn fail() -> ! {
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {}
}

/// Milestone 11: spend an untyped budget. This process holds a capability to a chunk of raw
/// memory (slot 0) and a report endpoint (slot 1). It maps page after page out of that untyped
/// into its own address space, writes and reads each one to prove it is real, and keeps going
/// until the untyped is exhausted. Then it reports how many it mapped.
///
/// The whole point is what the KERNEL does while this runs: nothing. Every page here comes out of
/// the untyped, so the kernel's free-frame count does not move. A test checks exactly that.
fn untyped_demo() -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const BASE_VA: u64 = 0x0000_0000_00c0_0000;

    // Signal that we are loaded and about to start spending the untyped. The test measures the
    // kernel's frame count HERE, so it sees only what we do from now on: map from our untyped.
    send(REPORT, 0, 0, 0);

    let mut mapped: u64 = 0;
    loop {
        let va = BASE_VA + mapped * 4096;
        // Retype a page out of our untyped and map it here. SAFETY: `svc`.
        let r = unsafe { invoke(UNTYPED, abi::untyped::MAP, va, 0, 0) };
        if let Some(e) = Error::from_ret(r) {
            // OutOfMemory means our budget is spent. Any other error is a real bug.
            if e != Error::OutOfMemory {
                fail();
            }
            break;
        }

        // Prove the page is genuinely ours: write a marker, read it back.
        let marker = 0xA11C_0000_0000_0000u64 | mapped;
        // SAFETY: the kernel just mapped this page writable in our address space.
        unsafe {
            core::ptr::write_volatile(va as *mut u64, marker);
            if core::ptr::read_volatile(va as *const u64) != marker {
                fail();
            }
        }

        mapped += 1;
        if mapped > 100_000 {
            fail(); // a bump allocator that never exhausts is a bug
        }
    }

    send(REPORT, mapped, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
