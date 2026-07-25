//! A shell, at EL0: the last program lifted out of hello into its own binary (milestone 19f.5).
//!
//! **Proof the whole stack works.** It reads command lines from the input driver (milestone 10's
//! receive side), prints through the console server (milestone 8), and asks init to spawn worker
//! processes on command. Every layer under it is exercised at once: EL0, per-process address
//! spaces, capabilities, IPC, and two userspace drivers, all distinct binaries now. The kernel is a
//! message router; everything the user sees is a conversation between processes.
//!
//! # The shell's world
//!
//! It holds, by convention (init granted them in this order):
//!
//! - slot 0/1: the console server's request/reply endpoints (print).
//! - slot 2: the input driver's line endpoint (read a line).
//! - slot 3: a spawn endpoint (ask init to start a worker).
//! - slot 4: a result endpoint (receive a spawned worker's answer).
//!
//! and two shared pages: one with the console server (output), one with the input driver (the line
//! buffer). No role selector; a standalone binary needs none. The syscall runtime (`invoke`/`recv`)
//! comes from the shared `user_rt` crate (19f.6).

#![no_std]
#![no_main]

use user_rt::{invoke, recv};

// Shared pages (must match init's / the kernel shell_service's wiring).
const OUT_VA: u64 = 0x0000_0000_0060_0000; // shared with the console server
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // shared with the input driver

// Capability slots.
const REQUEST: u64 = 0; // SEND to the console server
const REPLY: u64 = 1; // RECV the console ack
const LINE: u64 = 2; // RECV a completed input line
const SPAWN: u64 = 3; // SEND a spawn request
const RESULT: u64 = 4; // RECV a worker's result

/// Print a string through the console server: write it into the shared page, send the length, wait
/// for the ack (which means the buffer is free again).
fn print(s: &[u8]) {
    let n = s.len().min(4096);
    let out = OUT_VA as *mut u8;
    for (i, &b) in s[..n].iter().enumerate() {
        // SAFETY: the console shared page is mapped read/write.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
    // SAFETY: `svc`; the kernel validates the console capability.
    unsafe { invoke(REQUEST, abi::endpoint::SEND, n as u64, 0, 0) };
    recv(REPLY);
}

/// Print a small unsigned number.
fn print_num(mut v: u64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    print(&digits[i..]);
}

/// Read a command line from the input driver. The bytes land in the shared LINE page; we copy up to
/// `out.len()` of them and return the count.
fn read_line(out: &mut [u8]) -> usize {
    let len = recv(LINE).0 as usize;
    let src = LINE_VA as *const u8;
    let n = len.min(out.len());
    for (i, b) in out[..n].iter_mut().enumerate() {
        // SAFETY: the line page is mapped read-only and holds at least `len` bytes.
        *b = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
    n
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    print(b"\ncricker-os shell. every command below runs at EL0.\n");
    print(b"commands: help, echo <text>, run <n>\n");

    let mut line = [0u8; 128];
    loop {
        print(b"$ ");
        let n = read_line(&mut line);
        let cmd = &line[..n];
        // No echo here: the input driver echoes each character as you type it (raw terminal), so
        // echoing the whole line again would double it.

        if cmd == b"help" {
            print(b"  help        this text\n");
            print(b"  echo <text> print <text>\n");
            print(b"  run <n>     spawn a worker process that returns n*n\n");
        } else if let Some(rest) = strip_prefix(cmd, b"echo ") {
            print(rest);
            print(b"\n");
        } else if let Some(rest) = strip_prefix(cmd, b"run ") {
            let n = parse_num(rest);
            // Ask init's spawn service to start a worker computing n*n. It runs as its own EL0
            // process (the "worker" binary) and reports back on the result endpoint we hold.
            // SAFETY: `svc`.
            unsafe { invoke(SPAWN, abi::endpoint::SEND, n, 0, 0) };
            let answer = recv(RESULT).0;
            if answer == u64::MAX {
                print(b"  could not spawn a process (the kernel is out of memory)\n");
            } else {
                print(b"  a spawned process at EL0 computed ");
                print_num(n);
                print(b"*");
                print_num(n);
                print(b" = ");
                print_num(answer);
                print(b"\n");
            }
        } else if cmd.is_empty() {
            // blank line, just prompt again
        } else {
            print(b"  unknown command (try 'help')\n");
        }
    }
}

fn strip_prefix<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if s.len() >= prefix.len() && &s[..prefix.len()] == prefix {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn parse_num(s: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in s {
        if b.is_ascii_digit() {
            v = v.wrapping_mul(10) + (b - b'0') as u64;
        }
    }
    v
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { core::arch::asm!("brk #0", options(nostack, nomem)) };
    loop {
        core::hint::spin_loop();
    }
}
