#![no_std]
// `Result<_, ()>` throughout the build path, for `supervision_proto`'s reason: every failure here is
// a syscall that already returned its own error through the ABI, and a second, richer error would be
// inventing detail the kernel did not provide. See that crate's head comment.
#![allow(clippy::result_unit_err)]
//! **The interactive system, built once** (milestone 96).
//!
//! There are two inits, because the two boards' kernels hand off differently: `user::initrd()` loads
//! the archive entry `init`, which is `user/src/hello.rs`'s `init_boot` role on aarch64 and
//! `user/src/system_initializer.rs` on riscv64. There is **one** system they build, and this crate
//! is it. What each init still holds is the table of slot numbers its own kernel granted, which is a
//! fact about that boot path and nothing else; everything from parsing the archive to serving the
//! shell's last `run` is here.
//!
//! Before this crate the construction and the spawn service were written twice, about three hundred
//! near-identical lines, and the failure that caused is the reason the milestone exists: a fix that
//! lands in one init and not the other is **a boot that reaches userspace and prints nothing at
//! all**, with no fault and no message. That shape cost three separate lanes an evening each.
//! `script/shell-check` boots both ISAs and types at the prompt, which is what makes it the gate
//! that proves this: it is the only thing in the tree that runs a real init.
//!
//! # What the kernel hands over, and what this builds from it
//!
//! [`BootEndowment`] names the capabilities the kernel granted: a construction budget, the UART's registers
//! as a device capability, the UART receive interrupt, a read-only view of the wall clock, and, when
//! this boot attached a RedoxFS disk, the file service and the page its clients share with it. From
//! those, and nothing else, [`boot`] builds the whole interactive system out of its own budget:
//!
//! 1. the **console** server (output): reads text from a shared page, writes it to the UART;
//! 2. the **input** driver (keystrokes): waits on the UART receive interrupt, forwards bytes;
//! 3. the **line discipline** (`line_editor`, milestone 28): editing, echo, history, between them;
//! 4. the **shell**: prints and reads lines through the terminal endpoint, runs commands, and since
//!    milestone 86 holds a `READ` view of the wall clock so `time <command>` can measure one;
//! 5. the **terminal's sink adapter** (`terminal_sink_caretaker`, milestone 50), when the archive
//!    carries one: it holds the terminal and serves the sink contract, so a declared second stream
//!    can be pointed at the screen without handing anyone the endpoint that also reads the keyboard;
//! 6. the **undertaker** (`job_undertaker`), which collects a finished job's corpse so its region
//!    comes back to the pool.
//!
//! They are wired together with endpoints and shared pages this code creates. The kernel wires none
//! of it. Then init stays alive as the spawn service (milestone 31): the shell resolves a `run` into
//! a grant expression and directs init to load the named program and endow it with exactly what the
//! command named. Nothing here names an architecture: the console and input drivers hold the one
//! device-specific fact (the UART register layout), and the kernel grants the right device.
//!
//! # What it gives away once the system is up (milestone 22, the interactive increment)
//!
//! It used to hold the kernel's whole construction budget for life, which made every process in the
//! system one bug in init away from being built wrong. It no longer does. Once the boot servers
//! above are built it carves two bounded budgets off that root and **deletes the root**:
//!
//! - [`INIT_OWN_PAGES`] for its own scratch page tables, which is all it spends on itself; and
//! - [`JOBS_BUDGET_PAGES`] for the jobs the prompt asks for, one reclaimable region per job.
//!
//! It also gives up the UART device capability and the UART interrupt as soon as the drivers that
//! need them are built, the file service as soon as the shell holds it, and everything in
//! [`BootEndowment::unused`] with them. The proof is a negative control taken from inside the process and
//! printed at the prompt, exactly the shape `root_supervisor` uses: after the delete, `RETYPE` and
//! `RETYPE_OBJ` on that slot must answer `NoSuchSlot` (there is nothing there) rather than
//! `NotPermitted` (there is, and you may not).
//!
//! The job budget is **renewable**, which is what makes bounding it cheap. Every job is built in its
//! own region split off [`JOBS_BUDGET_PAGES`] and is born supervised: `job_undertaker`, a process
//! holding one endpoint capability and nothing else, collects each corpse through `Endpoint::REAP`
//! (DECISIONS §32) and the region's pages come back here (§13: a reclaimed region returns to its
//! owner, which is whoever split it). Before that, a spawned job's memory was spent for the life of
//! the boot.
//!
//! # What it refuses to load (milestone 104)
//!
//! The kernel measures the one program *it* loads, which is this one. Everything else in the
//! archive is loaded here, and those bytes used to be unchecked, so the chain of trust stopped at
//! init's entry. It does not now. The build packs a table of digests into the archive
//! ([`measured_boot::PROGRAM_MEASUREMENTS`]), the kernel's trust root vouches for that table exactly
//! as it vouches for this program's own bytes, and [`boot`] looks every program up in it before
//! loading it.
//!
//! **One rule: init runs nothing it cannot vouch for.** A digest that does not match is a refusal,
//! and so is a name the table does not mention, for the reason the kernel's empty trust root is
//! refused: a check that passes when there is nothing to check against is not a check.
//!
//! What a refusal *costs* is not a second policy. It falls out of what the program was for, which is
//! a question this crate already had to answer for an archive entry that is simply missing, so a
//! refused program is treated exactly as a missing one:
//!
//! - **console, input, `line_editor`, swish, `job_undertaker`.** The system is made of these, so not
//!   running one and not having a system are the same outcome. init prints which one it refused and
//!   traps, which is `kernel::trust::require`'s decision one link down.
//! - **`terminal_sink_caretaker`.** Already optional: a boot without an adapter comes up and a
//!   declared second stream finds an empty slot. A refused adapter costs that same feature.
//! - **The spawnable programs.** Left out of the table [`spawn_service`] indexes, so the prompt
//!   answers "could not spawn" for them and everything else still works. Halting a running machine
//!   because `wc` changed would turn a build defect into an unbootable system, and it buys nothing:
//!   the guarantee is that nothing unvouched-for runs, and not spawning it is that guarantee.
//!
//! Recording a mismatch and loading anyway was considered and refused. There is no audit log to put
//! a record in, and a measurement that changes nothing about what runs is theatre; a chain whose
//! second link is advisory is not a chain.
//!
//! # BUGS
//!
//! **A refused `console` or `line_editor` stops in silence.** Those two are what carry init's
//! output, so a refusal of either has no route to a person: init traps and the operator sees the
//! kernel's fault line for init and nothing else, indistinguishable from any other early init
//! failure. Everything refused after them is named on the console. There is no debug-print syscall
//! (the kernel-served `Console` object went away at milestone 8) and driving the UART from here
//! would be a second copy of the console driver, per ISA, inside the process the drivers exist to
//! keep small.
//!
//! **An absent required component still traps with no message**, unchanged from before this
//! existed. That is a build that did not pack it rather than bytes somebody swapped, and it has
//! never had one.
//!
//! **The measurement is of the archive, not of memory over time.** It is checked once, when the
//! program is loaded. Nothing re-measures a running process, and nothing measures the pages init
//! wrote into a child after `build_child` copied them.
//!
//! The return of pages is **LIFO** (§16, `crates/regions`): a job region that is not at the top of
//! the budget's watermark when it is reclaimed returns nothing, and its run is a hole until this
//! process dies, which it never does. Sequential commands at a prompt are exactly LIFO and recover
//! fully; two jobs alive at once (a pipeline stage that outlives its producer) permanently costs one
//! region. A long enough session of concurrent pipelines still ends at "could not spawn".
//!
//! The loader's scratch window is never unmapped, so init keeps a **writable mapping of every page
//! it ever laid down for a child**. Reaping a job undoes that (region reclaim revokes every mapping
//! of the pages first, §13), but the boot servers are never reclaimed, so init can still read and
//! write the console's, the line editor's, the input driver's, the shell's and the sink adapter's
//! memory. Giving the construction budget away does not reach that, and nothing in the ABI unmaps a
//! page.
//!
//! Printing the negative control costs one more of those: the shell's output frame stays mapped here
//! for life, because there is no unmap and `Frame::REVOKE` would take it from the shell too.
//!
//! **Init's cspace has sixteen slots, and running out of them prints nothing at all.** Every
//! capability held across a `build_child` is one the child's address space, frames and TCB cannot
//! have, and `build_child` answering `Err(())` is a silent halt. Two of the three evenings this file
//! has cost were that, once when the kernel grew two grants and once when a boot component was built
//! one step too early. The order below is load-bearing and the comments say where.
//!
//! Name: ratified 2026-08-04 (calef, milestone 96), and it is the ratification that raised
//! milestone 115. Refused `system_builder` (milestone 63 had already refused it, for a reason still
//! true: `builder.rs` calls itself "a minimal init: the system builder", so two programs would
//! claim one phrase) and `system_bootloader` (it claims a position in the boot sequence it does not
//! occupy, and milestone 88 will need the real one). A lane proposed `system_builder` anyway and
//! the maintainer endorsed it, because that refusal lived in one table cell inside one milestone
//! block and neither of them found it. The type this crate exports as `BootEndowment` was ratified
//! the same day, replacing `Grants`.

