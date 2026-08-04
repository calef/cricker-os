//! **The system builder: userspace init for the interactive shell** (parity D).
//!
//! The portable counterpart of hello's `init_boot` role (which is aarch64-wired only by living inside
//! the PL011-tied `hello`). The kernel loads this as the boot process, maps the initrd, and grants it
//! a budget (slot 0), the UART's registers as a device cap (slot 1), the UART receive interrupt as an
//! `Irq` cap (slot 2), a read-only mapping of the wall clock (slot 3), and, when this boot attached a
//! RedoxFS disk, the file service and its shared page (slots 4 and 5). From those, and nothing else,
//! it builds the whole interactive system out of its own budget:
//!
//! 1. the **console** server (output): reads text from a shared page, writes it to the UART;
//! 2. the **input** driver (keystrokes): waits on the UART receive interrupt, forwards bytes;
//! 3. the **line discipline** (`line_editor`, milestone 28): editing, echo, history, between them;
//! 4. the **shell**: prints and reads lines through the terminal endpoint, runs commands;
//! 5. the **terminal's sink adapter** (`terminal_sink`, milestone 50), when the archive carries one:
//!    it holds the terminal and serves the sink contract, so a declared second stream can be pointed
//!    at the screen without handing anyone the endpoint that also reads the keyboard;
//!
//! wired together with endpoints and shared pages this program creates. The kernel wires none of it.
//! Then it stays alive as the spawn service (milestone 31): the shell resolves a `run` into a grant
//! expression and directs init to load the named program and endow it with exactly what the command
//! named (the result endpoint, and an untyped budget the shell delegates for `run --mem N`). Nothing
//! here names an architecture: the console and input drivers hold the one device-specific fact (the
//! UART register layout), and the kernel grants the right device.
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
//! need them are built, and the file service as soon as the shell holds it. The proof is a negative
//! control taken from inside the process and printed at the prompt, exactly the shape
//! `root_supervisor` uses: after the delete, `RETYPE` and `RETYPE_OBJ` on that slot must answer
//! `NoSuchSlot` (there is nothing there) rather than `NotPermitted` (there is, and you may not).
//!
//! The job budget is **renewable**, which is what makes bounding it cheap. Every job is built in its
//! own region split off [`JOBS_BUDGET_PAGES`] and is born supervised: `job_undertaker`, a process holding
//! one endpoint capability and nothing else, collects each corpse through `Endpoint::REAP` (DECISIONS
//! §32) and the region's pages come back here (§13: a reclaimed region returns to its owner, which is
//! whoever split it). Before that, a spawned job's memory was spent for the life of the boot.
//!
//! # BUGS
//!
//! The return of pages is **LIFO** (§16, `crates/regions`): a job region that is not at the top of
//! the budget's watermark when it is reclaimed returns nothing, and its run is a hole until this
//! process dies, which it never does. Sequential commands at a prompt are exactly LIFO and recover
//! fully; two jobs alive at once (a pipeline stage that outlives its producer) permanently costs one
//! region. A long enough session of concurrent pipelines still ends at "could not spawn".
//!
//! `build_child`'s scratch window is never unmapped, so this process keeps a **writable mapping of
//! every page it ever laid down for a child**. Reaping a job undoes that (region reclaim revokes every
//! mapping of the pages first, §13), but the boot servers are never reclaimed, so init can still
//! read and write the console's, the line editor's, the input driver's, the shell's and the sink
//! adapter's memory. Giving the construction budget away does not reach that, and nothing in the ABI
//! unmaps a page.
//!
//! Printing the negative control costs one more of those: the shell's output frame stays mapped here
//! for life, because there is no unmap and `Frame::REVOKE` would take it from the shell too.

#![no_std]
#![no_main]

