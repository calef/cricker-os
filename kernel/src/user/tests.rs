use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// The std-transcript and FS-readiness assertions live with the std tests so both ISAs share one
// copy; see `std_tests`. Most of them are used only by the device/FS tests below, which have
// RISC-V twins in `riscv_virtio_tests` and are gated to aarch64 here.
#[cfg(target_arch = "aarch64")]
use super::std_tests::{
    assert_a_kill_mid_transaction_recovers, assert_attrs, assert_fs_service_ready,
    assert_smb_held_no_key, assert_smb_write_landed, assert_std_transcript, std_fs_expected,
};
use super::*;
use crate::arch::exceptions::{SVC_COUNT, USER_FAULTS, last_user_fault};
use crate::arch::{UserFault, UserFaultAccess, timer};
use crate::sched;

/// **The `hello` binary's ELF bytes**, pulled out of the initrd archive by name (milestone 19f).
/// This is the binary carrying the milestone 7-19 role catalogue: the printing client, the
/// untyped demo, the granter and receiver, the call server, the aspace builder, the init roles.
/// A test that loads a real user program wants the program's bytes, not the whole nifefs
/// archive; only the `spawn_init` tests pass the archive, because init parses it itself.
///
/// The archive name differs by ISA and that is the one place it shows. aarch64 packs hello as
/// **`init`**, because on that ISA hello *is* the boot program. RISC-V's `init` is the portable
/// `builder` demo, so hello is packed under its own name there. Both point at the same source
/// file compiled for the local target.
#[cfg(target_arch = "aarch64")]
const HELLO_ENTRY: &str = "init";
#[cfg(target_arch = "riscv64")]
const HELLO_ENTRY: &str = "hello";

fn init_image() -> &'static [u8] {
    program(HELLO_ENTRY).expect("no hello program in the initrd archive")
}

/// The `outlaw` program's ELF bytes: the two privilege-boundary behaviours that used to be
/// hand-assembled aarch64 machine code in this file. See [`OUTLAW_ROUND_TRIP`].
fn outlaw_image() -> &'static [u8] {
    program("outlaw").expect("no outlaw program in the initrd archive")
}

/// The `spinner` program's ELF bytes: a `_start` that is nothing but a loop. It was built for the
/// shell's forcible-interrupt tier (DECISIONS §24), and it is exactly the hostile binary
/// DECISIONS §5 describes, so the preemption test uses it rather than a second copy of the same idea.
fn spinner_image() -> &'static [u8] {
    program("spinner").expect("no spinner program in the initrd archive")
}

/// **An address in the kernel's own memory: mapped, readable by the kernel, forbidden to
/// userspace.** The address of a kernel static, rather than a per-ISA constant for the kernel's
/// text base, because taking the address of something the kernel is demonstrably using is true
/// on any ISA and any link layout, and needs no table of magic numbers to keep current.
fn a_kernel_address() -> u64 {
    static SENTINEL: u64 = 0x00C0_FFEE_D00D;
    &raw const SENTINEL as u64
}

/// Run `image` at user mode with `arg0`/`arg1` and no authority at all: no capabilities, no
/// extra mappings. It can run its own code and touch its own memory and name nothing else.
fn spawn_bare(image: &'static [u8], arg0: u64, arg1: u64) -> Option<crate::thread::Tid> {
    sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0,
                arg1,
                arg2: 0,
                grants: &[],
                maps: &[],
            },
        )
    })
}

/// **Take back a thread from [`spawn_bare`] that will never exit on its own.**
///
/// A bare thread belongs to no reclaimable region, so nothing tears it down when the test that
/// started it returns. For a subject whose whole point is that it never exits (a spinner, or a
/// child holding a measurement still), "the test ended" and "the thread ended" are unrelated
/// events, and the thread just keeps running.
///
/// That is not merely untidy. Two such threads spinning on a four-hart machine is a load every
/// later test in the suite then runs under, and it starved
/// `reclaim_frees_a_started_then_exited_childs_regions` into its watchdog on CI, intermittently,
/// nowhere near the tests responsible. See `notes/riscv-parity-scope.md`.
///
/// The kill is armed rather than immediate (DECISIONS §16), so this waits for the name to stop
/// resolving instead of assuming the thread is gone on return. Returns whether it actually went;
/// callers assert on it, because a silent failure here restores the leak this exists to remove.
fn reap_bare(tid: crate::thread::Tid) -> bool {
    if !sched::kill_thread(tid) {
        // Already gone. Nothing to wait for, and not a failure.
        return true;
    }
    wait_for(|| !sched::thread_present(tid))
}

/// The `worker` program's ELF bytes (milestone 19f.2), a distinct binary in the archive, not a
/// role of the init/hello binary. `_start(x0, x1, x2)` reads its input in `x1` and needs no
/// role selector.
fn worker_image() -> &'static [u8] {
    program("worker").expect("no worker program in the initrd archive")
}

/// The `net_stack` program's ELF bytes (milestone 30, piece 3): the smoltcp net server, a distinct
/// binary loaded by name. Used only by the net tests, whose RISC-V twins live in
/// `riscv_virtio_tests`.
#[cfg(target_arch = "aarch64")]
fn net_stack_image() -> &'static [u8] {
    program("net_stack").expect("no net_stack program in the initrd archive")
}

/// The net client's test selectors and its success word, matching `user/src/socket_test_client.rs`. The
/// client is a nonzero entry role of the `net_stack` binary, so it needs no image of its own.
#[cfg(target_arch = "aarch64")]
const NET_TEST_UDP_DNS: u64 = 1;
#[cfg(target_arch = "aarch64")]
const NET_TEST_TCP_ECHO: u64 = 2;
#[cfg(target_arch = "aarch64")]
const NET_TEST_TCP_REOPEN: u64 = 3;
#[cfg(target_arch = "aarch64")]
const NET_TEST_UDP_TFTP: u64 = 4;
#[cfg(target_arch = "aarch64")]
const NET_TEST_TCP_ACCEPT: u64 = 5;
/// The one port the inbound gate is granted (milestone 107). The runners forward a host port to
/// exactly this one, and the client asks for `fixture::DENIED_PORT` as well to prove the grant
/// refuses. Named here because the *spawn service* is what grants it, which is the point.
///
/// Taken from `socket_proto::fixture` rather than spelled again (milestone 64): three binaries and
/// this test have to agree on the number, and a literal repeated per call site is the shape rule 7
/// exists to retire. The runners' `hostfwd` spells it a second time because a shell script cannot
/// read a Rust crate, and that drift is loud (the prober reports the guest served none).
#[cfg(target_arch = "aarch64")]
const NET_LISTEN_PORT: u16 = socket_proto::fixture::LISTEN_PORT;
/// The fixed UDP ports the mDNS gate is granted (milestone 55), RFC 6762's 5353 and its
/// neighbour. Named here for the same reason as the listen port: the spawn service grants them,
/// and a program cannot ask for what it was not given.
///
/// **Two ports, for two clients with two different jobs.** `mdns_responder` holds 5353 and answers
/// real queries on it for the whole run. `socket_test_client` cannot then use 5353 to prove that a
/// *granted* port binds and is exclusive, so it uses 5354; the port outside the range (4444) is
/// what proves the refusal, and that is the check the responder cannot make about itself.
#[cfg(target_arch = "aarch64")]
const NET_MDNS_PORT: u16 = 5353;
#[cfg(target_arch = "aarch64")]
const NET_MDNS_GRANT_TOP: u16 = 5354;
/// Queries `mdns_responder` must answer before reporting OK, matching xtask's multicast prober:
/// one multicast browse and one legacy-unicast query, which are the two shapes RFC 6762 §6.7
/// splits a responder's behaviour on.
#[cfg(target_arch = "aarch64")]
const MDNS_QUERIES: u64 = 2;

/// The `mdns_responder` program's ELF bytes (milestone 55): the discovery half, spawned as a third
/// client of the same stack. A separate binary rather than a role of `net_stack`, because it is a
/// separate authority: it holds one UDP port and no share, and the SMB adapter holds the share and
/// no discovery.
#[cfg(target_arch = "aarch64")]
fn mdns_responder_image() -> &'static [u8] {
    program("mdns_responder").expect("no mdns_responder program in the initrd archive")
}
/// The port the SMB adapter is granted in the same boot (milestone 54): the echo port's
/// neighbour, so the two-port grant `[7778, 7779]` still refuses the client's 8080 probe. The
/// runners forward a second host port (`NIFE_SMB_HOSTFWD_PORT`) to this one. Guest-side the
/// number is arbitrary (hostfwd remaps); the serve-forever boot uses SMB's own 445.
#[cfg(target_arch = "aarch64")]
const SMB_LISTEN_PORT: u16 = 7779;
/// Connections the SMB adapter must serve, matching the prober: two, because the second is what
/// proves the adapter re-arms after a client disconnects, the same load-bearing round as the
/// echo gate's.
#[cfg(target_arch = "aarch64")]
const SMB_ROUNDS: u64 = 2;
#[cfg(target_arch = "aarch64")]
const NET_CLIENT_OK: u64 = 1;

/// The `smb_server` program's ELF bytes (milestone 54): the SMB adapter, spawned as a second
/// client of the inbound gate's stack.
#[cfg(target_arch = "aarch64")]
fn smb_server_image() -> &'static [u8] {
    program("smb_server").expect("no smb_server program in the initrd archive")
}
/// The client could not complete for an ENVIRONMENTAL reason (the host resolver never answered),
/// not because of a defect here. Only the non-gating real-DNS check can report it.
#[cfg(target_arch = "aarch64")]
const NET_CLIENT_NO_ANSWER: u64 = 2;

/// Spin the scheduler until `done()`, or give up after a wall-clock deadline. Returns whether it
/// happened. **Time-based, not a fixed yield count** (DECISIONS §28): with work spread across
/// cores, the test thread's own core is often idle, so a yield returns at once and a fixed count
/// of them elapses in almost no real time, timing out before a parallel result on another core
/// lands. A ~2 s deadline gives the other cores real time to finish while staying far under the
/// 60 s hang watchdog, so a genuine hang still fails.
///
/// `pub(super)` so the sibling test modules under `user` can use this one rather than growing a
/// sixth copy of it. Milestone 81 needed it in two of them: running on the physical core makes the
/// yield-count version fail for the *mirror* reason it fails on a loaded host, since a yield on an
/// idle core costs nanoseconds there. See notes/hvf-leg.md.
pub(super) fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        if done() {
            return true;
        }
        sched::yield_now();
    }
    done()
}

/// **We are running on `SP_EL1`, and the whole trap frame depends on it.**
///
/// At EL1 the name `sp` means `SP_EL1` if `SPSel.SP == 1`, and `SP_EL0` if it is 0. Every
/// `SAVE_CONTEXT` in the kernel does `sub sp, sp, #272` and every user entry does
/// `msr sp_el0, x3`. If `SPSel` were 0, those two would be **the same register**, and the
/// kernel would restore a user stack pointer straight into its own stack pointer.
///
/// This has been true since boot.s and we never checked it. A test that can only fail if
/// the world is upside down is still worth having when the failure is silent.
///
/// **aarch64-only, and there is no RISC-V analogue to write.** RISC-V does not bank the stack
/// pointer by privilege level at all: there is one `sp`, and the kernel swaps it with `sscratch`
/// on the way into a trap. The hazard this test guards (two names for one register, silently)
/// cannot exist there, so a RISC-V twin would have nothing to assert. See
/// notes/riscv-parity-scope.md.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn el1_runs_on_sp_el1() {
    let spsel = crate::arch::spsel();

    assert_eq!(
        spsel & 1,
        1,
        "SPSel says EL1 is using SP_EL0: the trap frame's sp_el0 field aliases the \
         kernel's own stack pointer"
    );
}

/// User mode. The boundary.
///
/// Two syscalls, not one, and that is the point: the second can only happen if the return from
/// the first **put us back at EL0/U-mode**. One proves we left. Two prove we came back.
#[test_case]
fn a_user_program_reaches_el0_and_returns_twice() {
    let before = SVC_COUNT.load(Ordering::Relaxed);

    spawn_bare(outlaw_image(), OUTLAW_ROUND_TRIP, 0).expect("spawn failed");

    assert!(
        wait_for(|| SVC_COUNT.load(Ordering::Relaxed) >= before + 2),
        "saw {} syscalls from user mode, wanted 2",
        SVC_COUNT.load(Ordering::Relaxed) - before,
    );
}

/// **The privilege boundary is real, and it is a PERMISSION fault, not a missing page.**
///
/// The address the user reaches for is mapped, and readable, and the kernel reads it all day.
/// The precondition assertions below say exactly that before the program is even started,
/// because without them the test is vacuous: "userspace cannot read this" proves nothing about
/// a page nobody mapped.
///
/// So a **permission** fault rather than a translation fault is the whole assertion. A
/// translation fault would mean we had merely failed to map something, which would pass a
/// sloppier test and prove nothing at all. Both ISAs assert it; only aarch64 is *told* it (see
/// `arch::UserFault`, and the BUGS note on the RISC-V classifier).
#[test_case]
fn a_user_program_cannot_read_a_kernel_address() {
    let kernel_addr = a_kernel_address();

    // The precondition, and it is what gives the assertion below its teeth.
    assert!(
        mmu::translate(kernel_addr).is_some(),
        "the kernel's own static is not mapped, so this test proves nothing",
    );

    let before = USER_FAULTS.load(Ordering::Relaxed);

    spawn_bare(outlaw_image(), OUTLAW_READ_KERNEL, kernel_addr).expect("spawn failed");

    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > before),
        "the user program read a kernel address and was NOT stopped",
    );

    let (kind, addr) = last_user_fault().expect("the kernel recorded no user fault");

    assert_eq!(
        kind,
        UserFault::Permission(UserFaultAccess::Read),
        "not a PERMISSION fault on a read: a translation fault would mean we had merely \
         failed to map something, which proves nothing about the privilege boundary",
    );
    assert_eq!(addr, kernel_addr, "faulted on the wrong address");

    // And the kernel is executing this line, which is the other half of the claim.
}

/// DECISIONS §5's arbitrary binary, at user mode, in the flesh.
///
/// A program with no yield, no syscall, and not even a function call: `spinner`'s whole `_start`
/// is a loop. The **only** thing in the universe that can take the CPU back from it is a timer
/// interrupt landing between two of its instructions. Milestone 6 proved this for a kernel
/// thread we compiled. This is the case that actually mattered.
#[test_case]
fn a_user_program_that_never_yields_is_preempted_anyway() {
    let preemptions = sched::preemptions();
    let faults = USER_FAULTS.load(Ordering::Relaxed);

    let spinner = spawn_bare(spinner_image(), 0, 0).expect("spawn failed");

    // Give it the CPU and then take it back, without asking.
    timer::spin_for(timer::frequency() / 10);

    assert!(
        sched::preemptions() > preemptions,
        "nothing was preempted while a user thread spun at EL0",
    );
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults,
        "the spinning user thread faulted; it was supposed to just spin",
    );

    // And we are here, running, having taken the CPU back from a program that never
    // offered it.

    // Now take it back for good. `spinner` never yields, never syscalls and never returns, so
    // nothing but an armed kill ends it, and leaving it running would spend a core for the rest
    // of the suite. The assertions above are already done, so the kill cannot weaken them.
    assert!(
        reap_bare(spinner),
        "the spinning user thread outlived its kill"
    );
}

