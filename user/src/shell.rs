//! A shell, at EL0. **Proof the whole stack works.**
//!
//! It reads command lines from the input driver (milestone 10's receive side), prints through the
//! console server (milestone 8), and spawns worker processes on command. Every layer under it is
//! exercised at once: EL0, per-process address spaces, capabilities, IPC, and two userspace
//! drivers. The kernel is a message router; everything the user sees is a conversation between
//! processes.
//!
//! # The shell's world
//!
//! It holds, by convention (the kernel granted them in this order):
//!
//! - slot 0/1: the console server's request/reply endpoints (print).
//! - slot 2: the input driver's line endpoint (read a line).
//! - slot 3: a spawn endpoint (ask the kernel to start a worker).
//! - slot 4: a result endpoint (receive a spawned worker's answer).
//!
//! and two shared pages: one with the console server (output), one with the input driver (the
//! line buffer).

use crate::{invoke, recv, send};
use abi::endpoint;

// Shared pages (must match the kernel's shell_service wiring).
const OUT_VA: u64 = 0x0000_0000_0060_0000; // shared with the console server
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // shared with the input driver

// Capability slots.
const REQUEST: u64 = 0; // SEND to the console server
const REPLY: u64 = 1; // RECV the console ack
const LINE: u64 = 2; // RECV a completed input line
const SPAWN: u64 = 3; // SEND a spawn request
const RESULT: u64 = 4; // RECV a worker's result

/// Print a string through the console server: write it into the shared page, send the length,
/// wait for the ack (which means the buffer is free again).
fn print(s: &[u8]) {
    let n = s.len().min(4096);
    let out = OUT_VA as *mut u8;
    for (i, &b) in s[..n].iter().enumerate() {
        // SAFETY: the console shared page is mapped read/write.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
    // SAFETY: `svc`; the kernel validates the console capability.
    unsafe { invoke(REQUEST, endpoint::SEND, n as u64, 0, 0) };
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

/// Read a command line from the input driver. The bytes land in the shared LINE page; we copy up
/// to `out.len()` of them and return the count.
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

pub fn run() -> ! {
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
            // Ask the kernel's spawn service to start a worker computing n*n. It runs as its own
            // EL0 process and reports back on the result endpoint we hold.
            // SAFETY: `svc`.
            unsafe { invoke(SPAWN, endpoint::SEND, n, 0, 0) };
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

/// A worker process. Milestone 10's "binary": spawned on command, computes, reports, and exits.
///
/// It receives its input `n` in `x1` (the kernel's spawn service put it there), computes `n*n`,
/// sends the answer to the shell on the result endpoint, and exits cleanly. A whole process
/// lifecycle — spawn, run, report, exit — driven by a line the user typed.
pub fn worker() -> ! {
    let n = worker_arg();
    let result = n.wrapping_mul(n);
    send(RESULT_SLOT, result, 0, 0);
    // Exit, rather than spin: this demonstrates the whole lifecycle. The kernel reaps us.
    // SAFETY: `svc`; SYS_EXIT never returns.
    unsafe { core::arch::asm!("svc #0", in("x8") abi::SYS_EXIT, in("x0") 0u64, options(nostack, nomem)) };
    loop {
        core::hint::spin_loop();
    }
}

/// The worker's result endpoint slot (the kernel grants exactly one capability).
const RESULT_SLOT: u64 = 0;

/// The worker's argument, delivered in `x1` at `_start` and stashed by the entry shim.
fn worker_arg() -> u64 {
    // SAFETY: written once by `_start` before this runs, single-threaded.
    unsafe { crate::WORKER_ARG }
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