use grant_plan::{Prog, spawnproto};
use line_editor::proto;
// The loader, and the tree's only one since milestone 96. Named here rather than qualified at every
// call, because the point of the crate is that there is one of these.
use supervision_proto::{
    Endow, build_child, retype_frame_from as retype_frame, retype_obj_from as retype_obj, tcb_start,
};
use user_rt::{call, cap_delete, invoke, recv, recv_cap, send};

/// **The capabilities the kernel granted this init, by slot.** The one thing the two boards do not
/// agree on, so it is data each init states rather than code this crate repeats.
///
/// The two orders come from `kernel::user::spawn_init` (aarch64) and `kernel::user::riscv_shell_boot`
/// (riscv64). They differ because the aarch64 path is shared with milestone 19d's test roles, which
/// were granted a report endpoint and a test interrupt this system has no use for; see
/// [`unused`](BootEndowment::unused).
pub struct BootEndowment {
    /// The construction budget, held `WRITE | GRANT`: everything this system is made of.
    pub untyped: u64,
    /// The UART's registers, a device capability to map into the console and input drivers.
    pub uart_dev: u64,
    /// The UART receive interrupt, an `Irq` capability to delegate into the input driver.
    pub uart_irq: u64,
    /// **The wall clock** (milestone 51's wiring): a `Frame` capability with `READ` and nothing
    /// else, granted ahead of the filesystem pair so its slot is the same on every boot, whether or
    /// not a disk was attached, and granted **unconditionally**: a boot with no clock service hands
    /// us a zeroed page, which reads as `clock_proto::state::UNKNOWN` and is the honest answer for a
    /// machine that does not know the time. init hands it on only to a child whose manifest declares
    /// a clock, and hands on `READ`, so nothing spawned from this prompt can set the time
    /// (DECISIONS §43).
    ///
    /// **The shell holds a narrowed view of this same frame** (milestone 86), mapped at
    /// [`SH_CLOCK_VA`], which is what `time <command>` measures with. There is deliberately no
    /// second field for it: the shell's clock is not a separate kernel grant but this one handed on,
    /// so a field would ask each board to state the same slot number twice with nothing checking
    /// that the two agree. `READ` and no `GRANT` there too, so the shell can read the wall clock and
    /// can hand one to nothing it spawns.
    pub clock_page: u64,
    /// **The filesystem, when this boot has one** (milestone 50). The kernel wires the block server
    /// and the FS server before it starts us and grants the service endpoint plus the page its
    /// clients share with it. The rights that endpoint carries arrive in `fs_rights`, and **0 means
    /// this boot attached no RedoxFS disk**, in which case these two slots hold nothing at all.
    pub fs_ep: u64,
    /// The page the file service's clients share with it; see [`fs_ep`](BootEndowment::fs_ep).
    pub fs_page: u64,
    /// **Capabilities the kernel granted that the interactive system never uses**, deleted with the
    /// device authority once the drivers exist.
    ///
    /// Empty on riscv64. On aarch64 it is the kernel's report endpoint and the milestone-19d.2b test
    /// SGI, both of which exist because that boot path is shared with the test roles: nothing
    /// receives on the report here, and no interactive component waits on that interrupt. An init
    /// that kept them would be keeping delegable authority for no reason, which is the same kind of
    /// thing the construction budget is.
    pub unused: &'static [u64],
}

/// Where the kernel maps the initrd archive, read-only. Must match `kernel::user::INITRD_VA`.
const INITRD_VA: u64 = 0x2000_0000;

/// Stack pages every child init builds gets, mapped down from `supervision_proto::CHILD_STACK_VA`.
///
/// **Twelve since DECISIONS §67**, and every step of that number was measured rather than chosen.
/// Four overflowed at the first `ls > out.txt`; eight held until `2>` put a **second** `FileOut` on
/// `run_pipeline`'s frame, each carrying a 256-byte staging buffer by value, and the scripted wiring
/// faulted twenty-four bytes below its lowest page. Four extra rather than one, because every
/// previous instance bought exactly enough and the next change found the wall again. The cost is
/// 48 KiB of address space per child, which is nothing next to a page table.
///
/// `kernel::user::pipeline_service`'s `SHELL_EXTRA_STACK` must stay level with this: a test wiring
/// with less headroom than the boot wiring finds faults the boot does not have (notes/pipes.md).
/// The kernel cannot depend on this crate (it would drag `user_rt`'s EL0 syscall stubs in), so that
/// one is still a number in two places, and this is the one it follows.
pub const CHILD_STACK_PAGES: u64 = 12;

/// Where a child that declares a clock maps it, read-only. Must match `user/src/date.rs`'s
/// `CLOCK_VA` and `kernel/src/user/clock_service.rs`.
const CHILD_CLOCK_VA: u64 = 0x00c0_0000;

/// Where a supervised (interruptible) child maps its shared job frame (DECISIONS §24). Below the ELF
/// load address (`0x40_0000`) and the stack; must match heeder.rs / spinner.rs's `JOB_FRAME_VA`.
const CHILD_JOBFRAME_VA: u64 = 0x0030_0000;