/// **The trap frame is where the trap path will look for it** (milestone 71).
///
/// The user-entry path writes a `TrapFrame` and the trap path rebuilds one, and the whole privilege
/// boundary rests on those two being the *same address*: `stack_top - size_of::<TrapFrame>()`. That
/// is an agreement between Rust and assembly, on two ISAs, and nothing checked it. It was wrong on
/// RISC-V for a year: the frame was computed from the live `sp` instead, which put it 16 bytes under
/// where `trap.s` builds an S-mode frame, so any interrupt in the window rewrote it. The symptom was
/// a thread dispatched to U-mode with a zero entry point, intermittently, only ever on CI.
///
/// So read that address and wait for this thread's U-mode PC to appear in it. `spinner` is the
/// subject because it never syscalls and never returns: anything that lands in its frame got there
/// by the timer preempting it at EL0, which is the agreement under test. A frame built somewhere
/// else never shows up here and the wait times out, which is precisely what the old RISC-V placement
/// would do.
///
/// **The window is checked, not just the value, and the test learned that the hard way.** Its first
/// form waited for a nonzero word and then asserted it was a user address, and it failed on RISC-V
/// against a correct kernel: on the exec path this thread was a *kernel* thread first, and the
/// frames of `thread_entry` and `run` occupy `[top - size, top)` until the user frame is written
/// over them. So "nonzero" was satisfied by a live kernel frame pointer before user entry had
/// happened at all. The condition has to name the U-mode text range, which every user program is
/// linked into (`dump_threads` records why that base is shared).
#[test_case]
fn a_user_threads_trap_frame_sits_where_the_trap_path_rebuilds_it() {
    const USER_TEXT: core::ops::Range<u64> = 0x40_0000..USER_STACK_VA;

    let spinner = spawn_bare(spinner_image(), 0, 0).expect("spawn failed");

    assert!(
        wait_for(|| sched::user_pc_of(spinner).is_some_and(|pc| USER_TEXT.contains(&pc))),
        "this thread's U-mode PC never appeared at stack_top - size_of::<TrapFrame>(), so the \
         user-entry path and the trap path do not agree on where the frame lives (read {:#x})",
        sched::user_pc_of(spinner).unwrap_or(0),
    );

    assert!(
        reap_bare(spinner),
        "the spinning user thread outlived its kill"
    );
}

/// Forge an ELF64 header by hand, so a test can ask for something no linker would emit.
///
/// A fixed buffer, since the kernel it tests has no heap (milestone 14 phase C): one ELF
/// header, one program header, sixteen bytes of code. The ELF **names its own load
/// address**, and this is the file that names the kernel's.
fn forged_elf(vaddr: u64, flags: u32) -> [u8; 136] {
    const EHDR: usize = 64;
    const PHDR: usize = 56;
    let code: [u8; 16] = [0; 16];

    let mut out = [0u8; EHDR + PHDR + 16];
    out[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    out[4] = 2; // ELFCLASS64
    out[5] = 1; // little-endian
    out[6] = 1; // EV_CURRENT
    out[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    // This build's own machine, so the forgery gets past the machine check and reaches the
    // property under test rather than being refused for being foreign.
    out[18..20].copy_from_slice(&elf::NATIVE_MACHINE.to_le_bytes());
    out[24..32].copy_from_slice(&vaddr.to_le_bytes()); // e_entry
    out[32..40].copy_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
    out[54..56].copy_from_slice(&(PHDR as u16).to_le_bytes());
    out[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

    let p = EHDR;
    out[p..p + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    out[p + 4..p + 8].copy_from_slice(&flags.to_le_bytes());
    out[p + 8..p + 16].copy_from_slice(&((EHDR + PHDR) as u64).to_le_bytes()); // p_offset
    out[p + 16..p + 24].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
    out[p + 32..p + 40].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_filesz
    out[p + 40..p + 48].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_memsz
    out
}

/// **A binary that asks to be loaded over the kernel.**
///
/// This is the attack. An ELF names its own load address, so a hostile one simply names the
/// base of the kernel's half and waits to see whether the loader is credulous.
///
/// It is refused **by construction, not by a check we remembered to write**: the user
/// `Mapper` is built with `Half::Low`, and a high address is not a thing it can express. The
/// same `WrongHalf` guard has been in `paging` since milestone 4, put there because a *host*
/// test discovered that bits 63:48 are not translated. It has been waiting for this file.
///
/// The address is `KERNEL_VA_BASE` rather than a constant, which is what makes this the same
/// attack on both ISAs: aarch64's kernel half starts at `0xffff_0000_0000_0000` and RISC-V's
/// Sv39 kernel half at `0xffff_ffc0_0000_0000`, and the loader must refuse either.
#[test_case]
fn an_elf_that_asks_to_be_loaded_over_the_kernel_is_refused() {
    let image = forged_elf(mmu::KERNEL_VA_BASE, elf::PF_R | elf::PF_X);

    assert_eq!(
        load(&image).err(),
        Some(LoadError::Unmappable(MapError::WrongHalf)),
        "the kernel agreed to map a user program on top of itself",
    );
}

/// And a binary asking for a page that is both writable and executable.
///
/// Caught in `crates/elf`, on the host, in microseconds. But assert it end-to-end too: the
/// value of the host test is that it is fast, not that it is the only line of defence.
#[test_case]
fn an_elf_that_asks_for_a_writable_executable_page_is_refused() {
    let image = forged_elf(0x40_0000, elf::PF_R | elf::PF_W | elf::PF_X);

    assert_eq!(
        load(&image).err(),
        Some(LoadError::NotLoadable(elf::Error::WritableAndExecutable)),
    );
}

/// Junk is refused, and refusing it does not take the kernel down.
#[test_case]
fn a_bad_binary_is_refused_rather_than_panicking() {
    assert!(load(b"#!/bin/sh\necho hi\n").is_err());
    assert!(load(&[]).is_err());
    assert!(load(&[0u8; 4096]).is_err());
    // And we are still executing, which is the assertion.
}

/// The initrd is there, and it is the program we built, for **this** machine: `Elf::parse`
/// refuses a foreign `e_machine`, so parsing at all is half the assertion.
#[test_case]
fn the_initrd_holds_a_native_executable() {
    let image = init_image();
    let e = elf::Elf::parse(image).expect("the initrd is not a loadable native ELF");

    assert_eq!(e.entry(), 0x40_0000, "linked somewhere unexpected");

    // Three segments, and NONE of them writable-and-executable. Counted straight off the
    // iterator: the kernel this test rides in has no heap to collect into (milestone 14).
    assert!(
        e.segments().count() >= 3,
        "expected .text, .rodata and .data"
    );
    assert!(e.segments().any(|s| s.is_executable() && !s.is_writable()));
    assert!(e.segments().any(|s| s.is_writable() && !s.is_executable()));

    // And one of them has a .bss: memsz > filesz. If this is not true, the test below is
    // vacuous, and we would never know.
    assert!(
        e.segments().any(|s| s.memsz as usize > s.data.len()),
        "no segment has a .bss, so the zero-fill is untested",
    );
}

/// **The whole of 7c.** A separately compiled binary, arriving in the initrd, running at user
/// mode.
///
/// The program checks its own image and speaks with the only two words it has: a syscall if
/// every expectation about its own memory holds, a trap instruction if not. **No data crosses
/// the boundary**, because there is no ABI yet and we are not going to invent one by accident.
///
/// So a syscall and no fault means: `.text` executed, `.rodata` was readable, `.data` was copied
/// from the file, `.bss` was zeroed (the file does not contain those bytes), and the stack
/// worked well enough to recurse eight frames. That is hello's `SELF_CHECK` role, which is the
/// same source compiled for whichever machine this build is.
#[test_case]
fn a_real_elf_from_the_initrd_runs_at_el0_and_verifies_itself() {
    let svc = SVC_COUNT.load(Ordering::Relaxed);
    let faults = USER_FAULTS.load(Ordering::Relaxed);

    spawn_bare(init_image(), 0, 0).expect("spawn failed");

    assert!(
        wait_for(|| SVC_COUNT.load(Ordering::Relaxed) > svc),
        "the program never reached its syscall",
    );
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults,
        "the program reached EL0 and then FAILED its own self-check: one of \
         .text/.rodata/.data/.bss/stack was not what the ELF asked for",
    );
}

/// **The milestone 15 witness: address spaces stay apart with NO flush on switch.**
///
/// Two spaces map the *same* virtual address to different frames holding different bytes.
/// We install A and read through the VA (loading A's translation, tagged with A's ASID,
/// into the TLB), then install B, which since milestone 15 flushes nothing, and read
/// again. If user mappings were still global, or the ASID did not ride TTBR0, or two
/// spaces shared a tag, B's read would hit A's still-cached entry and see A's byte: one
/// process reading another's memory, the exact bug the sledgehammer flush used to prevent.
///
/// **This ran on aarch64 only until milestone 58, and the reason it could not be ported is worth
/// keeping.** `riscv64::mmu::write_satp` used to issue an unconditional `sfence.vma` on every root
/// switch, so a RISC-V twin would have read B's byte because everything had just been flushed, not
/// because the tagging works: a test that cannot fail for its stated reason, which is worse than no
/// test. Porting it was never a test-portability change; it needed the ASID shootdown underneath.
/// Now that RISC-V's context switch flushes nothing either, the assertion means the same thing on
/// both ISAs and this is one suite, which is what DECISIONS §19 asks for.
#[test_case]
fn asid_tagging_keeps_address_spaces_apart_without_flushes() {
    let mut a = AddressSpace::new(2).expect("no space A");
    let mut b = AddressSpace::new(2).expect("no space B");

    let (asid_a, asid_b) = (mmu::asid_of(a.ttbr0()), mmu::asid_of(b.ttbr0()));
    assert_ne!(asid_a, asid_b, "two live spaces share an ASID");
    assert_ne!(asid_a, 0, "a user space got the kernel's ASID 0");
    assert_ne!(asid_b, 0, "a user space got the kernel's ASID 0");

    const VA: u64 = 0x40_0000;
    a.map_new(VA, Flags::user_data()).expect("map A")[0] = 0xAA;
    b.map_new(VA, Flags::user_data()).expect("map B")[0] = 0xBB;

    // Masked across the whole sequence: a preemption between an `activate_user` and its read would
    // put the reserved root back and turn `VA` into a kernel fault, which is a flaky test rather
    // than a finding. The window is a handful of instructions, so this is cheap insurance.
    let was_enabled = crate::arch::interrupts::disable();
    // And the ISA difference that kept this test on one architecture as surely as the flush did:
    // RISC-V's S-mode may not touch a `U` page unless `sstatus.SUM` says so, where EL1 may.
    let could_reach_user = mmu::permit_kernel_access_to_user_pages(true);

    // SAFETY: nothing is at EL0; we are a kernel thread mid-test, and each space outlives
    // its activation. The reads go through the live user translation, which is the point.
    let (read_a, read_b) = unsafe {
        mmu::activate_user(a.ttbr0());
        let ra = core::ptr::read_volatile(VA as *const u8);
        // Flushes NOTHING: milestone 15 on aarch64, milestone 58 on RISC-V.
        mmu::activate_user(b.ttbr0());
        let rb = core::ptr::read_volatile(VA as *const u8);
        mmu::deactivate_user();
        (ra, rb)
    };

    mmu::permit_kernel_access_to_user_pages(could_reach_user);
    crate::arch::interrupts::restore(was_enabled);

    assert_eq!(read_a, 0xAA);
    assert_eq!(
        read_b, 0xBB,
        "B read A's byte: a stale TLB entry crossed address spaces, so the tagging is broken \
         (aarch64: the nG bit and TTBR0's ASID; RISC-V: a non-global PTE and satp.ASID)",
    );
}

/// **An ASID flush reaches the OTHER cores, which is the whole of milestone 58 on RISC-V.**
///
/// The test above proves tagging keeps two spaces apart on one core. This one proves the other half
/// of the contract, the half that is a distributed protocol on one ISA and a single instruction on
/// the other: when `crates/asid` says "flush, then the number may tag someone else", the flush has
/// to have reached every core that could be holding an entry wearing it. aarch64's `tlbi aside1is`
/// broadcasts in hardware. RISC-V's `sfence.vma` affects only the hart that runs it, so `flush_asid`
/// has to IPI the others through SBI RFENCE and wait for them.
///
/// **This test fails without the shootdown**, which is the property worth having. The sequence:
///
///   1. a probe thread on **another** core installs space A and reads `VA`, which is what pulls the
///      translation into *that* core's TLB, tagged with A's ASID, and **leaves it installed**
///   2. this core moves `VA` onto a different frame **with no per-address invalidation at all**, so
///      the only thing that can announce the change anywhere is the ASID sweep
///   3. this core calls `flush_asid`
///   4. the probe reads `VA` again, on the same core, with the same space still installed
///
/// Step 4 must see the new frame. If the sweep stayed local it sees the old one, and that byte is
/// exactly the bug: a core still translating an address space that no longer means what it did.
///
/// Two of those choices are load-bearing. **The space is never re-installed**, because a `satp` or
/// `TTBR0` write between the reads is a second event a core (or an emulator) is entitled to treat as
/// a flush, and then step 4 would be right for a reason that has nothing to do with the shootdown.
/// And the mapping is **changed rather than recycled**: tearing the space down and handing its ASID
/// to a new one is the scenario that matters in production, but the allocator would likely hand the
/// new space the dead one's frames, and reading the right byte off the right frame by accident is
/// the same failure of proof.
///
/// # BUGS
///
/// - **The probe is placed, not pinned.** `spawn_on` is a hint and an idle core may steal, so the
///   probe records where it actually ran and the test asserts only that it was not *this* core. It
///   cannot land here: this thread spins without yielding, so its core never goes idle and never
///   steals. Both of the probe's reads happen inside one thread with interrupts masked, so they
///   cannot land on two different cores.
/// - **It needs two cores** and skips nothing if it has one: it fails, because the runner passes
///   `-smp 4` on both ISAs and a machine that came up single-core is a finding, not a reason to be
///   quiet.
/// - **A broken shootdown could hang this test instead of failing it**, if the reason it were broken
///   were that the remote core never services the request. The probe's spin is deadline-bounded and
///   reports `PROBE_GAVE_UP` so that case is named rather than silent, but the failure text would
///   then be about the handshake and the real cause would be one level down.
#[test_case]
fn an_asid_flush_reaches_the_other_cores() {
    use core::sync::atomic::{AtomicU8, AtomicUsize};

    const VA: u64 = 0x40_0000;
    const OLD: u8 = 0xAA;
    const NEW: u8 = 0xBB;

    /// 0 = starting, 1 = the probe has cached the translation, 2 = the remap and sweep are done,
    /// 3 = the probe has re-read. `SeqCst` throughout: this is a handshake between two cores whose
    /// whole purpose is ordering, and shaving it would be optimizing the thing under test.
    static STAGE: AtomicUsize = AtomicUsize::new(0);
    static SEEN_BEFORE: AtomicU8 = AtomicU8::new(0);
    static SEEN_AFTER: AtomicU8 = AtomicU8::new(0);
    static PROBE_CORE: AtomicUsize = AtomicUsize::new(usize::MAX);
    static PROBE_GAVE_UP: AtomicBool = AtomicBool::new(false);

    /// Spin (never yield) until `done`, so this core stays busy and cannot steal the probe.
    fn spin_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = timer::now() + 5 * timer::frequency();
        while timer::now() < deadline {
            if done() {
                return true;
            }
            core::hint::spin_loop();
        }
        done()
    }

    STAGE.store(0, Ordering::SeqCst);
    PROBE_CORE.store(usize::MAX, Ordering::SeqCst);
    PROBE_GAVE_UP.store(false, Ordering::SeqCst);

    let cores = crate::smp::online_count();
    assert!(
        cores >= 2,
        "a cross-core TLB shootdown test needs two online cores; this machine has {cores}",
    );
    let here = crate::cpu::id();
    // Any ONLINE core but this one, from the set rather than `0..count` (first-silicon sweep,
    // 2026-08-14): on a non-contiguous online set the range picks a parked core, the probe never
    // runs, and the shootdown test times out reading as a TLB bug.
    let target = crate::smp::online_cpus()
        .find(|&c| c != here)
        .expect("cores >= 2");

    let mut space = AddressSpace::new(2).expect("no address space");
    space.map_new(VA, Flags::user_data()).expect("map")[0] = OLD;
    let installed = space.ttbr0();
    let asid = mmu::asid_of(installed);

    // The frame `VA` will be moved onto, written through the direct map because the kernel cannot
    // name a user VA. It comes from the general allocator rather than the space's own region, so the
    // space never owns it and its teardown cannot free it out from under us.
    let fresh = crate::memory::alloc().expect("out of memory");
    // SAFETY: the allocator just handed us this frame exclusively; the direct map reaches it.
    unsafe { core::ptr::write_volatile(mmu::phys_to_virt(fresh.addr()) as *mut u8, NEW) };

    let probe = sched::spawn_on(target, move || {
        // Masked for the whole body. Two reasons, and the first is the one that matters: a
        // preemption would migrate this thread and split the two reads across cores, and it would
        // also swap the address space out from under them. This does NOT block the shootdown:
        // RISC-V's SBI RFENCE arrives as an M-mode software interrupt, which S-mode masking does not
        // touch, and aarch64's `tlbi` needs no interrupt at all. If that were wrong, this test would
        // hang rather than lie, which is the right way round.
        let was_enabled = crate::arch::interrupts::disable();
        let could_reach_user = mmu::permit_kernel_access_to_user_pages(true);
        PROBE_CORE.store(crate::cpu::id(), Ordering::SeqCst);

        // **The space stays installed across both reads**, which is deliberate and is the realistic
        // shape: the hazard is a core actively running an address space whose mapping changes under
        // it. It also keeps the measurement clean, because a `satp` write between the reads is a
        // second event an emulator or a core is entitled to treat as a flush, and then the second
        // read would be correct for a reason that has nothing to do with the shootdown.
        //
        // SAFETY: nothing is at user mode on this core; we are a kernel thread with interrupts
        // masked, and the space outlives this thread (the test waits for the reap before dropping
        // it). The space's root carries the kernel high half, so this core's own code and stack stay
        // mapped throughout.
        unsafe { mmu::activate_user(installed) };

        // SAFETY: `VA` is mapped user-readable in the installed space, and this ISA has been told
        // that the kernel may read a user page.
        SEEN_BEFORE.store(
            unsafe { core::ptr::read_volatile(VA as *const u8) },
            Ordering::SeqCst,
        );
        STAGE.store(1, Ordering::SeqCst);

        let deadline = timer::now() + 5 * timer::frequency();
        while STAGE.load(Ordering::SeqCst) < 2 {
            if timer::now() > deadline {
                PROBE_GAVE_UP.store(true, Ordering::SeqCst);
                break;
            }
            core::hint::spin_loop();
        }

        // SAFETY: as above. Same address, same installed space, no `satp` write in between.
        SEEN_AFTER.store(
            unsafe { core::ptr::read_volatile(VA as *const u8) },
            Ordering::SeqCst,
        );

        mmu::deactivate_user();
        mmu::permit_kernel_access_to_user_pages(could_reach_user);
        STAGE.store(3, Ordering::SeqCst);
        crate::arch::interrupts::restore(was_enabled);
    })
    .expect("spawn_on failed");

    assert!(
        spin_until(|| STAGE.load(Ordering::SeqCst) >= 1),
        "the probe never cached the translation on core {target}",
    );

    // Move `VA` onto the fresh frame with NO per-address invalidation, so `flush_asid` is the only
    // announcement in the system and the test measures it alone.
    {
        // SAFETY: `space.root()` is this live space's low-half root, and the direct map makes
        // `phys_to_ptr` valid for its tables. `|| None`: the tables reaching `VA` already exist, so
        // the mapper never allocates.
        let mut mapper = unsafe {
            Mapper::<_, _, mmu::Format>::new(space.root(), Half::Low, || None, phys_to_ptr)
        };
        let (old, flush) = mapper.unmap(VA).expect("VA was not mapped");
        assert_ne!(
            old,
            fresh.addr(),
            "the allocator handed back the frame we just unmapped: nothing would change"
        );
        mapper
            .map(VA, fresh.addr(), Flags::user_data())
            .expect("remap onto the fresh frame");

        // Discharged with a no-op ON PURPOSE, and this is the only place in the tree that does it.
        // The real per-address flush (`arch::mmu::flush_tlb`) broadcasts too, so using it here would
        // make the test pass whatever `flush_asid` did. `assume_no_stale_entry` would be a lie: a
        // stale entry is precisely what we arranged.
        flush.flush(|_| {});
    }

    mmu::flush_asid(asid);
    STAGE.store(2, Ordering::SeqCst);

    assert!(
        spin_until(|| STAGE.load(Ordering::SeqCst) == 3),
        "the probe never finished its second read",
    );
    assert!(
        !PROBE_GAVE_UP.load(Ordering::SeqCst),
        "the probe timed out waiting for the remap: the handshake, not the TLB, is broken",
    );
    assert_ne!(
        PROBE_CORE.load(Ordering::SeqCst),
        here,
        "the probe ran on this core, so a local flush would have covered it and the test proves \
         nothing about the shootdown",
    );

    assert_eq!(
        SEEN_BEFORE.load(Ordering::SeqCst),
        OLD,
        "the probe did not read the original mapping, so it never cached the entry this test is \
         about",
    );
    assert_eq!(
        SEEN_AFTER.load(Ordering::SeqCst),
        NEW,
        "STALE TLB ON ANOTHER CORE: core {} still translates {VA:#x} to the frame this space \
         stopped using, after a flush of its ASID. On RISC-V that means the SBI RFENCE never \
         reached it (sfence.vma is local); reuse the number and the next address space reads this \
         one's memory.",
        PROBE_CORE.load(Ordering::SeqCst),
    );

    assert!(
        wait_for(|| !sched::thread_present(probe)),
        "the probe thread was never reaped",
    );
    drop(space);
    crate::memory::free(fresh);
}

/// The loader honours the file's permissions, and does not widen them.
///
/// An ELF's `.rodata` segment is `PF_R` alone. The tempting shortcut is to map every
/// non-executable segment as `user_data()`, which is **writable**: quietly granting the
/// program authority its own file never asked for.
#[test_case]
fn a_read_only_segment_is_mapped_read_only() {
    let image = init_image();
    let (space, _) = load(image).expect("the initrd did not load");

    let rodata = elf::Elf::parse(image)
        .unwrap()
        .segments()
        .find(|s| s.is_readable() && !s.is_writable() && !s.is_executable())
        .expect("the test binary has no read-only segment");

    // Install it so we can ask the CPU's own tables, rather than our record of them.
    // SAFETY: nothing is at EL0 right now; we are a kernel thread mid-test.
    unsafe { mmu::activate_user(space.ttbr0()) };

    let (_, flags) = mmu::translate_user(rodata.vaddr).expect(".rodata is not mapped at all");

    assert!(
        flags.is_user_accessible(),
        "EL0 cannot read its own .rodata"
    );
    assert!(!flags.is_writable(), "the loader made .rodata WRITABLE");
    assert!(!flags.is_user_executable(), ".rodata is executable at EL0");
    assert!(
        !flags.is_kernel_executable(),
        ".rodata is executable at EL1"
    );

    mmu::deactivate_user();
    drop(space);
}

/// **The question the kernel must ask, asked of the hardware.**
///
/// `AT S1E0R` means *translate this address as EL0 would, for a read*. One instruction, and
/// it is the difference between a kernel and a confused deputy.
///
/// Note the precondition assertion. Without it the test is vacuous: "EL0 cannot read the
/// kernel's text" proves nothing if the kernel's text is not mapped in the first place.
///
/// **aarch64-only because it already has a RISC-V twin, not because RISC-V cannot ask.**
/// `riscv_virtio_tests::the_page_tables_say_u_mode_cannot_read_the_kernels_memory` asserts the
/// same three things there. They are deliberately separate tests rather than one portable one,
/// because the *mechanism* is what each is about: this one asks the silicon a question
/// (`AT S1E0R`), and RISC-V has no such instruction, so its twin walks the tables in software
/// and reads the `U` bit. Merging them would mean asserting only what both can say, which is
/// less than either says now.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_hardware_says_el0_cannot_read_the_kernels_memory() {
    const KERNEL_TEXT: u64 = 0xffff_0000_4008_0000;

    let (space, _) = load(init_image()).expect("the initrd did not load");

    // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
    unsafe { mmu::activate_user(space.ttbr0()) };

    // The precondition, and it is what gives the assertion below its teeth: that address IS
    // mapped, and the KERNEL can read it. It reads it all day.
    assert!(
        mmu::translate(KERNEL_TEXT).is_some(),
        "the kernel's text is not mapped, so this test proves nothing",
    );

    // And EL0 cannot. Not "we decline to"; the silicon says no.
    assert!(
        !mmu::user_can_read(KERNEL_TEXT),
        "the hardware says EL0 could read the kernel's own text",
    );
    assert!(!mmu::user_can_write(KERNEL_TEXT));

    // It can read its own code, or the check is a rubber stamp that says no to everything.
    assert!(
        mmu::user_can_read(0x40_0000),
        "EL0 cannot read its own .text, so the check refuses everything and proves nothing",
    );

    // And not an address in its own half that nobody mapped.
    assert!(!mmu::user_can_read(0x7000_0000));

    mmu::deactivate_user();
    drop(space);
}

/// **A program with the capability can print. The same program without it cannot.**
///
/// The binary is byte-identical. Nothing about it changed. What changed is what it was
/// *handed*, and that is the entire content of DECISIONS §10.
///
/// It reports by `brk`, which the kernel treats as a fault: the program expects `NoSuchSlot`
/// from an empty slot and expects `BadPointer` when it asks the kernel to read the kernel's
/// own memory, and it kills itself if either is wrong. So **no fault** means every one of
/// those held.
#[test_case]
fn a_user_client_moves_data_through_shared_memory() {
    // What the client prints first. Must match user/src/hello.rs.
    const FIRST_LINE: &[u8] = b"      hello from EL0, printed by a driver that also runs at EL0.\n";
    const SHARED_VA: u64 = 0x0000_0000_0060_0000;

    static CAPTURED: AtomicBool = AtomicBool::new(false);
    static LEN: AtomicU64 = AtomicU64::new(0);
    static mut BUF: [u8; 128] = [0; 128];

    let image = init_image();
    let request = sched::create_endpoint();
    let reply = sched::create_endpoint();

    // The shared page, owned by the test (not by either address space), so `map_physical`
    // will not free it. Deliberately leaked: the client spins forever, so there is no safe
    // moment to reclaim it, and one page is a fine price for the test.
    let shared = crate::memory::alloc().expect("no shared frame").addr();

    // The server: a kernel thread that reads the shared page and records the first message.
    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv(request);
            let len = m[0].min(128);
            if !CAPTURED.load(Ordering::SeqCst) {
                // SAFETY: the shared frame is ours via the direct map; the client wrote `len`
                // bytes before sending. Single-threaded capture.
                let src = crate::arch::mmu::phys_to_virt(shared) as *const u8;
                let dst = (&raw mut BUF).cast::<u8>();
                for i in 0..len as usize {
                    // SAFETY: both pointers are in range; BUF is 128 bytes and len <= 128.
                    unsafe {
                        core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i)));
                    };
                }
                LEN.store(len, Ordering::SeqCst);
                CAPTURED.store(true, Ordering::SeqCst);
            }
            sched::ipc_send(reply, [0, 0, 0]); // ack, so the client reuses the buffer
        }
    })
    .expect("spawn failed");

    // The client: the real binary, client role, wired to the endpoints and the shared page.
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 2, // printing-client role (matches user/src/hello.rs)
                arg1: 0,
                arg2: 0,
                grants: &[
                    crate::cap::endpoint_cap(request, crate::cap::Rights::WRITE),
                    crate::cap::endpoint_cap(reply, crate::cap::Rights::READ),
                ],
                maps: &[Mapping {
                    va: SHARED_VA,
                    phys: shared,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("spawn failed");

    assert!(
        wait_for(|| CAPTURED.load(Ordering::SeqCst)),
        "the server never received a message through shared memory",
    );
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults,
        "the client faulted instead of printing cleanly",
    );

    let len = LEN.load(Ordering::SeqCst) as usize;
    // SAFETY: written by the server thread, which stopped touching BUF once CAPTURED.
    let got = unsafe { core::slice::from_raw_parts((&raw const BUF).cast::<u8>(), len) };
    assert_eq!(
        got, FIRST_LINE,
        "the wrong bytes arrived through shared memory"
    );
}