use grant_plan::{Prog, spawnproto};
use line_editor::proto;
use user_rt::{call, cap_delete, exit, invoke, recv, recv_cap, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// The capabilities the kernel grants before this program runs.
const UNTYPED: u64 = 0; // the building budget
const UART_DEV: u64 = 1; // the UART registers, a device cap to delegate into the drivers
const UART_IRQ: u64 = 2; // the UART receive interrupt, an Irq cap to delegate into the input driver
/// **The filesystem, when this boot has one** (milestone 50). The kernel wires the block server and
/// the FS server before it starts us and grants the service endpoint plus the page its clients
/// share with it. The rights that endpoint carries arrive in `a2`, and **0 means this boot attached
/// no RedoxFS disk**, in which case these two slots hold nothing at all.
const FS_EP: u64 = 4;
const FS_PAGE: u64 = 5;
/// **The wall clock** (milestone 51's wiring): a `Frame` capability with `READ` and nothing else.
///
/// Granted ahead of the filesystem pair so its slot is the same on every boot, whether or not a disk
/// was attached, and granted **unconditionally**: a boot with no clock service hands us a zeroed
/// page, which reads as `clock_proto::state::UNKNOWN` and is the honest answer for a machine that
/// does not know the time. init hands it on only to a child whose manifest declares a clock, and
/// hands on `READ`, so nothing spawned from this prompt can set the time (DECISIONS §43).
const CLOCK_PAGE: u64 = 3;

/// Where a child that declares a clock maps it, read-only. Must match `user/src/date.rs`'s
/// `CLOCK_VA` and `kernel/src/user/clock_service.rs`.
const CHILD_CLOCK_VA: u64 = 0x00c0_0000;

const PAGE: u64 = 4096;
const CHILD_STACK_VA: u64 = 0x0050_0000;
/// Stack pages every child init builds gets, mapped down from [`CHILD_STACK_VA`].
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
const CHILD_STACK_PAGES: u64 = 12;

/// Where a supervised (interruptible) child maps its shared job frame (milestone 24). Below the ELF
/// load address (`0x40_0000`) and the stack; must match heeder.rs / spinner.rs's `JOB_FRAME_VA`.
const CHILD_JOBFRAME_VA: u64 = 0x0030_0000;

/// Pages of untyped we split off our own budget and hand the shell (milestone 31), so the shell can
/// in turn endow the programs it spawns (`run --mem N`) out of a budget that is genuinely *its own*.
/// The shell shrinks this by N pages per grant; the pages a spawned child pins are not reclaimed in
/// phase 1, so this is a session budget, not a renewable one.
const SH_BUDGET_PAGES: u64 = 128;

/// **What init keeps for itself after the boot servers are up** (milestone 22, the interactive
/// increment). It pays for one thing: the page tables reaching `build_child`'s scratch window, which
/// are init's own mappings and must never come out of a child's region (tearing that region down
/// would free init's tables under a window it never unmaps). One L3 covers 512 scratch pages and a
/// job maps at most a couple of dozen, so this is thousands of commands' worth.
const INIT_OWN_PAGES: u64 = 128;

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
const JOBS_BUDGET_PAGES: u64 = JOB_REGION_PAGES * 6;

/// Where init maps the shell's output frame in **its own** address space, to print the one line it
/// ever prints (the dropped-authority negative control). Well clear of init's segments, its stack,
/// and the scratch window at `0x1000_0000`.
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

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, initrd_len: u64, fs_rights: u64) -> ! {
    // The archive the kernel mapped read-only; its length arrived in a1.
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM, read-only, at INITRD_VA.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    let Ok(fs) = crickerfs::Fs::parse(archive) else {
        fail()
    };

    let Some(con_elf) = fs.read("console").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };
    let Some(in_elf) = fs.read("input").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };
    let Some(td_elf) = fs.read("line_editor").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };
    let Some(sh_elf) = fs.read("swish").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };
    // **The terminal's sink adapter** (milestone 50's last remainder). Optional on purpose: an
    // initrd built without it still boots, and a program that declares a second stream then finds
    // an empty slot and says what it has to say in-band. A missing component should cost a feature,
    // not a prompt.
    let sink_elf = fs
        .read("terminal_sink")
        .and_then(|b| elf::Elf::parse(b).ok());
    // The undertaker (milestone 22, the interactive increment). Read here with the rest,
    // because the archive is only readable while we hold it and every failure below is one `fail`.
    // Required rather than optional, unlike the adapter above: without it a bounded job pool fills
    // and the prompt stops spawning, which is a broken system and not a missing feature.
    let Some(reaper_elf) = fs
        .read("job_undertaker")
        .and_then(|b| elf::Elf::parse(b).ok())
    else {
        fail()
    };

    // The endpoints and shared pages we own and hand out, each retyped with full rights so we can
    // delegate narrowed views. `term_ep` is the terminal contract's one endpoint: the discipline
    // serves it; the input driver and the shell only hold WRITE on it, and neither can tell what
    // is on the other side (notes/terminal-contract.md).
    let request = must(retype_obj(abi::objtype::ENDPOINT));
    let reply = must(retype_obj(abi::objtype::ENDPOINT));
    let term_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let con_shared = must(retype_frame()); // line_editor -> console text
    let term_out = must(retype_frame()); // shell -> line_editor text and prompts
    let term_in = must(retype_frame()); // line_editor -> shell completed lines

    // 1. Console server: reads text from the shared page, writes it to the UART.
    let con = must(build_child(
        UNTYPED,
        UNTYPED,
        &con_elf,
        &[(request, abi::rights::READ), (reply, abi::rights::WRITE)],
        &[
            (CON_SHARED_VA, con_shared, abi::aspace::MAP_RO),
            (CON_UART_VA, UART_DEV, abi::aspace::MAP_RO), // mode ignored for a DeviceFrame cap
        ],
    ));
    must0(tcb_start(con, 0, 0, 0));
    cap_delete(con);

    // 2. The line discipline: serves the terminal endpoint, prints through the console. It is
    // the console's only client; everyone else prints through it.
    let line_editor = must(build_child(
        UNTYPED,
        UNTYPED,
        &td_elf,
        &[
            (term_ep, abi::rights::READ),
            (request, abi::rights::WRITE),
            (reply, abi::rights::READ),
        ],
        &[
            (CON_SHARED_VA, con_shared, abi::aspace::MAP_RW), // line_editor fills what the console reads
            (TERM_OUT_VA, term_out, abi::aspace::MAP_RO),
            (TERM_IN_VA, term_in, abi::aspace::MAP_RW),
        ],
    ));
    must0(tcb_start(line_editor, 0, 0, 0));
    cap_delete(line_editor);

    // 3. Input driver: waits on the UART receive interrupt, forwards raw bytes to the terminal.
    let input = must(build_child(
        UNTYPED,
        UNTYPED,
        &in_elf,
        &[(term_ep, abi::rights::WRITE), (UART_IRQ, abi::rights::READ)],
        &[(IN_UART_VA, UART_DEV, abi::aspace::MAP_RO)],
    ));
    must0(tcb_start(input, 0, 0, 0));
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
    // budget further down, one boot stage earlier.
    for c in [request, reply, con_shared, UART_DEV, UART_IRQ] {
        cap_delete(c);
    }

    // **The spawn channel is retyped here, not with the rest**, and the reason is the same sixteen
    // slots: holding two more endpoints through the three builds above is what pushed this cspace
    // over. They are the shell's and the service's, so this is also where they belong.
    let spawn_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let result_ep = must(retype_obj(abi::objtype::ENDPOINT));
    // **The supervision endpoint every job is born holding** (milestone 22, the interactive
    // increment; DECISIONS §26's spawn-slot convention). We keep it for its `GRANT`, which is all we
    // need it for: to place a `READ` view of it in each job's reserved fault slot. We never receive
    // on it. `job_undertaker` does, and collecting is the only thing that endpoint authorizes.
    let deaths = must(retype_obj(abi::objtype::ENDPOINT));

    // 4. The shell: prints and reads lines through the terminal, holds the spawn channel, and holds
    // its own untyped budget (slot 3) so `run --mem N` grants from memory that is genuinely the
    // shell's. WRITE lets it SPLIT the budget; GRANT lets it delegate the split to init. We carve
    // that budget from our own untyped and hand it over the same way we hand any capability.
    //
    // Slot 4 is the filesystem when this boot has one, which is the whole of what `>` and `<` need
    // (milestone 50, notes/pipes.md): the shell resolves a redirection against it and writes the
    // file itself. Narrowed to WRITE, which on an endpoint is the right to CALL, and without GRANT,
    // so the shell can hand it to nobody.
    let sh_budget = must(untyped_split(SH_BUDGET_PAGES));
    let with_fs = fs_rights != 0;
    let sh_caps: [(u64, u64); 5] = [
        (term_ep, abi::rights::WRITE),
        (spawn_ep, abi::rights::WRITE),
        (result_ep, abi::rights::READ),
        (sh_budget, abi::rights::WRITE | abi::rights::GRANT),
        (FS_EP, abi::rights::WRITE),
    ];
    let sh_maps: [(u64, u64, u64); 3] = [
        (SH_OUT_VA, term_out, abi::aspace::MAP_RW), // shell writes text and prompts here
        (LINE_VA, term_in, abi::aspace::MAP_RO),    // shell reads completed lines
        (SH_FS_VA, FS_PAGE, abi::aspace::MAP_RW),   // and its half of the FS contract
    ];
    // **Built but not started**, because the drop below has to happen while the shell's output page
    // is still ours alone to write: the negative control is printed through it, and a running shell
    // would be printing its banner into the same page.
    let shell = must(build_child(
        UNTYPED,
        UNTYPED,
        &sh_elf,
        if with_fs { &sh_caps } else { &sh_caps[..4] },
        if with_fs { &sh_maps } else { &sh_maps[..2] },
    ));
    cap_delete(sh_budget); // our copy; the shell holds its own now

    // Free every boot cap the spawn service does not need, so init's 16-slot cspace has room to
    // build a supervised child (which holds a job untyped and a job frame while build_child retypes
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
        cap_delete(FS_EP);
        cap_delete(FS_PAGE);
    }

    // 5. **The terminal's sink adapter** (milestone 50's last remainder, notes/sink-protocol.md,
    // DECISIONS §67). It holds the terminal `WRITE` and serves the sink contract on an endpoint of
    // its own, so a child can be handed "the terminal" as a place to put bytes **without** being
    // handed the terminal endpoint, which also carries `OP_READLINE` and would be the keyboard.
    //
    // **After the shell and before the giveaway, and both halves of that are load-bearing.**
    //
    // After the shell, because of this cspace's sixteen slots: building the adapter earlier put init
    // one slot over while `build_child` was retyping the shell's address space, and the symptom was
    // the one this file has already seen, a boot that reaches userspace and then prints nothing at
    // all. That constraint is about the shell's build, not about being the last thing built, and
    // milestone 22 is what made the difference visible: the adapter is now the fifth of six boot
    // components rather than the last of five, and the cspace has room either way.
    //
    // Before the giveaway, because this is a **system** component and the root untyped is what the
    // system is built from. Everything below hands that budget away and proves it is gone, so an
    // adapter built after it would have to come out of [`INIT_OWN_PAGES`], the scratch budget sized
    // for page tables and nothing else. Spending a whole program out of that pool would be invisible
    // here and would surface as some later child failing to map a scratch page.
    let term_sink = must(retype_obj(abi::objtype::ENDPOINT));
    if let Some(elf) = sink_elf.as_ref() {
        let adapter = must(build_child(
            UNTYPED,
            UNTYPED,
            elf,
            &[
                (term_sink, abi::rights::READ),
                (term_ep, abi::rights::WRITE),
            ],
            &[],
        ));
        // Started here even though the shell deliberately is not: the adapter owns no page and
        // prints only what a client sends it, and it has no clients until the spawn service below
        // hands one its endpoint. It cannot write into the page the announcement below stages in.
        must0(tcb_start(adapter, 0, 0, 0));
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
    let own_ut = must(untyped_split(INIT_OWN_PAGES));
    let jobs_ut = must(untyped_split(JOBS_BUDGET_PAGES));
    // The shell's output page, in our own space, so we can say what just happened. This mapping is
    // permanent (there is no unmap, and `Frame::REVOKE` would take the page from the shell too); see
    // this module's BUGS.
    // SAFETY: `invoke` traps to the kernel, which validates the capability and the method before
    // acting (user_rt's contract).
    if unsafe { invoke(term_out, abi::frame::MAP, INIT_OUT_VA, 1, UNTYPED) } != 0 {
        fail()
    }
    cap_delete(UNTYPED);

    // And prove it from the inside, on the two primitives that build things, before anything else
    // runs. `NoSuchSlot` (-1) rather than `NotPermitted` (-3) is the whole claim: the capability is
    // *gone*, not narrowed, so there is nothing there to name. This is `root_supervisor`'s proof at
    // the interactive prompt, and `script/shell-check` reads the sentence.
    // SAFETY: as above: the kernel validates the capability and the method.
    let frame = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
    // SAFETY: as above: the kernel validates the capability and the method.
    let object = unsafe { invoke(UNTYPED, abi::untyped::RETYPE_OBJ, abi::objtype::TCB, 0, 0) };
    announce(
        term_ep,
        if frame == -1 && object == -1 {
            b"init: construction budget dropped; retype answers NoSuchSlot\n"
        } else {
            b"init: construction budget NOT dropped; it can still build\n"
        },
    );
    cap_delete(term_ep);
    cap_delete(term_out);

    // The undertaker, out of what is left of our own budget. One capability, `READ` on the
    // supervision endpoint, and nothing else: it can free a job's memory and can never spend it.
    let reaper = must(build_child(
        own_ut,
        own_ut,
        &reaper_elf,
        &[(deaths, abi::rights::READ)],
        &[],
    ));
    must0(tcb_start(reaper, 0, 0, 0));
    cap_delete(reaper);

    // Role 0 (the prompt), and `arg1` is the rights its directory capability carries. A shell told 0
    // holds no directory and says so at every verb that would need one.
    must0(tcb_start(shell, 0, fs_rights, 0));
    cap_delete(shell);

    // The spawn service (milestone 31's grant expression, wire half; grant_plan::spawnproto). The shell
    // resolved a command into a program, an argument, and a memory-grant page count, and it directs
    // us rather than building the child itself: we hold the initrd, so we stay the ELF loader (the
    // parser lives in one place, out of the shell). We endow every child the result endpoint at slot
    // 0, and the untyped the shell delegates at slot 1 when a `--mem` grant rode along. Nothing else:
    // the child's authority is exactly what the command line named. See the spawn_service comment.
    let worker = fs.read("worker").and_then(|b| elf::Elf::parse(b).ok());
    let budgeter = fs.read("budgeter").and_then(|b| elf::Elf::parse(b).ok());
    let heeder = fs.read("heeder").and_then(|b| elf::Elf::parse(b).ok());
    let spinner = fs.read("spinner").and_then(|b| elf::Elf::parse(b).ok());
    let date = fs.read("date").and_then(|b| elf::Elf::parse(b).ok());
    let wc = fs.read("wc").and_then(|b| elf::Elf::parse(b).ok());
    spawn_service(
        Channels {
            spawn_ep,
            result_ep,
            deaths,
            own_ut,
            jobs_ut,
            // The terminal's sink, if this initrd carried an adapter to serve it. This is what a
            // declared second stream gets by default (DECISIONS §67): the shell names a file with
            // `2>` and otherwise the bytes go straight to the screen, through a process that can do
            // nothing else with them.
            term_sink: sink_elf.is_some().then_some(term_sink),
        },
        [
            worker.as_ref(),
            budgeter.as_ref(),
            heeder.as_ref(),
            spinner.as_ref(),
            date.as_ref(),
            // `rm` (milestone 47) has a slot and **deliberately no ELF**: it is endowed a directory
            // capability, which means a `fs_subtree_caretaker` this init would have to build per
            // invocation out of the FS endpoint it deletes above. Until it does, the shell refuses
            // the command before it reaches here, which is why an empty slot is honest rather than a
            // hole: spawning `rm` with nothing to remove from would be the worst failure this model
            // has, a program told to destroy something, holding nothing, saying nothing.
            None,
            // `wc` (milestone 50). It needs no filesystem: everything it does is decided by what is
            // in its input slot, and the shell can fill that from a pipe out of its own budget.
            // So unlike `rm`, this one is reachable from the interactive prompt.
            wc.as_ref(),
        ],
    )
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
/// Two shapes (`grant_plan::spawnproto`). A **normal** job: the shell sends the request and, if `--mem`
/// rode along, one delegated untyped; we split a region off the job pool, build the child in it,
/// endow it the result endpoint (and the budget) plus its supervision endpoint, and start it. A
/// **supervised** (interruptible) job: the shell leads the delegation with a job untyped and a shared
/// job frame; we build the whole child *from that untyped* (so the shell's region owns it and can
/// `DESTROY` it to tear it down, milestone 24), map the job frame in, endow nothing else, start it,
/// and send `SPAWN_OK` once as the shell's go-ahead. The `progs` array is indexed by [`Prog::id`], so
/// it is [`grant_plan::PROG_COUNT`] long: a variant added to `grant_plan` without a slot here would
/// be an out-of-bounds read in init.
///
/// **Only the normal shape is supervised**, and that is not an oversight. An interruptible job's
/// region belongs to the shell, which tears it down itself on the second `^C` (milestone 24's
/// forcible tier) and after a clean finish; endowing it a supervision endpoint here would put a
/// second party in the teardown path for memory that is not ours, racing the shell's `DESTROY`.
fn spawn_service(c: Channels, progs: [Option<&elf::Elf>; grant_plan::PROG_COUNT]) -> ! {
    let Channels {
        spawn_ep,
        result_ep,
        deaths,
        own_ut,
        jobs_ut,
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
        // frame), then the sink, then the source, then any --mem untyped. No promise, no receive, so
        // both sides stay in lockstep.
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

        let elf = prog.and_then(|p| progs[p.id() as usize]);
        // Read from the program's own declaration, not from the request: a clock is not something
        // the command line can designate, so there is no bit on the wire for it (`Manifest::clock`).
        let wants_clock = prog.is_some_and(|p| p.manifest().clock);

        if interruptible {
            // Build the whole child from the shell's job untyped, mapping the shared job frame; no
            // capabilities in its cspace (it reports through the frame and exits). SPAWN_OK is the
            // go-ahead the shell waits for before it starts watching the frame.
            let built = match (elf, job_ut, job_fr) {
                (Some(e), Some(ut), Some(fr)) => build_child(
                    own_ut,
                    ut,
                    e,
                    &[],
                    &[(CHILD_JOBFRAME_VA, fr, abi::aspace::MAP_RW)],
                )
                .ok(),
                _ => None,
            };
            match built {
                Some(tcb) => {
                    let ok = tcb_start(tcb, 0, arg, 0) == 0;
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
            // **The clock, which nothing on the command line asked for** (milestone 51's wiring).
            // It comes from the manifest rather than from the request, because a person does not
            // designate a clock: `date` declares that it reads one, and init is the only process
            // here holding a page it could hand over. Before the source and the budget, so `date`'s
            // clock is slot 1, which is unambiguous only because no manifest declares a clock *and*
            // an input. That is the same ordered-slot debt notes/pipes.md's BUGS already records.
            if wants_clock {
                caps[n] = (CLOCK_PAGE, abi::rights::READ);
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
            let clock_map = [(CHILD_CLOCK_VA, CLOCK_PAGE, abi::aspace::MAP_RO)];
            let maps: &[(u64, u64, u64)] = if wants_clock { &clock_map } else { &[] };
            // **A region of its own, so the job's memory can come home** (milestone 22, the
            // interactive increment). Everything the child is made of comes out of this carve, and a
            // single reclaim frees all of it; the alternative, building straight out of the pool,
            // spends those pages for the life of the boot because a watermark only moves forward.
            // An exhausted pool is the ordinary `SPAWN_FAILED` the shell already reports as "init is
            // out of memory", which is the honest sentence for a bounded budget with jobs still in
            // it. The clock frame is ours and is only *mapped* into the child, so it is untouched
            // when the region goes.
            let region = untyped_split_from(jobs_ut, JOB_REGION_PAGES).ok();
            let built = match (elf, region) {
                (Some(e), Some(r)) => {
                    // Born supervised: `deaths` goes in the reserved fault slot, where `START` reads
                    // it and clears it, so the job cannot forge messages on its own death channel.
                    // The declared second stream rides the same named-slot mechanism at the low slot
                    // its manifest picked; the two cannot collide, because the fault slot is last.
                    build_supervised_child(own_ut, r, e, &caps[..n], placed, maps, deaths).ok()
                }
                _ => None,
            };
            let ok = match built {
                Some(tcb) => {
                    let started = tcb_start(tcb, 0, arg, 0) == 0;
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
                    untyped_destroy(r);
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

/// Our ever-advancing scratch window: where we temporarily map each child frame to fill it. Never
/// unmapped, so a per-call reset would collide with a prior child's mappings. Starts below the initrd.
static SCRATCH_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x1000_0000);

/// Build a child from `elf`: lay each segment W^X at the VA it names, map a stack, map `maps` (each
/// `(child_va, our_slot, mode)`), retype a TCB, insert `caps` (each `(our_slot, rights)`) in order,
/// configure at the entry. Returns the TCB slot, ready to start. The userspace ELF loader, driven
/// entirely through the capability verbs. [`build_supervised_child`] is this plus a fault slot.
fn build_child(
    own_ut: u64,
    build_ut: u64,
    elf: &elf::Elf,
    caps: &[(u64, u64)],
    maps: &[(u64, u64, u64)],
) -> Result<u64, ()> {
    build_supervised_child_inner(own_ut, build_ut, elf, caps, &[], maps, None)
}

/// [`build_child`], with two kinds of capability that go in a **named** slot rather than the next
/// free one, both through `abi::tcb::CAP_INSERT`'s explicit target.
///
/// `fault` lands in the reserved [`abi::fault::FAULT_EP_SLOT`] so the child is born supervised
/// (DECISIONS §26's spawn-slot convention): `START` records it as the thread's supervision endpoint
/// and clears the slot, so the child cannot forge messages about its own death. That is what makes a
/// job's region reclaimable by `job_undertaker` after the job ends.
///
/// `placed` is `(child_slot, our_slot, rights)`, inserted after `caps`. One caller today: a declared
/// diagnostic stream (DECISIONS §67), which sits above every ordinary grant because how many of
/// those there are depends on what the command line named, and a program that probes one slot number
/// needs that number not to move. The kernel already had the mechanism, for the fault endpoint, for
/// the same reason, which is why the two share one call: the fault slot is the last in the cspace
/// and a manifest's diagnostics slot is far below it, so they cannot land on each other.
fn build_supervised_child(
    own_ut: u64,
    build_ut: u64,
    elf: &elf::Elf,
    caps: &[(u64, u64)],
    placed: &[(u64, u64, u64)],
    maps: &[(u64, u64, u64)],
    fault: u64,
) -> Result<u64, ()> {
    build_supervised_child_inner(own_ut, build_ut, elf, caps, placed, maps, Some(fault))
}

fn build_supervised_child_inner(
    own_ut: u64,
    build_ut: u64,
    elf: &elf::Elf,
    caps: &[(u64, u64)],
    placed: &[(u64, u64, u64)],
    maps: &[(u64, u64, u64)],
    fault: Option<u64>,
) -> Result<u64, ()> {
    // The child's aspace, code/data frames, stack, and TCB all come from `build_ut`: a region of its
    // own for a job at the prompt, the untyped the shell delegated for a supervised (interruptible)
    // one, our own budget for a boot server. Our *scratch* mappings below stay on `own_ut`: they are
    // ours, and a child's region must not have our page tables freed under it when it is torn down.
    let aspace = retype_obj_from(build_ut, abi::objtype::ASPACE)?;

    for seg in elf.segments() {
        let mode = if seg.is_executable() {
            abi::aspace::MAP_CODE
        } else if seg.is_writable() {
            abi::aspace::MAP_RW
        } else {
            abi::aspace::MAP_RO
        };
        let (start, end) = seg.page_range(PAGE);
        let mut va = start;
        while va < end {
            let frame = retype_frame_from(build_ut)?;
            let scratch = SCRATCH_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
            // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
            // before acting (user_rt's contract). A caller cannot break an invariant by passing a
            // bad slot or method; it gets an error back.
            if unsafe { invoke(frame, abi::frame::MAP, scratch, 1, own_ut) } != 0 {
                return Err(());
            }
            // SAFETY: `scratch` is a page we just mapped read/write in our own space.
            let dst = unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE as usize) };
            dst.fill(0);
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + PAGE).min(file_hi);
            if lo < hi {
                let d = (lo - va) as usize;
                let s = (lo - file_lo) as usize;
                let len = (hi - lo) as usize;
                dst[d..d + len].copy_from_slice(&seg.data[s..s + len]);
            }
            // SAFETY: as above: the kernel validates the capability and the method.
            if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, frame, mode) } != 0 {
                return Err(());
            }
            cap_delete(frame);
            va += PAGE;
        }
    }

    for k in 0..CHILD_STACK_PAGES {
        let stack_frame = retype_frame_from(build_ut)?;
        let va = CHILD_STACK_VA - k * PAGE;
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe {
            invoke(
                aspace,
                abi::aspace::MAP_INTO,
                va,
                stack_frame,
                abi::aspace::MAP_RW,
            )
        } != 0
        {
            return Err(());
        }
        cap_delete(stack_frame);
    }

    for &(va, our_slot, mode) in maps {
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, our_slot, mode) } != 0 {
            return Err(());
        }
    }

    let tcb = retype_obj_from(build_ut, abi::objtype::TCB)?;
    for &(our_slot, rights) in caps {
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, our_slot, rights, 0) } < 0 {
            return Err(());
        }
    }
    for &(child_slot, our_slot, rights) in placed {
        // `target = n` lands the capability in slot `n - 1`; 0 would mean "first free", which is the
        // behaviour this call exists to avoid.
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, our_slot, rights, child_slot + 1) } < 0 {
            return Err(());
        }
    }
    if let Some(ep) = fault {
        // The spawn-slot convention: a target of `n + 1` means slot `n`, so the supervision endpoint
        // lands in the reserved last slot rather than wherever first-free fell. `READ` is all it
        // needs and all it gets; `START` clears the slot anyway.
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe {
            invoke(
                tcb,
                abi::tcb::CAP_INSERT,
                ep,
                abi::rights::READ,
                abi::fault::FAULT_EP_SLOT + 1,
            )
        } < 0
        {
            return Err(());
        }
    }
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe {
        invoke(
            tcb,
            abi::tcb::CONFIGURE,
            elf.entry(),
            CHILD_STACK_VA + PAGE,
            aspace,
        )
    } != 0
    {
        return Err(());
    }
    Ok(tcb)
}

