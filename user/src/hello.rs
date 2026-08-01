//! The initrd program. **One binary, two roles**, chosen by the argument the kernel puts in
//! `x0` at `_start`, the way a real kernel hands a new process its argc.
//!
//! - **Role `CLIENT`**: an ordinary program that wants to print. It does not own a UART and
//!   cannot reach one. It writes its text into a page it *shares* with the console server, and
//!   sends the length over an endpoint. That is the whole of "printing" now.
//!
//! - **The console driver, at EL0** (milestone 8: this code used to be in the kernel; milestone 8
//!   is where it left). It owns a mapping of the PL011's registers and a read-only view of the
//!   shared page, and loops: receive a length, copy that many bytes from the shared page to the
//!   UART, acknowledge. It is its own binary now (`user/src/console.rs`, 19f.3), no longer a role
//!   of hello, so hello keeps only the printing client that drives it.
//!
//! # Why the bytes travel in shared memory and the length travels in a message
//!
//! DECISIONS §10: **IPC carries control, shared memory carries data.** The kernel is not in the
//! data path at all. It never sees the bytes, never copies them, never validates a pointer into
//! them. The confused-deputy problem that 7d had to defend against **cannot arise here**, because
//! the thing that could be confused (a kernel doing I/O for a user) no longer exists. The
//! architecture dissolved the bug.

#![no_std]
#![no_main]

mod virtio;

use abi::{Error, endpoint};
use capsh::{Prog, spawnproto};
use user_rt::{call, cap_delete, exit, invoke, recv, recv_cap as rt_recv_cap, send, yield_now};

/// Roles, as passed in `x0` by the kernel.
///
/// One binary, several behaviours. The kernel chooses by the argument it puts in `x0`, the way a
/// real kernel hands a new process its argv. A `SELF_CHECK` client needs no capabilities and no
/// shared memory (it only inspects its own image), which is why the milestone-7 tests can spawn
/// it bare; a `PRINTING` client needs the console endpoints and the shared page.
const SELF_CHECK: u64 = 0;
// Role 1 was the console server; it is its own binary now (`user/src/console.rs`, 19f.3).
const PRINTING: u64 = 2;
const VIRTIO_BLK: u64 = 3;
// Role 4 was the input driver; it is its own binary now (`user/src/input.rs`, 19f.4).
// Role 5 was the shell; it is its own binary now (`user/src/shell.rs`, 19f.5).
// Role 6 was the worker; it is its own binary now (`user/src/worker.rs`, 19f.2). init loads each of
// these from the archive by name; hello keeps only the milestone-tour demo roles below.
const UNTYPED_DEMO: u64 = 7;
const VIRTIO_ATTACK: u64 = 8;
const GRANTER: u64 = 9;
const RECEIVER: u64 = 10;
const FRAME_PRODUCER: u64 = 11;
const FRAME_CONSUMER: u64 = 12;
const VIRTIO_ATTACK_INDIRECT: u64 = 13;
const CALL_SERVER: u64 = 14;
const CALL_CLIENT: u64 = 15;
const REVOKE_DEMO: u64 = 16;
const EP_MAKER: u64 = 17;
const EP_USER: u64 = 18;
const ASPACE_BUILDER: u64 = 19;
const INIT: u64 = 20;
const CHILD: u64 = 21;
const DEV_CHILD: u64 = 22;
const IRQ_CHILD: u64 = 26;
// 23-25 and 27-29 are init roles, declared below with their functions.
const VIRTIO_BLK_WRITE: u64 = 30;
const VIRTIO_BLK_WRITE_ABANDON: u64 = 31;
/// The virtio-net driver (milestone 30); matches kernel/src/user.rs `virtio_service` and blk.rs.
const VIRTIO_NET: u64 = 40;
const VIRTIO_BLK_SERVER: u64 = 32;

/// The word the frame producer writes into a shared page and the consumer reads back through its
/// own mapping of the same physical page. One binary, so one constant serves both roles.
const FRAME_SENTINEL: u64 = 0xF00D_CAFE_D00D_1234;

// --- the shared layout, known to both roles because they are the same binary ---

/// The page shared between the printing client and the console server. The client writes text here;
/// the server reads it. Mapped read/write in the client, read-only in the server. (The console
/// server itself is its own binary now, `user/src/console.rs`, 19f.3; hello keeps only the client.)
const SHARED_VA: u64 = 0x0000_0000_0060_0000;

// --- capability slots, by convention (the kernel granted them in this order) ---

/// Client: slot 0 sends the print request. Server: slot 0 receives it.
const REQUEST: u64 = 0;
/// Client: slot 1 receives the ack. Server: slot 1 sends it.
const REPLY: u64 = 1;

// --- markers, so the client can check its own image was loaded correctly ---

#[unsafe(no_mangle)]
static RODATA_MARKER: [u8; 4] = [0xc0, 0xff, 0xee, 0xd0];
#[unsafe(no_mangle)]
static mut DATA_MARKER: u64 = 0x0000_c0ff_ee00_d0d0;
#[unsafe(no_mangle)]
static mut BSS_MARKER: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, _arg2: u64) -> ! {
    match role {
        PRINTING => printing_client(),
        VIRTIO_BLK => virtio::run(dma_phys),
        UNTYPED_DEMO => untyped_demo(),
        VIRTIO_ATTACK => virtio::run_attack(dma_phys),
        VIRTIO_ATTACK_INDIRECT => virtio::run_attack_indirect(dma_phys),
        VIRTIO_BLK_WRITE => virtio::run_write(dma_phys),
        VIRTIO_BLK_WRITE_ABANDON => virtio::run_write_abandon(dma_phys),
        VIRTIO_NET => virtio::run_net(dma_phys),
        VIRTIO_BLK_SERVER => virtio::run_blk_server(dma_phys),
        CALL_SERVER => call_server(),
        CALL_CLIENT => call_client(),
        REVOKE_DEMO => revoke_demo(),
        GRANTER => granter(),
        RECEIVER => receiver(),
        FRAME_PRODUCER => frame_producer(),
        FRAME_CONSUMER => frame_consumer(),
        EP_MAKER => ep_maker(),
        EP_USER => ep_user(),
        ASPACE_BUILDER => aspace_builder(),
        INIT => init(dma_phys), // x1 carries the initrd length
        INIT_DEV => init_dev(dma_phys),
        INIT_CONSOLE => init_console(dma_phys),
        INIT_IRQ => init_irq(dma_phys),
        IRQ_CHILD => irq_child(),
        INIT_BOOT => init_boot(dma_phys),
        INIT_WORKER => init_worker(dma_phys),
        INIT_COREMARK => init_coremark(dma_phys),
        CHILD => child(),
        DEV_CHILD => dev_child(),
        SELF_CHECK => self_check_client(),
        _ => self_check_client(),
    }
}

/// Prove our own image is intact. None of this needs a capability: it is all our own memory. A
/// mismatch means the loader is broken, and we say so the only way we can, with a `brk` that the
/// kernel turns into a fault.
fn self_check() {
    check(RODATA_MARKER == [0xc0, 0xff, 0xee, 0xd0]);
    // SAFETY: single-threaded, sole owner of this address space.
    unsafe {
        check(core::ptr::read_volatile(&raw const DATA_MARKER) == 0x0000_c0ff_ee00_d0d0);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 0); // .bss was zeroed
        core::ptr::write_volatile(&raw mut BSS_MARKER, 1);
        check(core::ptr::read_volatile(&raw const BSS_MARKER) == 1); // .data is writable
    }
    check(stack_works(7));
}

