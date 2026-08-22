//! **The timetable: scheduled execution where every entry is a grant** (milestone 129).
//!
//! A cron, and the inversion of one. Unix cron reads a text file and runs arbitrary commands as
//! ambient authority made periodic: whatever the crontab says happens, with the account's whole
//! reach behind it, and the crontab is therefore the attack surface. This reads a text file too and
//! the file looks similar, but an entry's command is a **grant expression** checked at registration
//! by `grant_plan::plan`, the same function the shell checks a prompt line with. What a scheduled
//! child holds is planned, printed, and fixed before the first tick.
//!
//! **The claim, in one sentence:** compromising this process yields the entries' summed endowments,
//! not the system, because there is no ambient authority for a scheduled child to fall back on and
//! this process holds nothing it was not handed.
//!
//! # What it holds, and that is the whole list
//!
//! - slot 0: the output endpoint (WRITE). Where the plan and the summary go, as `sink_proto` bytes.
//! - slot 1: an untyped budget (WRITE). What every instance is made of, and what pays for the
//!   loader's own scratch mappings.
//! - slot 2: the child report endpoint (WRITE|GRANT), handed to each instance as its slot 0.
//! - slot 3: the supervision endpoint (READ|GRANT), placed in each instance's reserved fault slot
//!   so every scheduled child is born supervised (DECISIONS §26), and invoked with
//!   `Endpoint::REAP` to collect the corpses (§32).
//! - `a0`: how many fires to perform before summarising and exiting. `0` means forever.
//! - `a1`: the length of the archive the spawn site mapped read-only at [`INITRD_VA`]. **Not the
//!   initrd**: it holds exactly the programs this document will ever build, because the plan is
//!   computable before the first tick and so the endowment can be narrowed to it. This process
//!   audits that and says what it found, in the line after the plan.
//!
//! **It wants a bigger stack than a small program does**, and a spawn site has to say so: a
//! `grant_plan::Endowment` is about a kilobyte (mostly the name set a directory grant can carry) and
//! the plan holds one per entry, so the working set is tens of kilobytes rather than hundreds of
//! bytes. `kernel/src/user/timetable_tests.rs` maps 32 pages and says why; eight died with a data
//! abort whose faulting address was the stack pointer, which is what a stack overflow looks like
//! from the kernel side and reads like a wild pointer if you have not seen it before.
//!
//! **And nothing else.** No clock page, no directory, no console, no network, no device. That list
//! is not modesty: it is why a scheduled `date` in `timetable.conf` is refused at registration
//! rather than run, and why the refusal names the timetable rather than the line.
//!
//! # The archive is narrowed to the plan, and this process says so
//!
//! `Registry::programs` is the complete set of programs the document will ever start, and it is
//! known before anything fires. So the spawn site builds an archive of exactly that set and hands
//! *that* over, rather than the initrd: after milestone 129's second stratum this process cannot
//! load `date`, `ps`, `swish` or anything else it will never run, even though it holds a working
//! image loader.
//!
//! It cannot narrow its own endowment, so what it does instead is **measure it** and print the
//! answer next to the plan (`timetable::Audit`). A scheduler handed the whole initrd still works
//! and says a different sentence, which is the property worth having: the width of the endowment is
//! a line on the console rather than a fact only the spawn site knows.
//!
//! # The loop, and the one thing it cannot do
//!
//! Poll the monotonic counter (ambient, `user_rt::monotonic_nanos`, no capability); fire what is
//! due; when the budget cannot back another instance, block on the supervision endpoint until a
//! corpse arrives and reclaim its region. That last step is the only blocking wait in the program,
//! which matters because **this kernel has exactly one wait point per process and no timed wait at
//! all** (milestone 106 is `NOT-STARTED` and gated on a decision). See `BUGS`.
//!
//! Name: provisional, along with `crates/timetable` and `user/timetable.conf`. Milestone 129's own
//! block declines to propose a name and says the eventual one is calef's; see the crate's module
//! doc for what was refused and why.
//!
//! # BUGS
//!
//! - **It spins between fires, and that is a missing kernel primitive rather than a lazy loop.**
//!   There is no sleep, no timeout and no deadline anywhere in this kernel, so a process that wants
//!   to act at a time can only yield and re-read the counter. `Registry::next_deadline` already
//!   computes exactly what a timed wait would block until, so the fix is one line here once
//!   milestone 106's fork is decided; until then a running timetable costs a core's worth of yields.
//!   **This program is that fork's fifth consumer** (the block counts four: `net_stack`'s retransmit
//!   window, milestone 51's `thread::sleep`, `RECV`'s no-timeout limitation, and the shell's `^C`
//!   poll), and it is the first one whose *whole purpose* is to act at a time.
//!
//! - **Corpses are collected lazily, when their memory is needed.** Nothing reaps between fires,
//!   because reaping means blocking on the supervision endpoint and blocking means not watching the
//!   clock. So the failure counts this program reports lag reality until the budget runs down.
//!   A wait that returns on either a message or a deadline fixes this too, and it is the same fork.
//!
//! - **A hung scheduled child stops the whole timetable.** When the budget cannot back another
//!   instance the loop blocks on the supervision endpoint, and a livelocked child never sends a
//!   death message, so nothing arrives and nothing else fires. That is §32's watchdog case verbatim
//!   (`Endpoint::REAP` collects corpses and refuses to kill, deliberately) and it is not this
//!   program's to fix: it waits for milestone 23. What it costs here is worth stating, because it is
//!   worse than it is for a shell: at a prompt the person who typed the command is sitting there and
//!   can press `^C`, and behind a schedule there is nobody.
//!
//! - **The narrowing is to the plan, not to one image per entry.** `user/src/spawner.rs` is handed
//!   a single image and "build me program X" is not a thing that can be asked of it; this is handed
//!   an archive and can still name anything in it. The residual is therefore the *union* of the
//!   plan's programs rather than one program per entry, so a document admitting three programs
//!   leaves an instance of one able to reach the other two's images. Closing that needs a
//!   capability per entry rather than one per timetable, which is a shape this tree does not have.
//!
//! - **`--mem` entries are refused**, and the mechanism the roadmap sketched for backing one does
//!   not work. `Held::mem_pages` is zero, so an entry naming a memory grant is `Unbacked::Memory`
//!   even though this process holds a budget.
//!
//!   The sketch said to split the grant out of the instance's own region "so a single `DESTROY`
//!   still reclaims both". **The kernel refuses exactly that**: `regions::destroy_outcome` returns
//!   `Refused` for any region with a live child, Kani proves it, and `sched::reap_supervised` hands
//!   that refusal straight back, so a corpse whose region carries a split grant can never be
//!   collected until the grant is destroyed first.
//!
//!   What the nesting *does* buy is the only correlation available, which is why it is still the
//!   right shape: a refused reap names the instance that carried a grant. Nothing here can pair a
//!   death with a grant otherwise, because a builder is never told its child's tid
//!   (`supervision_proto::build_child` hands back a TCB capability, and `abi::tcb` has no method
//!   that reads one out), so the only fact a supervisor has about a death is the tid the kernel
//!   stamped on it. What remains is therefore a decision rather than wiring: **how many `--mem`
//!   instances may be outstanding at once**, since the refusal only identifies one. `crates/timetable`
//!   already plans and refuses memory grants against `Held::mem_pages`, and its host tests cover it.
//!
//! - **Nothing is persistent.** Entries live in a document compiled into this binary, and both die
//!   with the boot. Milestone 129's block records this and points at whatever milestone gives
//!   services durable configuration at all, which does not exist yet.
//!
//! - **The document is compiled in, not read from disk.** `include_str!`, exactly as
//!   `user/src/mdns_responder.rs` does and for the same reason: reading a file needs a file
//!   capability wired through the spawn. The format, the parser, the line-numbered errors and every
//!   test are unaffected by where the bytes come from.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use timetable::{Held, Registry};
use user_rt::{cap_delete, exit, monotonic_nanos, reap, recv_fault, send, yield_now};