/// Pages of untyped split off our own budget and handed the shell (milestone 31), so the shell can
/// in turn endow the programs it spawns (`run --mem N`) out of a budget that is genuinely *its own*.
/// The shell shrinks this by N pages per grant; the pages a spawned child pins are not reclaimed in
/// phase 1, so this is a session budget, not a renewable one. Must match swish.rs's
/// `SH_BUDGET_PAGES`.
const SH_BUDGET_PAGES: u64 = 128;

/// **What init keeps for itself after the boot servers are up** (milestone 22, the interactive
/// increment). It pays for one thing: the page tables reaching the loader's scratch window, which
/// are init's own mappings and must never come out of a child's region (tearing that region down
/// would free init's tables under a window it never unmaps). One L3 covers 512 scratch pages and a
/// job maps at most a couple of dozen, so this is thousands of commands' worth.
pub const INIT_OWN_PAGES: u64 = 128;

/// **One job's region**: everything a spawned program is made of, so a single reclaim frees all of
/// it. The biggest program the prompt can spawn is `date` at seven pages, plus
/// [`CHILD_STACK_PAGES`], a TCB, an address-space root, the intermediate tables for the four windows
/// a child touches, and the §13 mapping records. Forty is that with room to spare, and it is spent
/// per *live* job rather than per job ever run.
const JOB_REGION_PAGES: u64 = 40;

/// **The job pool.** Six live jobs at once, which is far more than a prompt has ever needed and is
/// deliberately small: the whole claim of this increment is that a *bounded* budget is enough once
/// the regions come back, so a budget nobody could exhaust would prove nothing. `script/shell-check`
/// runs thirteen jobs through it, so widening this silently retires that gate.
pub const JOBS_BUDGET_PAGES: u64 = JOB_REGION_PAGES * 6;

/// Where init maps the shell's output frame in **its own** address space, to print the one line it
/// ever prints (the dropped-authority negative control). Well clear of init's segments, its stack,
/// and the loader's scratch window at `0x1000_0000`.
const INIT_OUT_VA: u64 = 0x0f00_0000;

// The VAs each program hardcodes; they must match console.rs / input.rs / line_editor.rs / swish.rs.
const CON_SHARED_VA: u64 = 0x0060_0000; // console reads text here; line_editor writes it
const CON_UART_VA: u64 = 0x0070_0000; // console's UART mapping
const TERM_OUT_VA: u64 = 0x0080_0000; // line_editor reads the shell's text/prompts here
const TERM_IN_VA: u64 = 0x0090_0000; // line_editor delivers completed lines here
const IN_UART_VA: u64 = 0x00a0_0000; // input driver's UART mapping
const SH_OUT_VA: u64 = 0x00c0_0000; // the shell's view of the TERM_OUT frame (swish.rs OUT_VA)
const LINE_VA: u64 = 0x00b0_0000; // the shell's view of the TERM_IN frame
const SH_FS_VA: u64 = 0x0060_0000; // the shell's half of the FS contract (swish.rs FS_VA)

/// Where the **shell** maps its own read-only clock (milestone 86). Must match swish.rs's
/// `SH_CLOCK_VA`. A different address from [`CHILD_CLOCK_VA`], because that is where a *child* maps
/// its clock and the shell already maps the terminal's output frame there; two address spaces may
/// agree on an address, one may not.
const SH_CLOCK_VA: u64 = 0x00d0_0000;