/// A program that checks its own image and then does nothing but exist. Needs no capabilities.
/// This is the "a real ELF ran and verified itself" program the milestone-7 tests spawn bare.
fn self_check_client() -> ! {
    self_check();

    // Make one syscall that needs no capability at all, to prove we reached EL0 and can trap
    // back in. Yield is authority over ourselves; nobody has to grant it.
    yield_now();

    // Then exit rather than spin. This is a one-shot role with nothing left to do, and a user
    // thread that never exits sits on a core for the rest of the boot: `no_leaked_threads` says so
    // in as many words, and it was three such leaks that starved a later test off a four-hart
    // machine entirely.
    exit();
}

/// A program that checks its own image and then prints, through the console server, using the
/// endpoints and shared page the kernel handed it.
fn printing_client() -> ! {
    self_check();

    // These cannot fail: this role is only ever spawned WITH the console, so `print` holds its
    // capabilities. A failure would be a `brk`, which is what we want if the wiring is wrong.
    check(print(b"      hello from EL0, printed by a driver that also runs at EL0.\n").is_ok());
    check(print(b"      the kernel never saw these bytes.\n").is_ok());

    // Done, so exit. This used to spin ("so the timer can prove it still preempts us"), which the
    // dedicated `spinner` binary proves better and without leaving a CPU-bound thread behind for
    // the rest of the run. See `self_check_client`.
    exit();
}

/// Print `bytes` by handing them to the console server through shared memory.
///
/// Returns `Ok` if we hold the endpoints to reach the server, `Err(NoSuchSlot)` if we were not
/// given them. The bytes go in the shared page; only the length crosses the endpoint.
fn print(bytes: &[u8]) -> Result<(), Error> {
    let n = bytes.len().min(4096);

    // SAFETY: the shared page is mapped read/write in our address space. We own it between an
    // ack and the next send, which the reply below is what guarantees.
    let shared = SHARED_VA as *mut u8;
    for (i, &b) in bytes[..n].iter().enumerate() {
        unsafe { core::ptr::write_volatile(shared.add(i), b) };
    }

    // The length is the message. The data is already in place, shared, uncopied.
    let r = unsafe { invoke(REQUEST, endpoint::SEND, n as u64, 0, 0) };
    if let Some(e) = Error::from_ret(r) {
        return Err(e); // e.g. NoSuchSlot: we were not handed a console
    }

    // Wait for the server to finish reading the buffer before we touch it again.
    let (_ack, _, _) = recv(REPLY);
    Ok(())
}

// The IPC primitives (send/recv/invoke/exit) come from the shared `user_rt` crate (19f.6).

/// Receive a data word and, if the sender delegated one, a capability. Returns `(w0, slot)`, where
/// `slot` is where the received capability landed in our cspace, or `endpoint::NO_CAP` if none came.
///
/// A thin shape over `user_rt::recv_cap`, which returns the third word this caller does not want.
fn recv_cap(slot: u64) -> (u64, u64) {
    let (w0, got, _) = rt_recv_cap(slot);
    (w0, got)
}

/// **The Call/Reply server, milestone 12.** Holds `RECV` on a request endpoint (slot 0) and a report
/// endpoint (slot 1). It answers one caller it was never individually wired to, then proves the
/// reply capability is one-shot by trying to use it a second time and reporting that the kernel
/// refused. See kernel/src/user.rs call_service.
fn call_server() -> ! {
    const EP: u64 = 0;
    const REPORT: u64 = 1;

    let (w0, reply_slot, w1) = rt_recv_cap(EP);
    // Answer the caller: w0 + w1. This consumes the one-shot reply capability.
    // SAFETY: `svc`; the kernel validates the reply capability in `reply_slot`.
    check(unsafe { invoke(reply_slot, abi::reply::REPLY, w0 + w1, 0, 0) } == 0);
    // A second reply on the same slot must fail: the cap was consumed on first use.
    // SAFETY: `svc`.
    let second = unsafe { invoke(reply_slot, abi::reply::REPLY, 0xBAD, 0, 0) };
    send(REPORT, if second < 0 { 1 } else { 0 }, 0, 0); // 1 = refused (one-shot held), 0 = a hole
    exit();
}

/// **The Call/Reply client, milestone 12.** Holds `WRITE` on the request endpoint (slot 0) and a
/// report endpoint (slot 1). It calls with two words and reports the reply.
fn call_client() -> ! {
    const EP: u64 = 0;
    const REPORT: u64 = 1;

    let (r0, _r1) = call(EP, 40, 2); // expect 42 back
    send(REPORT, r0, 0, 0);
    exit();
}

/// **The revoke demo, milestone 13.** Holds an untyped budget (slot 0) and a report endpoint (slot
/// 1). It retypes a page, maps it, then `REVOKE`s it: the kernel unmaps the page and deletes every
/// capability to it, this process's own included, so a second operation on the frame slot finds
/// nothing there. Reports 1 if REVOKE succeeded and the slot is now empty. See kernel/src/user.rs
/// revoke_service.
fn revoke_demo() -> ! {
    const UNTYPED: u64 = 0; // retype + page tables
    const REPORT: u64 = 1;
    const VA: u64 = 0x0000_0000_00c0_0000;

    // Retype a page into a Frame capability we hold, then map it writable.
    // SAFETY: `svc`. The result is the slot the new capability landed in.
    let frame = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
    check(frame >= 0);
    let frame = frame as u64;
    check(unsafe { invoke(frame, abi::frame::MAP, VA, 1, UNTYPED) } == 0);
    // SAFETY: VA is now a mapped, writable page in our address space.
    unsafe { core::ptr::write_volatile(VA as *mut u64, 0xABCD) };

    // Revoke: unmap the page everywhere and delete every capability to it, ours included. The frame
    // was retyped with GRANT, so we are allowed to. SAFETY: `svc`.
    let revoked = unsafe { invoke(frame, abi::frame::REVOKE, 0, 0, 0) };
    // Our Frame capability is gone now: a second operation on that slot must fail (NoSuchSlot). We
    // do NOT touch VA again, which is unmapped and would fault. SAFETY: `svc`.
    let after = unsafe { invoke(frame, abi::frame::MAP, VA, 1, UNTYPED) };

    send(REPORT, if revoked == 0 && after < 0 { 1 } else { 0 }, 0, 0);
    exit();
}

/// Where the kernel maps the initrd into init (must match user.rs INITRD_VA).
const INITRD_VA: u64 = 0x2000_0000;

/// The bytes of the program named `name` in the initrd (milestone 19f). The initrd is a crickerfs
/// archive the kernel maps read-only at [`INITRD_VA`]; init indexes it by name rather than treating
/// the whole blob as a single ELF. `initrd_len` (the archive length) arrives in `x1` at entry.
/// Returns `None` if the archive will not parse or holds no such program.
///
/// Through 19f.1 every program is still a role of *this* binary, so callers look up `"init"` (the
/// binary the kernel loaded) and enter it at a different role; 19f.2 adds distinct entries a caller
/// can name directly (`"worker"` and so on).
fn program(initrd_len: u64, name: &str) -> Option<&'static [u8]> {
    // SAFETY: the kernel mapped `initrd_len` bytes of the initrd, read-only, at INITRD_VA. It is
    // reserved RAM that outlives every process, so the 'static lifetime is honest.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    crickerfs::Fs::parse(archive).ok()?.read(name)
}

