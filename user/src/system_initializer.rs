//! **The system builder: userspace init for the interactive shell** (parity D).
//!
//! The portable counterpart of hello's `init_boot` role (which is aarch64-wired only by living inside
//! the PL011-tied `hello`). The kernel loads this as the boot process, maps the initrd, and grants it
//! a budget (slot 0), the UART's registers as a device cap (slot 1), and the UART receive interrupt
//! as an `Irq` cap (slot 2). From those, and nothing else, it builds the
//! whole interactive system out of its own budget:
//!
//! 1. the **console** server (output): reads text from a shared page, writes it to the UART;
//! 2. the **input** driver (keystrokes): waits on the UART receive interrupt, forwards bytes;
//! 3. the **line discipline** (`line_editor`, milestone 28): editing, echo, history, between them;
//! 4. the **shell**: prints and reads lines through the terminal endpoint, runs commands;
//!
//! wired together with endpoints and shared pages this program creates. The kernel wires none of it.
//! Then it stays alive as the spawn service (milestone 31): the shell resolves a `run` into a grant
//! expression and directs init to load the named program and endow it with exactly what the command
//! named (the result endpoint, and an untyped budget the shell delegates for `run --mem N`). Nothing
//! here names an architecture: the console and input drivers hold the one device-specific fact (the
//! UART register layout), and the kernel grants the right device.

#![no_std]
#![no_main]

use grant_plan::{Prog, spawnproto};
use user_rt::{cap_delete, exit, invoke, recv, recv_cap, send};

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
const FS_EP: u64 = 3;
const FS_PAGE: u64 = 4;

const PAGE: u64 = 4096;
const CHILD_STACK_VA: u64 = 0x0050_0000;
/// Stack pages every child init builds gets, mapped down from [`CHILD_STACK_VA`].
///
/// **Eight rather than four since milestone 50**, and it is a measured number rather than a round
/// one: the shell's redirection path carries a parsed line, an array of planned endowments, a
/// listing buffer and a file buffer all by value, and four pages overflowed at the first
/// `ls > out.txt` (a data abort one word below the lowest stack page). The kernel's own scripted
/// wiring had already found the same floor and maps seven. The cost is 16 KiB per child, which is
/// nothing next to a page table.
const CHILD_STACK_PAGES: u64 = 8;

/// Where a supervised (interruptible) child maps its shared job frame (milestone 24). Below the ELF
/// load address (0x40_0000) and the stack; must match heeder.rs / spinner.rs's JOB_FRAME_VA.
const CHILD_JOBFRAME_VA: u64 = 0x0030_0000;