/// **Build the interactive system and become its spawn service.** Never returns: the last thing it
/// does is park in `RECV` on the shell's spawn channel for the life of the boot.
///
/// `initrd_len` is the archive length the kernel passed at entry; `fs_rights` is the `fs_proto::dir`
/// rights the file-service endpoint carries, and 0 means this boot attached no disk.
pub fn boot(g: &BootEndowment, initrd_len: u64, fs_rights: u64) -> ! {
    // The archive the kernel mapped read-only; its length arrived at entry.
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM, read-only, at INITRD_VA. It is
    // reserved memory that outlives every process, so the borrow is honest for the whole boot.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    let Ok(fs) = crickerfs::Fs::parse(archive) else {
        fail()
    };

    // **The table init measures what it loads against** (milestone 104). The kernel vouched for this
    // entry before it started us, exactly as it vouched for our own bytes
    // (`kernel::trust::require_program_measurements`), so the table is worth what this process is
    // worth and the chain extends by induction rather than by widening the kernel.
    //
    // Bytes that are not UTF-8 become the **empty** table rather than a fault, and an empty table
    // vouches for nothing: every lookup below answers `Unmeasured`, the console is refused, and the
    // boot stops. That is the same direction to be wrong in as the kernel's empty trust root, and it
    // is the failure mode a measured boot must never get backwards.
    let table = fs
        .read(measured_boot::PROGRAM_MEASUREMENTS)
        .and_then(|b| core::str::from_utf8(b).ok())
        .unwrap_or("");

    let con_elf = measured(&fs, table, "console");
    let in_elf = measured(&fs, table, "input");
    let td_elf = measured(&fs, table, "line_editor");
    let sh_elf = measured(&fs, table, "swish");
    // **The terminal's sink adapter** (milestone 50's last remainder). Optional on purpose: an
    // initrd built without it still boots, and a program that declares a second stream then finds
    // an empty slot and says what it has to say in-band. A missing component should cost a feature,
    // not a prompt. An adapter the table refuses costs exactly the same feature, which is the whole
    // policy in one line: init treats what it cannot vouch for as what is not there.
    let sink_elf = measured(&fs, table, "terminal_sink_caretaker").elf;
    // The undertaker (milestone 22, the interactive increment). Read here with the rest, because
    // the archive is only readable while we hold it and every failure below is one `fail`. Required
    // rather than optional, unlike the adapter above: without it a bounded job pool fills and the
    // prompt stops spawning, which is a broken system and not a missing feature.
    let reaper_elf = measured(&fs, table, "job_undertaker");

    // **The programs the shell can spawn** (milestone 31), measured and parsed here rather than
    // after the giveaway: the announcement further down is the only thing init ever says, so the
    // verdicts have to exist before it. One `Option<elf::Elf>` is five words, so moving seven of
    // them up the frame costs a third of a kilobyte and buys a person being told at boot instead of
    // at the prompt.
    //
    // The refusals are collected in the same pass, because a second pass would hash every program a
    // second time. Only entries the archive **has** count as refusals: `rm` is deliberately not
    // loadable here, and a program this initrd never packed was never spawnable, so neither is news.
    let mut progs: [Option<elf::Elf>; grant_plan::PROG_COUNT] = core::array::from_fn(|_| None);
    let mut refused = [""; grant_plan::PROG_COUNT];
    let mut refused_n = 0usize;
    for (id, slot) in progs.iter_mut().enumerate() {
        let Some(name) = Prog::from_id(id as u64).and_then(archive_name) else {
            continue;
        };
        let found = measured(&fs, table, name);
        if found.unvouched {
            refused[refused_n] = name;
            refused_n += 1;
        }
        *slot = found.elf;
    }

    let ut = g.untyped;

    // **The two components that have to exist before init can say anything.** The console writes
    // the UART and the line discipline is its only client, so a refusal of either has no route to a
    // person and this is the one case that stops in silence (see this module's BUGS). Everything
    // else is checked below, after they are running.
    let (Some(con_elf), Some(td_elf)) = (con_elf.elf, td_elf.elf) else {
        fail()
    };

    // The endpoints and shared pages we own and hand out, each retyped with full rights so we can
    // delegate narrowed views. `term_ep` is the terminal contract's one endpoint: the discipline
    // serves it; the input driver and the shell only hold WRITE on it, and neither can tell what
    // is on the other side (notes/terminal-contract.md).
    let request = must(retype_obj(ut, abi::objtype::ENDPOINT));
    let reply = must(retype_obj(ut, abi::objtype::ENDPOINT));
    let term_ep = must(retype_obj(ut, abi::objtype::ENDPOINT));
    let con_shared = must(retype_frame(ut)); // line_editor -> console text
    let term_out = must(retype_frame(ut)); // shell -> line_editor text and prompts
    let term_in = must(retype_frame(ut)); // line_editor -> shell completed lines

    // 1. Console server: reads text from the shared page, writes it to the UART.
    let con = must(build_child(
        ut,
        ut,
        &con_elf,
        &Endow {
            caps: &[(request, abi::rights::READ), (reply, abi::rights::WRITE)],
            maps: &[
                (CON_SHARED_VA, con_shared, abi::aspace::MAP_RO),
                (CON_UART_VA, g.uart_dev, abi::aspace::MAP_RO), // mode ignored for a DeviceFrame
            ],
            stack_pages: CHILD_STACK_PAGES,
            ..Endow::new()
        },
    ));
    must_ok(tcb_start(con, 0, 0, 0));
    cap_delete(con);

    // 2. The line discipline: serves the terminal endpoint, prints through the console. It is
    // the console's only client; everyone else prints through it.
    let line_editor = must(build_child(
        ut,
        ut,
        &td_elf,
        &Endow {
            caps: &[
                (term_ep, abi::rights::READ),
                (request, abi::rights::WRITE),
                (reply, abi::rights::READ),
            ],
            maps: &[
                (CON_SHARED_VA, con_shared, abi::aspace::MAP_RW), // it fills what the console reads
                (TERM_OUT_VA, term_out, abi::aspace::MAP_RO),
                (TERM_IN_VA, term_in, abi::aspace::MAP_RW),
            ],
            stack_pages: CHILD_STACK_PAGES,
            ..Endow::new()
        },
    ));
    must_ok(tcb_start(line_editor, 0, 0, 0));
    cap_delete(line_editor);

    // **Refuse the system if a required component is not the one that was measured** (milestone
    // 104), here, because this is the earliest point at which init can be read by a person and the
    // latest at which nothing unmeasured has been built. The console and the line discipline above
    // are running; nothing else is.
    //
    // Halting is not a second policy. The policy is that init runs nothing it cannot vouch for, and
    // for a component the whole system is made of, not running it and not having a system are the
    // same outcome. What it costs is decided by what the program was for, which is a question init
    // already had to answer for an archive entry that is simply missing.
    let unvouched: [&str; 3] = [
        if in_elf.unvouched { "input" } else { "" },
        if sh_elf.unvouched { "swish" } else { "" },
        if reaper_elf.unvouched {
            "job_undertaker"
        } else {
            ""
        },
    ];
    if unvouched.iter().any(|n| !n.is_empty()) {
        // The shell's output page, in our own space, so the refusal can be read. The giveaway below
        // maps it for the same reason and this path never reaches the giveaway; a refusal nobody
        // can see is most of what this milestone was written to fix.
        // SAFETY: `invoke` traps to the kernel, which validates the capability and the method before
        // acting (user_rt's contract).
        if unsafe { invoke(term_out, abi::frame::MAP, INIT_OUT_VA, 1, ut) } == 0 {
            let mut buf = [0u8; SENTENCE];
            announce(
                term_ep,
                sentence(
                    &mut buf,
                    b"init: cannot vouch for",
                    &unvouched,
                    b"; halting rather than building an unmeasured system\n",
                ),
            );
        }
        fail()
    }
    // Past the refusal, so an absent component still traps the way it always has: a build that did
    // not pack the console's client is a broken build, not an attack, and it has never had a
    // message.
    let (Some(in_elf), Some(sh_elf), Some(reaper_elf)) = (in_elf.elf, sh_elf.elf, reaper_elf.elf)
    else {
        fail()
    };

    // 3. Input driver: waits on the UART receive interrupt, forwards raw bytes to the terminal.
    let input = must(build_child(
        ut,
        ut,
        &in_elf,
        &Endow {
            caps: &[
                (term_ep, abi::rights::WRITE),
                (g.uart_irq, abi::rights::READ),
            ],
            maps: &[(IN_UART_VA, g.uart_dev, abi::aspace::MAP_RO)],
            stack_pages: CHILD_STACK_PAGES,
            ..Endow::new()
        },
    ));
    must_ok(tcb_start(input, 0, 0, 0));
    cap_delete(input);

    // **The console's three capabilities go back now, before the shell is built**, and that is not
    // tidiness: this cspace has sixteen slots, and milestone 50 added two more kernel grants (the
    // file service and its page). With them held, the shell's `build_child` had no slot left to
    // retype an address space into and failed silently, which presented as a boot that brought up
    // the console and then printed nothing. Nothing below needs these: line_editor is the console's
    // only client and it already holds its narrowed copies.
    //
    // **The device and the interrupt go with them** (milestone 22, the interactive increment). Both
    // drivers that need them exist and hold their own narrowed copies, and nothing below builds
    // another driver, so an init that kept them would be keeping the authority to hand the UART to
    // anything it later builds. Dropping them here is the same act as dropping the construction
    // budget further down, one boot stage earlier. `unused` is whatever else this board's kernel
    // granted that the interactive system never had a use for.
    for c in [request, reply, con_shared, g.uart_dev, g.uart_irq] {
        cap_delete(c);
    }
    for &c in g.unused {
        cap_delete(c);
    }

    // **The spawn channel is retyped here, not with the rest**, and the reason is the same sixteen
    // slots: holding two more endpoints through the three builds above is what pushed this cspace
    // over. They are the shell's and the service's, so this is also where they belong.
    let spawn_ep = must(retype_obj(ut, abi::objtype::ENDPOINT));
    let result_ep = must(retype_obj(ut, abi::objtype::ENDPOINT));
    // **The supervision endpoint every job is born holding** (milestone 22, the interactive
    // increment; DECISIONS §26's spawn-slot convention). We keep it for its `GRANT`, which is all we
    // need it for: to place a `READ` view of it in each job's reserved fault slot. We never receive
    // on it. `job_undertaker` does, and collecting is the only thing that endpoint authorizes.
    let deaths = must(retype_obj(ut, abi::objtype::ENDPOINT));

    // 4. The shell: prints and reads lines through the terminal, holds the spawn channel, and holds
    // its own untyped budget (slot 3) so `run --mem N` grants from memory that is genuinely the
    // shell's. WRITE lets it SPLIT the budget; GRANT lets it delegate the split to init. We carve
    // that budget from our own untyped and hand it over the same way we hand any capability.
    //
    // Slot 4 is the filesystem when this boot has one, which is the whole of what `>` and `<` need
    // (milestone 50, notes/pipes.md): the shell resolves a redirection against it and writes the
    // file itself. Narrowed to WRITE, which on an endpoint is the right to CALL, and without GRANT,
    // so the shell can hand it to nobody.
    //
    // **And a read-only clock last** (milestone 86), which is what `time <command>` measures with.
    // It is [`BootEndowment::clock_page`], the same frame this init was granted and hands to a child
    // whose manifest declares a clock; the shell is simply another holder of a narrowed view. `READ`
    // and no `GRANT`, deliberately: the shell can read the wall clock and can hand one to nothing it
    // spawns, so which processes can read the time is still decided by the manifests this crate
    // reads (DECISIONS §43) rather than by anything typed at a prompt.
    //
    // Last in the list, so a boot with no filesystem takes exactly the path it took before this
    // existed. Its slot therefore moves (4 without a disk, 5 with one), which is why the shell is
    // *told* the number in `x2`/`a2` instead of assuming one; see swish.rs's `CLOCK_SLOT`.
    let sh_budget = must(untyped_split(ut, SH_BUDGET_PAGES));
    let with_fs = fs_rights != 0;
    let sh_caps: [(u64, u64); 6] = [
        (term_ep, abi::rights::WRITE),
        (spawn_ep, abi::rights::WRITE),
        (result_ep, abi::rights::READ),
        (sh_budget, abi::rights::WRITE | abi::rights::GRANT),
        (g.fs_ep, abi::rights::WRITE),
        (g.clock_page, abi::rights::READ),
    ];
    let sh_maps: [(u64, u64, u64); 4] = [
        (SH_OUT_VA, term_out, abi::aspace::MAP_RW), // shell writes text and prompts here
        (LINE_VA, term_in, abi::aspace::MAP_RO),    // shell reads completed lines
        (SH_FS_VA, g.fs_page, abi::aspace::MAP_RW), // and its half of the FS contract
        (SH_CLOCK_VA, g.clock_page, abi::aspace::MAP_RO), // and the clock it times with
    ];
    // A boot with no disk gets the same lists with the FS pair taken out of the middle, which is a
    // second pair of arrays rather than a slice: the clock is granted either way, and "the last
    // entry stays" is not something a range can say.
    let no_fs_caps: [(u64, u64); 5] = [sh_caps[0], sh_caps[1], sh_caps[2], sh_caps[3], sh_caps[5]];
    let no_fs_maps: [(u64, u64, u64); 3] = [sh_maps[0], sh_maps[1], sh_maps[3]];
    // Which slot the clock landed in, for the shell's `x2`. It is the count of what went before it,
    // which is the same arithmetic `build_child` does when it fills the cspace from zero.
    let sh_clock_slot: u64 = if with_fs { 5 } else { 4 };
    // **Built but not started**, because the drop below has to happen while the shell's output page
    // is still ours alone to write: the negative control is printed through it, and a running shell
    // would be printing its banner into the same page.
    let shell = must(build_child(
        ut,
        ut,
        &sh_elf,
        &Endow {
            caps: if with_fs { &sh_caps } else { &no_fs_caps },
            maps: if with_fs { &sh_maps } else { &no_fs_maps },
            stack_pages: CHILD_STACK_PAGES,
            ..Endow::new()
        },
    ));
    cap_delete(sh_budget); // our copy; the shell holds its own now

    // Free every boot cap the spawn service does not need, so init's 16-slot cspace has room to
    // build a supervised child (which holds a job untyped and a job frame while the loader retypes
    // an aspace, frames, and a TCB). The drivers and the shell hold the narrowed copies that matter.
    //
    // **Only the input frame, and the two that stay have a reason each.** `term_ep` is still ours to
    // delegate: the sink adapter below is handed `WRITE` on it, and the drop announcement is a
    // `CALL` on it. `term_out` is where that announcement stages its bytes. Both go back the moment
    // their last use is done, and after that this process holds no way to reach the terminal at all.
    cap_delete(term_in);
    // The filesystem too. init is the ELF loader, not an FS client: the shell holds the narrowed
    // copies and this process never speaks `fs_proto`. The day `rm` is reachable from the prompt,
    // init keeps the endpoint instead, because building a `fs_subtree_caretaker` is its job and not
    // the shell's.
    if with_fs {
        cap_delete(g.fs_ep);
        cap_delete(g.fs_page);
    }

    // 5. **The terminal's sink adapter** (milestone 50's last remainder, notes/sink-protocol.md,
    // DECISIONS §67). It holds the terminal `WRITE` and serves the sink contract on an endpoint of
    // its own, so a child can be handed "the terminal" as a place to put bytes **without** being
    // handed the terminal endpoint, which also carries `OP_READLINE` and would be the keyboard.
    //
    // **After the shell and before the giveaway, and both halves of that are load-bearing.**
    //
    // After the shell, because of this cspace's sixteen slots: building the adapter earlier put init
    // one slot over while the loader was retyping the shell's address space, and the symptom was
    // the one this system has already seen, a boot that reaches userspace and then prints nothing at
    // all. That constraint is about the shell's build, not about being the last thing built, and
    // milestone 22 is what made the difference visible: the adapter is now the fifth of six boot
    // components rather than the last of five, and the cspace has room either way.
    //
    // Before the giveaway, because this is a **system** component and the root untyped is what the
    // system is built from. Everything below hands that budget away and proves it is gone, so an
    // adapter built after it would have to come out of [`INIT_OWN_PAGES`], the scratch budget sized
    // for page tables and nothing else. Spending a whole program out of that pool would be invisible
    // here and would surface as some later child failing to map a scratch page.
    let term_sink = must(retype_obj(ut, abi::objtype::ENDPOINT));
    if let Some(elf) = sink_elf.as_ref() {
        let adapter = must(build_child(
            ut,
            ut,
            elf,
            &Endow {
                caps: &[
                    (term_sink, abi::rights::READ),
                    (term_ep, abi::rights::WRITE),
                ],
                stack_pages: CHILD_STACK_PAGES,
                ..Endow::new()
            },
        ));
        // Started here even though the shell deliberately is not: the adapter owns no page and
        // prints only what a client sends it, and it has no clients until the spawn service below
        // hands one its endpoint. It cannot write into the page the announcement below stages in.
        must_ok(tcb_start(adapter, 0, 0, 0));
        cap_delete(adapter);
    }

    // **Give the construction budget away** (milestone 22, the interactive increment). Two bounded
    // carves and then the root itself: after this line init can spend at most `INIT_OWN_PAGES` on
    // itself and `JOBS_BUDGET_PAGES` on the prompt's jobs, and it can no longer reach the rest of the
    // memory the kernel handed it or delegate the root to anything it builds.
    //
    // Two budgets rather than one, and the split is load-bearing rather than tidy: the job pool's
    // watermark must move for **jobs only**, or the LIFO return-of-pages (§16) never fires. A scratch
    // page table carved out of the same region between a job's split and its reap would sit above
    // that job's run, so the reclaim would find it is not the top and give back nothing.
    let own_ut = must(untyped_split(ut, INIT_OWN_PAGES));
    let jobs_ut = must(untyped_split(ut, JOBS_BUDGET_PAGES));
    // The shell's output page, in our own space, so we can say what just happened. This mapping is
    // permanent (there is no unmap, and `Frame::REVOKE` would take the page from the shell too); see
    // this module's BUGS.
    // SAFETY: `invoke` traps to the kernel, which validates the capability and the method before
    // acting (user_rt's contract).
    if unsafe { invoke(term_out, abi::frame::MAP, INIT_OUT_VA, 1, ut) } != 0 {
        fail()
    }
    cap_delete(ut);

    // And prove it from the inside, on the two primitives that build things, before anything else
    // runs. `NoSuchSlot` (-1) rather than `NotPermitted` (-3) is the whole claim: the capability is
    // *gone*, not narrowed, so there is nothing there to name. This is `root_supervisor`'s proof at
    // the interactive prompt, and `script/shell-check` reads the sentence.
    // SAFETY: as above: the kernel validates the capability and the method.
    let frame = unsafe { invoke(ut, abi::untyped::RETYPE, 0, 0, 0) };
    // SAFETY: as above: the kernel validates the capability and the method.
    let object = unsafe { invoke(ut, abi::untyped::RETYPE_OBJ, abi::objtype::TCB, 0, 0) };
    announce(
        term_ep,
        if frame == -1 && object == -1 {
            b"init: construction budget dropped; retype answers NoSuchSlot\n"
        } else {
            b"init: construction budget NOT dropped; it can still build\n"
        },
    );
    // **And what the measurement decided** (milestone 104), on the same terms as the line above: a
    // claim about what init refuses is worth what the check behind it is worth, and only init can
    // run that check. The affirmative line is the load-bearing one. A measured boot's natural bug is
    // for the check to evaporate when the build step does not run, and a boot that says nothing
    // looks exactly like a boot that measured everything, so `script/shell-check` reads this
    // sentence and a boot that stopped measuring fails the gate instead of passing quietly.
    let mut buf = [0u8; SENTENCE];
    announce(
        term_ep,
        if refused_n == 0 {
            b"init: every program measured against the archive table\n"
        } else {
            sentence(
                &mut buf,
                b"init: measurement refused",
                &refused[..refused_n],
                b"; they cannot be spawned\n",
            )
        },
    );
    cap_delete(term_ep);
    cap_delete(term_out);

    // 6. The undertaker, out of what is left of our own budget. One capability, `READ` on the
    // supervision endpoint, and nothing else: it can free a job's memory and can never spend it.
    let reaper = must(build_child(
        own_ut,
        own_ut,
        &reaper_elf,
        &Endow {
            caps: &[(deaths, abi::rights::READ)],
            stack_pages: CHILD_STACK_PAGES,
            ..Endow::new()
        },
    ));
    must_ok(tcb_start(reaper, 0, 0, 0));
    cap_delete(reaper);

    // Role 0 (the prompt), and `arg1` is the rights its directory capability carries. A shell told 0
    // holds no directory and says so at every verb that would need one. `arg2` is the clock slot
    // (milestone 86), which moves with whether this boot had a disk, so it is told rather than
    // assumed.
    must_ok(tcb_start(shell, 0, fs_rights, sh_clock_slot));
    cap_delete(shell);

    spawn_service(
        Channels {
            spawn_ep,
            result_ep,
            deaths,
            own_ut,
            jobs_ut,
            clock_page: g.clock_page,
            // The terminal's sink, if this initrd carried an adapter to serve it. This is what a
            // declared second stream gets by default (DECISIONS §67): the shell names a file with
            // `2>` and otherwise the bytes go straight to the screen, through a process that can do
            // nothing else with them.
            term_sink: sink_elf.is_some().then_some(term_sink),
        },
        &progs,
    )
}

