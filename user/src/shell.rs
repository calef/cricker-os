//! The capability shell, at EL0: designation is authorization (milestone 31, phase 1).
//!
//! Milestone 10 gave this program a command line; milestone 31 gives that command line a meaning it
//! does not have on Unix. A Unix child inherits your whole authority and then `open()`s whatever its
//! uid allows. Here the command line is a **grant expression**: naming a resource in a command is
//! how you grant it, and a program that names nothing gets nothing beyond the report channel every
//! spawn carries. There is no ambient authority to fall back on, so when a program needs something
//! the command did not grant, the refusal reads "you hold no such capability", not a Unix-flavored
//! EPERM. The parsing and the manifest checking are the host-tested `capsh` crate; this file is the
//! wiring that turns a checked grant into real, delegated capabilities.
//!
//! What the shell can grant, it grants from what it *holds*. The headline is `run --mem N prog`,
//! which endows a program N pages of untyped **split from the shell's own budget** (slot 3) and
//! delegated to it. The shell holds no filesystem capability yet, so `run prog file:PATH` is refused
//! "you hold no such capability"; that syntax starts working when milestone 32's FS server lands,
//! with no change to the grammar. `caps` prints the shell's whole endowment, and `caps run ...`
//! previews exactly what a command would grant, making DECISIONS §14's "reading one literal tells
//! you a process's whole authority" interactively true.
//!
//! # The shell's world
//!
//! It holds, by convention (init granted them in this order):
//!
//! - slot 0: the terminal endpoint (CALL: OP_WRITE / OP_READLINE).
//! - slot 1: a spawn endpoint (direct init to start a program; capsh::spawnproto).
//! - slot 2: a result endpoint (receive a spawned program's answer).
//! - slot 3: an untyped budget, the memory it grants with `--mem`.
//!
//! and two pages shared with the terminal: OUT_VA (we write text and prompts) and LINE_VA
//! (completed lines arrive). No role selector; the syscall runtime comes from `user_rt`.

#![no_std]
#![no_main]

use capsh::{Command, Endowment, Prog, Refusal, RunSpec, spawnproto};
use linedisc::proto;
use user_rt::{call, cap_delete, invoke, recv, send};

// Pages shared with the terminal (must match the wiring in init).
const OUT_VA: u64 = 0x0000_0000_0060_0000; // we write; the terminal reads
const LINE_VA: u64 = 0x0000_0000_00b0_0000; // the terminal writes; we read

// Capability slots.
const TERM: u64 = 0; // CALL requests on the terminal
const SPAWN: u64 = 1; // SEND a spawn request to init
const RESULT: u64 = 2; // RECV a spawned program's answer
const BUDGET: u64 = 3; // our own untyped; SPLIT a grant off it for `--mem`

/// The budget init granted us at boot (must match sysinit / hello init_boot's SH_BUDGET_PAGES).
/// We cannot query how much remains (there is no such syscall), so `caps` prints the initial grant.
const SH_BUDGET_PAGES: u64 = 128;

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

/// Print a small unsigned number in base 10.
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
    print(b"\ncricker-os capability shell. naming a resource in a command IS granting it.\n");
    print(b"commands: help, echo <text>, caps [command], run [--mem N] <prog> [arg]\n");

    let mut line = [0u8; 128];
    loop {
        let (n, flags) = read_line(b"$ ", &mut line);
        if flags & proto::FLAG_INTERRUPTED != 0 {
            // ^C: the terminal discarded the line; come back for the next one.
            continue;
        }
        if flags & proto::FLAG_EOF != 0 {
            // ^D on an empty line. This shell is the session; there is nowhere to exit to.
            print(b"  (end of input; this shell has nowhere to exit to)\n");
            continue;
        }
        dispatch(&line[..n]);
    }
}

/// Parse one line with `capsh` and act on it. All parsing and the manifest check are the host-tested
/// crate; this function is only IO and capability moves.
fn dispatch(cmd: &[u8]) {
    match capsh::parse(cmd) {
        Command::Empty => {}
        Command::Help => help(),
        Command::Echo(text) => {
            print(text);
            print(b"\n");
        }
        Command::Caps(tail) => caps(tail),
        Command::Run(spec) => run(spec),
        Command::Unknown(_) => print(b"  unknown command (try 'help')\n"),
    }
}

fn help() {
    print(b"  help                    this text\n");
    print(b"  echo <text>             print <text>\n");
    print(b"  caps                    print this shell's whole endowment\n");
    print(b"  caps run ...            preview what a run command would grant\n");
    print(b"  run worker <n>          spawn a process that returns n*n\n");
    print(b"  run --mem N budgeter    grant a process N pages from this shell's budget\n");
    print(b"\n  naming a resource grants it; a program that names nothing can touch nothing.\n");
}

/// Resolve a `run`, then either refuse it at the prompt (a mismatch the manifest caught) or spawn
/// it, granting exactly what the command named and nothing else.
fn run(spec: RunSpec) {
    match capsh::plan(&spec) {
        Err(refusal) => refuse(spec, refusal),
        Ok(endow) => spawn(endow),
    }
}