/// `map_physical` puts one physical frame into an address space at a chosen VA, with exactly
/// the permissions asked for and no more. The mechanism a driver leaves the kernel on.
#[test_case]
fn map_physical_maps_a_shared_frame_and_a_device_page() {
    const DATA_VA: u64 = 0x0000_0000_0060_0000;
    const DEV_VA: u64 = 0x0000_0000_0070_0000;
    // A real device's MMIO on this machine, whichever machine it is: the virtio-mmio bus base.
    // It was the PL011's `0x0900_0000`, which is an aarch64 `virt` fact; the point of the test
    // is that a device-typed mapping lands where it was asked to, and either address serves it.
    let device_phys = mmu::VIRTIO_MMIO_BASE;

    let mut space = AddressSpace::new(2).expect("no address space");
    let frame = crate::memory::alloc().expect("no frame").addr();

    space
        .map_physical(DATA_VA, frame, Flags::user_data())
        .expect("shared map failed");
    space
        .map_physical(DEV_VA, device_phys, Flags::user_device())
        .expect("device map failed");

    // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
    unsafe { mmu::activate_user(space.ttbr0()) };

    let (data_pa, data_f) = mmu::translate_user(DATA_VA).expect("shared page not mapped");
    assert_eq!(data_pa, frame, "shared page maps the wrong frame");
    assert!(data_f.is_user_accessible() && data_f.is_writable());
    assert!(!data_f.is_user_executable());

    let (dev_pa, dev_f) = mmu::translate_user(DEV_VA).expect("device page not mapped");
    assert_eq!(
        dev_pa, device_phys,
        "device page maps the wrong physical address"
    );
    assert!(dev_f.is_user_accessible() && dev_f.is_writable());

    mmu::deactivate_user();
    crate::memory::free(Frame::from_addr(frame));
    drop(space);
}

/// A thread can name nothing until somebody hands it something.
#[test_case]
fn a_new_thread_holds_no_capabilities() {
    use crate::cap::Error;

    // The current thread is a kernel thread, spawned by the harness, and was handed nothing.
    for slot in 0..16 {
        assert_eq!(
            sched::current_cap(slot).err(),
            Some(Error::NoSuchSlot),
            "slot {slot} is not empty in a thread nobody granted anything",
        );
    }
}

/// **A userspace driver reads a file off a real virtio disk.** Milestone 9, end to end.
///
/// The kernel enumerates the bus and hands a driver at EL0 the device registers, a DMA page,
/// and an interrupt. The driver sets up a virtqueue, reads the superblock by DMA, parses the
/// nifefs directory, reads the `motd` file, and reports its first bytes. We check them
/// against the known contents, which proves real disk data crossed DMA and the EL0 boundary.
///
/// It also proves the interrupt path (9a) carried the completion: `ROUTED_IRQS` counts device
/// interrupts turned into messages, and it must rise. And it proves the idle thread works: the
/// driver blocks waiting for that interrupt with nothing else to run, and the scheduler idles
/// rather than declaring a deadlock.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
    use crate::arch::exceptions::ROUTED_IRQS;

    let Some(report) = virtio_service::start(init_image()) else {
        // No disk attached to this run. Nothing to test; do not fail.
        crate::println!("    (no virtio disk attached; skipping)");
        return;
    };

    let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);

    // Blocks until the driver has done the whole read. If the driver faults, it never sends,
    // and the scheduler idles; the QEMU-level timeout is the backstop.
    let word = sched::ipc_recv(report)[0];

    assert_eq!(
        &word.to_le_bytes(),
        b"nife: re",
        "the driver reported the wrong file contents",
    );
    assert!(
        ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
        "the read completed but no device interrupt was delivered as a message",
    );
}