/// The archive entry a spawnable program is loaded from, or `None` for one the prompt cannot reach.
///
/// `rm` (milestone 47) is packed in the archive and is deliberately not loadable here: it is endowed
/// a **directory** capability, which means a `fs_subtree_caretaker` this init would have to build per
/// invocation out of the FS endpoint it deletes during the boot. Until it does, the shell refuses the
/// command before it reaches the spawn service, which is why an empty slot is honest rather than a
/// hole: spawning `rm` with nothing to remove from would be the worst failure this model has, a
/// program told to destroy something, holding nothing, saying nothing.
fn archive_name(p: Prog) -> Option<&'static str> {
    match p {
        Prog::Rm => None,
        other => Some(other.name()),
    }
}

/// Turn a `recv_cap` slot into `Some(slot)`, or `None` if the message carried no capability.
fn opt_cap(slot: u64) -> Option<u64> {
    if slot == abi::endpoint::NO_CAP {
        None
    } else {
        Some(slot)
    }
}

/// Everything the spawn service holds for its whole life, so the loop's signature says what init's
/// remaining authority *is*: two channels, one supervision endpoint it only ever delegates from, and
/// two bounded budgets. The root construction budget is deliberately not in here; it is gone.
struct Channels {
    /// READ: the shell's `run` requests arrive here.
    spawn_ep: u64,
    /// WRITE: a child's answer channel, and our own spawn-failed sentinel.
    result_ep: u64,
    /// GRANT: placed `READ` in every job's reserved fault slot, so `job_undertaker` collects it.
    deaths: u64,
    /// Our own scratch budget: page tables for the loader's scratch window, and nothing else.
    own_ut: u64,
    /// The job pool. One region per job, split off here and returned here when the job is reaped.
    jobs_ut: u64,
    /// READ on the wall clock, endowed to a child whose manifest declares one (DECISIONS §43).
    clock_page: u64,
    /// WRITE-delegable: the endpoint the terminal's sink adapter serves, which is where a declared
    /// second stream goes when the command line named no file for it (DECISIONS §67). `None` when
    /// this initrd carried no adapter, and then a declaring child simply gets no second stream.
    /// This is authority to *print*, and nothing else: the adapter holds the terminal, we do not.
    term_sink: Option<u64>,
}