/// **The archive entry holding this binary**, which is not the same name on both machines.
///
/// Several init roles build a child out of *this* program's own ELF and re-enter it at a different
/// role ([`CHILD`], [`DEV_CHILD`], [`IRQ_CHILD`]). To do that they have to find hello in the archive,
/// and the name it is packed under differs: aarch64 packs hello as `init`, because there hello *is*
/// the boot program; RISC-V's `init` is the portable `builder` demo, so hello goes in under its own
/// name. The kernel side of the same fact is `kernel::user::INIT_ROLES_ENTRY`; the two must agree.
///
/// This was a hardcoded `"init"`, which is right on aarch64 and silently wrong on RISC-V: init
/// happily built a child out of `builder`'s ELF and started it at a role `builder` does not have, so
/// the child reached for an initrd mapping it did not own, faulted, was killed, and the test waiting
/// on its report blocked until the watchdog fired. Nothing said "wrong program"; it just never
/// answered.
#[cfg(target_arch = "aarch64")]
const ROLES_ENTRY: &str = "init";
#[cfg(target_arch = "riscv64")]
const ROLES_ENTRY: &str = "hello";

/// The init role that builds a device-driver child (milestone 19d.2); matches kernel test wiring.
const INIT_DEV: u64 = 23;
/// The init role that brings up the real console server and prints through it (milestone 19d.2b).
const INIT_CONSOLE: u64 = 24;
/// The init role that builds an interrupt-driven child, to prove IRQ delegation (milestone 19d.2b).
const INIT_IRQ: u64 = 25;
/// The init role that IS the boot path: brings up the console and announces the system (19d.2c).
const INIT_BOOT: u64 = 27;
/// The init role that builds a worker, passes it an argument via START, and reports its answer
/// (milestone 19e: the first workload that needs START to carry data, not just a role).
const INIT_WORKER: u64 = 28;
/// The argument init hands its worker in the [`INIT_WORKER`] role; the worker returns its square.
const WORKER_INPUT: u64 = 7;
/// The init role that builds the CoreMark compute workload and reports the CRC it computed
/// (milestone 19e: the first *real* workload, not a toy).
const INIT_COREMARK: u64 = 29;
/// The word a milestone-19d child reports through the endpoint init granted it.
const CHILD_WORD: u64 = 0xC0FFEE;

/// **The init task, milestone 19d.** The first program the kernel starts, and the one that
/// starts the others: the ELF parser lives here, in userspace, not in the kernel. init holds a
/// building untyped (slot 0) and a report endpoint (slot 1, `WRITE|GRANT`); the initrd is mapped
/// read-only at [`INITRD_VA`], and its length arrives in `x1`.
///
/// It parses that ELF (the `elf` crate, linked into userspace) and loads it as a **child**: a
/// second instance of this same program, entered at role [`CHILD`], built entirely by init out
/// of its own budget through the granular verbs (retype an address space, copy each segment into
/// retyped frames and map them in, retype a TCB, endow it, configure, start). The child reports
/// a word home; receiving it proves init parsed a real ELF and built a running process, with the
/// kernel never touching the child's bytes. See kernel/src/user.rs spawn_init.
fn init(initrd_len: u64) -> ! {
    init_build(initrd_len, false)
}