/// **`std::fs` end to end over the FS-service contract** (milestone 27 phase two, the FS half).
///
/// An ordinary Rust program, granted **one directory capability** and nothing else that names a
/// filesystem, opens the file the host-made RedoxFS image ships, reads it with `Read` and
/// `read_to_string`, stats it, and gets refused when it tries to name anything outside that
/// directory. The bytes it prints are the file's own, so the assertion covers the whole path:
/// disk, DMA-confined block server, FS server running an engine we did not write, the file
/// contract, std's PAL, and the stdout endpoint.
///
/// What it proves that the hand-written client's test does not: `std::fs::File::open` has no
/// global namespace to resolve against, and the mapping to a granted directory holds from inside
/// std, including the refusal of `..`, of an absolute path, and of a nested path. And the same
/// binary run without slot 4 gets `Unsupported`, which the offline std test asserts.
// RISC-V twin: `riscv_virtio_tests::std_fs_reads_a_file_through_a_granted_directory_capability`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn std_fs_reads_a_file_through_a_granted_directory_capability() {
    let Some((readiness, report)) = fs_service::start_std(
        init_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        std_exerciser_image(),
    ) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    assert_fs_service_ready(readiness);

    let mut want = [0u8; 768];
    let n = std_fs_expected(&mut want);
    assert_std_transcript(report, &want[..n], "std fs");
}

/// **The RedoxFS filesystem service, end to end** (milestone 32 phase 2, the flagship
/// userspace-reuse story). Three confined processes: a block server drives the RedoxFS disk over
/// DMA, an FS server mounts it over blk IPC and serves files from its own heap, and a client
/// opens `motd` through a granted directory capability, reads it, writes a pattern to `scratch`
/// and reads it back, then reports. The client names nothing but its directory endpoint, so a
/// success here is the whole capability contract holding: designation is authorization, the
/// handle is a server-minted token, and a real CoW filesystem we did not write runs confined.
// RISC-V twin: `riscv_virtio_tests::the_fs_server_serves_redoxfs_over_a_capability_contract`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_fs_server_serves_redoxfs_over_a_capability_contract() {
    let Some((readiness, report)) = fs_service::start(
        init_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        0, // the end-to-end proof role, not the benchmark loop
    ) else {
        // No RedoxFS disk attached to this run. Nothing to test; do not fail.
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };

    // The two servers' readiness sentinels, if this test is the one that wired them (the
    // `std::fs` test shares the same service, and each sentinel is sent exactly once).
    assert_fs_service_ready(readiness);

    // Then: the client has read motd, round-tripped scratch, exercised extended attributes, and
    // reported. If any of the three processes faults, it never sends and the QEMU-level timeout
    // is the backstop.
    let [head, status, attrs, ..] = sched::ipc_recv(report);
    assert_eq!(
        status,
        fs_proto::fixture::SUCCESS,
        "the client did not report success: a check in the read or write path failed",
    );
    assert_eq!(
        &head.to_le_bytes()[..],
        &fs_proto::fixture::MOTD[..8],
        "the client read the wrong motd bytes off the RedoxFS image",
    );
    assert_attrs(attrs);
}

/// **A read-only per-file grant, attacked** (milestone 31 phase 2, notes/grant-expression.md).
///
/// `run wc report.txt` must hand over one file, not the directory it lives in. This wires
/// exactly that: an `fs_file_caretaker` holding the directory capability, a confined program
/// holding only the caretaker's endpoint, and a grant of `motd`, read-only. The program is the
/// attacker role of `fs_test_client`, and it spends its life trying to make that sentence false.
///
/// **What makes it a witness and not a formality.** Every attempt is against something that
/// really exists and that the process one hop up the chain can really reach: `scratch` is on
/// the image, one directory entry away, and the caretaker could open it on any request.
/// Milestone 33's attacker was handed a real neighbour's address rather than a fictional one
/// for the same reason. And this test alone would still be weak, because a caretaker that
/// refused *everything* would pass it; that is what the writable twin below is for, and why the
/// verdict is a bitmap rather than a boolean.
// RISC-V twin: `riscv_virtio_tests::a_read_only_per_file_grant_survives_an_attacker`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_read_only_per_file_grant_survives_an_attacker() {
    let Some(verdict) = attack_a_grant(fs_proto::grant::READ, false) else {
        return;
    };
    assert_eq!(
        verdict,
        0,
        "the read-only per-file grant leaked: {}",
        describe_escape(verdict),
    );
}

/// **A read-only per-file grant carries its file's attributes, and cannot write them**
/// (milestone 61).
///
/// It is the same run as the test above, read a second way, which is why it costs nothing: a
/// clean verdict of zero already says both halves. `GRANTED_ATTRS_FAILED` clear means the
/// listing and the get went **through** the caretaker to the store, which they could not do
/// before this milestone (all four attribute verbs answered `EOPNOTSUPP`); `WROTE_ATTR` clear
/// means the set did not, because a read-only grant must not forward one.
///
/// Stated as its own assertion rather than left inside the zero, because "the verdict was zero"
/// does not tell a reader which properties were in it, and the whole point of a bitmap is that
/// each bit is a sentence. The writable twin below is what stops this passing vacuously.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_read_only_per_file_grant_reads_its_files_attributes_and_writes_none() {
    use fs_proto::fixture::escape;
    let Some(verdict) = attack_a_grant(fs_proto::grant::READ, false) else {
        return;
    };
    assert_eq!(
        verdict & escape::GRANTED_ATTRS_FAILED,
        0,
        "the caretaker did not forward the attribute reads, so a program behind a per-file \
         grant still cannot reach its own file's attributes",
    );
    assert_eq!(
        verdict & escape::WROTE_ATTR,
        0,
        "a read-only grant forwarded SETXATTR: an attribute is a way to change a file",
    );
}

/// **A writable per-file grant, attacked** (the second witness, and the first one's control).
///
/// Same caretaker, same attacker, same neighbouring file; only the granted direction changes.
/// Two things fall out of it, and the second is why it exists:
///
/// - A writable file capability really does write, and still reaches **only** its one file. The
///   widening is exactly one axis wide.
/// - The read-only test above is now meaningful. Its refusals are a narrowed capability rather
///   than a caretaker that says no to everything, because here the same requests, through the
///   same code, succeed. A confinement test with no witness that the thing being confined
///   *works* is a test that passes when the feature is missing entirely.
// RISC-V twin: `riscv_virtio_tests::a_writable_per_file_grant_writes_that_file_and_still_only_that_file`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_writable_per_file_grant_writes_that_file_and_still_only_that_file() {
    use fs_proto::fixture::escape;
    let Some(verdict) = attack_a_grant(fs_proto::grant::READ | fs_proto::grant::WRITE, true) else {
        return;
    };
    // `WROTE_ATTR` joined the expected set in milestone 61, and it is the third way to change a
    // file: bytes, length, and what is attached to it. A direction check that covered only the
    // first two would have left one open.
    let expected = escape::WROTE | escape::TRUNCATED | escape::WROTE_ATTR;
    assert_eq!(
        verdict,
        expected,
        "a writable grant must write, truncate and set an attribute on its own file and do \
         nothing else: {}",
        describe_escape(verdict & !expected),
    );
}

/// Wire a per-file grant of the given direction, run the attacker against it, and return its
/// verdict bitmap. `None` when no RedoxFS disk is attached (nothing to test; do not fail).
#[cfg(target_arch = "aarch64")]
fn attack_a_grant(rights: u64, writable: bool) -> Option<u64> {
    let Some(report) = fs_service::start_granted(
        init_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_file_caretaker").expect("no fs_file_caretaker program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        fs_service::Grant {
            // The writable run damages what it is granted, so it is granted the file the fixture
            // discipline already covers; see the attacker's own note.
            name: if writable {
                fs_proto::fixture::SCRATCH_NAME
            } else {
                fs_proto::fixture::MOTD_NAME
            },
            rights,
            role: 2, // ROLE_ATTACKER
            arg: writable as u64,
        },
    ) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return None;
    };
    // The two handshakes happened inside `start_granted`, before this attacker existed: they
    // are what makes the caretaker's own staged request safe on a page all three share.
    let [tag, verdict, ..] = sched::ipc_recv(report);
    assert_eq!(
        tag,
        fs_proto::fixture::VERDICT,
        "the attacker's report is not a verdict word",
    );
    Some(verdict)
}

/// Name the bits an escape verdict set, so a failure reads as a sentence instead of a bitmap.
#[cfg(target_arch = "aarch64")]
fn describe_escape(v: u64) -> &'static str {
    use fs_proto::fixture::escape;
    if v & escape::SECOND_FILE != 0 {
        "it opened a file the grant does not designate"
    } else if v & escape::WROTE != 0 {
        "it wrote through a read-only grant"
    } else if v & escape::TRUNCATED != 0 {
        "it truncated through a read-only grant"
    } else if v & escape::CREATED != 0 {
        "it created a file through a file capability"
    } else if v & escape::FORGED_HANDLE != 0 {
        "it reached a file with a handle it was never given"
    } else if v & escape::GRANTED_READ_FAILED != 0 {
        "the granted read itself failed, so nothing above was actually proven"
    } else if v & escape::WROTE_ATTR != 0 {
        "it set an extended attribute through a read-only grant"
    } else if v & escape::GRANTED_ATTRS_FAILED != 0 {
        "it could not reach its own file's attributes, which milestone 61 says it carries"
    } else {
        "nothing (an empty verdict should not have failed an assertion)"
    }
}

/// **The FS server's stack is sized by measurement, and this is the measurement.**
///
/// Runs after both FS clients, so the poison in the server's stack pages has been overwritten to
/// exactly the depth RedoxFS reached across a mount, reads, writes, a create and two truncates.
/// It prints the number and fails if less than a quarter of the grant is left.
///
/// This exists because the previous size was a guess, and the guess was **528 bytes short**.
/// Milestone 31 phase 2's `CREATE` and `TRUNCATE` added one more level of tree recursion, the FS
/// server ran off the bottom of its stack mid-request, and the kernel killed it, correctly and
/// legibly. What was not legible was anything downstream: the std client sat blocked on a `CALL`
/// nobody would ever answer, and since other tests had left processes spinning on other cores,
/// the no-progress heartbeat saw a healthy system. The only instrument that fired was the
/// per-test wall-clock ceiling, so a 368-byte overflow presented as "`std_fs` takes 914 seconds".
/// A number nobody can defend is a number that will be wrong again; this one now has a witness.
/// **A kill mid-transaction, on the real device** (milestone 37, DECISIONS §34 condition 1).
/// The host sweep proves the property over every fault point against a reconstructed platter;
/// this proves it once through the whole stack, with a real virtio write torn in half, a real
/// FS-server process killed inside its own transaction, and a real second process recovering the
/// disk it left behind. See `std_tests::assert_a_kill_mid_transaction_recovers`.
// RISC-V twin: `riscv_virtio_tests::a_kill_mid_transaction_leaves_the_filesystem_consistent`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_kill_mid_transaction_leaves_the_filesystem_consistent() {
    assert_a_kill_mid_transaction_recovers(
        init_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
    );
}

// RISC-V twin: `riscv_virtio_tests::the_fs_servers_stack_still_has_headroom`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_fs_servers_stack_still_has_headroom() {
    let Some((used, total)) = fs_service::fs_stack_used() else {
        crate::println!("    (no FS service wired this boot; skipping)");
        return;
    };
    crate::println!("    (FS server stack high-water: {used} of {total} bytes) ");
    assert!(
        used * 4 <= total * 3,
        "the FS server used {used} of {total} stack bytes: under a quarter left. RedoxFS \
         recurses in 8 KiB frames, so the next verb that deepens a tree walk will overflow and \
         the server will die mid-request. Raise FS_STACK_PAGES.",
    );
}

/// **A userspace driver completes a DHCP round trip over virtio-net.** Milestone 30, end to
/// end, and the proof the multi-queue confinement carries a real NIC.
///
/// The kernel enumerates the NIC and hands a driver at EL0 a confined `Virtio` capability, a DMA
/// page, and an interrupt. The driver brings up BOTH virtqueues (receive = 0, transmit = 1),
/// posts a receive buffer, transmits a hand-built DHCP DISCOVER, and waits for QEMU user-mode
/// networking's OFFER. It reports the offered address, which must land in slirp's 10.0.2.0/24.
/// Because a valid OFFER for our transaction is the only path to that report, a match proves the
/// DISCOVER left (TX) and the OFFER returned (RX), across both queues and both directions of the
/// confinement, with no TCP/IP stack in the loop.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net() {
    let Some(report) = virtio_service::start_net(init_image()) else {
        // No NIC on this run (a bare boot). The test runners always attach one, so this
        // branch is not the parity gate. See scripts/qemu-runner-*.sh (NIFE_NET).
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };

    let yiaddr = sched::ipc_recv(report)[0] as u32;
    assert_eq!(
        yiaddr & 0xffff_ff00,
        0x0A00_0200,
        "the DHCP OFFER's yiaddr {yiaddr:#010x} is not in QEMU slirp's 10.0.2.0/24: the round \
         trip did not complete correctly",
    );
    // We do NOT assert a fresh routed interrupt here, unlike the disk read test. The net
    // driver's completion is the used ring advancing, not one interrupt per operation (the same
    // discipline the disk driver's complete loop follows, notes/dma.md), and the net test suite
    // shares one NIC across many drivers and servers (piece 3): a leftover completion from a
    // prior operator can be counted before this test's baseline and then consumed as a stale
    // wakeup, so a strict interrupt-delta is unreliable. The OFFER round trip above is the proof
    // that the interrupt path carried the completion.
}

/// The same DHCP round trip over the PCIe transport, behind the IOMMU (milestone 30, §20): the
/// NIC is confined in hardware to its DMA region, and the driver binary is byte-identical to the
/// mmio one. Proves the multi-queue confinement and the net driver work over the bus real
/// hardware uses.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci() {
    let Some(report) = virtio_service::start_net_pci(init_image()) else {
        crate::println!("    (no virtio-net-pci device attached; skipping)");
        return;
    };

    let yiaddr = sched::ipc_recv(report)[0] as u32;
    assert_eq!(
        yiaddr & 0xffff_ff00,
        0x0A00_0200,
        "the DHCP OFFER's yiaddr {yiaddr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
    );
}

/// **The net server: smoltcp running DHCP over the confined NIC** (milestone 30, piece 3). The
/// integration proof and the thesis headline for networking: a real, reused TCP/IP stack
/// (smoltcp, not hand-built) runs entirely at EL0, brings the NIC up through the `Virtio`
/// capability, and completes a DHCP handshake against QEMU user-mode networking. The kernel
/// knows nothing about DHCP; it owns only the DMA confinement. The server reports the acquired
/// address, which must land in slirp's 10.0.2.0/24, so only a real DHCP round trip driven by
/// smoltcp over the confined NIC can produce it.
// RISC-V twin: `riscv_virtio_tests::the_net_server_acquires_a_dhcp_lease_over_smoltcp`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_net_server_acquires_a_dhcp_lease_over_smoltcp() {
    let Some((report, net)) = virtio_service::start_net_server(net_stack_image()) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    let addr = sched::ipc_recv(report)[0] as u32;
    assert_eq!(
        addr & 0xffff_ff00,
        0x0A00_0200,
        "smoltcp's DHCP lease {addr:#010x} is not in QEMU slirp's 10.0.2.0/24",
    );
    net.release_or_fail("a net test's net_stack");
}

/// The net server over the PCIe transport, behind the IOMMU (milestone 30, §20): smoltcp drives
/// a NIC confined in hardware and still gets its lease.
// RISC-V twin: `riscv_virtio_tests::the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci() {
    let Some((report, net)) = virtio_service::start_net_server_pci(net_stack_image()) else {
        crate::println!("    (no virtio-net-pci device attached; skipping)");
        return;
    };
    let addr = sched::ipc_recv(report)[0] as u32;
    assert_eq!(
        addr & 0xffff_ff00,
        0x0A00_0200,
        "smoltcp's DHCP lease {addr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
    );
    net.release_or_fail("a net test's net_stack");
}

