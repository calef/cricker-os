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

use user_rt::{cap_delete, exit, invoke, recv, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// The capabilities the kernel grants before this program runs.
const UNTYPED: u64 = 0; // the building budget
const UART_DEV: u64 = 1; // the UART registers, a device cap to delegate into the drivers
const UART_IRQ: u64 = 2; // the UART receive interrupt, an Irq cap to delegate into the input driver

const PAGE: u64 = 4096;
const CHILD_STACK_VA: u64 = 0x0050_0000;
const CHILD_STACK_PAGES: u64 = 4;

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

    // 4. The shell: prints and reads lines through the terminal, holds the spawn channel.
    let shell = must(build_child(
        &sh_elf,
        &[
            (term_ep, abi::rights::WRITE),
            (spawn_ep, abi::rights::WRITE),
            (result_ep, abi::rights::READ),
        ],
        &[
            (SH_OUT_VA, term_out, abi::aspace::MAP_RW), // shell writes text and prompts here
            (LINE_VA, term_in, abi::aspace::MAP_RO),    // shell reads completed lines
        ],
    ));
    must0(tcb_start(shell, 0, 0, 0));
    cap_delete(shell);

    // The spawn service. The shell SENDs `n` on `spawn_ep`; we build a `worker` endowed with
    // `result_ep` (WRITE) and start it with `n` in a1. The worker squares `n`, SENDs the answer
    // straight to `result_ep`, and exits; the shell, holding `result_ep` for READ, receives it. We
    // never see the answer, only build the pipe. If the build fails (budget spent), answer u64::MAX so
    // the shell degrades gracefully instead of hanging.
    let worker = fs.read("worker").and_then(|b| elf::Elf::parse(b).ok());
    loop {
        let n = recv(spawn_ep).0;
        let built = worker
            .as_ref()
            .and_then(|w| build_child(w, &[(result_ep, abi::rights::WRITE)], &[]).ok());
        match built {
            Some(w) => {
                if tcb_start(w, 0, n, 0) != 0 {
                    send(result_ep, u64::MAX, 0, 0);
                }
                cap_delete(w);
            }
            None => {
                send(result_ep, u64::MAX, 0, 0);
            }
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