/// The document. Compiled in; see `BUGS`.
const CONFIG: &str = include_str!("../timetable.conf");

/// The output endpoint: the plan, and the summary. `sink_proto` bytes.
const OUT: u64 = 0;
/// The budget every instance is made of, and what pays this loader's scratch mappings.
const BUDGET: u64 = 1;
/// Handed to each instance as its slot 0, so a scheduled child can report its answer.
const CHILD_REPORT: u64 = 2;
/// Placed in each instance's reserved fault slot, and what corpses are collected through.
const DEATHS: u64 = 3;

/// Where the spawn site maps the initrd archive, read-only. Must match the kernel's wiring.
///
/// The same address `user/src/builder.rs` uses, deliberately: two programs that read the archive
/// the same way should not disagree about where it is.
const INITRD_VA: u64 = 0x2000_0000;

/// Pages per instance region. Enough for a small program's segments, its stack, its address-space
/// tables and its TCB, and the same number `user/src/spawner.rs` arrived at for the same job.
///
/// A per-instance region rather than one shared pool is what makes a single `Untyped::DESTROY`
/// reclaim a whole dead child, which is the property the reap depends on.
const INSTANCE_PAGES: u64 = 48;

/// How many times to retry a reap whose region still holds something that can run.
///
/// The same safety net as `user/src/job_undertaker.rs`'s, and the same reasoning: a `NotPermitted`
/// is a fact about the region's other residents rather than about this corpse, one preemption is
/// enough, and running out is still a loud failure because a corpse that never becomes collectable
/// is a leak that ends this process's memory a few fires later with nothing to point at.
const REAP_ATTEMPTS: usize = 1024;