/// **The boot init, milestone 19d.2c.** This is the program the kernel hands the machine to, and
/// it builds the whole interactive system out of its own budget: a console server (output), an
/// input driver (keystrokes, on the UART receive interrupt), and the shell, wired together with
/// endpoints and shared pages init creates. The kernel wires none of it; init is the system
/// builder. init then stays alive as the spawn service: `run <n>` in the shell asks init to build
/// a worker that returns n*n, started with `n` in x1 (the multi-arg START of milestone 19e).
fn init_boot(_x1: u64) -> ! {
    // Read the initrd length from x1 (passed by spawn_init); the arg name is generic in _start.
    // SAFETY: the kernel mapped the initrd read-only at INITRD_VA; its length is in x1.
    let initrd_len = _x1;
    const UNTYPED: u64 = 0;
    const UART_DEV: u64 = 2;
    const UART_IRQ: u64 = 4;
    // Pages we split off our own budget and hand the shell, so `run --mem N` grants memory that is
    // genuinely the shell's own (milestone 31). Must match shell.rs's SH_BUDGET_PAGES.
    const SH_BUDGET_PAGES: u64 = 128;

    // The VAs each program hardcodes. Console, input, lineedit, and shell are all their own
    // binaries (19f.3-5, 28), each started with no role; the VAs must match
    // console.rs/input.rs/lineedit.rs/shell.rs.
    const CON_SHARED_VA: u64 = 0x0060_0000; // console reads text here; lineedit writes it
    const CON_UART_VA: u64 = 0x0070_0000; // console's UART mapping
    const TERM_OUT_VA: u64 = 0x0080_0000; // lineedit reads the shell's text/prompts here
    const TERM_IN_VA: u64 = 0x0090_0000; // lineedit delivers completed lines here
    const IN_UART_VA: u64 = 0x00a0_0000; // input driver's UART mapping
    const SH_OUT_VA: u64 = 0x0060_0000; // the shell's view of the TERM_OUT frame
    const LINE_VA: u64 = 0x00b0_0000; // the shell's view of the TERM_IN frame
    const DEV: u64 = abi::aspace::MAP_RO; // mode arg ignored for a DeviceFrame cap

    // Every service init builds is now its own binary, loaded from the archive by name. init itself
    // is the only role of hello left on this path, and it loads nothing of hello into a child.
    let Some(con_bytes) = program(initrd_len, "console") else {
        halt_forever()
    };
    let Ok(con_elf) = elf::Elf::parse(con_bytes) else {
        halt_forever()
    };
    let Some(in_bytes) = program(initrd_len, "input") else {
        halt_forever()
    };
    let Ok(in_elf) = elf::Elf::parse(in_bytes) else {
        halt_forever()
    };
    let Some(td_bytes) = program(initrd_len, "lineedit") else {
        halt_forever()
    };
    let Ok(td_elf) = elf::Elf::parse(td_bytes) else {
        halt_forever()
    };
    let Some(sh_bytes) = program(initrd_len, "shell") else {
        halt_forever()
    };
    let Ok(sh_elf) = elf::Elf::parse(sh_bytes) else {
        halt_forever()
    };

    // The endpoints and shared pages init owns and hands out. Endpoints come from retype with full
    // rights, so init keeps RWG and delegates narrowed views. `term_ep` is the terminal contract's
    // one endpoint (milestone 28, notes/terminal-contract.md): the line discipline serves it; the
    // input driver and the shell hold WRITE and cannot tell what is behind it.
    let Ok(request) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        halt_forever()
    };
    let Ok(reply) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        halt_forever()
    };
    let Ok(term_ep) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        halt_forever()
    };
    let Ok(spawn_ep) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        halt_forever()
    };
    let Ok(result_ep) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        halt_forever()
    };
    let Ok(con_shared) = retype_frame(UNTYPED) else {
        halt_forever()
    };
    let Ok(term_out) = retype_frame(UNTYPED) else {
        halt_forever()
    };
    let Ok(term_in) = retype_frame(UNTYPED) else {
        halt_forever()
    };

    // 1. Console server: reads text from the shared page, writes it to the UART.
    let con_caps: &[(u64, u64)] = &[(request, abi::rights::READ), (reply, abi::rights::WRITE)];
    let con_maps: &[(u64, u64, u64)] = &[
        (CON_SHARED_VA, con_shared, abi::aspace::MAP_RO),
        (CON_UART_VA, UART_DEV, DEV),
    ];
    let Ok(con) = build_child(UNTYPED, &con_elf, con_caps, con_maps) else {
        halt_forever()
    };
    check(tcb_start(con, 0, 0, 0) == 0); // no role selector: console is its own binary
    cap_delete(con);

    // 2. The line discipline (milestone 28): serves the terminal endpoint, prints through the
    // console. It is the console's only client; everyone else prints through it.
    let td_caps: &[(u64, u64)] = &[
        (term_ep, abi::rights::READ),
        (request, abi::rights::WRITE),
        (reply, abi::rights::READ),
    ];
    let td_maps: &[(u64, u64, u64)] = &[
        (CON_SHARED_VA, con_shared, abi::aspace::MAP_RW), // lineedit fills what the console reads
        (TERM_OUT_VA, term_out, abi::aspace::MAP_RO),
        (TERM_IN_VA, term_in, abi::aspace::MAP_RW),
    ];
    let Ok(lineedit) = build_child(UNTYPED, &td_elf, td_caps, td_maps) else {
        halt_forever()
    };
    check(tcb_start(lineedit, 0, 0, 0) == 0);
    cap_delete(lineedit);

    // 3. Input driver: waits on the UART receive interrupt, forwards raw bytes to the terminal.
    let in_caps: &[(u64, u64)] = &[(term_ep, abi::rights::WRITE), (UART_IRQ, abi::rights::READ)];
    let in_maps: &[(u64, u64, u64)] = &[(IN_UART_VA, UART_DEV, DEV)];
    let Ok(input) = build_child(UNTYPED, &in_elf, in_caps, in_maps) else {
        halt_forever()
    };
    check(tcb_start(input, 0, 0, 0) == 0); // no role selector: input is its own binary
    cap_delete(input);

    // 4. The shell: prints and reads lines through the terminal, holds the spawn channel, and holds
    // its own untyped budget (slot 3) so `run --mem N` grants from memory that is genuinely the
    // shell's. WRITE lets it SPLIT the budget; GRANT lets it delegate the split to init. We carve it
    // from our own untyped and hand it over the same way we hand any capability.
    let Ok(sh_budget) = untyped_split(UNTYPED, SH_BUDGET_PAGES) else {
        halt_forever()
    };
    let sh_caps: &[(u64, u64)] = &[
        (term_ep, abi::rights::WRITE),
        (spawn_ep, abi::rights::WRITE),
        (result_ep, abi::rights::READ),
        (sh_budget, abi::rights::WRITE | abi::rights::GRANT),
    ];
    let sh_maps: &[(u64, u64, u64)] = &[
        (SH_OUT_VA, term_out, abi::aspace::MAP_RW), // shell writes text and prompts here
        (LINE_VA, term_in, abi::aspace::MAP_RO),    // shell reads completed lines
    ];
    let Ok(shell) = build_child(UNTYPED, &sh_elf, sh_caps, sh_maps) else {
        halt_forever()
    };
    check(tcb_start(shell, 0, 0, 0) == 0); // no role selector: shell is its own binary
    cap_delete(shell);
    cap_delete(sh_budget); // our copy; the shell holds its own now

    // Free every boot cap the spawn service does not need, so init's 16-slot cspace has room to
    // build a supervised child (which holds a job untyped and a job frame while build_child retypes
    // an aspace, frames, and a TCB). The drivers and the shell hold the narrowed copies that matter;
    // these originals were only init's to hand out. The service keeps UNTYPED, spawn_ep, result_ep.
    for c in [request, reply, term_ep, con_shared, term_out, term_in] {
        cap_delete(c);
    }

    // The programs the shell can spawn (milestone 31), parsed once from the archive; every spawn
    // request builds a fresh child. A missing or unparseable entry stays None and the service
    // answers "could not spawn" for it.
    let worker = program(initrd_len, "worker").and_then(|bytes| elf::Elf::parse(bytes).ok());
    let budgeter = program(initrd_len, "budgeter").and_then(|bytes| elf::Elf::parse(bytes).ok());
    let heeder = program(initrd_len, "heeder").and_then(|bytes| elf::Elf::parse(bytes).ok());
    let spinner = program(initrd_len, "spinner").and_then(|bytes| elf::Elf::parse(bytes).ok());
    let date = program(initrd_len, "date").and_then(|bytes| elf::Elf::parse(bytes).ok());
    let wc = program(initrd_len, "wc").and_then(|bytes| elf::Elf::parse(bytes).ok());
    // Indexed by Prog::id(): worker=0, budgeter=1, heeder=2, spinner=3, date=4, rm=5. The array is
    // `Prog::PROG_COUNT` long because the service indexes it with an id the shell chose; a variant
    // added to `capsh` without a slot here would be an out-of-bounds read in init.
    let progs: [Option<&elf::Elf>; capsh::PROG_COUNT] = [
        worker.as_ref(),
        budgeter.as_ref(),
        heeder.as_ref(),
        spinner.as_ref(),
        date.as_ref(),
        // `rm` has a slot and no ELF, deliberately: it is endowed a directory capability and this
        // boot wires no FS service to narrow one from, so the shell refuses the command before it
        // gets here. See the same slot in sysinit.
        None,
        // `wc` (milestone 50): the right-hand side of a pipe. It needs no filesystem, so unlike
        // `rm` it is reachable from this prompt.
        wc.as_ref(),
    ];

    // The spawn service (milestone 31's grant expression + milestone 24's supervised jobs;
    // capsh::spawnproto). A **normal** job: the shell sends the request and, if `--mem` rode along,
    // one delegated untyped; init builds the child from its own budget, endows it the result
    // endpoint (and the budget), starts it. A **supervised** (interruptible) job: the shell leads
    // the delegation with a job untyped and a shared job frame; init builds the whole child *from
    // that untyped* (so the shell's region owns it and can `DESTROY` it to tear it down), maps the
    // frame in, endows nothing else, starts it, and sends SPAWN_OK once as the shell's go-ahead.
    loop {
        let (w0, w1, w2) = recv(spawn_ep); // blocks until the shell asks
        let prog = Prog::from_id(spawnproto::prog_id(w0));
        let arg = spawnproto::arg(w1);
        let mem_pages = spawnproto::mem_pages(w2);
        let wiring = spawnproto::wiring(w2);
        let interruptible = wiring.interruptible;

        // Receive the delegated caps in protocol order: the interrupt pair first (job untyped, job
        // frame), then any --mem untyped. No promise, no receive, so both sides stay in lockstep.
        let opt = |slot: u64| {
            if slot == endpoint::NO_CAP {
                None
            } else {
                Some(slot)
            }
        };
        let (job_ut, job_fr) = if interruptible {
            (opt(recv_cap(spawn_ep).1), opt(recv_cap(spawn_ep).1))
        } else {
            (None, None)
        };
        // Milestone 50's two: the child's output slot and its input slot, in protocol order.
        let sink = if wiring.sink {
            opt(recv_cap(spawn_ep).1)
        } else {
            None
        };
        let source = if wiring.source {
            opt(recv_cap(spawn_ep).1)
        } else {
            None
        };
        let budget = if mem_pages > 0 {
            opt(recv_cap(spawn_ep).1)
        } else {
            None
        };

        let elf = prog.and_then(|p| progs[p.id() as usize]);

        if interruptible {
            // Build the whole child from the shell's job untyped, mapping the shared job frame; no
            // caps in its cspace. SPAWN_OK is the go-ahead the shell waits for before it watches.
            let built = match (elf, job_ut, job_fr) {
                (Some(e), Some(ut), Some(fr)) => {
                    build_child(ut, e, &[], &[(CHILD_JOBFRAME_VA, fr, abi::aspace::MAP_RW)]).ok()
                }
                _ => None,
            };
            match built {
                Some(tcb) => {
                    let ok = tcb_start(tcb, 0, arg, 0) == 0;
                    let word = if ok {
                        spawnproto::SPAWN_OK
                    } else {
                        spawnproto::SPAWN_FAILED
                    };
                    send(result_ep, word, 0, 0);
                    cap_delete(tcb);
                }
                None => {
                    send(result_ep, spawnproto::SPAWN_FAILED, 0, 0);
                }
            }
        } else {
            // Slot 0 is the output: the shared result endpoint, or the sink the shell delegated
            // (milestone 50). Slot 1 is the input source when there is one, and otherwise the
            // `--mem` untyped; see sysinit for why that ordering is safe today and where it stops
            // being. The child never learns which of the three its slot 0 holds.
            let out = (sink.unwrap_or(result_ep), abi::rights::WRITE);
            let mut caps = [out; 3];
            let mut n = 1usize;
            if let Some(src) = source {
                // READ only: a pipe's reader must not be able to write back up its own input.
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
                    // x0 unused (standalone binary); the argument is in x1.
                    let started = tcb_start(tcb, 0, arg, 0) == 0;
                    cap_delete(tcb); // init's TCB cap; the child keeps running until it exits
                    started
                }
                None => false,
            };
            // A redirected child's answer goes somewhere else, so the shell has nothing to read;
            // one ack is what stops a failed spawn from being invisible.
            if wiring.sink {
                let word = if ok {
                    spawnproto::SPAWN_OK
                } else {
                    spawnproto::SPAWN_FAILED
                };
                send(result_ep, word, 0, 0);
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

/// Halt this thread forever without exiting, for an init that has nothing left to do but whose
/// exit would tear nothing useful down. A `-> !` helper the `else` arms above can fall into.
fn halt_forever() -> ! {
    loop {
        yield_now();
    }
}

/// **init delegates an interrupt to a driver it builds, milestone 19d.2b.** The third and last
/// delegatable device authority (after endpoints and device MMIO): an *interrupt capability*. init
/// holds one for a test interrupt (slot 3, the kernel routed it); it builds a child and hands it
/// that Irq cap, then starts the child. The child blocks in the interrupt's `WAIT` until the
/// interrupt fires, then reports. Receiving the report proves init can build an interrupt-driven
/// driver -- the mechanism the input and virtio drivers need for their completions.
fn init_irq(initrd_len: u64) -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const TEST_IRQ: u64 = 3; // the Irq cap the kernel granted init (spawn_init)

    let Some(init_bytes) = program(initrd_len, ROLES_ENTRY) else {
        fail_report(REPORT)
    };
    let Ok(elf) = elf::Elf::parse(init_bytes) else {
        fail_report(REPORT)
    };

    // The child gets the report endpoint (slot 0) and the interrupt (slot 1).
    let caps: &[(u64, u64)] = &[
        (REPORT, abi::rights::WRITE),
        (TEST_IRQ, abi::rights::READ), // WAIT/ACK the interrupt
    ];
    let Ok(tcb) = build_child(UNTYPED, &elf, caps, &[]) else {
        fail_report(REPORT)
    };
    check(tcb_start(tcb, IRQ_CHILD, 0, 0) == 0);
    exit();
}

/// **init builds a worker and hands it an argument, milestone 19e.** The first workload that needs
/// `START` to carry *data*, not just a role: every child before this took only its role in `x0`,
/// but a worker computes on an input, and that input has to reach it. init builds a `worker`
/// child endowed with the report endpoint (slot 0) and starts it with [`WORKER_INPUT`] in `x1`
/// (the second `START` argument, new in 19e). The worker squares it and reports home. Receiving
/// `WORKER_INPUT * WORKER_INPUT` proves the argument crossed the `START` boundary intact: the
/// mechanism the interactive `run <n>` command and, later, real spawned services stand on.
fn init_worker(initrd_len: u64) -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;

    // The worker is its own binary now (19f.2), loaded from the archive by name, not a role of this
    // one. init parses it exactly as it parses any program it did not write.
    let Some(worker_bytes) = program(initrd_len, "worker") else {
        fail_report(REPORT)
    };
    let Ok(elf) = elf::Elf::parse(worker_bytes) else {
        fail_report(REPORT)
    };

    // The worker's whole authority: the report endpoint as its slot 0, so its one SEND lands where
    // the test (or, in the boot system, the shell) is waiting.
    let caps: &[(u64, u64)] = &[(REPORT, abi::rights::WRITE)];
    let Ok(tcb) = build_child(UNTYPED, &elf, caps, &[]) else {
        fail_report(REPORT)
    };
    // x0 is unused (a standalone binary needs no role selector); the input is in x1 (the multi-arg
    // START that 19e added).
    check(tcb_start(tcb, 0, WORKER_INPUT, 0) == 0);
    exit();
}