/// **The socket contract, UDP end to end** (milestone 30, piece 3 phase B; DECISIONS §25). A
/// client process holds a `Stack` endpoint and its own untyped, mints a shared frame, delegates
/// it, opens a UDP socket by id, sends a datagram, and reads the reply back through the same
/// frame. No ambient network: the client acts only through the capability it was granted, and the
/// bytes cross in the shared frame, never in a message. Proves the whole path, client to `net_stack` to
/// smoltcp to the confined NIC, over the mmio transport.
///
/// The peer is **slirp's own TFTP server** (10.0.2.2:69), served inside libslirp, so the exchange
/// is deterministic and never leaves the emulator. This test used to query 10.0.2.3:53, which
/// reads like a local resolver but is not one: libslirp NATs that address to the *host's*
/// nameserver, so the gate depended on the developer's DNS answering at that instant and flaked
/// (~2.5% per query, measured). The real-resolution case still runs, non-gating, below.
// RISC-V twin: `riscv_virtio_tests::a_client_completes_a_udp_round_trip_through_the_socket_contract`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_client_completes_a_udp_round_trip_through_the_socket_contract() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_UDP_TFTP,
        false,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the UDP round trip against slirp's TFTP server failed (client code {verdict:#x})",
    );
    net.release_or_fail("a net test's net_stack");
}

/// The same UDP round trip over the PCIe transport, behind the IOMMU.
// RISC-V twin: `riscv_virtio_tests::a_client_completes_a_udp_round_trip_through_the_socket_contract_pci`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_client_completes_a_udp_round_trip_through_the_socket_contract_pci() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_UDP_TFTP,
        true,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net-pci device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the UDP round trip over PCIe failed (client code {verdict:#x})",
    );
    net.release_or_fail("a net test's net_stack");
}

/// **Real DNS resolution, deliberately non-gating.** The query goes to 10.0.2.3, which libslirp
/// NATs to the host's configured nameserver (`get_dns_addr_libresolv`), so whether it is answered
/// is a fact about the developer's machine, not about this kernel. The client retries like any
/// resolver client and reports `NO_ANSWER` if the host never replied, which we print and skip: a
/// committed gate must not depend on somebody's router. What still fails loudly is a response
/// that arrives and is *wrong* (not our transaction id, or not a response), because that would be
/// our defect. The deterministic UDP coverage is the TFTP pair above. See notes/net.md.
// RISC-V twin: `riscv_virtio_tests::a_client_resolves_a_real_dns_name_when_the_host_resolver_answers`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_client_resolves_a_real_dns_name_when_the_host_resolver_answers() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_UDP_DNS,
        false,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    if verdict == NET_CLIENT_NO_ANSWER {
        crate::println!(
            "    (the host's resolver did not answer; real-DNS check skipped, not a failure)"
        );
        return;
    }
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "a DNS response came back but was not a valid reply to our query (client code \
         {verdict:#x}): a socket-contract defect, not a network problem",
    );
    net.release_or_fail("a net test's net_stack");
}

/// **The socket contract, TCP end to end** (milestone 30, piece 3 phase B). A client opens a TCP
/// socket by id, connects to slirp's guestfwd echo peer (10.0.2.9:7777, piped to `/bin/cat`),
/// sends a payload, receives the echo, and closes. The full round trip, handshake through
/// bidirectional data to teardown, deterministic and zero-host-setup (nothing outlives QEMU),
/// through the client, `net_stack`, smoltcp, and the confined NIC.
// RISC-V twin: `riscv_virtio_tests::a_client_echoes_over_tcp_through_the_socket_contract`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_client_echoes_over_tcp_through_the_socket_contract() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_TCP_ECHO,
        false,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the TCP echo round trip through the socket contract failed (client code {verdict:#x})",
    );
    net.release_or_fail("a net test's net_stack");
}

/// The same TCP echo round trip over the PCIe transport, behind the IOMMU.
// RISC-V twin: `riscv_virtio_tests::a_client_echoes_over_tcp_through_the_socket_contract_pci`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_client_echoes_over_tcp_through_the_socket_contract_pci() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_TCP_ECHO,
        true,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net-pci device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the TCP echo round trip over PCIe failed (client code {verdict:#x})",
    );
    net.release_or_fail("a net test's net_stack");
}

/// **Regression: reusing a socket id is safe** (the ephemeral-port fix). A client opens a TCP
/// socket on id 0, connects to the echo peer, closes it, then reopens the same id and connects
/// again. `net_stack` derived the local port from the socket id, so the reopen reused the exact port and
/// the second connect stalled on a slirp flow that had not cleared; the rotating allocator hands
/// the reopen a fresh port, so both connects complete. The client reports OK only if they do.
// RISC-V twin: `riscv_virtio_tests::a_reopened_socket_id_connects_again_over_tcp`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_reopened_socket_id_connects_again_over_tcp() {
    let Some((report, net)) = virtio_service::start_net_stack(
        net_stack_image(),
        NET_TEST_TCP_REOPEN,
        false,
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "reopening a socket id and connecting again failed (client code {verdict:#x}): the \
         ephemeral local port is not independent of the socket id",
    );
    net.release_or_fail("a net test's net_stack");
}

/// **The guest is connected TO, on a port it was granted** (milestone 107). Every network exchange
/// this project had proved was outbound: the TCP gate connects to a slirp `guestfwd` peer, the UDP
/// gate is a request, the DHCP bring-up is a client. nife could reach the network and could
/// not be reached.
///
/// Here a **host process** opens a TCP connection to a port QEMU forwards into the guest
/// (`hostfwd`, the mirror of the `guestfwd` the outbound gate uses), sends a payload, and reads
/// back an answer the guest *composed*. The guest holds only a `Stack` endpoint and a shared frame,
/// listens on the one port its spawn granted, accepts, reads, answers, and does the whole thing
/// again on the same listener, which is what proves the listener re-arms rather than serving one
/// connection and going deaf. The host side is xtask's inbound prober, running beside the suite the
/// way the scanout referee does.
///
/// **The grant half rides in the same exchange**, before the first connection: 8080 must be refused
/// as a matter of *authority* (`LISTEN_DENIED`, a distinct answer from "in use", because the two
/// call for opposite responses from a client), 7778 must bind, and asking for 7778 again on a second
/// socket id must collide. That is who-binds-the-port answered concretely: a port is an exclusive
/// name in a shared namespace, so it is authority, and the **spawn service** decides the range.
/// Note that no frame is attached until after all of it, because a listener carries no bytes.
///
/// It is one test rather than two because, when it was written, two net servers did not fit: the
/// second cost an untyped region nothing reclaimed, and the boot had no such run left (see
/// `virtio::MAX_DEVICES`). **That constraint was lifted on 2026-08-16**, when a net service became
/// reclaimable (notes/frames.md); the tests stay merged because splitting them is its own change
/// with its own argument. The stage codes stand in for the names the second test would have had.
///
/// **The mDNS-shaped exchange rides in this same spawn too** (milestone 55's stack half), for the
/// same memory reason, re-measured by the lane that built it: a twelfth net server died as
/// `Unmappable(OutOfFrames)` in an unrelated later test. After the accept rounds, the client
/// proves the three things a responder needs and nothing else touches: binding UDP 5353 is an
/// authority (a port outside the spawn's `udp_bind_grant` is refused, the granted one binds and
/// is exclusive; and since this spawn's word carries both grants, the composed packing is what
/// the machine exercises); a datagram addressed to 224.0.0.251, not to the guest, is accepted
/// because the stack joined the group (without smoltcp's `multicast` feature it dies in the IPv4
/// input path, unseen by UDP); and the querier's source endpoint rides back on RECV, which RFC
/// 6762 §6.7's semantics turn on. Slirp cannot carry multicast, so the host side is xtask's
/// multicast prober on the frame-level hub the runner wires beside slirp: it takes the guest's
/// own multicast send off the wire (which is what proves SENDTO to a group reaches it), injects
/// the group-addressed query with a spoofed source nothing on the network holds, and requires
/// the guest's composed answer. See notes/mdns.md for what QEMU still cannot prove.
///
/// **Milestone 54 rides the same spawn, for the same reason.** The SMB adapter (`smb_server`) is
/// a second client of this stack, granted the neighbouring port, and xtask's SMB prober drives a
/// real SMB2 exchange (negotiate, session setup, tree connect, create, and a read whose bytes the
/// prober asserts, twice over two connections) through a second `hostfwd` while the echo exchange
/// runs. Two verdicts come back and both gate. See `virtio_service::start_net_stack_with_smb` for
/// why they share a NIC, and notes/smb.md for the protocol's scope.
///
/// **And the session is authenticated** (milestone 54's identity item): the share is wired
/// `SMB_SHARE_FS_AUTHENTICATED`, so the adapter admits nobody without an NTLMv2 proof that
/// milestone 65's credential service accepts. That makes this the first boot holding the block
/// server, the FS server, `net_stack`, the SMB adapter **and** the credential service at once, and
/// it puts four processes on the path of one `ls`: a host client proves who it is to a program that
/// holds no key, which asks a program that holds no filesystem, over a share whose bytes come from a
/// program that holds no network. The prober's refusal legs are what prove the gate is a gate, and
/// [`assert_smb_held_no_key`] is what proves the adapter came away with nothing.
///
/// **And the share is the real filesystem** (the milestone's second act): this test first wires
/// the FS service and runs `fs_test_client`'s seed role, which writes
/// `fs_proto::fixture::SMB_SEED` through the FS server, and then hands the adapter the directory
/// capability. What the prober reads back is therefore bytes that crossed
/// RedoxFS -> `fs_proto` -> the `Share` seam -> SMB2 -> TCP, seeded by a different process than
/// the one serving them. This is the first boot that holds the block server, the FS server,
/// `net_stack`, and the SMB adapter at once, which is why it prints the free-frame count: the
/// number is the headroom claim, measured rather than argued. With a NIC but no RedoxFS disk the
/// adapter falls back to its fixture, and the prober (whose expectation is the seeded file)
/// fails the run; under `script/test` both devices are always attached.
// RISC-V twin: `riscv_virtio_tests::a_host_process_connects_to_the_guest_and_is_answered`.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_host_process_connects_to_the_guest_and_is_answered() {
    // E2's baseline (milestone 134): `sched::thread_count()` counts every live thread in the
    // WHOLE boot, and the full suite runs 279 `#[test_case]`s in one continuous boot rather than
    // one per test, so an absolute reading taken here would include whatever earlier tests left
    // allocated (a first attempt read 95, most of which turned out to be exactly that). Taking the
    // delta against this baseline is what isolates "how many threads did wiring THIS topology
    // create" from "how many threads exist in the boot at this point in the suite".
    let e2_baseline_threads = sched::thread_count();
    // Seed the share through the FS service before the adapter exists: the file must be on the
    // filesystem before the prober's first session can open it.
    let fs = fs_service::start(
        init_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        7, // ROLE_SMB_SEED: write fixture::SMB_SEED at fixture::SMB_SEED_NAME, report, exit
    )
    .map(|(readiness, seed_report)| {
        assert_fs_service_ready(readiness);
        let status = sched::ipc_recv(seed_report)[0];
        assert_eq!(
            status,
            fs_proto::fixture::SUCCESS,
            "the seeding client could not put the SMB gate's file on the filesystem",
        );
        let (ep, shared) = fs_service::root_directory(
            init_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
        )
        .expect("the FS service was wired a moment ago");
        // Read-write **and authenticated**: the write half of the gate needs the adapter to accept
        // a write, the read-only refusals are proven by `smb_proto`'s host tests against a share
        // that says no, and since milestone 54's identity item the prober has to prove who it is
        // before it may do either.
        (ep, shared, virtio_service::SMB_SHARE_FS_AUTHENTICATED)
    });
    if fs.is_none() {
        crate::println!("    (no RedoxFS disk attached; the SMB adapter serves its fixture)");
    }
    crate::println!(
        "    (combined boot wired: {} frames free before the net + SMB spawn)",
        crate::memory::free_frames()
    );
    // Taken before `fs` is handed to the spawn below: the write verifier needs to know whether
    // there was a filesystem at all, and the spawn consumes the capability.
    let had_fs = fs.is_some();
    // **The credential service, milestone 65's, sealed** (milestone 54's identity item). The same
    // wiring the credential tests use, latched once per boot, so this is the *real* service holding
    // the *real* key: the adapter below gets its verify endpoint and nothing else, and xtask's
    // prober computes a proof on the host over the password nobody in the guest has. Wired only
    // when there is a filesystem to authenticate access to, because the authenticated share mode is
    // an fs-backed mode.
    //
    // `provisioned()` returning `None` (no virtio-rng device; milestone 145) folds into the same
    // `Option` this closure already produces for `had_fs == false`.
    let cred = had_fs
        .then(super::credential_tests::provisioned)
        .flatten()
        .map(|(w, _, _)| (w.verify, super::credential_service::verify_frame()));
    let Some((report, smb_report, mdns_report, net)) = virtio_service::start_net_stack_with_smb(
        net_stack_image(),
        smb_server_image(),
        mdns_responder_image(),
        NET_TEST_TCP_ACCEPT,
        NET_LISTEN_PORT,
        SMB_LISTEN_PORT,
        SMB_ROUNDS,
        MDNS_QUERIES,
        fs,
        cred,
        socket_proto::udp_bind_grant(NET_MDNS_PORT, NET_MDNS_GRANT_TOP),
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };
    // E2 (milestone 134, design/roadmap/134-the-measurements-that-decide.md): the thread census on
    // the customer path. Every process this boot's customer-facing topology needs is already
    // spawned by this point (the FS service's block server and FS server, if `had_fs`; `net_stack`;
    // the echo client; the SMB adapter; the mDNS responder; the credential service, if `cred` is
    // `Some`), and none of them spawns another kernel thread per connection or per request (each is
    // a single-threaded event loop over its own endpoint), so this count is already the peak: it
    // does not grow further as the host prober's connections arrive. See notes/benchmarks.md and
    // this milestone's register entry for what this settles. Reported as a delta against
    // `e2_baseline_threads` (see this function's top), not as the absolute reading: the absolute
    // count includes whatever earlier tests in this suite's one continuous boot left allocated,
    // which is not this measure's subject.
    crate::println!(
        "    (E2 thread census: {} threads created by wiring this customer-path topology \
         ({} live now, {e2_baseline_threads} live before this test wired anything): main + block \
         server + FS server (had_fs={had_fs}) + net_stack + echo client + SMB adapter + mDNS \
         responder + credential service (present={}))",
        sched::thread_count().saturating_sub(e2_baseline_threads),
        sched::thread_count(),
        cred.is_some(),
    );
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the guest did not serve the inbound exchange (client code {verdict:#x}). 0xE050 or \
         0xE080 mean a port outside a grant was bound anyway, which is the capability failure; \
         0xE060 or 0xE070 means nobody ever connected, which is the host side: is the runner \
         adding a hostfwd (NIFE_HOSTFWD_PORT) and is xtask's inbound prober running beside this \
         suite? 0xE082 or 0xE084 mean the UDP bind grant admitted or refused the wrong port",
    );
    let verdict = sched::ipc_recv(smb_report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the SMB adapter did not serve a mount-shaped exchange (code {verdict:#x}). 0xE11x is the \
         listen grant, 0xE120 means no SMB connection ever arrived (is the runner adding the \
         NIFE_SMB_HOSTFWD_PORT hostfwd, and is xtask's SMB prober running?), 0xE121 means a \
         connection arrived but no SMB message was answered on it, 0xE130 an arg2 share mode \
         nobody defined",
    );
    let verdict = sched::ipc_recv(mdns_report)[0];
    assert_eq!(
        verdict, NET_CLIENT_OK,
        "the mDNS responder did not answer the queries xtask injected (code {verdict:#x}). \
         0xE2xx is this program's range: 0xE20L means its configuration document is wrong at line \
         L, 0xE220 means it was spawned without the UDP bind grant it needs, 0xE221 that something \
         else already held 5353, and 0xE240 that nothing ever asked it anything, which is either \
         the joined group's RX acceptance or the host side (NIFE_MCAST_PORT and xtask's multicast \
         prober). What the prober asserts about the ANSWERS is separate and reported by xtask",
    );
    // Before the release: the verifier spawns a fresh FS client, and reclaiming the net
    // service's regions is the last thing this test should do.
    assert_smb_write_landed(had_fs);
    assert_smb_held_no_key(had_fs);
    net.release_or_fail("a net test's net_stack");
}

/// The `std_exerciser` std program's ELF bytes. The same binary the offline std test spawns; given
/// the network here, its `UdpSocket::bind` probe succeeds and it runs the net transcript.
#[cfg(target_arch = "aarch64")]
fn std_exerciser_image() -> &'static [u8] {
    program("std_exerciser").expect("no std_exerciser program in the initrd archive")
}

/// The exact transcript `std_exerciser` prints when it is granted the network **and refused every
/// listening port**. Pinned so a drift in the net PAL, the contract, or the demo is a loud diff
/// rather than a mystery.
///
/// `listen refused` is milestone 64's negative control and it costs this boot nothing: the stack
/// this program is spawned with carries `socket_proto::NO_LISTEN_GRANT`, so
/// `std::net::TcpListener::bind` answers `PermissionDenied`, and the program says so on its way
/// past rather than quietly running a smaller demo. A lane that broke the grant check open would
/// turn this line into `listen ok` and fail here, in a transcript comparison, rather than passing
/// with more authority than it was given.
#[cfg(target_arch = "aarch64")]
const STD_NET_EXPECTED: &[u8] = b"std net on nife\nlisten refused\nudp ok\ntcp echo ok\n";

/// The exact transcript the same binary prints when its stack **is** granted the listening port
/// (milestone 64's inbound half). Four lines, and each is a separate claim: the granted port binds,
/// a port outside the grant is refused as a matter of authority, the granted port is exclusive, and
/// the listener served `socket_proto::fixture::ROUNDS` connections one after another.
#[cfg(target_arch = "aarch64")]
const STD_LISTEN_EXPECTED: &[u8] =
    b"std net on nife\nlisten ok\ndenied refused\nin use refused\nserved 2\n";

/// **`std::net` end to end over the socket contract** (milestone 27 phase two): the `std_exerciser`
/// std binary, given the network, does a real UDP DNS query and a TCP echo round trip through
/// `std::net::{UdpSocket, TcpStream}`, whose PAL binds to `net_stack`'s contract. The program never
/// sees a capability or a socket id; it writes to a socket and reads from it. This closes the
/// `net honestly unsupported` gap from phase one: std's networking runs on the native ABI,
/// reaching the same path the hand-written client does through std's blocking API. Its stdout
/// is reassembled off the endpoint and compared byte for byte, the `std_exerciser` discipline.
// RISC-V twin: `riscv_virtio_tests::std_net_runs_over_the_socket_contract`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn std_net_runs_over_the_socket_contract() {
    let Some((report, net)) = virtio_service::start_net_std(
        net_stack_image(),
        std_exerciser_image(),
        socket_proto::NO_LISTEN_GRANT,
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };

    assert_std_transcript(report, STD_NET_EXPECTED, "std net");
    net.release_or_fail("a net test's net_stack");
}

/// **A `std::net::TcpListener` serves a port it was granted, and refuses one it was not**
/// (milestone 64's inbound half, on milestone 107's contract).
///
/// The ordinary `std_exerciser` binary, spawned over a stack whose listen grant is exactly
/// `NET_LISTEN_PORT`, binds that port through `std::net`, accepts connections a **host** process
/// opens through QEMU's `hostfwd`, reads each request and composes an answer. The program names no
/// capability and no socket id; it calls `bind`, `accept`, `read_exact` and `write_all`, which is
/// what a Rust server is written out of. This is the difference between "a crate compiles" and "a
/// server runs", and it is what milestone 55's Samba-shaped workload stands on.
///
/// **Two connections, and the second is the load-bearing one.** A listener that accepts once and
/// goes deaf would pass a one-round gate and is precisely what a file server cannot use; the re-arm
/// happens inside `ACCEPT` (notes/net.md) and nothing but a second `accept()` proves it.
///
/// **The refusals ride in this same spawn rather than in a test of their own**, which is the
/// machine's call and not a preference: a net test spends minutes in `net_stack`'s userspace
/// smoltcp poll, so a boot is the expensive unit. `denied refused` is
/// `socket_proto::fixture::DENIED_PORT`, outside the grant this stack carries, answered
/// `PermissionDenied`; `in use refused` is the granted port asked for twice, answered `AddrInUse`.
/// The **whole-stack** refusal is the sibling test above, whose stack carries no grant at all.
///
/// The host's half is `xtask`'s inbound prober, which requires its own bytes back from the guest
/// and fails the leg if nobody answered. The guest's half covers "the connection was served"; the
/// host's covers "and what came back was the guest's own answer", which the guest cannot know.
// RISC-V twin: `riscv_virtio_tests::a_std_program_serves_a_granted_listening_port`.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_std_program_serves_a_granted_listening_port() {
    let Some((report, net)) = virtio_service::start_net_std(
        net_stack_image(),
        std_exerciser_image(),
        socket_proto::listen_grant(NET_LISTEN_PORT, NET_LISTEN_PORT),
    ) else {
        crate::println!("    (no virtio-net device attached; skipping)");
        return;
    };

    assert_std_transcript(report, STD_LISTEN_EXPECTED, "std listen");
    net.release_or_fail("a net test's net_stack");
}

