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
//! 3. the **line discipline** (`termd`, milestone 28): editing, echo, history, between them;
//! 4. the **shell**: prints and reads lines through the terminal endpoint, runs commands;
//!
//! wired together with endpoints and shared pages this program creates. The kernel wires none of it.
//! Then it stays alive as the spawn service: `run <n>` in the shell asks it to build a `worker` that
//! returns n*n. Nothing here names an architecture: the console and input drivers hold the one
//! device-specific fact (the UART register layout), and the kernel grants the right device.

#![no_std]
#![no_main]

use capsh::{Prog, spawnproto};
use user_rt::{cap_delete, exit, invoke, recv, recv_cap, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// The capabilities the kernel grants before this program runs.
const UNTYPED: u64 = 0; // the building budget
const UART_DEV: u64 = 1; // the UART registers, a device cap to delegate into the drivers
const UART_IRQ: u64 = 2; // the UART receive interrupt, an Irq cap to delegate into the input driver

const PAGE: u64 = 4096;
const CHILD_STACK_VA: u64 = 0x0050_0000;
const CHILD_STACK_PAGES: u64 = 4;

/// Pages of untyped we split off our own budget and hand the shell (milestone 31), so the shell can
/// in turn endow the programs it spawns (`run --mem N`) out of a budget that is genuinely *its own*.
/// The shell shrinks this by N pages per grant; the pages a spawned child pins are not reclaimed in
/// phase 1, so this is a session budget, not a renewable one.
const SH_BUDGET_PAGES: u64 = 128;

// The VAs each program hardcodes; they must match console.rs / input.rs / termd.rs / shell.rs.
const CON_SHARED_VA: u64 = 0x0060_0000; // console reads text here; termd writes it
const CON_UART_VA: u64 = 0x0070_0000; // console's UART mapping
const TERM_OUT_VA: u64 = 0x0080_0000; // termd reads the shell's text/prompts here
const TERM_IN_VA: u64 = 0x0090_0000; // termd delivers completed lines here
const IN_UART_VA: u64 = 0x00a0_0000; // input driver's UART mapping
const SH_OUT_VA: u64 = 0x0060_0000; // the shell's view of the TERM_OUT frame
const LINE_VA: u64 = 0x00b0_0000; // the shell's view of the TERM_IN frame

#[unsafe(no_mangle)]
pub extern "C" fn _start(_x0: u64, initrd_len: u64, _x2: u64) -> ! {
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
    let Some(td_elf) = fs.read("termd").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };
    let Some(sh_elf) = fs.read("shell").and_then(|b| elf::Elf::parse(b).ok()) else {
        fail()
    };

    // The endpoints and shared pages we own and hand out, each retyped with full rights so we can
    // delegate narrowed views. `term_ep` is the terminal contract's one endpoint: the discipline
    // serves it; the input driver and the shell only hold WRITE on it, and neither can tell what
    // is on the other side (notes/terminal-contract.md).
    let request = must(retype_obj(abi::objtype::ENDPOINT));
    let reply = must(retype_obj(abi::objtype::ENDPOINT));
    let term_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let spawn_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let result_ep = must(retype_obj(abi::objtype::ENDPOINT));
    let con_shared = must(retype_frame()); // termd -> console text
    let term_out = must(retype_frame()); // shell -> termd text and prompts
    let term_in = must(retype_frame()); // termd -> shell completed lines

    // 1. Console server: reads text from the shared page, writes it to the UART.
    let con = must(build_child(
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
    let termd = must(build_child(
        &td_elf,
        &[
            (term_ep, abi::rights::READ),
            (request, abi::rights::WRITE),
            (reply, abi::rights::READ),
        ],
        &[
            (CON_SHARED_VA, con_shared, abi::aspace::MAP_RW), // termd fills what the console reads
            (TERM_OUT_VA, term_out, abi::aspace::MAP_RO),
            (TERM_IN_VA, term_in, abi::aspace::MAP_RW),
        ],
    ));
    must0(tcb_start(termd, 0, 0, 0));
    cap_delete(termd);

    // 3. Input driver: waits on the UART receive interrupt, forwards raw bytes to the terminal.
    let input = must(build_child(
        &in_elf,
        &[(term_ep, abi::rights::WRITE), (UART_IRQ, abi::rights::READ)],
        &[(IN_UART_VA, UART_DEV, abi::aspace::MAP_RO)],
    ));
    must0(tcb_start(input, 0, 0, 0));
    cap_delete(input);

    // 4. The shell: prints and reads lines through the terminal, holds the spawn channel, and holds
    // its own untyped budget (slot 3) so `run --mem N` grants from memory that is genuinely the
    // shell's. WRITE lets it SPLIT the budget; GRANT lets it delegate the split to init. We carve
    // that budget from our own untyped and hand it over the same way we hand any capability.
    let sh_budget = must(untyped_split(SH_BUDGET_PAGES));
    let shell = must(build_child(
        &sh_elf,
        &[
            (term_ep, abi::rights::WRITE),
            (spawn_ep, abi::rights::WRITE),
            (result_ep, abi::rights::READ),
            (sh_budget, abi::rights::WRITE | abi::rights::GRANT),
        ],
        &[
            (SH_OUT_VA, term_out, abi::aspace::MAP_RW), // shell writes text and prompts here
            (LINE_VA, term_in, abi::aspace::MAP_RO),    // shell reads completed lines
        ],
    ));
    must0(tcb_start(shell, 0, 0, 0));
    cap_delete(shell);
    cap_delete(sh_budget); // our copy; the shell holds its own now

    // The spawn service (milestone 31's grant expression, wire half; capsh::spawnproto). The shell
    // resolved a `run` into a program, an argument, and a memory-grant page count, and it directs us
    // rather than building the child itself: we hold the initrd, so we stay the ELF loader (the
    // parser lives in one place, out of the shell). We endow every child the result endpoint at slot
    // 0, and the untyped the shell delegates at slot 1 when a `--mem` grant rode along. Nothing else:
    // the child's authority is exactly what the command line named. See the spawn_service comment.
    let worker = fs.read("worker").and_then(|b| elf::Elf::parse(b).ok());
    let budgeter = fs.read("budgeter").and_then(|b| elf::Elf::parse(b).ok());
    spawn_service(spawn_ep, result_ep, worker.as_ref(), budgeter.as_ref())
}

/// The spawn service loop: serve the shell's `run` requests forever. Init is the ELF loader the
/// shell directs; it inserts only what the shell endows plus the shared report channel, so a
/// spawned program can reach nothing the command line did not name.
///
/// The exchange (capsh::spawnproto): the shell `SEND`s the request (program id, argument, page
/// count); if the count is non-zero, it `SEND_CAP`s exactly one capability next, an untyped it split
/// from its own budget, which we receive here. We build the child, endow it, and start it with the
/// argument in a1. On any failure we send the spawn-failed sentinel on the result endpoint so the
/// shell's single read completes with a legible failure instead of hanging.
fn spawn_service(
    spawn_ep: u64,
    result_ep: u64,
    worker: Option<&elf::Elf>,
    budgeter: Option<&elf::Elf>,
) -> ! {
    loop {
        let (w0, w1, w2) = recv(spawn_ep);
        let prog = Prog::from_id(spawnproto::prog_id(w0));
        let arg = spawnproto::arg(w1);
        let mem_pages = spawnproto::mem_pages(w2);

        // A promised memory grant arrives as the delegated untyped over the next SEND_CAP; no
        // promise, no receive, so the two sides stay in lockstep on the endpoint.
        let budget = if mem_pages > 0 {
            let slot = recv_cap(spawn_ep).1;
            if slot == abi::endpoint::NO_CAP {
                None
            } else {
                Some(slot)
            }
        } else {
            None
        };

        let elf = match prog {
            Some(Prog::Worker) => worker,
            Some(Prog::Budgeter) => budgeter,
            None => None,
        };

        let built = elf.and_then(|e| match budget {
            // The delegated budget goes in narrowed to WRITE: the child may spend it, not lend it.
            Some(b) => build_child(
                e,
                &[(result_ep, abi::rights::WRITE), (b, abi::rights::WRITE)],
                &[],
            )
            .ok(),
            None => build_child(e, &[(result_ep, abi::rights::WRITE)], &[]).ok(),
        });

        match built {
            Some(tcb) => {
                if tcb_start(tcb, 0, arg, 0) != 0 {
                    send(result_ep, spawnproto::SPAWN_FAILED, 0, 0);
                }
                cap_delete(tcb);
            }
            None => {
                send(result_ep, spawnproto::SPAWN_FAILED, 0, 0);
            }
        }
        // Drop our copy of the delegated budget (it is the child's now), so a long session of
        // `run --mem` does not exhaust init's own 16-slot cspace. A no-op when there was none.
        if let Some(b) = budget {
            cap_delete(b);
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
fn build_child(elf: &elf::Elf, caps: &[(u64, u64)], maps: &[(u64, u64, u64)]) -> Result<u64, ()> {
    let aspace = retype_obj(abi::objtype::ASPACE)?;

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
            let frame = retype_frame()?;
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
        let stack_frame = retype_frame()?;
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

    let tcb = retype_obj(abi::objtype::TCB)?;
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
    let r = unsafe { invoke(UNTYPED, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

fn retype_frame() -> Result<u64, ()> {
    let r = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
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