/// Print a refusal in the capability model's voice. The fixed half is `capsh`'s (host-tested so the
/// wording cannot drift); the shell supplies the program name where one helps.
fn refuse(spec: RunSpec, refusal: Refusal) {
    print(b"  ");
    // A named-but-unresolvable program, or an un-grantable resource, name the offending thing.
    match refusal {
        Refusal::NoSuchProgram => {
            print(spec.prog);
            print(b": ");
        }
        _ => {
            if let Some(p) = Prog::from_name(spec.prog) {
                print(p.name().as_bytes());
                print(b": ");
            }
        }
    }
    print(refusal.message().as_bytes());
    print(b"\n");
}

/// Grant and spawn. The one moment authority moves: split any memory grant off our own budget,
/// direct init to load the program, delegate the grant, and read the one answer that comes back.
fn spawn(e: Endowment) {
    // A memory grant is carved from the shell's own untyped. If our budget is spent, say so plainly
    // rather than sending init a promise we cannot keep.
    let mem_slot = if e.mem_pages > 0 {
        match untyped_split(e.mem_pages) {
            Some(slot) => Some(slot),
            None => {
                print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
                return;
            }
        }
    } else {
        None
    };

    // The request: program id, argument, page count (capsh::spawnproto).
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages);
    send(SPAWN, w0, w1, w2);

    // If a budget rode along, delegate it now, narrowed to WRITE|GRANT so init can re-insert it into
    // the child (init narrows it again to WRITE there: the child spends it, it does not lend it).
    if let Some(slot) = mem_slot {
        // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the endpoint and GRANT on the untyped.
        unsafe {
            invoke(
                SPAWN,
                abi::endpoint::SEND_CAP,
                slot,
                abi::rights::WRITE | abi::rights::GRANT,
                spawnproto::CAP_TAG,
            )
        };
        cap_delete(slot); // our copy is delegated; free the slot
    }

    // One reader, one word: a real program's answer, or init's spawn-failed sentinel.
    let answer = recv(RESULT).0;
    outcome(e, answer);
}

/// Report what the spawned program did, in terms of the grant it was given.
fn outcome(e: Endowment, answer: u64) {
    if answer == spawnproto::SPAWN_FAILED {
        print(b"  could not spawn (init is out of memory)\n");
        return;
    }
    match e.prog {
        Prog::Worker => {
            print(b"  a process at EL0 computed ");
            print_num(e.arg);
            print(b"*");
            print_num(e.arg);
            print(b" = ");
            print_num(answer);
            print(b"\n");
        }
        Prog::Budgeter => {
            print(b"  the process mapped ");
            print_num(answer);
            print(b" pages out of the ");
            print_num(e.mem_pages);
            print(b"-page budget you granted (the rest paid for its page tables)\n");
        }
    }
}

/// Print the shell's whole endowment, or, with a tail, preview what that command would grant. This
/// is the introspection that makes "reading one literal tells you a process's authority" real.
fn caps(tail: &[u8]) {
    let tail = capsh::trim(tail);
    if tail.is_empty() {
        print(b"  this shell holds, and nothing else:\n");
        print(b"    cap 0  endpoint  terminal   read lines, write text\n");
        print(b"    cap 1  endpoint  spawn      direct init to start a program\n");
        print(b"    cap 2  endpoint  result     read a spawned program's answer\n");
        print(b"    cap 3  untyped   ");
        print_num(SH_BUDGET_PAGES);
        print(b" pages  the memory it grants with --mem (initial)\n");
        print(
            b"  it can name no files, no devices, no other process. authority is what it holds.\n",
        );
        return;
    }
    // Only `run` commands carry a grant to preview.
    let Command::Run(spec) = capsh::parse(tail) else {
        print(b"  caps previews a 'run' command's grant; try: caps run --mem 16 budgeter\n");
        return;
    };
    match capsh::plan(&spec) {
        Err(refusal) => refuse(spec, refusal),
        Ok(e) => preview(e),
    }
}

/// Print the endowment a resolved `run` would hand the new process.
fn preview(e: Endowment) {
    print(b"  run ");
    print(e.prog.name().as_bytes());
    print(b" would grant the new process, and nothing else:\n");
    print(b"    cap 0  endpoint  result   report its answer back\n");
    if e.mem_pages > 0 {
        print(b"    cap 1  untyped   ");
        print_num(e.mem_pages);
        print(b" pages  split from this shell's budget\n");
    }
    print(b"    arg    ");
    if matches!(e.prog, Prog::Worker) {
        print_num(e.arg);
        print(b"\n");
    } else {
        print(b"(none)\n");
    }
    print(b"  reading the command is reading its whole authority.\n");
}

/// Carve `pages` off our own untyped budget (slot 3) into a delegatable child untyped. `None` when
/// the budget is exhausted.
fn untyped_split(pages: u64) -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped and returns a negative error
    // (OutOfMemory) when the budget cannot back `pages`.
    let r = unsafe { invoke(BUDGET, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { None } else { Some(r as u64) }
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