/// **init builds the CoreMark compute workload, milestone 19e: the first real workload.** Same shape
/// as `init_worker`, but the child is the `"coremark"` binary and it computes something substantial
/// (a CoreMark-derived run) rather than a toy square. init grants it the report endpoint (slot 0)
/// and starts it; the workload runs a fixed iteration count and SENDs the run's CRC home. Receiving
/// `coremark::PINNED_CRC_64` proves a real compute program ran correctly against the native ABI.
fn init_coremark(initrd_len: u64) -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;

    let Some(bytes) = program(initrd_len, "coremark") else {
        fail_report(REPORT)
    };
    let Ok(elf) = elf::Elf::parse(bytes) else {
        fail_report(REPORT)
    };
    let caps: &[(u64, u64)] = &[(REPORT, abi::rights::WRITE)];
    let Ok(tcb) = build_child(UNTYPED, &elf, caps, &[]) else {
        fail_report(REPORT)
    };
    check(tcb_start(tcb, 0, 0, 0) == 0); // no args: the workload's iteration count is fixed
    exit();
}

/// **An interrupt-driven child, milestone 19d.2b.** Holds a report endpoint (slot 0) and an
/// interrupt capability (slot 1), both handed to it by init. It waits for the interrupt as a
/// message, then reports the agreed word. Blocking here forever if the interrupt never arrives is
/// the negative case: the test would hang, so a passing test is the interrupt being delivered
/// through the capability init delegated.
fn irq_child() -> ! {
    const REPORT: u64 = 0;
    const IRQ: u64 = 1;
    const IRQ_WORD: u64 = 0x1590; // "IRQ 0" ish; any fixed value the test asserts

    // SAFETY: `svc`; WAIT blocks until the interrupt the kernel routed for this cap fires.
    let _ = unsafe { invoke(IRQ, abi::irq::WAIT, 0, 0, 0) };
    send(REPORT, IRQ_WORD, 0, 0);
    exit();
}

/// **init brings up the real console server, milestone 19d.2b.** The step past 19d.2a's ID-read
/// probe: init builds the *actual* print server (its own `"console"` binary since 19f.3) as a child
/// and drives it. The server needs four things, and init provides all of them out of its own budget
/// and the
/// capabilities it holds: a request endpoint (the server RECVs a length on it), a reply endpoint
/// (it ACKs), a shared page (the client writes text, the server reads it), and the UART's
/// registers (device-typed, from 19d.2a). init then plays the client: it writes a line into the
/// shared page, sends the length, the server prints it to the real UART and acks, and init reports
/// the acked length home. The report proves the whole userspace-built console works: a driver init
/// constructed, wired to a channel init created, driving hardware init delegated.
fn init_console(initrd_len: u64) -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const UART_DEV: u64 = 2;
    const SHARED_VA: u64 = 0x0060_0000; // must match the console server's SHARED_VA
    const CHILD_UART_VA: u64 = 0x0070_0000; // must match the console server's UART_VA

    // The console server is its own binary now (19f.3): init loads "console" by name and builds it,
    // rather than entering hello at a console role.
    let Some(con_bytes) = program(initrd_len, "console") else {
        send(REPORT, 0, 0, 0);
        exit();
    };
    let Ok(elf) = elf::Elf::parse(con_bytes) else {
        send(REPORT, 0, 0, 0);
        exit();
    };

    // The channel to the server, and a shared page to hand it the text.
    let Ok(request) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        fail_report(REPORT)
    };
    let Ok(reply) = retype_obj(UNTYPED, abi::objtype::ENDPOINT) else {
        fail_report(REPORT)
    };
    let Ok(shared) = retype_frame(UNTYPED) else {
        fail_report(REPORT)
    };

    // Map the shared page read/write in init's own space, so init (the client) can write into it.
    if unsafe { invoke(shared, abi::frame::MAP, SHARED_VA, 1, UNTYPED) } != 0 {
        fail_report(REPORT);
    }

    // Build the server: slot 0 = request (READ, it receives), slot 1 = reply (WRITE, it acks);
    // the shared page read-only and the UART device-typed, at the VAs the server expects.
    let caps: &[(u64, u64)] = &[(request, abi::rights::READ), (reply, abi::rights::WRITE)];
    let maps: &[(u64, u64, u64)] = &[
        (SHARED_VA, shared, abi::aspace::MAP_RO),
        (CHILD_UART_VA, UART_DEV, abi::aspace::MAP_RO),
    ];
    let Ok(tcb) = build_child(UNTYPED, &elf, caps, maps) else {
        fail_report(REPORT)
    };
    check(tcb_start(tcb, 0, 0, 0) == 0); // no role selector: console is its own binary

    // Now init is the client. Write a line into the shared page, ask the server to print it.
    let msg = b"cricker-os: the console server was built and started by userspace init.