/// **The shell's `run` mechanism: spawn a process, get its answer.** Milestone 10's core.
///
/// A worker process is started at EL0 with an argument, computes `n*n`, reports the result on
/// an endpoint it was handed, and exits. The whole lifecycle a shell drives when you type
/// `run n`, minus the interactive loop, which is exercised by the piped demo instead.
#[test_case]
fn a_spawned_worker_process_computes_and_reports() {
    let result = sched::create_endpoint();
    let faults = USER_FAULTS.load(Ordering::Relaxed);

    sched::spawn(move || {
        run(
            worker_image(), // its own binary now (19f.2), not a role of hello
            Spawn {
                arg0: 0, // no role selector; the input is in x1
                arg1: 9, // the worker computes 9*9
                arg2: 0,
                grants: &[crate::cap::endpoint_cap(result, crate::cap::Rights::WRITE)],
                maps: &[],
            },
        )
    })
    .expect("spawn failed");

    let answer = sched::ipc_recv(result)[0];
    assert_eq!(answer, 81, "the spawned worker computed the wrong answer");
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults,
        "the worker faulted instead of computing cleanly",
    );
}

/// **The kernel stops allocating.** Milestone 11's whole point, as one number.
///
/// We carve an untyped region, then a process maps page after page out of it until the region
/// is exhausted. The assertion that matters: the kernel's used-frame count **does not change
/// while the process allocates**, because every page comes from the untyped, not the kernel
/// allocator. A process cannot make the kernel allocate, so it cannot exhaust kernel memory;
/// it runs out of its own budget and stops, cleanly, with the kernel untouched.
#[test_case]
fn a_process_spends_untyped_and_the_kernel_never_allocates() {
    let used = || crate::memory::stats().expect("no allocator").used;

    const PAGES: u64 = 24;
    let (region, report, demo) =
        untyped_service::start(init_image(), PAGES).expect("could not create the untyped region");

    // The process sends a "ready" signal once it is fully loaded (its ELF and stack are
    // kernel-allocated, like any process). We measure the frame count THERE, so the window we
    // check contains only what it does next: map pages out of its untyped.
    sched::ipc_recv(report); // ready
    let baseline = used();
    let faults = USER_FAULTS.load(Ordering::Relaxed);

    let mapped = sched::ipc_recv(report)[0]; // the count, after it exhausted the untyped

    assert_eq!(
        used(),
        baseline,
        "the kernel allocated {} frames while a process mapped {mapped} pages: untyped is not \
         backing the process's memory",
        used() as i64 - baseline as i64,
    );
    assert!(mapped > 0, "the process mapped nothing");
    assert_eq!(
        USER_FAULTS.load(Ordering::Relaxed),
        faults,
        "the process faulted instead of exhausting its budget cleanly",
    );

    // And the untyped is genuinely spent: the process mapped until it ran dry.
    let (watermark, total) = crate::untyped::usage(region).expect("region vanished");
    assert_eq!(total, PAGES);
    assert!(
        watermark >= mapped,
        "the process mapped {mapped} pages but the untyped only advanced {watermark}",
    );
    assert!(
        total - watermark < 4,
        "the untyped had {} pages left unspent; the process gave up early",
        total - watermark,
    );

    // Every measurement above is taken. The demo spins to hold the free-frame count still while
    // it is read, which is the reason it does not exit on its own, and also the reason it has to
    // be ended here: past this point it is a spinning thread with nothing left to hold.
    assert!(reap_bare(demo), "the untyped demo outlived its kill");
}

/// **The DMA-confinement fix, end to end.** A malicious driver at EL0 holds a real `Virtio`
/// capability and its own DMA region, and points a descriptor at the kernel's image, asking
/// the device to write there. Because the device has no IOMMU, this would succeed if the
/// driver could ring it directly. The kernel validates every descriptor on submit and refuses
/// this one, so the device is never told to go and never touches the kernel. The driver
/// reports `1` when it was refused.
// RISC-V twin: `riscv_virtio_tests::the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
    let Some(report) = virtio_service::start_attacker(init_image()) else {
        crate::println!("    (no virtio disk attached; skipping)");
        return;
    };
    let refused = sched::ipc_recv(report)[0];
    assert_eq!(
        refused, 1,
        "a malicious driver's descriptor pointing at kernel memory was NOT refused: the \
         device could have DMA'd over the kernel",
    );
}

/// **The indirect-descriptor escape, end to end.** The direct-descriptor test above proves the
/// obvious case. This proves the subtle one: a driver that negotiates `INDIRECT_DESC` and
/// submits an indirect descriptor whose inner table (in its own region) aims the device at the
/// kernel. A validator that walked only the flat chain would pass the outer descriptor and let
/// the device follow the table out. The kernel strips the feature and refuses the flag, so the
/// device is never rung. The driver reports `1` when it was refused.
// RISC-V twin: `riscv_virtio_tests::the_kernel_refuses_an_indirect_descriptor_escape`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn the_kernel_refuses_an_indirect_descriptor_escape() {
    let Some(report) = virtio_service::start_attacker_indirect(init_image()) else {
        crate::println!("    (no virtio disk attached; skipping)");
        return;
    };
    let refused = sched::ipc_recv(report)[0];
    assert_eq!(
        refused, 1,
        "an indirect descriptor whose inner table pointed at kernel memory was NOT refused: \
         the device could have followed it out of the driver's region",
    );
}

/// **The PCIe transport on aarch64, end to end** (DECISIONS §18): the same driver reads the
/// same file off the disk QEMU attached as `virtio-blk-pci`, found by ECAM enumeration in
/// the highmem window, BARs placed by the kernel, the completion arriving as INTx through
/// the GIC (SPI 3 + swizzle). The riscv twin proved the seam on the PLIC board; this proves
/// the same subsystem, from the same portable crate and seam, on the second bus of the
/// second interrupt controller.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_reads_a_file_over_the_pcie_transport`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_reads_a_file_over_the_pcie_transport() {
    use crate::arch::exceptions::ROUTED_IRQS;

    let Some(report) = virtio_service::start_pci(init_image()) else {
        crate::println!("    (no virtio-pci disk on the bus; skipping)");
        return;
    };

    let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);
    let word = sched::ipc_recv(report)[0];

    assert_eq!(
        &word.to_le_bytes(),
        b"nife: re",
        "the driver reported the wrong file contents over pci",
    );
    assert!(
        ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
        "the read completed but no INTx interrupt was delivered through the GIC",
    );
}

/// **A userspace driver writes a block and reads it back.** Milestone 32 phase 1: the write
/// verb, end to end, through the same validated transport as the read path. The driver
/// writes a pattern to the scratch block, wipes its buffer, reads the block back, verifies
/// every byte in-process, re-checks the superblock and directory around it, and reports the
/// read-back head. A matching report therefore certifies the round trip AND that the write
/// landed only on its own block.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_writes_a_block_and_reads_it_back`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_writes_a_block_and_reads_it_back() {
    let Some(report) = virtio_service::start_writer(init_image()) else {
        crate::println!("    (no virtio disk attached; skipping)");
        return;
    };
    let word = sched::ipc_recv(report)[0];
    assert_eq!(
        &word.to_le_bytes(),
        b"CRKWRIT1",
        "the driver did not read back the pattern it wrote",
    );
}

/// The same write round trip over the PCIe transport (DECISIONS §18): the write verb must
/// hold on both buses, exactly as the read path does, or the transport seam has a
/// direction-shaped hole.
// RISC-V twin: `riscv_virtio_tests::a_userspace_driver_writes_a_block_over_the_pcie_transport`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_userspace_driver_writes_a_block_over_the_pcie_transport() {
    let Some(report) = virtio_service::start_writer_pci(init_image()) else {
        crate::println!("    (no virtio-pci disk on the bus; skipping)");
        return;
    };
    let word = sched::ipc_recv(report)[0];
    assert_eq!(
        &word.to_le_bytes(),
        b"CRKWRIT1",
        "the driver did not read back the pattern it wrote over pci",
    );
}

/// **A driver killed mid-write leaves the device and the transport sane.** Errors here eat
/// filesystems, so this is the write path's teardown proof: a driver submits a validated
/// write and dies (panics, is killed, is reaped) without ever collecting the completion,
/// acknowledging the interrupt, or advancing its ring bookkeeping. The device still owes a
/// completion into the dead driver's DMA region, which is safe precisely because that frame
/// is kernel-allocated and deliberately never reclaimed on thread death (`map_physical`'s
/// "Drop leaves it alone" rule): the DMA lands in memory the allocator never re-issued.
/// Then the full writer runs against the SAME device, resets it, and must complete its own
/// round trip, which proves the abandoned request wedged nothing: not the device, not the
/// validator's per-registration state, not the disk.
// RISC-V twin: `riscv_virtio_tests::a_driver_killed_mid_write_leaves_the_device_and_transport_sane`. Gated here rather than run twice: that
// module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
// second copy through hello's roles would double the suite's slowest tests to prove
// nothing new. See this module's comment on the two kinds of gate.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn a_driver_killed_mid_write_leaves_the_device_and_transport_sane() {
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    let Some(report) = virtio_service::start_write_abandoner(init_image()) else {
        crate::println!("    (no virtio disk attached; skipping)");
        return;
    };

    // 1 = the kernel validated the write and rang the device; the request is genuinely in
    // flight (or already complete) when the driver dies.
    assert_eq!(
        sched::ipc_recv(report)[0],
        1,
        "the abandoner never got its write submitted",
    );

    // The deliberate death: panic -> brk -> killed. Wait for the kill so the survivor below
    // runs against a device whose previous operator is really gone.
    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "the abandoner never died; nothing was killed mid-write",
    );

    // The survivor: the same full write-verify driver, same physical device. It must succeed
    // from a clean device reset, in-flight completion and all.
    let report = virtio_service::start_writer(init_image())
        .expect("the disk vanished between the abandoner and the survivor");
    let word = sched::ipc_recv(report)[0];
    assert_eq!(
        &word.to_le_bytes(),
        b"CRKWRIT1",
        "after a mid-write kill, a fresh driver could not use the device",
    );
}

