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
//! What the shell can grant, it grants from what it *holds*. The headline is `prog --mem N`, which
//! endows a program N pages of untyped **split from the shell's own budget** (slot 3) and delegated
//! to it. A bare name in a file position is the second, and it is refused here with "you hold no
//! such capability", which is a statement about this shell's cspace rather than a placeholder: the
//! boot that starts it wires no FS service, so there is no directory to narrow. The mechanism a
//! grant would use exists and is proven on both ISAs (`user/src/fwarden.rs`); see [`holdings`] and
//! notes/grant-expression.md for exactly what is left. `caps` prints the shell's whole endowment,
//! and `caps <command>` previews exactly what that command would grant, making DECISIONS §14's
//! "reading one literal tells you a process's whole authority" interactively true.
//!
//! # The grammar lost two words in milestone 47
//!
//! There is no `run` verb: a bare program name spawns it, so builtins and programs are typed the
//! same way and nobody has to know which class a command is in. And there is no `file:` prefix: a
//! bare token in a file position designates the file, because the direction (the half that matters)
//! was always the manifest's to declare and the prefix only marked the half already on the screen.
//! `caps` is the visibility surface that remains, which is an argument for keeping it good.
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

use capsh::{Action, Command, Endowment, Escalation, Prog, Refusal, RunSpec, jobframe, spawnproto};
use lineedit::proto;
use user_rt::{call, cap_delete, invoke, recv, send, yield_now};

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

/// **What this shell holds, which is what decides whether a named file can be backed.**
///
/// A per-file grant is a directory capability narrowed to one name (milestone 31 phase 2,
/// `user/src/fwarden.rs`). Narrowing needs a directory to narrow, and this shell's endowment stops
/// at slot 3: init grants it a terminal, a spawn channel, a result channel and a budget, and nothing
/// that names a filesystem, because the boot that starts this shell wires no FS service. So the
/// answer here is `false`, and a file named at a program that takes one gets the milestone's
/// headline refusal, which is **true** rather than a placeholder: this shell really does hold no
/// such capability. (No shipped program declares `FileSpec::Required` yet, so that refusal is
/// reachable from `plan_against` and not from the prompt; see notes/grant-expression.md.)
///
/// It is a function rather than a constant so that the day the boot path wires an FS service and
/// grants a directory at a slot, the one place that changes is here. The planning, the manifest
/// vocabulary, and the `caps` preview are already written against it; see notes/grant-expression.md
/// for what is left.
fn holdings() -> capsh::Holdings {
    capsh::Holdings { dir: false }
}

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
    print(b"commands: help, echo <text>, caps [command], <prog> [--mem N] [arg]\n");

    let mut line = [0u8; 128];
    loop {
        let (n, flags) = read_line(b"$ ", &mut line);
        if flags & proto::FLAG_INTERRUPTED != 0 {
            // ^C at the prompt: the terminal discarded the line. Account for this interrupt so it
            // does not leak into the next job's watch, then come back for the next line.
            CONSUMED.store(intr_count(), core::sync::atomic::Ordering::Relaxed);
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
    }
}

fn help() {
    print(b"  help                    this text\n");
    print(b"  echo <text>             print <text>\n");
    print(b"  caps                    print this shell's whole endowment\n");
    print(b"  caps <command>          preview what that command would grant\n");
    print(b"  worker <n>              spawn a process that returns n*n\n");
    print(b"  budgeter --mem N        grant a process N pages from this shell's budget\n");
    print(b"  date                    print the wall-clock time\n");
    print(b"  <prog> <name>           grant a process one file, and only that file\n");
    print(b"\n  naming a resource grants it; a program that names nothing can touch nothing.\n");
}

/// Resolve an invocation, then either refuse it at the prompt (a mismatch the manifest caught) or
/// spawn it, granting exactly what the command named and nothing else.
fn run(spec: RunSpec) {
    match capsh::plan(&spec, holdings()) {
        Err(refusal) => refuse(spec, refusal),
        // A supervised job runs under the two-tier ^C path (milestone 24); a fast job is simply
        // spawned and waited on.
        Ok(endow) if endow.interruptible => spawn_interruptible(endow),
        Ok(endow) => spawn(endow),
    }
}