";
    // SAFETY: init mapped the shared page read/write at SHARED_VA above.
    unsafe {
        let dst = core::slice::from_raw_parts_mut(SHARED_VA as *mut u8, msg.len());
        dst.copy_from_slice(msg);
    }
    check(send(request, msg.len() as u64, 0, 0) == 0); // the server prints, then acks on reply
    let (acked, _, _) = recv(reply);
    send(REPORT, acked, 0, 0); // report the length the server acknowledged
    exit();
}

/// Report a build failure (word 0) and exit. A `-> !` helper so the `else` arms above read cleanly.
fn fail_report(report: u64) -> ! {
    send(report, 0, 0, 0);
    exit();
}

/// A variant of init that builds the **device driver** child (19d.2): same loader, but it hands
/// the child the UART device capability it holds (slot 2). Entered at role [`INIT_DEV`].
fn init_dev(initrd_len: u64) -> ! {
    init_build(initrd_len, true)
}

fn init_build(initrd_len: u64, device: bool) -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const UART_DEV: u64 = 2; // the UART device cap the kernel granted init (spawn_init)
    const CHILD_UART_VA: u64 = 0x0070_0000;

    let Some(init_bytes) = program(initrd_len, ROLES_ENTRY) else {
        send(REPORT, 0, 0, 0);
        exit();
    };
    let elf = match elf::Elf::parse(init_bytes) {
        Ok(e) => e,
        Err(_) => {
            send(REPORT, 0, 0, 0);
            exit();
        }
    };

    // The child's authority: its report endpoint at slot 0 (WRITE). A driver also gets the UART.
    let caps: &[(u64, u64)] = &[(REPORT, abi::rights::WRITE)];
    let no_maps: &[(u64, u64, u64)] = &[];
    let dev_maps: &[(u64, u64, u64)] = &[(CHILD_UART_VA, UART_DEV, abi::aspace::MAP_RO)];
    let maps = if device { dev_maps } else { no_maps };

    match build_child(UNTYPED, &elf, caps, maps) {
        Ok(child_tcb) => {
            let role = if device { DEV_CHILD } else { CHILD };
            check(tcb_start(child_tcb, role, 0, 0) == 0);
        }
        Err(_) => {
            send(REPORT, 0, 0, 0);
        }
    }
    exit();
}

/// **A device driver child, milestone 19d.2.** Built by init exactly like [`child`], but init
/// also mapped a device's MMIO (the PL011 UART) into this child's address space at `UART_VA`
/// before starting it. This child reads the PL011's PrimeCell identification registers, whose
/// values are the fixed `0xB105F00D` ("BIOS FOOD") every real PL011 returns, and reports them.
/// Reading that constant proves the mapping is a real, device-typed view of the actual UART, not
/// normal memory and not the wrong page: init delegated device authority and the driver used it.
fn dev_child() -> ! {
    const REPORT: u64 = 0; // init inserted the report cap as slot 0
    const UART_VA: u64 = 0x0070_0000; // where init mapped the UART registers

    // The four PrimeCell ID bytes live at 0xFF0, 0xFF4, 0xFF8, 0xFFC and read 0x0D,0xF0,0x05,0xB1.
    // SAFETY: init mapped the UART, device-typed, at UART_VA before starting us; these are
    // read-only ID registers, so reading them has no side effect.
    let id = unsafe {
        let base = UART_VA as *const u32;
        let b0 = base.byte_add(0xFF0).read_volatile() & 0xFF;
        let b1 = base.byte_add(0xFF4).read_volatile() & 0xFF;
        let b2 = base.byte_add(0xFF8).read_volatile() & 0xFF;
        let b3 = base.byte_add(0xFFC).read_volatile() & 0xFF;
        (b3 << 24) | (b2 << 16) | (b1 << 8) | b0
    };
    send(REPORT, id as u64, 0, 0);
    exit();
}

/// **A milestone-19d child.** Built by init from an ELF init parsed, entered here with role
/// [`CHILD`] in `x0`. Its whole authority is one capability init granted it in slot 0: a report
/// endpoint. It SENDs the agreed word and exits, which is the observable proof that init's
/// userspace load produced a running thread.
fn child() -> ! {
    const REPORT: u64 = 0; // init inserted the report cap as the child's first slot
    send(REPORT, CHILD_WORD, 0, 0);
    exit();
}

/// Build a child process from `elf`, out of `untyped`. `caps` are inserted into the child's cspace
/// at slots 0, 1, ... in order (each `(init_slot, rights)`: the capability init holds in
/// `init_slot`, narrowed to `rights`). `maps` are extra pages mapped into the child before it
/// starts (each `(child_va, init_slot, mode)`: init's Frame or DeviceFrame cap, mapped at
/// `child_va` with a `MAP_*` mode) -- how init hands a driver its registers and a shared buffer
/// (19d.2). Returns the child's TCB slot, ready to start. This is init's ELF loader, mirroring the
/// kernel's `map_segments` but driven entirely through the granular verbs.
fn build_child(
    untyped: u64,
    elf: &elf::Elf,
    caps: &[(u64, u64)],
    maps: &[(u64, u64, u64)],
) -> Result<u64, ()> {
    const PAGE: u64 = 4096;
    const CHILD_STACK_VA: u64 = 0x0050_0000;

    let aspace = retype_obj(untyped, abi::objtype::ASPACE)?;

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
            let frame = retype_frame(untyped)?;
            // A fresh scratch VA in init's own space. Persistent across build_child calls: init
            // never unmaps its scratch window, so a per-call reset would collide with the prior
            // child's mappings (that was the 19d.2c multi-child bring-up bug).
            let scratch = SCRATCH_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
            // Map the frame writable in init's own space, fill it, then hand it to the child. The
            // scratch mapping's page tables come from INIT's budget, never `untyped`: for a
            // supervised job `untyped` is the shell's region, and a forcible teardown `DESTROY`s it,
            // which must not free init's own page tables out from under its persistent scratch window
            // (milestone 24). The frame itself is the child's, from `untyped`.
            if unsafe { invoke(frame, abi::frame::MAP, scratch, 1, INIT_BUDGET) } != 0 {
                return Err(());
            }
            // Zero the page (free .bss), then copy this page's slice of the segment's file bytes.
            // SAFETY: `scratch` is a page we just mapped read/write in our own space.
            let dst = unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE as usize) };
            dst.fill(0);
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + PAGE).min(file_hi);
            if lo < hi {
                let d = (lo - va) as usize;
                let sidx = (lo - file_lo) as usize;
                let n = (hi - lo) as usize;
                dst[d..d + n].copy_from_slice(&seg.data[sidx..sidx + n]);
            }
            // Into the child at the segment's own VA, with the segment's permissions.
            if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, frame, mode) } != 0 {
                return Err(());
            }
            // Done with this frame's capability; free the slot so the next retype reuses it.
            cap_delete(frame);
            va += PAGE;
        }
    }

    // A multi-page stack for the child: the shell and drivers have deeper stacks than one page.
    // Mapped growing down from CHILD_STACK_TOP; each page is its own retyped frame.
    const CHILD_STACK_PAGES: u64 = 4;
    for k in 0..CHILD_STACK_PAGES {
        let stack_frame = retype_frame(untyped)?;
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

    // The extra mappings: a device's registers, a shared buffer. Each is a cap init holds, mapped
    // into the child at the given VA. The child accesses them directly; it never holds the caps.
    for &(va, init_slot, mode) in maps {
        if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, init_slot, mode) } != 0 {
            return Err(());
        }
    }

    // The thread, then its initial authority, one grant per slot in order, then configured.
    let tcb = retype_obj(untyped, abi::objtype::TCB)?;
    for &(init_slot, rights) in caps {
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, init_slot, rights, 0) } < 0 {
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

/// init's ever-advancing scratch window: where it temporarily maps each child frame to fill it.
/// Persistent across every `build_child` call because init never unmaps these; a per-call reset
/// collides with a prior child's mappings. Starts below the initrd (0x2000_0000), room for
/// thousands of pages before it reaches it.
static SCRATCH_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x1000_0000);