/// The spawn service loop: serve the shell's `run` requests forever. Init is the ELF loader the
/// shell directs; it inserts only what the shell endows, so a spawned program can reach nothing the
/// command line did not name.
///
/// Two shapes (`grant_plan::spawnproto`). A **normal** job: the shell sends the request and, if
/// `--mem` rode along, one delegated untyped; we split a region off the job pool, build the child in
/// it, endow it the result endpoint (and the budget) plus its supervision endpoint, and start it. A
/// **supervised** (interruptible) job: the shell leads the delegation with a job untyped and a shared
/// job frame; we build the whole child *from that untyped* (so the shell's region owns it and can
/// `DESTROY` it to tear it down, DECISIONS §24), map the job frame in, endow nothing else, start it,
/// and send `SPAWN_OK` once as the shell's go-ahead. The `progs` array is indexed by [`Prog::id`], so
/// it is [`grant_plan::PROG_COUNT`] long: a variant added to `grant_plan` without a slot here would
/// be an out-of-bounds read in init.
///
/// **Only the normal shape is supervised**, and that is not an oversight. An interruptible job's
/// region belongs to the shell, which tears it down itself on the second `^C` (§24's
/// forcible tier) and after a clean finish; endowing it a supervision endpoint here would put a
/// second party in the teardown path for memory that is not ours, racing the shell's `DESTROY`.
fn spawn_service(c: Channels, progs: &[Option<elf::Elf>; grant_plan::PROG_COUNT]) -> ! {
    let Channels {
        spawn_ep,
        result_ep,
        deaths,
        own_ut,
        jobs_ut,
        clock_page,
        term_sink,
    } = c;
    loop {
        let (w0, w1, w2) = recv(spawn_ep);
        let prog = Prog::from_id(spawnproto::prog_id(w0));
        let arg = spawnproto::arg(w1);
        let mem_pages = spawnproto::mem_pages(w2);
        let wiring = spawnproto::wiring(w2);
        let interruptible = wiring.interruptible;

        // Receive the delegated caps in protocol order: the interrupt pair first (job untyped, job
        // frame), then the sink, then the source, then the diagnostics, then any --mem untyped. No
        // promise, no receive, so both sides stay in lockstep.
        let (job_ut, job_fr) = if interruptible {
            (opt_cap(recv_cap(spawn_ep).1), opt_cap(recv_cap(spawn_ep).1))
        } else {
            (None, None)
        };
        let sink = if wiring.sink {
            opt_cap(recv_cap(spawn_ep).1)
        } else {
            None
        };
        let source = if wiring.source {
            opt_cap(recv_cap(spawn_ep).1)
        } else {
            None
        };
        let diagnostics = if wiring.diagnostics {
            opt_cap(recv_cap(spawn_ep).1)
        } else {
            None
        };
        let budget = if mem_pages > 0 {
            opt_cap(recv_cap(spawn_ep).1)
        } else {
            None
        };

        let elf = prog.and_then(|p| progs[p.id() as usize].as_ref());
        // Read from the program's own declaration, not from the request: a clock is not something
        // the command line can designate, so there is no bit on the wire for it (`Manifest::clock`).
        let wants_clock = prog.is_some_and(|p| p.manifest().clock);

        if interruptible {
            // Build the whole child from the shell's job untyped, mapping the shared job frame; no
            // capabilities in its cspace (it reports through the frame and exits). SPAWN_OK is the
            // go-ahead the shell waits for before it starts watching the frame.
            let built = match (elf, job_ut, job_fr) {
                (Some(e), Some(job), Some(fr)) => build_child(
                    own_ut,
                    job,
                    e,
                    &Endow {
                        maps: &[(CHILD_JOBFRAME_VA, fr, abi::aspace::MAP_RW)],
                        stack_pages: CHILD_STACK_PAGES,
                        ..Endow::new()
                    },
                )
                .ok(),
                _ => None,
            };
            match built {
                Some(tcb) => {
                    let ok = tcb_start(tcb, 0, arg, 0);
                    send(
                        result_ep,
                        if ok {
                            spawnproto::SPAWN_OK
                        } else {
                            spawnproto::SPAWN_FAILED
                        },
                        0,
                        0,
                    );
                    cap_delete(tcb);
                }
                None => {
                    send(result_ep, spawnproto::SPAWN_FAILED, 0, 0);
                }
            }
        } else {
            // **Slot 0 is the output**, and milestone 50 is the whole of what changed here: it is
            // the shared result endpoint unless the shell delegated a sink, in which case the sink
            // goes there instead and the child never learns that anything is different. `>` and the
            // left of a `|` are this line.
            //
            // Slot 1 is the input source when there is one, and otherwise the `--mem` untyped, which
            // is safe only because no manifest declares both today. `grant_plan` is where that stops
            // being true, and the order here is the contract; see notes/pipes.md's BUGS.
            let out = (sink.unwrap_or(result_ep), abi::rights::WRITE);
            let mut caps = [out; 4];
            let mut n = 1usize;
            // **The clock, which nothing on the command line asked for** (milestone 51's wiring).
            // It comes from the manifest rather than from the request, because a person does not
            // designate a clock: `date` declares that it reads one, and init is the only process
            // here holding a page it could hand over. Before the source and the budget, so `date`'s
            // clock is slot 1, which is unambiguous only because no manifest declares a clock *and*
            // an input. That is the same ordered-slot debt notes/pipes.md's BUGS already records.
            if wants_clock {
                caps[n] = (clock_page, abi::rights::READ);
                n += 1;
            }
            if let Some(src) = source {
                // READ only. A pipe's reader must not be able to write back up its own input, which
                // would make a pipeline a two-way channel nobody asked for.
                caps[n] = (src, abi::rights::READ);
                n += 1;
            }
            if let Some(b) = budget {
                // Narrowed to WRITE: the child may spend it, not lend it.
                caps[n] = (b, abi::rights::WRITE);
                n += 1;
            }
            // **The declared second stream, at the slot the manifest names** (DECISIONS §67). Read
            // from the program's own declaration for the same reason the clock is: the shell knows
            // there is one (it minted the endpoint) and only the program knows where it goes. The
            // slot is high and explicit rather than next-in-line, because how many low slots this
            // child gets depends on what else the line granted, and a stream the program probes for
            // by number cannot move under it.
            let diag_slot = prog.and_then(|p| p.manifest().output.diagnostics_slot());
            // **Where the second stream goes when the line did not say.** The shell delegates an
            // endpoint only for a `2>`, because that is the case it has to back a file for. With no
            // operator on the line the destination is the **terminal's own sink**, which is init's
            // to endow exactly as the clock is: the shell holds nothing it could hand over, and a
            // person does not designate a screen.
            //
            // That is also what keeps a redirected `date`'s complaint off the redirection. The
            // shell drains the output into the file and never sees these bytes at all.
            let default_diag = term_sink.filter(|_| diag_slot.is_some());
            // Either half missing means no second stream reaches the child, and it then says what it
            // has to say in-band, which is what every program did before §67.
            let placed_buf = match (diagnostics.or(default_diag), diag_slot) {
                (Some(ep), Some(slot)) => Some([(slot, ep, abi::rights::WRITE)]),
                _ => None,
            };
            let placed: &[(u64, u64, u64)] = match &placed_buf {
                Some(p) => p,
                None => &[],
            };
            let clock_map = [(CHILD_CLOCK_VA, clock_page, abi::aspace::MAP_RO)];
            let maps: &[(u64, u64, u64)] = if wants_clock { &clock_map } else { &[] };
            // **A region of its own, so the job's memory can come home** (milestone 22, the
            // interactive increment). Everything the child is made of comes out of this carve, and a
            // single reclaim frees all of it; the alternative, building straight out of the pool,
            // spends those pages for the life of the boot because a watermark only moves forward.
            // An exhausted pool is the ordinary `SPAWN_FAILED` the shell already reports as "init is
            // out of memory", which is the honest sentence for a bounded budget with jobs still in
            // it. The clock frame is ours and is only *mapped* into the child, so it is untouched
            // when the region goes.
            let region = untyped_split(jobs_ut, JOB_REGION_PAGES).ok();
            let built = match (elf, region) {
                (Some(e), Some(r)) => {
                    // Born supervised: `deaths` goes in the reserved fault slot, where `START` reads
                    // it and clears it, so the job cannot forge messages on its own death channel.
                    // The declared second stream rides the same named-slot mechanism at the low slot
                    // its manifest picked; the two cannot collide, because the fault slot is last.
                    build_child(
                        own_ut,
                        r,
                        e,
                        &Endow {
                            caps: &caps[..n],
                            placed,
                            maps,
                            fault: Some(deaths),
                            stack_pages: CHILD_STACK_PAGES,
                            ..Endow::new()
                        },
                    )
                    .ok()
                }
                _ => None,
            };
            let ok = match built {
                Some(tcb) => {
                    let started = tcb_start(tcb, 0, arg, 0);
                    cap_delete(tcb);
                    started
                }
                None => false,
            };
            // Our capability to the job's region goes back now. It was only ever the means of
            // building: since §32 the reap is a method on the supervision endpoint, so nothing in
            // this system holds a capability to a *live* job's memory. A build or a start that
            // failed leaves nothing running in the region, so we reclaim it here rather than wait
            // for a death that will never come.
            if let Some(r) = region {
                if !ok {
                    supervision_proto::untyped_destroy(r);
                }
                cap_delete(r);
            }
            // **A redirected child owes the shell no answer**, because its answer is going
            // somewhere else, so the shell has nothing to read and no way to find out that the
            // spawn failed. One ack closes that hole. An unredirected child is unchanged: the
            // child's own message is the shell's single read, and a failure is the sentinel.
            if wiring.sink {
                send(
                    result_ep,
                    if ok {
                        spawnproto::SPAWN_OK
                    } else {
                        spawnproto::SPAWN_FAILED
                    },
                    0,
                    0,
                );
            } else if !ok {
                send(result_ep, spawnproto::SPAWN_FAILED, 0, 0);
            }
            // **A child that was never built cannot end its own second stream**, and the shell
            // drains that stream to `OP_EOF` before it reads anything else, so nothing would ever
            // come back. init closes it on the child's behalf. It is the same hole `SPAWN_OK`
            // closed for the output side, one stream over.
            if !ok && let Some(ep) = diagnostics {
                send(ep, sink_proto::eof(), 0, 0);
            }
        }

        // Drop our copies of every delegated cap: the child holds what it needs (the job frame is
        // mapped, the budget and the streams inserted), and the shell holds the originals it kept
        // (the job untyped for teardown, the pipe it minted). This keeps init's 16-slot cspace from
        // filling across a long session.
        for s in [job_ut, job_fr, sink, source, diagnostics, budget]
            .into_iter()
            .flatten()
        {
            cap_delete(s);
        }
    }
}