/// Verdict codes on [`OUT`]'s stream, so a spawn site that reads nothing else can still tell what
/// happened. The plan and the summary are text; these are the two ways the program ends.
const E_CONFIG: u64 = 0xE300; // the document did not parse; the low byte is the line number
const E_ARCHIVE: u64 = 0xE301; // the initrd archive did not parse
const E_IMAGE: u64 = 0xE302; // a program an admitted entry names is not in the archive
const E_BUDGET: u64 = 0xE303; // the budget cannot back even one instance

#[unsafe(no_mangle)]
pub extern "C" fn _start(fires_wanted: u64, initrd_len: u64, _a2: u64) -> ! {
    // SAFETY: the spawn site mapped `initrd_len` bytes of the reserved initrd region read-only at
    // INITRD_VA, which is the same contract `user/src/builder.rs` is started under.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };

    let doc = match timetable::parse(CONFIG) {
        Ok(d) => d,
        // The line number rides in the low byte, so a wrong document is findable from the verdict
        // alone even when nobody is reading the text stream.
        Err(e) => {
            say(b"timetable: the document does not parse: ");
            say(e.message().as_bytes());
            say(b"\n");
            done(E_CONFIG | (e.line() as u64 & 0xff));
        }
    };

    // **What this process holds, stated once, in the vocabulary registration checks against.**
    // Every field is false or zero and every one of them is a fact a reader can check against the
    // slot list in this module's header. Widening the timetable is an edit here and a visible
    // change in the printed plan, which is the property worth having.
    let held = Held::default();

    let mut reg = Registry::register(&doc, held);

    // **The plan, before anything fires.** This is the milestone's claim in the form a person meets
    // it: what every scheduled child will hold, or why it will never run, printed while nothing has
    // happened yet. A crontab has nothing to print here.
    timetable::write_plan(&reg, &mut say);

    let Ok(fs) = nifefs::Fs::parse(archive) else {
        done(E_ARCHIVE)
    };

    // **What the archive it was handed reaches, next to what the plan will build.** The plan above
    // says what each scheduled child holds; this says what *this* process holds, and the two are
    // different questions. A scheduler handed the whole initrd has a one-program plan and a
    // capability that reaches every program in the tree, and nothing in the plan would say so.
    //
    // It measures rather than enforces, because a process cannot narrow its own endowment: the
    // width is the spawn site's decision (`kernel/src/user/timetable_tests.rs` builds a sub-archive
    // from exactly `Registry::programs`), and saying it out loud is what makes the decision
    // checkable from in here rather than only from out there.
    let mut audit = timetable::Audit::of(&reg);
    for entry in fs.entries() {
        if let Some(name) = entry.name_str() {
            audit.saw(name);
        }
    }
    audit.write(&mut say);

    // Resolve every admitted entry's program **now**, so a plan that names a program the archive
    // does not carry fails loudly at startup rather than as a fire that quietly does not happen.
    // This is also the moment `Registry::programs`' claim becomes checkable: nothing after this
    // point looks anything else up.
    let mut images: [Option<elf::Elf<'static>>; timetable::MAX_ENTRIES] =
        [const { None }; timetable::MAX_ENTRIES];
    for (i, row) in reg.rows().iter().enumerate() {
        let Some(e) = row.endowment() else { continue };
        let Some(bytes) = fs.read(e.prog.name()) else {
            say(b"timetable: no such program in the archive: ");
            say(e.prog.name().as_bytes());
            say(b"\n");
            done(E_IMAGE)
        };
        let Ok(elf) = elf::Elf::parse(bytes) else {
            done(E_IMAGE)
        };
        images[i] = Some(elf);
    }

    // The end of the plan, and the start of the running. One line, because the plan and everything
    // after it travel down one endpoint and a reader has to know where one stops: `kernel/src/user/
    // timetable_tests.rs` reads to exactly this line, which is also how a person reading a console
    // knows nothing had fired before it.
    say(b"timetable: armed\n");

    reg.arm(monotonic_nanos());

    let mut fired = 0u64;
    let mut outstanding = 0u64;
    let mut exits = 0u64;
    let mut faults = 0u64;

    while fires_wanted == 0 || fired < fires_wanted {
        let now = monotonic_nanos();
        let mut any = false;
        while let Some(i) = reg.due(now) {
            any = true;
            // Several entries can come due in one pass, so the count has to be checked **inside**
            // the pass and not only around it. Without this a run asked for four fires can perform
            // five, and the fifth child blocks forever on a report nobody is left to take, which
            // shows up as a hang rather than as an off-by-one.
            if fires_wanted != 0 && fired >= fires_wanted {
                break;
            }
            let Some(elf) = images[i].as_ref() else {
                continue;
            };
            let Some(e) = reg.rows()[i].endowment() else {
                continue;
            };
            // Fire. If the budget cannot back another instance, block until a corpse comes back and
            // its region with it, then try once more. A second failure is a budget too small for
            // even one instance, which is a wiring error rather than congestion.
            if !fire(elf, e.arg) {
                if outstanding == 0 {
                    // Nothing is out, so there is nothing to wait for: the budget is too small for
                    // even one instance, which is a wiring error rather than congestion.
                    say(b"timetable: the budget cannot back one instance\n");
                    done(E_BUDGET)
                }
                collect(&mut exits, &mut faults);
                outstanding -= 1;
                if !fire(elf, e.arg) {
                    say(b"timetable: the budget cannot back one instance\n");
                    done(E_BUDGET)
                }
            }
            outstanding += 1;
            fired += 1;
        }
        if !any {
            // Nothing is due. There is no timed wait in this kernel, so this is a yield and not a
            // sleep; see `BUGS`.
            yield_now();
        }
    }

    // Drain: every child this timetable started is collected before it reports, so the counts it
    // prints are complete rather than whatever had happened to arrive.
    while outstanding > 0 {
        collect(&mut exits, &mut faults);
        outstanding -= 1;
    }

    say(b"timetable: ");
    say_num(fired);
    say(b" fires, ");
    say_num(exits);
    say(b" clean exits, ");
    say_num(faults);
    say(b" faults\n");
    done(0)
}

/// Build one instance in its own region and start it with `arg`.
///
/// It is endowed exactly two things and both are in the plan the timetable already printed: the
/// child report endpoint as its slot 0, and its supervision endpoint in the reserved fault slot,
/// which `START` reads and then clears so the child holds no authority on its own death channel.
///
/// Nothing is kept afterwards and there is nothing left worth keeping: the TCB capability is not the
/// thread, and since DECISIONS §32 the region capability is not the reap either. The pages come back
/// to this budget when the corpse is collected.
fn fire(elf: &elf::Elf, arg: u64) -> bool {
    let Ok(region) = supervision_proto::untyped_split(BUDGET, INSTANCE_PAGES) else {
        return false;
    };
    let Ok(tcb) = supervision_proto::build_child(
        BUDGET,
        region,
        elf,
        &supervision_proto::ChildEndowment {
            caps: &[(CHILD_REPORT, abi::rights::WRITE)],
            fault: Some(DEATHS),
            ..supervision_proto::ChildEndowment::new()
        },
    ) else {
        // The region is ours and the child does not exist, so hand the pages straight back rather
        // than leaking them into a budget that will refuse the next fire.
        supervision_proto::untyped_destroy(region);
        return false;
    };
    if !supervision_proto::tcb_start(tcb, 0, arg, 0) {
        cap_delete(tcb);
        supervision_proto::untyped_destroy(region);
        return false;
    }
    cap_delete(tcb);
    cap_delete(region);
    true
}

/// **Block until one child dies, then collect it**, counting whether it finished or crashed.
///
/// The two events are counted apart for the reason DECISIONS §26 delivers both: a crash is a fact
/// about a scheduled job worth reporting, and a clean exit is the normal end of one. A scheduler
/// that reported only a total would hide exactly the number an operator wants.
///
/// The kernel is the only sender on this endpoint (§26 clears the child's fault slot at `START`), so
/// the tid is trustworthy without a badge.
fn collect(exits: &mut u64, faults: &mut u64) {
    let (event, tid, _pc, _addr, _rsvd) = recv_fault(DEATHS);
    if event == abi::fault::EVENT_EXIT {
        *exits += 1;
    } else {
        *faults += 1;
    }
    for _ in 0..REAP_ATTEMPTS {
        if reap(DEATHS, tid) == 0 {
            return;
        }
        yield_now();
    }
    user_rt::trap()
}

/// Write bytes down the output endpoint, `sink_proto`-framed.
fn say(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        let (w0, w1, w2, n) = sink_proto::pack(rest);
        send(OUT, w0, w1, w2);
        rest = &rest[n..];
    }
}

/// A decimal number down the same stream.
fn say_num(v: u64) {
    let mut digits = [0u8; 20];
    let mut n = 0;
    let mut v = v;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    let mut out = [0u8; 20];
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    say(&out[..n]);
}

/// End the stream, report the verdict, and stop.
///
/// The `sink_proto` end-of-stream comes first so a reader draining text sees a stream that ended
/// rather than one that stopped, and the verdict word after it so a spawn site reading one word
/// still learns how this went.
fn done(code: u64) -> ! {
    send(OUT, sink_proto::eof(), 0, 0);
    send(OUT, code, 0, 0);
    exit();
}

user_rt::panic_handler!();