/// init's own budget lives in slot 0 (both spawn_init paths grant it there). `build_child` uses it
/// for its scratch mappings regardless of which untyped a child is built from, so tearing a
/// supervised child's region down never frees init's own page tables (milestone 24).
const INIT_BUDGET: u64 = 0;

/// Where a supervised (interruptible) child maps its shared job frame (milestone 24). Below the ELF
/// load address (0x40_0000) and the stack; must match heeder.rs / spinner.rs's JOB_FRAME_VA.
const CHILD_JOBFRAME_VA: u64 = 0x0030_0000;

/// Retype a kernel object (endpoint | aspace | tcb) out of `untyped`; returns its cap slot.
fn retype_obj(untyped: u64, objtype: u64) -> Result<u64, ()> {
    let r = unsafe { invoke(untyped, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Retype a page of `untyped` into a Frame capability; returns its cap slot.
fn retype_frame(untyped: u64) -> Result<u64, ()> {
    let r = unsafe { invoke(untyped, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Carve `pages` off `untyped` into a new child untyped we can delegate (milestone 31). The SPLIT
/// grants full rights on the child, including GRANT, so a memory budget can be handed on.
fn untyped_split(untyped: u64, pages: u64) -> Result<u64, ()> {
    let r = unsafe { invoke(untyped, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Start a configured TCB, handing the child `arg0`, `arg1`, `arg2` as its first three registers.
/// Returns the syscall result.
fn tcb_start(tcb: u64, arg0: u64, arg1: u64, arg2: u64) -> i64 {
    unsafe { invoke(tcb, abi::tcb::START, arg0, arg1, arg2) }
}

/// **Building another address space, milestone 19b.** Holds an untyped budget (slot 0) and a
/// report line (slot 1). It retypes part of its own memory into an address space, retypes a
/// frame, maps the frame into the space it built, and proves the kernel keeps the rules there
/// too: the same va twice is refused. Nothing can run in the built space yet (TCBs are 19c);
/// what this witnesses is that a process can construct one at all.
fn aspace_builder() -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const VA: u64 = 0x0040_0000;

    // SAFETY: `svc` throughout.
    let aspace = unsafe {
        invoke(
            UNTYPED,
            abi::untyped::RETYPE_OBJ,
            abi::objtype::ASPACE,
            0,
            0,
        )
    };
    let mut verdict = 0u64;
    if aspace >= 0 {
        verdict |= 1; // built a space out of our own pages
        let frame = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
        if frame >= 0 {
            let mapped =
                unsafe { invoke(aspace as u64, abi::aspace::MAP_INTO, VA, frame as u64, 1) };
            if mapped == 0 {
                verdict |= 2; // mapped our frame into the space we built
            }
            let again =
                unsafe { invoke(aspace as u64, abi::aspace::MAP_INTO, VA, frame as u64, 1) };
            if again < 0 {
                verdict |= 4; // the same va twice was refused: break-before-make holds there too
            }
        }
    }
    send(REPORT, verdict, 0, 0);
    exit();
}

/// **Minting an endpoint from our own memory, milestone 19a.** Holds an untyped budget (slot 0),
/// a channel (slot 1), and nothing else. It retypes a page of its own untyped into a brand-new
/// endpoint (`RETYPE_OBJ`), an object no kernel wiring created, then delegates a READ view of it
/// over the channel and SENDs a word into it. If the kernel's object really works, a peer we
/// have never met receives that word over an endpoint that did not exist a moment ago.
fn ep_maker() -> ! {
    const UNTYPED: u64 = 0;
    const CHANNEL: u64 = 1;

    // SAFETY: `svc`. Retype one page of our budget into an endpoint; the kernel returns the slot
    // where our full-rights capability to it landed.
    let ep = unsafe {
        invoke(
            UNTYPED,
            abi::untyped::RETYPE_OBJ,
            abi::objtype::ENDPOINT,
            0,
            0,
        )
    };
    check(ep >= 0);
    let ep = ep as u64;

    // Delegate a READ-only view (recv, never send) to whoever is on the channel; we keep WRITE.
    // SAFETY: `svc`.
    check(unsafe { invoke(CHANNEL, endpoint::SEND_CAP, ep, abi::rights::READ, 0) } == 0);

    // Speak first through our own creation: blocks until the peer receives, which is the proof.
    check(send(ep, 0x77, 0, 0) == 0);
    exit();
}

/// **The peer, milestone 19a.** Holds the channel (slot 0) and a report endpoint (slot 1).
/// Receives a capability to an endpoint that some other process minted out of its own memory,
/// listens on it, and reports what arrives. It never saw an untyped and never asked the kernel
/// to create anything: its authority to listen arrived entirely by delegation.
fn ep_user() -> ! {
    const CHANNEL: u64 = 0;
    const REPORT: u64 = 1;

    let (_w, slot) = recv_cap(CHANNEL);
    check(slot != endpoint::NO_CAP);

    let (w0, _, _) = recv(slot); // listen on the minted endpoint
    send(REPORT, w0, 0, 0); // report the word that crossed it
    exit();
}

/// **The delegation demo, granter's half.** Holds a channel to send over (slot 0) and a resource
/// capability held `WRITE | GRANT` (slot 1). It passes the resource on, narrowed to `WRITE` so the
/// receiver can use it but not lend it further. The whole point of a capability system, in four
/// lines: authority a process holds, handed to another process, at runtime, with less power than it
/// arrived with. See kernel/src/user.rs delegation_service.
fn granter() -> ! {
    const CHANNEL: u64 = 0;
    const RESOURCE: u64 = 1;

    // SAFETY: `svc`. Delegate RESOURCE, narrowed to WRITE (dropping GRANT), over CHANNEL.
    unsafe { invoke(CHANNEL, endpoint::SEND_CAP, RESOURCE, abi::rights::WRITE, 0) };

    exit(); // one-shot: our authority is passed on, so we leave and the kernel reaps us
}

/// **The delegation demo, receiver's half.** Holds the channel (slot 0), a report endpoint
/// (slot 1), and a loopback endpoint (slot 2) it uses only to *attempt* re-delegation. It receives
/// the delegated capability, proves it works by invoking it, then proves it cannot pass it on.
fn receiver() -> ! {
    const CHANNEL: u64 = 0;
    const REPORT: u64 = 1;
    const LOOPBACK: u64 = 2;
    const USED_WORD: u64 = 0x5A; // must match kernel/src/user.rs delegation_service::USED_WORD

    // Receive the delegated capability. It lands in a fresh slot of our own cspace; RECV_CAP tells
    // us which one. We were never told the slot in advance: the kernel chose it and named it to us.
    let (_data, got) = recv_cap(CHANNEL);
    let received = got != endpoint::NO_CAP;

    // Use it. A SEND on the received capability rendezvous with whoever holds the other end, which
    // proves a capability minted for us by another process carries real authority.
    if received {
        send(got, USED_WORD, 0, 0);
    }

    // Try to pass it on. We hold it WITHOUT grant, so the kernel refuses before any rendezvous, and
    // the invoke returns an error. LOOPBACK needs no receiver: the refusal happens at the check.
    let redelegate = unsafe { invoke(LOOPBACK, endpoint::SEND_CAP, got, abi::rights::WRITE, 0) };
    let refused = redelegate < 0;

    // Verdict: bit 0 we received a capability, bit 1 re-delegation was refused. 0b11 is the story.
    let code = (received as u64) | ((refused as u64) << 1);
    send(REPORT, code, 0, 0);

    exit(); // one-shot: reported, so we leave and the kernel reaps us
}

/// **The frame demo, producer's half.** Retypes a page out of its own untyped into a `Frame`
/// capability, maps it read/write, writes a sentinel, and hands the consumer a READ-only view of
/// the *same physical page*. The kernel never copies the data and was never told these two
/// processes would share memory: they composed the sharing themselves out of a capability.
fn frame_producer() -> ! {
    const UNTYPED: u64 = 0; // retype the frame and draw page tables from here
    const CHANNEL: u64 = 1; // delegate the frame to the consumer over here
    const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

    // Retype: a page out of our budget becomes a Frame capability we hold. Nothing is mapped yet.
    // SAFETY: `svc`. The result is the slot the new capability landed in.
    let frame = unsafe { invoke(UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
    check(frame >= 0);

    // Map it read/write; the page tables to reach FRAME_VA come from the same untyped.
    // SAFETY: `svc`.
    check(unsafe { invoke(frame as u64, abi::frame::MAP, FRAME_VA, 1, UNTYPED) } == 0);

    // Write the sentinel the consumer will read back through its own mapping of this page.
    // SAFETY: FRAME_VA is now a mapped, writable page in our address space.
    unsafe { core::ptr::write_volatile(FRAME_VA as *mut u64, FRAME_SENTINEL) };

    // Delegate a READ-only view: drop WRITE and GRANT on the way over. The rendezvous is also the
    // synchronization edge that makes our write visible to the consumer. SAFETY: `svc`.
    unsafe {
        invoke(
            CHANNEL,
            endpoint::SEND_CAP,
            frame as u64,
            abi::rights::READ,
            0,
        )
    };

    exit();
}

/// **The frame demo, consumer's half.** Receives the delegated frame, maps the same physical page
/// read-only, reads the producer's sentinel back (proof the memory is shared), and confirms it
/// cannot map the page writable, because it was handed the frame with `READ` alone.
fn frame_consumer() -> ! {
    const CHANNEL: u64 = 0; // RECV_CAP the frame here
    const UNTYPED: u64 = 1; // page tables for our own mappings come from here
    const REPORT: u64 = 2; // report the verdict here
    const FRAME_VA: u64 = 0x0000_0000_00A0_0000;
    const RW_VA: u64 = 0x0000_0000_00B0_0000;

    let (_data, frame) = recv_cap(CHANNEL);
    let received = frame != endpoint::NO_CAP;

    let mut read_ok = false;
    let mut rw_refused = false;
    if received {
        // Map the shared page read-only and read the producer's sentinel through it.
        // SAFETY: `svc`.
        let mapped = unsafe { invoke(frame, abi::frame::MAP, FRAME_VA, 0, UNTYPED) } == 0;
        if mapped {
            // SAFETY: FRAME_VA is now a mapped, readable page.
            let seen = unsafe { core::ptr::read_volatile(FRAME_VA as *const u64) };
            read_ok = seen == FRAME_SENTINEL;
        }

        // Try to map it read/write. We hold it READ only, so the kernel refuses before mapping.
        // SAFETY: `svc`.
        let rw = unsafe { invoke(frame, abi::frame::MAP, RW_VA, 1, UNTYPED) };
        rw_refused = rw < 0;
    }

    // Verdict: bit 0 we read the shared sentinel, bit 1 a writable mapping was refused.
    let code = (read_ok as u64) | ((rw_refused as u64) << 1);
    send(REPORT, code, 0, 0);
    exit();
}

#[inline(never)]
fn stack_works(n: u64) -> bool {
    let local = [n; 8];
    if n == 0 {
        return local[0] == 0;
    }
    core::hint::black_box(&local);
    stack_works(n - 1)
}

/// The only way this program can say "no": a `brk`, which the kernel treats as a fault and kills
/// us for. A failed check must be indistinguishable from a broken program, because it is one.
fn check(ok: bool) {
    if !ok {
        fail();
    }
}

fn fail() -> ! {
    // The one arch-specific line in the program: aarch64 `brk`, RISC-V `ebreak`. Both trap, and
    // the kernel turns a trap from userspace into a kill.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` raises a breakpoint the kernel turns into a fault. That is the point.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` raises a breakpoint the kernel turns into a fault. That is the point.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}

/// Milestone 11: spend an untyped budget. This process holds a capability to a chunk of raw
/// memory (slot 0) and a report endpoint (slot 1). It maps page after page out of that untyped
/// into its own address space, writes and reads each one to prove it is real, and keeps going
/// until the untyped is exhausted. Then it reports how many it mapped.
///
/// The whole point is what the KERNEL does while this runs: nothing. Every page here comes out of
/// the untyped, so the kernel's free-frame count does not move. A test checks exactly that.
fn untyped_demo() -> ! {
    const UNTYPED: u64 = 0;
    const REPORT: u64 = 1;
    const BASE_VA: u64 = 0x0000_0000_00c0_0000;

    // Signal that we are loaded and about to start spending the untyped. The test measures the
    // kernel's frame count HERE, so it sees only what we do from now on: map from our untyped.
    send(REPORT, 0, 0, 0);

    let mut mapped: u64 = 0;
    loop {
        let va = BASE_VA + mapped * 4096;
        // Retype a page out of our untyped and map it here. SAFETY: `svc`.
        let r = unsafe { invoke(UNTYPED, abi::untyped::MAP, va, 0, 0) };
        if let Some(e) = Error::from_ret(r) {
            // OutOfMemory means our budget is spent. Any other error is a real bug.
            if e != Error::OutOfMemory {
                fail();
            }
            break;
        }

        // Prove the page is genuinely ours: write a marker, read it back.
        let marker = 0xA11C_0000_0000_0000u64 | mapped;
        // SAFETY: the kernel just mapped this page writable in our address space.
        unsafe {
            core::ptr::write_volatile(va as *mut u64, marker);
            if core::ptr::read_volatile(va as *const u64) != marker {
                fail();
            }
        }

        mapped += 1;
        if mapped > 100_000 {
            fail(); // a bump allocator that never exhausts is a bug
        }
    }

    send(REPORT, mapped, 0, 0);
    // **This one must NOT exit**, unlike the other one-shot roles. The test reads the kernel's
    // used-frame count the instant this report lands, and exiting here would tear this address
    // space down in that same window, so the number it reads would be the teardown's rather than
    // the measurement's. Spinning holds the state still until the assertion has looked at it.
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    fail()
}