// -------------------------------------------------------------------------------------------
// The thin shapes over the ABI. The loader itself is `supervision_proto`'s, which is the tree's
// only one since milestone 96.
// -------------------------------------------------------------------------------------------

/// Carve `pages` off `ut` into a new child untyped we can delegate (milestone 31). The SPLIT grants
/// full rights on the child, including GRANT, so a memory budget can be handed on. The error code
/// `supervision_proto` returns is for the dropped-authority proof, which this crate takes from the
/// raw `invoke` instead, so it is dropped here.
fn untyped_split(ut: u64, pages: u64) -> Result<u64, ()> {
    supervision_proto::untyped_split(ut, pages).map_err(|_| ())
}

/// **Say one sentence at the terminal**, through the line discipline, the way the shell does: stage
/// the bytes in the shell's output page (mapped here at [`INIT_OUT_VA`]) and `CALL` `OP_WRITE`.
///
/// The only thing this process ever prints, and it is called before the shell is started so nothing
/// else is writing that page. It exists for the negative control: a claim about what init can no
/// longer do is worth only as much as the check behind it, and only the holder can run that check.
fn announce(term_ep: u64, text: &[u8]) {
    let out = INIT_OUT_VA as *mut u8;
    for (i, &b) in text.iter().enumerate() {
        // SAFETY: the shell's output frame is mapped read/write here, and one line is far under a
        // page.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
    call(term_ep, proto::req(proto::OP_WRITE, text.len() as u64), 0);
}

// -------------------------------------------------------------------------------------------
// The measurement (milestone 104): init measures what init loads.
// -------------------------------------------------------------------------------------------

/// What init found when it asked the archive for a program.
///
/// One rule produces both fields: **init loads nothing it cannot vouch for.** So `elf` is `None`
/// whenever the entry is missing, refused, or not an ELF, and every caller below treats those the
/// same way it always treated a missing entry. `unvouched` exists only for the sentence init prints:
/// a build that did not pack a program and a program whose bytes are not the ones this system was
/// measured against are the same decision and very different news.
struct Lookup<'a> {
    elf: Option<elf::Elf<'a>>,
    /// The archive **had** this entry and the table would not vouch for it.
    unvouched: bool,
}

