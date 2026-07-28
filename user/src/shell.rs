//! A shell, at EL0: the last program lifted out of hello into its own binary (milestone 19f.5).
//!
//! **Proof the whole stack works.** It talks to one terminal endpoint (milestone 28's contract,
//! notes/terminal-contract.md) and asks init to spawn worker processes on command. Every layer
//! under it is exercised at once: EL0, per-process address spaces, capabilities, IPC, and three
//! userspace components (input driver, line discipline, console server), all distinct binaries.
//! The kernel is a message router; everything the user sees is a conversation between processes.
//!
//! What the shell does NOT know is the point. It cannot tell whether its terminal endpoint is
//! served by the full line discipline or by bare drivers; it never sees a keystroke, an escape
//! sequence, or an echo. Line editing, history, and ^C all happen on the other side of the
//! endpoint. The shell writes text, reads lines, and handles the two flags the contract defines
//! (end of input, interrupted).
//!
//! # The shell's world
//!
//! It holds, by convention (init granted them in this order):
//!
//! - slot 0: the terminal endpoint (CALL: OP_WRITE / OP_READLINE).
//! - slot 1: a spawn endpoint (ask init to start a worker).
//! - slot 2: a result endpoint (receive a spawned worker's answer).
//!
//! and two pages shared with the terminal: OUT_VA (we write text and prompts) and LINE_VA
//! (completed lines arrive). No role selector; the syscall runtime comes from `user_rt`.

#![no_std]
#![no_main]

use linedisc::proto;
use user_rt::{call, invoke, recv};

// Pages shared with the terminal (must match the wiring in init / the kernel shell_service).
const OUT_VA: u64 = 0x0000_0000_0060_0000; // we write; the terminal reads
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // the terminal writes; we read

// Capability slots.
const TERM: u64 = 0; // CALL requests on the terminal
const SPAWN: u64 = 1; // SEND a spawn request
const RESULT: u64 = 2; // RECV a worker's result

/// Print through the terminal: write the text into the shared page, CALL OP_WRITE. The reply
/// means the bytes are on the wire and the page is ours again.
fn print(s: &[u8]) {
    let n = s.len().min(4096);
    stage(s, n);
    call(TERM, proto::req(proto::OP_WRITE, n as u64), 0);
}

/// Copy `n` bytes into the outgoing shared page.
fn stage(s: &[u8], n: usize) {
    let out = OUT_VA as *mut u8;
    for (i, &b) in s[..n].iter().enumerate() {
        // SAFETY: the terminal's output page is mapped read/write at OUT_VA.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
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

/// Read a command line: stage the prompt, CALL OP_READLINE, and block until the terminal has a
/// line for us. The editing (cursor keys, history, backspace) happens entirely on the far side;
/// we get the finished line in LINE_VA and its length and flags in the reply.
fn read_line(prompt: &[u8], out: &mut [u8]) -> (usize, u64) {
    stage(prompt, prompt.len());
    let (len, flags) = call(TERM, proto::req(proto::OP_READLINE, prompt.len() as u64), 0);
    let len = (len as usize).min(out.len());
    let src = LINE_VA as *const u8;
    for (i, b) in out[..len].iter_mut().enumerate() {
        // SAFETY: the line page is mapped read-only and holds at least `len` bytes.
        *b = unsafe { core::ptr::read_volatile(src.add(i)) };
    }
    (len, flags)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, _x1: u64, _x2: u64) -> ! {
    print(b"\ncricker-os shell. every command below runs at EL0.\n");
    print(b"commands: help, echo <text>, run <n>\n");

    let mut line = [0u8; 128];
    loop {
        let (n, flags) = read_line(b"$ ", &mut line);
        if flags & proto::FLAG_INTERRUPTED != 0 {
            // ^C: the terminal discarded the line; come back for the next one.
            continue;
        }
        if flags & proto::FLAG_EOF != 0 {
            // ^D on an empty line. This shell is the session; there is nothing to exit to.
            print(b"  (end of input; this shell has nowhere to exit to)\n");
            continue;
        }
        let cmd = &line[..n];

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