/// A dead user thread's address space is freed, all of it, including its page tables.
///
/// The milestone 6 reaper test found that stack VAs were bump-allocated and never reused,
/// because `unmap_page` leaves intermediate tables standing. An `AddressSpace` sidesteps
/// that entirely: it dies **all at once**, so it never unmaps anything. It records every
/// frame the mapper hands it, leaves and tables alike, and frees the lot.
///
/// The assertion is exact in the direction this test owns: a leaked frame keeps `used` above
/// the baseline forever and fails. Approximate there would have hidden the milestone 6 bug.
#[test_case]
fn a_dead_user_thread_frees_its_whole_address_space() {
    let used = || crate::memory::stats().expect("no allocator").used;

    // Warm up: the first user thread ever created pays for page tables in a region of
    // kernel VA that nothing has touched. Measure the STEADY state, which is the one that
    // has to hold forever.
    //
    // Snapshot the fault count BEFORE spawning (as the loop below always did). The old
    // order, spawn then snapshot, was a race: on SMP the outlaw can fault in that gap, the
    // snapshot swallows its fault, and the wait below times out on a count that will never
    // move again. Latent until milestone 14 phase A.2/A.3 made spawn-to-fault fast enough
    // to lose the race about once in seven runs.
    //
    // Pin the outlaws to THIS core (DECISIONS §28 made `spawn` scatter them). Frame accounting
    // must be exact to catch a leak (the milestone-6 bug this test guards), but a thread's frames
    // are freed by `finish_switch` on whatever core reaps it, *after* it leaves the thread table
    // and outside SCHED. Scattered across cores, that free is asynchronous, so `used()` fluctuates
    // and never reads exact. Kept on the test's own core, each outlaw's fault, reap, and frame
    // free happen synchronously under the test's own yields, so `used()` is exact again. This
    // tests the reaper, not placement, so pinning costs nothing.
    let here = crate::cpu::id();
    let image = outlaw_image();
    let kernel_addr = a_kernel_address();
    let outlaw_here = move || {
        sched::spawn_on(here, move || {
            run(
                image,
                Spawn {
                    arg0: OUTLAW_READ_KERNEL,
                    arg1: kernel_addr,
                    arg2: 0,
                    grants: &[],
                    maps: &[],
                },
            )
        })
    };

    // Each outlaw's reap is proven by `thread_present` on ITS Tid, not by `thread_count()`
    // returning to a baseline sampled at the top of the test. The count is the whole table, so a
    // baseline taken while an earlier test's teardown is still in flight is a number the system
    // moves on its own; the per-Tid wait is immune to neighbours by construction. Same fix as the
    // reap wait in `reclaim_frees_a_started_then_exited_childs_regions`; see
    // notes/load-sensitive-assertions.md.
    let f0 = USER_FAULTS.load(Ordering::Relaxed);
    let warmup = outlaw_here().expect("spawn failed");
    assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
    assert!(wait_for(|| !sched::thread_present(warmup)));

    // Sample the baseline only once `used()` has STOPPED MOVING, for the same reason the
    // assertion below waits rather than reading instantly, applied to the other end. The warm-up
    // outlaw's address space is freed by `finish_switch` on whatever core actually ran it, which
    // under §28 placement need not be this one, and that free lands a beat *after* its Tid
    // stops resolving. Sampling `before` inside that window captures frames that are about
    // to come back, `used()` then settles BELOW `before`, and a wait for equality could never
    // succeed. The failure said so plainly when it happened: it reported "-18 frames did not come
    // back", a NEGATIVE leak, which no real leak can produce. Found when an unrelated change to
    // the std::fs test shifted this test's timing; the race was already here.
    // Two agreeing samples a yield apart mean nothing is in flight. Bounded by `wait_for`'s own
    // deadline, so a genuinely unstable allocator fails the test rather than spinning here.
    let mut last = used();
    let settled = wait_for(|| {
        sched::yield_now();
        let now = core::mem::replace(&mut last, used());
        now == last
    });
    assert!(
        settled,
        "frame accounting never settled before the baseline"
    );
    let before = last;

    for _ in 0..4 {
        let f = USER_FAULTS.load(Ordering::Relaxed);
        let outlaw = outlaw_here().expect("spawn failed");
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f));
        assert!(wait_for(|| !sched::thread_present(outlaw)));
    }

    // Exact in the leak direction, but allow the asynchronous reap to settle. Pinning the outlaws
    // with `spawn_on` is a placement HINT, not a pin (DECISIONS §28): an idle core can steal one
    // before this core runs it, and then the frame free (finish_switch dropping the address space,
    // after the thread leaves the table and outside SCHED) lands on that core a beat after the Tid
    // resolves to nothing. So wait for `used()` to come back to `before` rather than reading it
    // the instant the last outlaw is gone. Still a leak trap: a real leak (the milestone-6 bug)
    // never gives the frames back, so this wait times out and fails.
    //
    // `<=`, not `==`. Equality demanded that no OTHER test's teardown free a frame during this
    // window, which is not a property four outlaw address spaces are responsible for, and it is
    // the only way the old form could fail with a NEGATIVE count: it did, on CI and on a quiet
    // aarch64 dev machine, as "-19 frames did not come back", frames arriving from outside the
    // measured window. A leak still fails identically: every frame the outlaws keep holds
    // `used()` above `before` forever. See notes/load-sensitive-assertions.md.
    //
    // The count in the message is the one the wait DECIDED on, not a fresh sample, and this site is
    // why the distinction is worth code. `used() as i64 - before as i64` re-reads the allocator
    // after the wait has already given up, so frames arriving in that gap make a genuine timeout
    // print zero or a NEGATIVE count: the exact "-19 frames did not come back" that cost this
    // family three separate diagnoses, now emitted by a form that can no longer fail for that
    // reason. A reader who trusts the sign would re-run the old investigation from the top.
    // `wait_for` re-evaluates the predicate once past its deadline, so a `false` return leaves
    // `seen > before` and the count below is positive by construction, no cast required.
    let mut seen = before;
    let came_back = wait_for(|| {
        seen = used();
        seen <= before
    });
    assert!(
        came_back,
        "four user address spaces came and went and {} frames did not come back",
        seen - before,
    );
}

/// **Milestone 19b: a user-built address space is a first-class citizen of every memory
/// mechanism.** Retype a space out of a region, map a frame into it, and check the three
/// things that make it real: the CPU's walker sees the mapping with the exact flags asked
/// for; §13 revocation reaches into the user-built space (the record was paid and filed, so
/// `revoke_frame` unmaps it there like anywhere); and destroying the pinned backing region
/// frees nothing while the space lives in it.
#[test_case]
fn a_user_built_aspace_maps_translates_and_revokes() {
    let region = crate::untyped::create(8).expect("no region");
    let name = user_aspace_create(region).expect("no aspace");
    let root = user_aspace_root(name).expect("aspace has no root");

    let frame_region = crate::untyped::create(2).expect("no frame region");
    let phys = crate::untyped::retype_page(frame_region).expect("no frame");
    let va = 0x40_0000u64;

    user_aspace_map(name, va, phys, Flags::user_rodata()).expect("map_into failed");

    let (mapped_pa, flags) = mmu::translate_at(root, va).expect("the walker sees no mapping");
    assert_eq!(mapped_pa, phys, "mapped the wrong frame");
    assert!(!flags.is_writable(), "asked read-only, got writable");
    assert!(
        !flags.is_global(),
        "a user mapping in a built space must be ASID-tagged"
    );

    // Same va twice: refused, the break-before-make contract holds for built spaces too.
    assert!(
        user_aspace_map(name, va, phys, Flags::user_rodata()).is_err(),
        "double-map at one va was allowed"
    );

    // The reach of §13: revoking the frame unmaps it from the space nobody exec'd.
    crate::revoke::revoke_frame(phys);
    assert!(
        mmu::translate_at(root, va).is_none(),
        "revocation does not reach a user-built address space",
    );

    // The pin: the backing region hosts a live root, so destroy must free nothing.
    let free_before = crate::memory::stats().unwrap().free();
    crate::untyped::destroy(region);
    assert_eq!(
        crate::memory::stats().unwrap().free(),
        free_before,
        "destroy reclaimed the region under a live user-built space",
    );

    // The frame region is unpinned (it only ever produced a plain frame), so destroy
    // reclaims it whole, the already-revoked frame included. No manual free: the region
    // owns its pages, and freeing one twice is the allocator's double-free panic.
    crate::untyped::destroy(frame_region);
}

/// **Milestone 19d.2b: init delegates an interrupt to a driver it builds.** The last
/// delegatable device authority after endpoints and device MMIO: an interrupt capability.
/// init holds one for a test SGI (the kernel routed it), builds a child, hands it the Irq
/// cap, and starts it. The child blocks in the interrupt's WAIT; the test raises the SGI; the
/// interrupt is delivered as a message through the delegated capability, the child wakes and
/// reports. A hang would mean the interrupt never reached the init-built child, so a passing
/// test is the proof. Completes the "init delegates every authority kind" story the
/// interrupt-driven drivers (input, virtio) rest on.
///
/// **aarch64-only, because RISC-V has no second interrupt to raise.** It has no
/// software-generated interrupt a test can assert on itself at all (the SBI IPI arrives down the
/// software-interrupt arm and never reaches `irq_route`), so the only line it can raise by hand
/// is the console UART's own, which `spawn_init` is already routing for the input driver init
/// builds. A twin would have to share that one source between init's UART capability and the
/// test's delegated one, and would then prove delivery through whichever route was bound last
/// rather than through the delegated capability, which is the entire claim. The *property*
/// (an interrupt arriving as a message through a delegated Irq cap) is proved on RISC-V by
/// `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`, which asserts
/// `ROUTED_IRQS` rises while a userspace driver waits on its own Irq cap, and by
/// `sched::tests::an_interrupt_becomes_a_message`. See notes/interrupts.md.
#[cfg(target_arch = "aarch64")]
#[test_case]
fn userspace_init_delegates_an_interrupt_to_a_child() {
    const IRQ_WORD: u64 = 0x1590;
    const INIT_IRQ_ROLE: u64 = 25;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_IRQ_ROLE, report);

    // Raise the test interrupt. The endpoint counts it if the child is not waiting yet (it is
    // still being built), and the child's WAIT drains that pending signal, so there is no race.
    crate::drivers::gic::send_sgi(INIT_TEST_SGI, crate::cpu::id());

    let word = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        word, IRQ_WORD,
        "the interrupt never reached the init-built child through the delegated Irq cap",
    );
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19d.2b: userspace init brings up the real console server.** Past 19d.2a's
/// ID-read probe: init builds the *actual* print server as a child, wires it a request/reply
/// channel and a shared page and the UART, then plays the client, asking it to print a line.
/// The server prints to the real UART (visible in the QEMU log) and acks the length; init
/// reports that length. Receiving the exact message length proves a userspace-built console
/// works end to end: a driver init constructed, on a channel init created, driving hardware
/// init delegated. This is the shape the real boot path (19d.2c) uses.
#[test_case]
fn userspace_init_brings_up_the_console_server() {
    // The message length the init_console role prints and the server acks. Kept in sync with
    // user/src/hello.rs init_console (the b"..." there); a mismatch fails loudly, not silently.
    const MSG_LEN: u64 = 66;
    const INIT_CONSOLE_ROLE: u64 = 24;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_CONSOLE_ROLE, report);

    let acked = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        acked, MSG_LEN,
        "the init-built console server did not print-and-ack: {acked} bytes, expected {MSG_LEN}",
    );
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19d.2: userspace init builds a device driver and hands it the hardware.**
/// The step beyond 19d.1: not just a child, but a child that touches a *device*. init holds a
/// UART **device capability** (a new delegatable authority to map MMIO device-typed), builds
/// a driver child, and maps the UART's registers into it. The child reads the PL011's
/// PrimeCell identification registers, whose value is the fixed `0xB105F00D` every real PL011
/// returns. Receiving that constant proves the whole chain: device access is a capability the
/// kernel minted and init delegated, `MAP_INTO` mapped it device-typed (not cached normal
/// memory, which would corrupt MMIO), and a userspace-init-built driver drove real hardware.
///
/// **aarch64-only, because the assertion is a PL011 register and RISC-V `virt` has no PL011.**
/// `0xB105F00D` in the PrimeCell identification registers is what makes this test exact rather
/// than "the read did not fault"; the NS16550 on the other machine has no equivalent constant to
/// name, and swapping in a virtio magic number would be a different test wearing this one's
/// name. Device delegation to a userspace driver *is* proved on RISC-V, by
/// `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`, which is a stronger
/// version of the same claim (device MMIO, a DMA region, and an interrupt, all delegated).
#[cfg(target_arch = "aarch64")]
#[test_case]
fn userspace_init_builds_a_driver_that_reads_real_hardware() {
    const PL011_PRIMECELL_ID: u64 = 0xB105_F00D;
    const INIT_DEV_ROLE: u64 = 23;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_DEV_ROLE, report);

    let id = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        id, PL011_PRIMECELL_ID,
        "the init-built driver did not read the PL011's id: device delegation or the              device-typed mapping is broken",
    );
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19d: userspace init parses a real ELF and builds a running process from it.**
/// The kernel loads exactly one program, init (a role of the same binary), and hands it the
/// initrd mapped read-only plus a building budget and a report endpoint. init parses that
/// ELF *in userspace* (the `elf` crate, no longer in the kernel's trusted core) and loads it
/// as a child through the granular verbs: retype an address space, copy each segment into
/// retyped frames and map them, retype and endow a TCB, configure, start. The child runs code
/// the kernel never parsed and reports the agreed word. Receiving it is the whole thesis of
/// milestone 19 working end to end: a verified kernel that runs a workload it did not load.
#[test_case]
fn userspace_init_parses_an_elf_and_builds_a_running_child() {
    const CHILD_WORD: u64 = 0xC0FFEE;
    const INIT_ROLE: u64 = 20;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_ROLE, report);

    let word = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        word, CHILD_WORD,
        "init did not build a running child from the ELF it parsed in userspace",
    );
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19e: init builds a worker, passes it an argument, and gets the answer back.**
/// Every child before this took only its role in `x0`. A worker computes on an input, so 19e
/// widened `START` to carry three initial registers. init builds a worker, starts it with the
/// input in `x1`, and the worker squares it and reports. Receiving `n*n` (not `n`, not garbage)
/// proves the argument crossed `START` into a fresh EL0 thread's registers intact. This is the
/// mechanism a real spawn service runs on: a workload parameterized by data, not just identity.
#[test_case]
fn init_builds_a_worker_and_passes_it_an_argument() {
    const INIT_WORKER_ROLE: u64 = 28;
    const WORKER_INPUT: u64 = 7;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_WORKER_ROLE, report);

    let answer = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        answer,
        WORKER_INPUT * WORKER_INPUT,
        "the worker did not receive its START argument: expected n*n back",
    );
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19e: init runs a real compute workload and it comes out right.** The worker's
/// `n*n` proved the mechanism; this proves a *substantial* program. init builds the `"coremark"`
/// binary (a CoreMark-derived run: list sort, matrix multiply, state machine, folded into a CRC),
/// starts it, and the workload SENDs the run's checksum home. Receiving `coremark::PINNED_CRC_64`
/// (`0x7954`, the value the host `coremark` test also pins) proves a real workload ran correctly
/// against the native ABI, and that the same computation gives the same answer on the kernel's
/// target as on the host, which is the property a cross-OS comparison will rest on.
#[test_case]
fn init_runs_the_coremark_workload_and_it_checks_out() {
    const INIT_COREMARK_ROLE: u64 = 29;

    let report = crate::sched::create_endpoint();
    let init = spawn_init(initrd().expect("no initrd"), INIT_COREMARK_ROLE, report);

    let [crc, ticks, freq, _, _] = crate::sched::ipc_recv(report);
    assert_eq!(
        crc,
        coremark::PINNED_CRC_64 as u64,
        "the CoreMark workload computed the wrong checksum",
    );
    // The workload self-timed via CNTVCT_EL0. Nonzero ticks and a real frequency prove EL0 can
    // read the virtual counter (CNTKCTL_EL1.EL0VCTEN), the foundation the primitive suite needs.
    // (Under TCG the magnitude is icount fiction, but it still advances, so the read works.)
    assert!(
        ticks > 0,
        "the workload's self-timing read a frozen counter"
    );
    assert!(freq > 0, "CNTFRQ_EL0 read as zero at EL0");
    init.release_or_fail("an init test's building budget");
}