/// Print a refusal in the capability model's voice. The fixed half is `capsh`'s (host-tested so the
/// wording cannot drift); the shell supplies the program name where one helps.
fn refuse(spec: RunSpec, refusal: Refusal) {
    print(b"  ");
    // A named-but-unresolvable program, or an un-grantable resource, name the offending thing. A
    // line of nothing but flags (`--mem 16`) names no program at all, and printing an empty name
    // followed by a colon would be worse than printing the bare refusal.
    match refusal {
        Refusal::NoSuchProgram if !spec.prog.is_empty() => {
            print(spec.prog);
            print(b": ");
        }
        Refusal::NoSuchProgram => {}
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
    // A grant this path cannot deliver must stop here, loudly. `plan` already refuses a `file:` when
    // `holdings().dir` is false, so today this is unreachable; it exists because the day that flips,
    // the thing that must NOT happen is a child spawned without the file the command named while the
    // prompt says nothing. Authority the user thought they granted must never quietly evaporate,
    // which is the same rule that makes an unexpected token a refusal instead of a shrug.
    if e.file.is_some() {
        print(
            b"  a file grant needs init to build the warden; this shell cannot deliver one yet\n",
        );
        return;
    }
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

    // The request: program id, argument, page count, not interruptible (capsh::spawnproto).
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages, false);
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

    // One reader, one word: a real program's answer, or init's spawn-failed sentinel. `date` is the
    // exception: it answers in **text**, framed the way every std program's stdout is, so its
    // messages are drained by a reader that knows that framing.
    if matches!(e.prog, Prog::Date) {
        report_text();
        return;
    }
    let answer = recv(RESULT).0;
    outcome(e, answer);
}

/// Drain one framed line of text from the result endpoint and print it.
///
/// The framing is the std PAL's stdout framing (`w0` = the byte count, `w1`|`w2` = the bytes,
/// little-endian), which `date` shares deliberately so there is one convention for "a program
/// printed something". `SEND` blocks until a receiver takes it, so stopping at the newline consumes
/// exactly the messages that line was made of and leaves the endpoint clean for the next command.
///
/// It reads one line because the programs that answer this way print one, and the shell must not
/// block the prompt forever waiting for a second that is not coming. A `date` spawned with the
/// provenance selector prints two, which is why that selector is not reachable from here until the
/// manifest can declare arity (capsh's `Prog::Date`).
fn report_text() {
    print(b"  ");
    for _ in 0..MAX_TEXT_CHUNKS {
        let (n, w1, w2) = recv(RESULT);
        if n == spawnproto::SPAWN_FAILED {
            print(b"could not spawn (init is out of memory)\n");
            return;
        }
        let n = (n as usize).min(16);
        let mut buf = [0u8; 16];
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = if i < 8 {
                (w1 >> (8 * i)) as u8
            } else {
                (w2 >> (8 * (i - 8))) as u8
            };
        }
        print(&buf[..n]);
        if buf[..n].contains(&b'\n') {
            return;
        }
    }
    print(b"\n");
}

/// The most 16-byte chunks one reported line may take before the shell stops reading. `date`'s
/// longest line is 66 bytes (five chunks); the ceiling exists so a program that never sends its
/// newline costs one truncated line instead of a hung prompt.
const MAX_TEXT_CHUNKS: usize = 8;

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
        // Supervised jobs report through the job frame and the interruptible path, not here; `date`
        // answers in text and is drained by `report_text` before this is reached.
        Prog::Heeder | Prog::Spinner | Prog::Date => {}
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
        if holdings().dir {
            print(
                b"    cap 4  endpoint  directory  the files it can narrow into per-file grants\n",
            );
        } else {
            print(b"    (no directory capability: a name on the line has nothing to narrow)\n");
        }
        print(b"    (no clock: 'date' spawned from here holds no clock capability, and says so)\n");
        print(b"  it can name no devices and no other process. authority is what it holds.\n");
        return;
    }
    // Only a program invocation carries a grant to preview; `caps help` has nothing to say.
    let Command::Run(spec) = capsh::parse(tail) else {
        print(b"  caps previews a command's grant; try: caps budgeter --mem 16\n");
        return;
    };
    match capsh::plan(&spec, holdings()) {
        Err(refusal) => refuse(spec, refusal),
        Ok(e) => preview(e),
    }
}