/// Pages of untyped we split off our own budget and hand the shell (milestone 31), so the shell can
/// in turn endow the programs it spawns (`run --mem N`) out of a budget that is genuinely *its own*.
/// The shell shrinks this by N pages per grant; the pages a spawned child pins are not reclaimed in
/// phase 1, so this is a session budget, not a renewable one.
const SH_BUDGET_PAGES: u64 = 128;

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
    for c in [request, reply, con_shared] {
        cap_delete(c);
    }

    // **The spawn channel is retyped here, not with the rest**, and the reason is the same sixteen
    // slots: holding two more endpoints through the three builds above is what pushed this cspace
    // over. They are the shell's and the service's, so this is also where they belong.
    let spawn_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let result_ep = must(retype_obj(abi::objtype::ENDPOINT));

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
    let shell = must(build_child(
        UNTYPED,
        &sh_elf,
        if with_fs { &sh_caps } else { &sh_caps[..4] },
        if with_fs { &sh_maps } else { &sh_maps[..2] },
    ));
    // Role 0 (the prompt), and `arg1` is the rights its directory capability carries. A shell told 0
    // holds no directory and says so at every verb that would need one.
    must0(tcb_start(shell, 0, fs_rights, 0));
    cap_delete(shell);
    cap_delete(sh_budget); // our copy; the shell holds its own now

    // Free every boot cap the spawn service does not need, so init's 16-slot cspace has room to
    // build a supervised child (which holds a job untyped and a job frame while build_child retypes
    // an aspace, frames, and a TCB). The drivers and the shell hold the narrowed copies that matter.
    for c in [term_ep, term_out, term_in] {
        cap_delete(c);
    }
    // The filesystem too. init is the ELF loader, not an FS client: the shell holds the narrowed
    // copies and this process never speaks `fs_proto`. The day `rm` is reachable from the prompt,
    // init keeps the endpoint instead, because building a `fs_subtree_caretaker` is its job and not
    // the shell's.
    if with_fs {
        cap_delete(FS_EP);
        cap_delete(FS_PAGE);
    }

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
        spawn_ep,
        result_ep,
        [
            worker.as_ref(),
            budgeter.as_ref(),
            heeder.as_ref(),
            spinner.as_ref(),
            date.as_ref(),
            // `rm` (milestone 47) has a slot and **deliberately no ELF**: it is endowed a directory
            // capability, and this boot wires no FS service, so there is nothing to narrow one from.
            // The shell refuses the command before it reaches here ("you hold no such capability"),
            // which is why an empty slot is honest rather than a hole: spawning `rm` with nothing to
            // remove from would be the worst failure this model has, a program told to destroy
            // something, holding nothing, saying nothing.
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

/// The spawn service loop: serve the shell's `run` requests forever. Init is the ELF loader the
/// shell directs; it inserts only what the shell endows, so a spawned program can reach nothing the
/// command line did not name.
///
/// Two shapes (grant_plan::spawnproto). A **normal** job: the shell sends the request and, if `--mem`
/// rode along, one delegated untyped; we build the child from our own budget, endow it the result
/// endpoint (and the budget), and start it. A **supervised** (interruptible) job: the shell leads
/// the delegation with a job untyped and a shared job frame; we build the whole child *from that
/// untyped* (so the shell's region owns it and can `DESTROY` it to tear it down, milestone 24), map
/// the job frame in, endow nothing else, start it, and send `SPAWN_OK` once as the shell's
/// go-ahead. The `progs` array is indexed by [`Prog::id`], so it is [`grant_plan::PROG_COUNT`] long: a
/// variant added to `grant_plan` without a slot here would be an out-of-bounds read in init.
fn spawn_service(
    spawn_ep: u64,
    result_ep: u64,
    progs: [Option<&elf::Elf>; grant_plan::PROG_COUNT],
) -> ! {
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
        let budget = if mem_pages > 0 {
            opt_cap(recv_cap(spawn_ep).1)
        } else {
            None
        };

        let elf = prog.and_then(|p| progs[p.id() as usize]);

        if interruptible {
            // Build the whole child from the shell's job untyped, mapping the shared job frame; no
            // capabilities in its cspace (it reports through the frame and exits). SPAWN_OK is the
            // go-ahead the shell waits for before it starts watching the frame.
            let built = match (elf, job_ut, job_fr) {
                (Some(e), Some(ut), Some(fr)) => {
                    build_child(ut, e, &[], &[(CHILD_JOBFRAME_VA, fr, abi::aspace::MAP_RW)]).ok()
                }
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
            let mut caps = [out; 3];
            let mut n = 1usize;
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
            let built = elf.and_then(|e| build_child(UNTYPED, e, &caps[..n], &[]).ok());
            let ok = match built {
                Some(tcb) => {
                    let started = tcb_start(tcb, 0, arg, 0) == 0;
                    cap_delete(tcb);
                    started
                }
                None => false,
            };
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
        }

        // Drop our copies of every delegated cap: the child holds what it needs (the job frame is
        // mapped, the budget and the streams inserted), and the shell holds the originals it kept
        // (the job untyped for teardown, the pipe it minted). This keeps init's 16-slot cspace from
        // filling across a long session.
        for s in [job_ut, job_fr, sink, source, budget].into_iter().flatten() {
            cap_delete(s);
        }
    }
}

/// Our ever-advancing scratch window: where we temporarily map each child frame to fill it. Never
/// unmapped, so a per-call reset would collide with a prior child's mappings. Starts below the initrd.
static SCRATCH_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x1000_0000);

/// Build a child from `elf` out of our budget: lay each segment W^X at the VA it names, map a stack,
/// map `maps` (each `(child_va, our_slot, mode)`), retype a TCB, insert `caps` (each `(our_slot,
/// rights)`) in order, configure at the entry. Returns the TCB slot, ready to start. The userspace
/// ELF loader, driven entirely through the capability verbs.
fn build_child(
    build_ut: u64,
    elf: &elf::Elf,
    caps: &[(u64, u64)],
    maps: &[(u64, u64, u64)],
) -> Result<u64, ()> {
    // The child's aspace, code/data frames, stack, and TCB all come from `build_ut`. For a normal
    // job that is our own budget (slot 0); for a supervised (interruptible) job it is the untyped
    // the shell delegated, so the whole child lives in a region the shell holds and can `DESTROY`
    // to tear it down (milestone 24). Our *scratch* mappings below stay on our own budget (UNTYPED):
    // they are ours, and a child's region must not have our page tables freed under it on teardown.
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
            if unsafe { invoke(frame, abi::frame::MAP, scratch, 1, UNTYPED) } != 0 {
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
        if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, our_slot, mode) } != 0 {
            return Err(());
        }
    }

    let tcb = retype_obj_from(build_ut, abi::objtype::TCB)?;
    for &(our_slot, rights) in caps {
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, our_slot, rights, 0) } < 0 {
            return Err(());
        }
    }
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
    let r = unsafe { invoke(ut, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

fn retype_frame() -> Result<u64, ()> {
    retype_frame_from(UNTYPED)
}

fn retype_frame_from(ut: u64) -> Result<u64, ()> {
    let r = unsafe { invoke(ut, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Carve `pages` off our own untyped into a new child untyped we can delegate (milestone 31). The
/// SPLIT grants us full rights on the child, including GRANT, so we can hand a memory budget on.
fn untyped_split(pages: u64) -> Result<u64, ()> {
    let r = unsafe { invoke(UNTYPED, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

fn tcb_start(tcb: u64, a0: u64, a1: u64, a2: u64) -> i64 {
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
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    exit();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