/// **Milestone 19c.3, the whole point: one process builds and starts another, and it runs.**
/// The kernel drives the four verbs the way init eventually will: retype an address space and
/// a TCB, map a code page (containing a hand-assembled EL0 stub) and a stack into the space,
/// insert a report endpoint into the child's cspace, configure the TCB (entry, stack, space),
/// and START it. The child, code no wiring wrote and a thread no `spawn` created, drops to
/// EL0, invokes the capability it was granted to SEND a word home, and exits. Receiving that
/// word proves every verb: the retype, the maps, the cap insert, the configure, the start,
/// and a real EL0 thread built from parts.
#[test_case]
fn a_process_can_build_start_and_run_a_child_thread() {
    const CODE_VA: u64 = 0x40_0000;
    const STACK_VA: u64 = 0x50_0000;
    // The child's program: SEND(slot 0, endpoint::SEND, REPORT_WORD) then EXIT, nine
    // instructions, with the child's first granted cap in slot 0. This file used to carry three
    // separate aarch64 copies of it; `supervision_tests` already keeps one pair (aarch64 and
    // RISC-V) for its own children, so all three now share that pair. Same shape, one definition,
    // and the tests below run on either machine.
    let code = super::supervision_tests::REPORT_STUB;
    let expect_word = super::supervision_tests::REPORT_WORD;

    // The child's address space, and a region to carve its code and stack frames from.
    let as_region = crate::untyped::create(8).expect("no aspace region");
    let aspace = user_aspace_create(as_region).expect("no aspace");
    let frames_region = crate::untyped::create(2).expect("no frame region");

    let code_phys = crate::untyped::retype_page(frames_region).expect("no code frame");
    // Write the program through the direct map, then make it coherent for the fetcher.
    // SAFETY: a fresh frame we own, direct-mapped.
    unsafe {
        let dst = mmu::phys_to_virt(code_phys) as *mut u32;
        for (i, &insn) in code.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }
    sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
    user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

    let stack_phys = crate::untyped::retype_page(frames_region).expect("no stack frame");
    user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    // The child's one authority: WRITE on a report endpoint, so it can SEND but not receive.
    let report = crate::sched::create_endpoint();
    let report_cap = crate::cap::endpoint_cap(
        report,
        crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
    );

    // Build the thread from parts.
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    let slot = crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
    assert_eq!(
        slot, 0,
        "the child's first cap must land in slot 0 (the code assumes it)"
    );

    // Not before it is whole: START must refuse an unconfigured embryo.
    assert!(
        crate::sched::start_tcb(tid, [0; 3]).is_err(),
        "START ran a half-built thread (no address space, no entry)",
    );

    crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
        .expect("configure");
    crate::sched::start_tcb(tid, [0; 3]).expect("start");

    // And starting twice must refuse: it is no longer an embryo.
    assert!(
        crate::sched::start_tcb(tid, [0; 3]).is_err(),
        "START ran a thread that was already running",
    );

    let got = crate::sched::ipc_recv(report)[0];
    assert_eq!(
        got, expect_word,
        "the child never reported: a built-from-parts thread did not reach EL0 and run",
    );
}

/// **Object revocation, piece 3: a started thread and its bound address space are reclaimed
/// after it exits.** Build and start a child as above, but carve its code and stack from the
/// *same* region as its address space, so one region holds the child's whole world (root,
/// tables, code, stack) and its TCB is in another. Once the child has run, exited, and been
/// reaped, both regions reclaim and the free-frame count returns *exactly* to baseline. The bound
/// address space died with the thread (the `Drop` chain), leaving its region object-free for
/// `reclaim_region` to unpin and free.
///
/// # This test used to probe the refusal first, and that probe was killing its own child
///
/// Milestone 72. It opened by asserting `reclaim_region(tcb_region).is_err()` while the child was
/// still runnable, over a comment reading "the refusal leaves the region untouched". That comment
/// went stale when DECISIONS §16 was amended: a refused reclaim is **not** passive, it *arms the
/// kill* on every live thread in the region so the owner's retry can tear a runaway down (§24's
/// `^C` escalation depends on it). So the probe marked this child `killed`, and `schedule()`
/// converts a killed thread to a corpse at its next preemption. Win the race and the child reaches
/// its `SEND` first and the test passes; lose it and the child is reaped without ever sending, the
/// `ipc_recv` below never returns, and the machine goes fully idle: the intermittent lost-wakeup
/// hang that kept `cpu-matrix` red on branches that could not have caused it.
///
/// Proved by widening the window rather than by waiting for the race: a call-free delay loop in
/// front of `REPORT_STUB` guarantees a preemption before the `SEND`, and with the probe present
/// that hangs the watchdog on **both** ISAs, first run, every run. It is not a RISC-V defect; the
/// riscv64 leg simply lost the race more often. With the probe gone the same widened child passes.
///
/// The refusal itself is not lost coverage: `force_kill_tests` proves refuse-then-arm-then-reclaim
/// directly, on a runaway that is *meant* to die, which is the only subject a destructive probe can
/// honestly be pointed at.
#[test_case]
fn reclaim_frees_a_started_then_exited_childs_regions() {
    const CODE_VA: u64 = 0x40_0000;
    const STACK_VA: u64 = 0x50_0000;
    // SEND(slot 0, endpoint::SEND, REPORT_WORD) then EXIT, the shared stub (see the test above).
    let code = super::supervision_tests::REPORT_STUB;
    let expect_word = super::supervision_tests::REPORT_WORD;

    // The report endpoint is created before the baseline: it lives in the kernel's own pinned
    // endpoint region (never reclaimed here; endpoint revocation is a later piece), so it must
    // not count against the frame accounting.
    let report = crate::sched::create_endpoint();
    let frames_before = crate::memory::free_frames();

    // The child's whole address space in one region: root, tables, code, and stack.
    let as_region = crate::untyped::create(8).expect("no aspace region");
    let aspace = user_aspace_create(as_region).expect("no aspace");

    let code_phys = crate::untyped::retype_page(as_region).expect("no code frame");
    // SAFETY: a fresh frame we own, direct-mapped.
    unsafe {
        let dst = mmu::phys_to_virt(code_phys) as *mut u32;
        for (i, &insn) in code.iter().enumerate() {
            dst.add(i).write(insn);
        }
    }
    sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
    user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

    let stack_phys = crate::untyped::retype_page(as_region).expect("no stack frame");
    user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    let report_cap = crate::cap::endpoint_cap(
        report,
        crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
    );
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
    crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
        .expect("configure");
    crate::sched::start_tcb(tid, [0; 3]).expect("start");

    // **Nothing may touch `tcb_region` between here and the child's report.** A refused
    // `reclaim_region` arms §16's kill on the child and dooms it; see this test's own note above.

    // Let it run: it SENDs the word and exits. Receiving proves it reached EL0.
    let got = crate::sched::ipc_recv(report)[0];
    assert_eq!(got, expect_word, "the child never reported");

    // Let the reaper collect the now-Finished child. A Finished thread is removed when its own
    // core switches away from it, and DECISIONS §28's placement can have put this child on
    // ANOTHER core, so yielding on THIS core cannot make that happen: a hundred cheap yields
    // complete long before the remote core's next timer tick. So wait on the clock, not on a
    // yield count. Still a leak trap rather than a masked failure: a child that is never reaped
    // times out and fails; only cross-core reap lag is tolerated.
    //
    // **And wait on this child, not on a headcount.** This asked whether `thread_count()` had
    // returned to a baseline sampled at the top of the test, which is the size of the WHOLE
    // thread table: the previous test's processes are still tearing down at that instant, so the
    // baseline was a number the system would move on its own, and the wait was really waiting
    // for everything else to hold still. It failed exactly that way once on RISC-V, where the
    // slower machine leaves more teardown in flight. `thread_present` asks the question the test
    // means. The whole history here is a wait that keeps being written against something wider
    // than the property: it was a yield count until §28's scattering broke it, then a
    // clock-bounded headcount until this. A sibling wait below had the same defect.
    assert!(
        wait_for(|| !crate::sched::thread_present(tid)),
        "the exited child was never reaped",
    );

    // Both regions reclaim now: the TCB's, and the address space's (its bound space died with
    // the thread, so the region is object-free, needing only unpin and free).
    crate::sched::reclaim_region(tcb_region).expect("reclaim the TCB region after exit");
    crate::sched::reclaim_region(as_region).expect("reclaim the address-space region after exit");

    assert_eq!(
        crate::memory::free_frames(),
        frames_before,
        "every frame the child used must come back to baseline",
    );
}

/// **Spawn-to-reap repeats without leaking: the whole milestone's payoff.** Build, start, run,
/// exit, reap, and reclaim a region-backed EL0 child, in a loop. Every iteration returns the
/// free-frame count to the same baseline, and the region slots are reused (generational), so the
/// loop neither leaks memory nor exhausts the region table. This is the property "spawn's
/// prerequisite" was always about: not retype (that had shipped), but reclamation, so a workload
/// can come and go over and over. A few iterations under TCG is enough to catch any per-cycle
/// leak; the real magnitudes wait on the EL0 `lat_proc` benchmark.
#[test_case]
fn spawn_to_reap_repeats_without_leaking() {
    const CODE_VA: u64 = 0x40_0000;
    const STACK_VA: u64 = 0x50_0000;
    let code = super::supervision_tests::REPORT_STUB;
    let expect_word = super::supervision_tests::REPORT_WORD;

    let report = crate::sched::create_endpoint();
    let baseline = crate::memory::free_frames();

    for round in 0..6 {
        let as_region = crate::untyped::create(8).expect("aspace region");
        let aspace = user_aspace_create(as_region).expect("aspace");
        let code_phys = crate::untyped::retype_page(as_region).expect("code frame");
        // SAFETY: a fresh frame we own, direct-mapped.
        unsafe {
            let dst = mmu::phys_to_virt(code_phys) as *mut u32;
            for (i, &insn) in code.iter().enumerate() {
                dst.add(i).write(insn);
            }
        }
        sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
        user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");
        let stack_phys = crate::untyped::retype_page(as_region).expect("stack frame");
        user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

        let report_cap = crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        );
        let tcb_region = crate::untyped::create(2).expect("tcb region");
        let tid = crate::sched::create_tcb(tcb_region).expect("tcb");
        crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
        crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
            .expect("configure");
        crate::sched::start_tcb(tid, [0; 3]).expect("start");

        assert_eq!(
            crate::sched::ipc_recv(report)[0],
            expect_word,
            "round {round}: the child never reported"
        );
        // On the clock, not on yields, and on THIS child rather than on a headcount, both for
        // the reasons spelled out in the test above: §28 can place the child on another core and
        // only that core's switch reaps it, and `thread_count()` is the size of the whole table,
        // so waiting for it to return to a baseline is waiting for the rest of the system.
        // A lagging reap here would surface as the reclaim below refusing a live thread.
        assert!(
            wait_for(|| !crate::sched::thread_present(tid)),
            "round {round}: the child was never reaped",
        );
        crate::sched::reclaim_region(tcb_region).expect("reclaim tcb region");
        crate::sched::reclaim_region(as_region).expect("reclaim aspace region");

        assert_eq!(
            crate::memory::free_frames(),
            baseline,
            "round {round}: spawn-to-reap leaked; the cycle does not return to baseline",
        );
    }
}

/// **Milestone 19b, end to end: a process constructs an address space from EL0.** The
/// builder retypes a space and a frame from its own budget, maps the frame in, and checks
/// the kernel enforces break-before-make inside the space it built. Verdict 0b111 or bust.
#[test_case]
fn a_process_can_build_an_address_space_from_el0() {
    let report = aspace_service::wire(init_image());
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, 0b111,
        "aspace build verdict {verdict:#b}: bit0 retype, bit1 map_into, bit2 double-map refused",
    );
}

/// **Milestone 19a, end to end: an endpoint minted by a process out of its own memory
/// carries IPC between processes.** The maker retypes a page of its untyped into an endpoint
/// (`RETYPE_OBJ`), delegates a READ view to a peer it has never met, and sends a word into
/// it; the peer listens on the received capability and reports what arrived. No kernel
/// wiring created the endpoint: budget, mint, delegation, and rendezvous are all the
/// processes' own acts. The word arriving is the whole granular-construction story of 19a
/// working at EL0.
#[test_case]
fn a_process_can_mint_an_endpoint_and_ipc_flows_over_it() {
    let report = retype_ep_service::wire(init_image());
    let word = sched::ipc_recv(report)[0];
    assert_eq!(
        word, 0x77,
        "the word never crossed the process-minted endpoint",
    );
}

/// **Capability delegation, end to end.** A granter process passes a resource capability to a
/// receiver process over an IPC channel, narrowed to `WRITE`. Three things must hold, and this
/// checks all three: the receiver *gets* the capability, the receiver can *use* it (a
/// capability minted for it by another process works when it invokes it), and the receiver
/// *cannot pass it on* because it was handed the capability without `GRANT`. This is the
/// operation that makes the capability model composable by processes instead of brokered by the
/// kernel at spawn. See user/src/hello.rs and `user::delegation_service`.
#[test_case]
fn a_capability_can_be_delegated_over_ipc_and_grant_gates_re_delegation() {
    let image = init_image();
    let (resource, report) = delegation_service::wire(image);

    // The receiver invoked the *delegated* capability to SEND this word. Collecting it here is
    // proof the capability the granter minted for the receiver actually carries authority.
    let used = sched::ipc_recv(resource)[0];
    assert_eq!(
        used,
        delegation_service::USED_WORD,
        "a delegated capability did not work when its recipient invoked it",
    );

    // The receiver's own two-bit verdict: bit 0 it received a capability, bit 1 the kernel
    // refused its attempt to re-delegate a capability it holds without GRANT.
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict & 0b01,
        0b01,
        "the receiver never received the delegated capability",
    );
    assert_eq!(
        verdict & 0b10,
        0b10,
        "a capability held WITHOUT grant was allowed to be re-delegated: rights did not gate it",
    );
}

/// **Milestone 12: a process calls a server it was never wired to, and the reply cap is
/// one-shot.** The client `CALL`s across the boundary; the server `RECV_CAP`s the request plus a
/// kernel-minted reply capability naming the caller, answers through it (the round trip through
/// the real syscall path), then tries to answer a second time and reports that the kernel
/// refused. This is what a pre-wired reply endpoint cannot guarantee.
#[test_case]
fn a_process_calls_a_server_and_the_reply_is_one_shot() {
    let (call_report, oneshot_report) = call_service::wire(init_image());

    let reply = sched::ipc_recv(call_report)[0];
    assert_eq!(
        reply, 42,
        "the CALL did not return the server's reply (40 + 2)"
    );

    let one_shot = sched::ipc_recv(oneshot_report)[0];
    assert_eq!(
        one_shot, 1,
        "the server's second reply was NOT refused: the reply capability is not one-shot",
    );
}

/// **Milestone 13: a process revokes a frame across the boundary.** It retypes a page, maps it,
/// then `REVOKE`s it; the kernel unmaps the page and deletes every capability to it, the
/// process's own included, so a second operation on that slot finds nothing there. This exercises
/// the REVOKE syscall path (rights, unmap, cap deletion). The multi-address-space unmapping and
/// the safe reclamation are proven directly in kernel/src/revoke.rs.
#[test_case]
fn a_process_revokes_a_frame_and_loses_the_capability() {
    let report = revoke_service::wire(init_image());
    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict, 1,
        "REVOKE did not both succeed and leave the frame slot empty",
    );
}

/// **Frame capabilities, end to end.** A producer retypes a page into a `Frame`, maps it, writes
/// a sentinel, and delegates a READ-only view to a consumer. Two things must hold: the consumer
/// reads the producer's sentinel through its *own* mapping of the same physical page (the memory
/// is genuinely shared, and the kernel copied nothing), and the consumer *cannot* map that page
/// writable, because it was handed the frame with `READ` alone. This is §10's "shared memory
/// carries data" done by the processes rather than wired by the kernel at spawn. See
/// user/src/hello.rs and `user::frame_service`.
#[test_case]
fn a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it() {
    let image = init_image();
    let report = frame_service::wire(image);

    let verdict = sched::ipc_recv(report)[0];
    assert_eq!(
        verdict & 0b01,
        0b01,
        "the consumer did not read the producer's sentinel through the shared frame: the page was not shared",
    );
    assert_eq!(
        verdict & 0b10,
        0b10,
        "a frame delegated READ-only was mappable writable: rights did not confine the mapping",
    );
}