/// Print the endowment a resolved invocation would hand the new process.
fn preview(e: Endowment) {
    print(b"  ");
    print(e.prog.name().as_bytes());
    print(b" would grant the new process, and nothing else:\n");
    print(b"    cap 0  endpoint  result   report its answer back\n");
    if e.mem_pages > 0 {
        print(b"    cap 1  untyped   ");
        print_num(e.mem_pages);
        print(b" pages  split from this shell's budget\n");
    }
    // A file endowment reads as one line naming the file and the direction, because that IS the
    // whole authority: an endpoint served by a file warden that will answer for this name and no
    // other. The direction comes from the program's manifest, not from anything typed, which is why
    // it is worth printing: the line you typed plus this table is the child's complete authority.
    if let Some(g) = e.file {
        print(b"    cap 2  endpoint  file     ");
        print(g.name);
        print(if g.writable {
            b"  (read+write, and nothing else on the disk)\n".as_slice()
        } else {
            b"  (read-only, and nothing else on the disk)\n".as_slice()
        });
    }
    // `date`'s authority is a read-only mapping of the clock page, and it is **init's to endow, not
    // this shell's**: it is not designated on the line and no token could designate it. Saying so
    // here is the point of `caps` being the sole visibility surface. The day the shell is granted a
    // clock to delegate, this line becomes a cap row, and until then the preview must not let a
    // reader believe the command line is the whole story.
    if matches!(e.prog, Prog::Date) {
        print(b"    (clock: this shell holds none to delegate, so it will report the time\n");
        print(b"     as unknown. the clock is init's to endow; no token on the line can.)\n");
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

// ---- the two-tier interrupt path (milestone 24, DECISIONS §24) ----

const PAGE: u64 = 4096;
/// Pages the construction untyped for a supervised child holds: its aspace, code, stack, and TCB.
/// The heeder and spinner are tiny; this is generous. DESTROY returns these pages to our budget.
const JOB_UNTYPED_PAGES: u64 = 32;
/// Where we map a supervised job's shared frame in our own space. It advances per job, because there
/// is no unmap syscall: each job gets a fresh window and the old mapping is simply left behind (one
/// page of address space, and one frame from our budget, is the honest per-job cost).
static SH_JOBFRAME_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x0000_0000_00c0_0000);

/// The terminal's `^C` count we have already accounted for. A watermark, not a per-job baseline, and
/// that distinction is load-bearing: a `^C` typed the instant after `run heeder` is counted by the
/// terminal *before* the shell finishes spawning and starts watching (the input driver runs first).
/// Diffing against a watermark carried across the session catches it anyway; a fresh baseline read at
/// watch-start would already include it and miss the interrupt. A prompt `^C` (a failed read) and a
/// finished job each advance the watermark, so neither leaks into the next job.
static CONSUMED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Run a supervised foreground job: mint its resources from our own budget, direct init to build it
/// from a region we hold, then watch it under the two-tier `^C` escalation until it stops or we tear
/// it down. This is the whole of DECISIONS §24 on the shell's side.
fn spawn_interruptible(e: Endowment) {
    // Mint the job's resources. RETYPE the shared frame first, then SPLIT the construction budget,
    // so the budget is the top of our watermark and DESTROY returns its pages cleanly (LIFO).
    let job_fr = match retype_frame() {
        Some(s) => s,
        None => {
            print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
            return;
        }
    };
    let job_ut = match untyped_split(JOB_UNTYPED_PAGES) {
        Some(s) => s,
        None => {
            cap_delete(job_fr);
            print(b"  this shell's memory budget is exhausted; nothing left to grant\n");
            return;
        }
    };

    // Map the shared frame into our own space so we can signal the job and read its status.
    let va = SH_JOBFRAME_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
    if !map_frame(job_fr, va) {
        cap_delete(job_fr);
        cap_delete(job_ut);
        print(b"  could not map the job frame\n");
        return;
    }
    // A freshly retyped frame is zeroed, but make the shared contract explicit.
    jf_store(va, jobframe::INTERRUPT, 0);
    jf_store(va, jobframe::DONE, 0);
    jf_store(va, jobframe::STATUS, 0);
    jf_store(va, jobframe::HEARTBEAT, 0);

    // Direct init: an interruptible request, then the job untyped and the job frame, both delegated
    // WRITE|GRANT (init builds from the untyped and maps the frame). We keep our own copies: the
    // untyped to tear the job down, the frame to signal it and read it.
    let (w0, w1, w2) = spawnproto::request(e.prog.id(), e.arg, e.mem_pages, true);
    send(SPAWN, w0, w1, w2);
    send_cap(job_ut);
    send_cap(job_fr);

    // init acks once the child is running: that is the shell's go-ahead to start watching.
    if recv(RESULT).0 != spawnproto::SPAWN_OK {
        print(b"  could not spawn (init is out of memory)\n");
        cap_delete(job_fr);
        cap_delete(job_ut);
        return;
    }
    print(b"  running ");
    print(e.prog.name().as_bytes());
    print(b" in the foreground. ^C interrupts it.\n");

    watch(va, job_ut);

    // Account for every ^C up to now, so the job's interrupts do not leak into the next one.
    CONSUMED.store(intr_count(), core::sync::atomic::Ordering::Relaxed);
    // Drop our caps. If the job was reclaimed, the untyped cap is already stale; cap_delete just
    // frees the slot.
    cap_delete(job_fr);
    cap_delete(job_ut);
}

/// Watch a running supervised job: poll its done flag and the terminal's `^C` count, drive the
/// escalation policy, and act. The busy-poll with `yield` is how one thread watches two things (the
/// job and `^C`) with only blocking primitives and no non-blocking receive (DECISIONS §24, wait A).
fn watch(va: u64, job_ut: u64) {
    // The baseline is the session watermark, not a fresh read: a ^C counted during the spawn is
    // already reflected in intr_count but not yet in CONSUMED, so diffing against CONSUMED sees it.
    let base = CONSUMED.load(core::sync::atomic::Ordering::Relaxed);
    let mut esc = Escalation::new();
    let mut fed: u64 = 0;
    loop {
        // Finished on its own (cooperatively or naturally)?
        if jf_load(va, jobframe::DONE) != 0 {
            let status = jf_load(va, jobframe::STATUS);
            let beats = jf_load(va, jobframe::HEARTBEAT);
            reclaim(job_ut);
            report_finished(status, beats);
            return;
        }
        // Fold in every ^C the terminal has seen since we started watching.
        let n = intr_count().wrapping_sub(base);
        let mut action = Action::None;
        while fed < n {
            fed += 1;
            let a = esc.on_interrupt();
            if !matches!(a, Action::None) {
                action = a; // Forcible (a second ^C) wins over Cooperative (the first)
            }
        }
        if matches!(action, Action::None) {
            action = esc.on_tick(); // the grace timeout: escalate a job that ignored the first ^C
        }
        match action {
            Action::Cooperative => {
                jf_store(va, jobframe::INTERRUPT, 1);
                print(b"  ^C: asked the job to stop.\n");
            }
            Action::Forcible => {
                forcible(job_ut);
                return;
            }
            Action::None => {}
        }
        yield_now();
    }
}

/// Report a job that stopped on its own.
fn report_finished(status: u64, beats: u64) {
    if status == jobframe::STATUS_INTERRUPTED {
        print(b"  the job caught the interrupt and stopped cleanly after ");
        print_num(beats);
        print(b" work units.\n");
    } else {
        print(b"  the job finished after ");
        print_num(beats);
        print(b" work units.\n");
    }
}

/// The forcible tier: tear the job's region down with the owner's `DESTROY`. The shell holds the
/// untyped the child was built from, so this reclaims its every object. Once the §16 amendment lands
/// (DESTROY force-kills a live resident thread), this ends even a runaway that ignored the first ^C.
fn forcible(job_ut: u64) {
    print(b"  ^C again: tearing the job down.\n");
    if reclaim(job_ut) {
        print(b"  the job's process was torn down and its memory reclaimed.\n");
    } else {
        print(b"  teardown refused: DESTROY force-kills a live thread once the kernel amendment lands.\n");
    }
}

/// Reclaim the job's region, retrying because a cooperatively-exiting child may still be finishing
/// its last instruction (DESTROY refuses while a thread is live). Returns whether it succeeded.
fn reclaim(job_ut: u64) -> bool {
    for _ in 0..256 {
        // SAFETY: `svc`/`ecall`; DESTROY reclaims the region or refuses (a live thread, pre-amendment).
        if unsafe { invoke(job_ut, abi::untyped::DESTROY, 0, 0, 0) } == 0 {
            return true;
        }
        yield_now();
    }
    false
}

/// RETYPE one page of our budget into a Frame capability we hold. `None` when the budget is spent.
fn retype_frame() -> Option<u64> {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the untyped.
    let r = unsafe { invoke(BUDGET, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { None } else { Some(r as u64) }
}

/// Map the frame in `slot` read/write at `va` in our own space; page tables come from our budget.
fn map_frame(slot: u64, va: u64) -> bool {
    // SAFETY: `svc`/`ecall`; the kernel checks the frame cap and the address.
    unsafe { invoke(slot, abi::frame::MAP, va, 1, BUDGET) == 0 }
}

/// Delegate the capability in `slot` to init over the spawn endpoint, narrowed to WRITE|GRANT (init
/// builds from an untyped or maps a frame, and narrows further from there). We keep our own copy.
fn send_cap(slot: u64) {
    // SAFETY: `svc`/`ecall`; the kernel checks WRITE on the endpoint and GRANT on the delegated cap.
    unsafe {
        invoke(
            SPAWN,
            abi::endpoint::SEND_CAP,
            slot,
            abi::rights::WRITE | abi::rights::GRANT,
            spawnproto::CAP_TAG,
        )
    };
}

/// Ask the terminal how many `^C` it has seen (a non-blocking poll; see proto::OP_INTRCOUNT).
fn intr_count() -> u64 {
    call(TERM, proto::req(proto::OP_INTRCOUNT, 0), 0).0
}

/// Read a word from the mapped job frame.
fn jf_load(va: u64, off: usize) -> u64 {
    // SAFETY: the job frame is mapped read/write at `va`; `off` is a valid word offset.
    unsafe { core::ptr::read_volatile((va as usize + off) as *const u64) }
}

/// Write a word to the mapped job frame.
fn jf_store(va: u64, off: usize, v: u64) {
    // SAFETY: as above; this word is the shell's to write (one writer per word, see jobframe).
    unsafe { core::ptr::write_volatile((va as usize + off) as *mut u64, v) }
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