fn retype_obj(objtype: u64) -> Result<u64, ()> {
    retype_obj_from(UNTYPED, objtype)
}

fn retype_obj_from(ut: u64, objtype: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

fn retype_frame() -> Result<u64, ()> {
    retype_frame_from(UNTYPED)
}

fn retype_frame_from(ut: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Carve `pages` off our own untyped into a new child untyped we can delegate (milestone 31). The
/// SPLIT grants us full rights on the child, including GRANT, so we can hand a memory budget on.
fn untyped_split(pages: u64) -> Result<u64, ()> {
    untyped_split_from(UNTYPED, pages)
}

/// [`untyped_split`] against a named budget: the job pool, once the root is gone.
fn untyped_split_from(ut: u64, pages: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Reclaim a region nothing is running in: the unwind path for a job whose build or start failed.
/// Refused by the kernel while a live thread occupies it, which is what makes it safe to call on a
/// half-built job and never on a running one.
fn untyped_destroy(ut: u64) {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(ut, abi::untyped::DESTROY, 0, 0, 0) };
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
        // SAFETY: the shell's output frame is mapped read/write here, and one line is far under a page.
        unsafe { core::ptr::write_volatile(out.add(i), b) };
    }
    call(term_ep, proto::req(proto::OP_WRITE, text.len() as u64), 0);
}

fn tcb_start(tcb: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(tcb, abi::tcb::START, a0, a1, a2) }
}

/// Unwrap a `Result<u64, ()>` or fault: a half-built system is not worth limping along.
fn must(r: Result<u64, ()>) -> u64 {
    match r {
        Ok(v) => v,
        Err(()) => fail(),
    }
}

/// Fault unless the syscall returned 0.
fn must0(r: i64) {
    if r != 0 {
        fail();
    }
}

fn fail() -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