/// Read a program out of the archive and measure it against the table, or refuse it.
///
/// `Unmeasured` (the table says nothing about this name) and `Mismatch` (it says something else) are
/// both refusals here, which is `measured_boot`'s own rule and the kernel's: a check that passes
/// when there is nothing to check against is not a check. In particular a table that failed to
/// generate refuses **everything**, and the boot stops at the console rather than coming up
/// unmeasured.
fn measured<'a>(fs: &crickerfs::Fs<'a>, table: &str, name: &str) -> Lookup<'a> {
    let Some(bytes) = fs.read(name) else {
        return Lookup {
            elf: None,
            unvouched: false,
        };
    };
    if measured_boot::verify_in_manifest(table, name, bytes).is_err() {
        return Lookup {
            elf: None,
            unvouched: true,
        };
    }
    Lookup {
        elf: elf::Elf::parse(bytes).ok(),
        unvouched: false,
    }
}

/// The buffer one printed sentence is composed in. A page would fit, but the sentences are one line
/// each and a fixed frame-local buffer is what keeps [`sentence`] free of an allocator.
const SENTENCE: usize = 192;

/// Compose `prefix name name ... suffix` into `buf`, skipping empty names.
///
/// A hand-rolled `write!` because there is no allocator and `core::fmt` would drag its machinery
/// into a program whose whole job is to be small. It **truncates** rather than failing: a diagnostic
/// cut short still names the first thing that went wrong, and the alternative is a refusal that says
/// nothing at all.
fn sentence<'b>(
    buf: &'b mut [u8; SENTENCE],
    prefix: &[u8],
    names: &[&str],
    suffix: &[u8],
) -> &'b [u8] {
    fn push(buf: &mut [u8; SENTENCE], n: &mut usize, src: &[u8]) {
        for &b in src {
            if *n < SENTENCE {
                buf[*n] = b;
                *n += 1;
            }
        }
    }
    let mut n = 0usize;
    push(buf, &mut n, prefix);
    for name in names {
        if name.is_empty() {
            continue;
        }
        push(buf, &mut n, b" ");
        push(buf, &mut n, name.as_bytes());
    }
    push(buf, &mut n, suffix);
    &buf[..n]
}

/// Unwrap a `Result<u64, ()>` or fault: a half-built system is not worth limping along.
fn must(r: Result<u64, ()>) -> u64 {
    match r {
        Ok(v) => v,
        Err(()) => fail(),
    }
}

/// Fault unless the syscall succeeded.
fn must_ok(ok: bool) {
    if !ok {
        fail();
    }
}

/// Trap. The kernel prints the pc and kills the process, which is the legible way for a builder to
/// say that the system it was asked to build cannot exist.
fn fail() -> ! {
    supervision_proto::fail()
}
