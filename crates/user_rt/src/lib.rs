//! **`user_rt`**: the tiny EL0 runtime shared by nife userspace programs (milestone 19f.6).
//!
//! One syscall wrapper (`invoke`) and the three things every program builds on it: `send`, `recv`,
//! and `exit`. That is the whole crate. It exists because milestones 19f.2-5 split the userspace
//! into distinct binaries (`worker`, `console`, `input`, `shell`, plus `hello`), each of which had
//! copied these functions verbatim. The extraction waited on purpose until the split was done: only
//! then was the shared surface known rather than guessed, which is the DECISIONS rule about not
//! building an abstraction before its requirements exist.
//!
//! Milestone 27 added one more thing every program *may* build on: [`heap`], the untyped-backed
//! `GlobalAlloc` that turns the budget a program was granted into `Vec` and `String`. It is a
//! module, not a default: a program that never allocates links no allocator.
//!
//! The `#[panic_handler]` is **still not an item here, and now the trap underneath it is**
//! (milestone 130). A panic handler is per-final-binary: exactly one may exist in a linked program,
//! so an item in this library would force it on every program that links the crate and collide with
//! any program that wants its own (as `hello` does). That has been recorded since 19f.6 and it is
//! still true. What went stale beside it was the sentence "each binary keeps its own one-line
//! handler; it is trivial": the handler grew to fifteen lines with two `unsafe` blocks and two
//! `// SAFETY:` comments, and by the time anyone counted, the trap instruction was inlined at
//! **forty-eight sites** in seven variants, one of which called [`exit`] instead of trapping and so
//! reported a clean death for a panicking program. The constraint was right and the inference from
//! it was not: a *handler* cannot live in a library, but the *trap* always could.
//!
//! So [`trap`] is here, and [`panic_handler!`] is a macro that expands to the handler in the
//! binary. The linking property survives, and the claim below about this being the one place in
//! userspace that names the two ABIs becomes true rather than aspirational. Device helpers (a UART
//! `putc`, echo logic) still stay in the drivers that own them: those are not runtime, they are the
//! program.
//!
//! # Examples
//!
//! **This crate is the one place in the tree where an example genuinely cannot run**, and the reason
//! is worth stating rather than hiding behind a fence marker. Every function here traps to the kernel
//! from EL0: `svc`/`ecall` on a machine with no nife kernel under it is a fault, not a syscall, and
//! `script/test`'s host pass excludes this crate and everything that depends on it (the exclusion set
//! is derived and checked by `script/lint`). So the examples below are `no_run`: they are type-checked
//! against the real signatures on an aarch64 host and are **not executed anywhere**. The things that
//! *can* be checked are the wire contracts layered over them, which is where those crates put their
//! examples (`sink_proto`, `fs_proto`, `entropy_proto`).
//!
//! A program's whole life, in the four calls that make up this crate. Note what is absent: there is
//! no `open`, no path, and no way to name anything that was not handed over.
//!
//! ```no_run
//! use user_rt::{exit, recv, send};
//!
//! /// A pipeline stage: read three words off the endpoint in slot 0, pass them to slot 1.
//! fn relay() -> ! {
//!     const IN: u64 = 0;
//!     const OUT: u64 = 1;
//!     loop {
//!         // `recv` blocks until a sender rendezvouses. The rendezvous IS the flow control: there
//!         // is no buffer to fill and no back-pressure to invent.
//!         let (w0, w1, w2) = recv(IN);
//!         if send(OUT, w0, w1, w2) < 0 {
//!             // A negative return is an `abi::Error`. `Gone` here means the reader exited, which
//!             // is this system's SIGPIPE, arriving as a return code rather than as a signal.
//!             exit();
//!         }
//!     }
//! }
//! ```
//!
//! A client of a service uses [`call`], which blocks until the reply lands. The reply arrives through
//! a one-shot capability the kernel mints, so the client never names the server and the server never
//! names the client:
//!
//! ```no_run
//! use user_rt::call;
//!
//! # fn ask() {
//! const SERVICE: u64 = 2;
//! let (r0, r1) = call(SERVICE, 0x0100_0000_0000_0008, 0);
//!
//! // Negative-as-u64 is enormous, which is how a wire contract tells "no capability in that slot"
//! // from an answer without a probe request. See `entropy_proto::delivered`.
//! assert!((r0 as i64) >= 0 || abi::Error::from_ret(r0 as i64).is_some());
//! # let _ = r1;
//! # }
//! ```
//!
//! # Two ABIs, one surface
//!
//! The syscall instruction and the register file differ by architecture, and this is the one place
//! in userspace that names them. aarch64 uses `svc #0` with the syscall number in `x8` and arguments
//! in `x0..x5`; RISC-V uses `ecall` with the number in `a7` and arguments in `a0..a5`. Both return in
//! the first argument register (`x0` / `a0`). The kernel reconciles the two in `TrapFrame`
//! (DECISIONS §17); here we simply select the right asm at compile time. Every function's signature,
//! semantics, and the `abi` constants are identical across both.
//!
//! Name: unrecorded, and half-argued in the way that keeps it that way. Milestone 63 treats
//! `user_` as settled precedent while renaming `uheap`: "the `u` was *userspace*, and `user_rt`
//! already establishes `user_` as the prefix for it." So the prefix is on the record because this
//! crate established it, which is a circle rather than a reason. The `rt` half is an
//! **abbreviation**, the first of the naming tenet's three failure modes, and nothing weighs it
//! against `runtime`. Introduced 2026-07-25 when the shared EL0 runtime was lifted out; the
//! decision here is worth more than one name, because half a convention rests on it.

#![no_std]

pub mod heap;

/// Invoke a capability: the one syscall a userspace program makes. `cap` names a capability in the
/// process's cspace, `method` selects the operation, and `a0..a2` are its arguments; the return is
/// the kernel's `i64` result. Everything else in this crate is built on this.
///
/// # Safety
/// `svc`/`ecall` traps to the kernel. The kernel validates the capability and the method before
/// acting; that is its whole job. The caller is trusting the kernel, not the other way around.
#[cfg(target_arch = "aarch64")]
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: the `svc` traps to the kernel. The register constraints name the ABI (DECISIONS §10): `x8` the syscall number, `x0..x3` the arguments, `x0` the result. `asm!` is unsafe because the compiler cannot check that, not because a caller can get it wrong.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") cap => ret,
            in("x1") method,
            in("x2") a0,
            in("x3") a1,
            in("x4") a2,
            options(nostack),
        );
    }
    ret
}

/// Invoke a capability (RISC-V). See the aarch64 twin above for the contract; only the trap
/// instruction and register file differ: `ecall`, number in `a7`, args in `a0..a4`, result in `a0`.
///
/// # Safety
/// `ecall` traps to the kernel, which validates the capability and method before acting. Same
/// contract as the aarch64 twin: the caller trusts the kernel, not the other way around.
#[cfg(target_arch = "riscv64")]
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
    // SAFETY: the `ecall` traps to the kernel. The register constraints name the ABI (DECISIONS §10): `a7` the syscall number, `a0..a4` the arguments, `a0` the result. `asm!` is unsafe because the compiler cannot check that, not because a caller can get it wrong.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") cap => ret,
            in("a1") method,
            in("a2") a0,
            in("a3") a1,
            in("a4") a2,
            options(nostack),
        );
    }
    ret
}

/// `SEND` three words on the endpoint capability in `slot`. Blocks until a receiver takes them.
pub fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    // SAFETY: `svc` traps to EL1, which validates the capability named by `slot`.
    unsafe { invoke(slot, abi::endpoint::SEND, w0, w1, w2) }
}

/// **Collect the corpse of a child this supervision endpoint supervises** (DECISIONS §32).
/// `tid` is the thread id the kernel stamped on the death message [`recv_fault`] returned. `0` on
/// success; a negative [`abi::Error`] otherwise, and the three that matter are worth telling apart:
/// `StillAlive` (not dead yet, so wait or escalate to the owner's `Untyped::DESTROY`),
/// `NotSupervised` (not a child of this endpoint, or already collected), and `NotPermitted` (the
/// corpse's region is not reclaimable yet).
///
/// The point of the method: this needs no capability to the child's memory, so a supervisor can be
/// a process that cannot build one. The reclaimed pages go back to the builder's budget.
pub fn reap(slot: u64, tid: u64) -> i64 {
    // SAFETY: `svc`/`ecall`; the kernel validates the capability and the supervision relationship.
    unsafe { invoke(slot, abi::endpoint::REAP, tid, 0, 0) }
}

/// **Read one entry of the domain this supervision endpoint supervises** (milestone 126,
/// `endpoint::SURVEY`). Returns `(next_cursor, tid, state)`: start at `cursor = 0`, feed each
/// `next_cursor` back, and stop when [`abi::survey::DONE`] comes back.
///
/// A negative first word is an [`abi::Error`], and the one that matters is `NotPermitted`: this
/// endpoint capability does not carry `READ`, so the holder may send here but not look. **That is
/// a refusal and not an empty domain**, and a caller must print it as one.
///
/// Three words out of one `invoke`, so it is written like [`recv`] rather than through the
/// single-value helper.
#[cfg(target_arch = "aarch64")]
pub fn survey(slot: u64, cursor: u64) -> (i64, u64, u64) {
    let (mut r0, mut w1, mut w2): (i64, u64, u64);
    // SAFETY: `svc`. SURVEY returns three words in x0/x1/x2.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => r0,
            in("x1") abi::endpoint::SURVEY,
            lateout("x1") w1,
            inlateout("x2") cursor => w2,
            in("x3") 0u64,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (r0, w1, w2)
}

/// `SURVEY` one entry (RISC-V). See the aarch64 twin; `ecall`, slot in `a0`, `SURVEY` in `a1`, the
/// cursor in `a2`, the three returned words in `a0`/`a1`/`a2`.
#[cfg(target_arch = "riscv64")]
pub fn survey(slot: u64, cursor: u64) -> (i64, u64, u64) {
    let (mut r0, mut w1, mut w2): (i64, u64, u64);
    // SAFETY: `ecall`. SURVEY returns three words in a0/a1/a2.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => r0,
            inlateout("a1") abi::endpoint::SURVEY => w1,
            inlateout("a2") cursor => w2,
            in("a3") 0u64,
            in("a4") 0u64,
            options(nostack),
        );
    }
    (r0, w1, w2)
}

/// `RECV` three words on the endpoint capability in `slot`. Blocks until a sender arrives; returns
/// the three words the sender passed in `x0`, `x1`, `x2`.
#[cfg(target_arch = "aarch64")]
pub fn recv(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `svc`. RECV returns three words in x0/x1/x2.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => w0,
            in("x1") abi::endpoint::RECV,
            lateout("x1") w1,
            lateout("x2") w2,
            in("x3") 0u64,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// `RECV` three words (RISC-V). See the aarch64 twin; `ecall`, slot in `a0`, `RECV` in `a1`, the
/// three returned words in `a0`/`a1`/`a2`.
#[cfg(target_arch = "riscv64")]
pub fn recv(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `ecall`. RECV returns three words in a0/a1/a2.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => w0,
            inlateout("a1") abi::endpoint::RECV => w1,
            lateout("a2") w2,
            in("a3") 0u64,
            in("a4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// `RECV` **all five words** on the endpoint capability in `slot`: `(w0, w1, w2, w3, w4)`.
///
/// The same `RECV` [`recv`] makes, read to its full width. `RECV` has returned five registers since
/// milestone 22 phase A (the kernel writes `w1..w4` directly; DECISIONS §26 implementation note 4),
/// because a fault notification is five words: `(event, tid, pc, addr, reserved)`. Ordinary
/// three-word IPC leaves the top two zero, which is why [`recv`] can keep ignoring them.
///
/// This exists for a **supervisor**, and it is the first thing in userspace to read `w3`: a
/// restart policy needs the event and the tid, but a *checker* needs the faulting address, which is
/// the only word that says where the dead thread actually pointed. No new syscall and no new method
/// (§26's whole surface claim): just the rest of a result that was already being returned.
#[cfg(target_arch = "aarch64")]
pub fn recv_fault(slot: u64) -> (u64, u64, u64, u64, u64) {
    let (mut w0, mut w1, mut w2, mut w3, mut w4): (u64, u64, u64, u64, u64);
    // SAFETY: `svc`. RECV returns five words in x0..x4.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => w0,
            in("x1") abi::endpoint::RECV,
            lateout("x1") w1,
            lateout("x2") w2,
            in("x3") 0u64,
            lateout("x3") w3,
            in("x4") 0u64,
            lateout("x4") w4,
            options(nostack),
        );
    }
    (w0, w1, w2, w3, w4)
}

/// `RECV` all five words (RISC-V). See the aarch64 twin; `ecall`, the five words in `a0`..`a4`.
#[cfg(target_arch = "riscv64")]
pub fn recv_fault(slot: u64) -> (u64, u64, u64, u64, u64) {
    let (mut w0, mut w1, mut w2, mut w3, mut w4): (u64, u64, u64, u64, u64);
    // SAFETY: `ecall`. RECV returns five words in a0..a4.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => w0,
            inlateout("a1") abi::endpoint::RECV => w1,
            lateout("a2") w2,
            inlateout("a3") 0u64 => w3,
            inlateout("a4") 0u64 => w4,
            options(nostack),
        );
    }
    (w0, w1, w2, w3, w4)
}

/// `RECV_CAP` on the endpoint capability in `slot`: receive a message that may carry a
/// capability. Blocks until one arrives; returns `(w0, cap_slot, w1)`, where `cap_slot` is where
/// the incoming capability landed in this thread's cspace, or [`abi::endpoint::NO_CAP`] if the
/// message carried none. This is how a server receives a [`call`]: the delivered capability is
/// the one-shot Reply naming the caller (milestone 12, DECISIONS §12).
#[cfg(target_arch = "aarch64")]
pub fn recv_cap(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `svc`. RECV_CAP returns three words in x0/x1/x2.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => w0,
            in("x1") abi::endpoint::RECV_CAP,
            lateout("x1") w1,
            lateout("x2") w2,
            in("x3") 0u64,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// `RECV_CAP` (RISC-V). See the aarch64 twin; `ecall`, the three returned words in `a0`/`a1`/`a2`.
#[cfg(target_arch = "riscv64")]
pub fn recv_cap(slot: u64) -> (u64, u64, u64) {
    let (mut w0, mut w1, mut w2): (u64, u64, u64);
    // SAFETY: `ecall`. RECV_CAP returns three words in a0/a1/a2.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => w0,
            inlateout("a1") abi::endpoint::RECV_CAP => w1,
            lateout("a2") w2,
            in("a3") 0u64,
            in("a4") 0u64,
            options(nostack),
        );
    }
    (w0, w1, w2)
}

/// `CALL` on the endpoint capability in `slot`: send two words and block until the server
/// replies through the one-shot Reply capability the kernel mints (milestone 12). Returns the
/// two reply words. The atomic send-and-wait that makes a request unmistakably answerable.
#[cfg(target_arch = "aarch64")]
pub fn call(slot: u64, w0: u64, w1: u64) -> (u64, u64) {
    let (mut r0, mut r1): (u64, u64);
    // SAFETY: `svc`. CALL returns the two reply words in x0/x1.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_INVOKE,
            inlateout("x0") slot => r0,
            in("x1") abi::endpoint::CALL,
            lateout("x1") r1,
            in("x2") w0,
            in("x3") w1,
            in("x4") 0u64,
            options(nostack),
        );
    }
    (r0, r1)
}

/// `CALL` (RISC-V). See the aarch64 twin; `ecall`, the two reply words in `a0`/`a1`.
#[cfg(target_arch = "riscv64")]
pub fn call(slot: u64, w0: u64, w1: u64) -> (u64, u64) {
    let (mut r0, mut r1): (u64, u64);
    // SAFETY: `ecall`. CALL returns the two reply words in a0/a1.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_INVOKE,
            inlateout("a0") slot => r0,
            inlateout("a1") abi::endpoint::CALL => r1,
            in("a2") w0,
            in("a3") w1,
            in("a4") 0u64,
            options(nostack),
        );
    }
    (r0, r1)
}

/// `REPLY` through the one-shot Reply capability in `slot`: deliver two words to the blocked
/// caller and wake it. The capability is consumed by the kernel on use (that is what makes it
/// one-shot), so the slot is free again when this returns.
pub fn reply(slot: u64, r0: u64, r1: u64) -> i64 {
    // SAFETY: `svc`/`ecall`; the kernel validates the Reply capability and consumes it.
    unsafe { invoke(slot, abi::reply::REPLY, r0, r1, 0) }
}

/// **Map the `Frame` capability in `frame_slot` at `va`**, drawing the page tables from the untyped
/// in `untyped_slot`. `true` if the page is now there.
///
/// The verb a process that *holds* a page uses to put it in its own address space (milestone 108).
/// It replaces a page the kernel wired into the process at spawn, and the difference is not
/// cosmetic: a spawn-time mapping has no capability behind it, so nobody can narrow it, hand it on,
/// or take it back, while a frame the process mapped itself is recorded in the revocation database
/// and can be pulled out from under it by `Frame::REVOKE`. See notes/frames.md.
///
/// `writable` needs `WRITE` on the frame; a read-only mapping needs `READ`. A caller handed a
/// narrowed view that asks for more than it holds gets `false` and no mapping, which is the rights
/// ladder doing its job rather than an error to route around.
pub fn map_frame(frame_slot: u64, va: u64, writable: bool, untyped_slot: u64) -> bool {
    // SAFETY: `svc`/`ecall`. The kernel validates the frame capability, the rights, the address and
    // the untyped before it touches a page table.
    unsafe {
        invoke(
            frame_slot,
            abi::frame::MAP,
            va,
            writable as u64,
            untyped_slot,
        ) == 0
    }
}

/// Give up the CPU (`SYS_YIELD`). Returns when the scheduler runs this thread again; if another
/// thread is ready, control goes there and back, which is one context-switch round trip.
#[cfg(target_arch = "aarch64")]
pub fn yield_now() {
    // SAFETY: `svc`; SYS_YIELD gives up the CPU and returns with nothing to clean up.
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Give up the CPU (RISC-V). `ecall`, `SYS_YIELD` in `a7`.
#[cfg(target_arch = "riscv64")]
pub fn yield_now() {
    // SAFETY: `ecall`; SYS_YIELD gives up the CPU and returns with nothing to clean up.
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Drop the capability in `slot` from this thread's cspace (`SYS_CAP_DELETE`). Deleting an empty
/// slot is a no-op. A program that retypes many objects (a loader, a spawner) frees each slot as
/// soon as it is done with it, so its fixed cspace does not fill.
#[cfg(target_arch = "aarch64")]
pub fn cap_delete(slot: u64) {
    // SAFETY: `svc`; SYS_CAP_DELETE frees a slot in the caller's own cspace, nothing to clean up.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_CAP_DELETE,
            in("x0") slot,
            options(nostack, nomem),
        );
    }
}

/// Drop the capability in `slot` (RISC-V). `ecall`, `SYS_CAP_DELETE` in `a7`, slot in `a0`.
#[cfg(target_arch = "riscv64")]
pub fn cap_delete(slot: u64) {
    // SAFETY: `ecall`; SYS_CAP_DELETE frees a slot in the caller's own cspace, nothing to clean up.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_CAP_DELETE,
            in("a0") slot,
            options(nostack, nomem),
        );
    }
}

/// The virtual counter, `CNTVCT_EL0`: a monotonic tick count for self-timing. Readable at EL0 only
/// because the kernel opened `CNTKCTL_EL1.EL0VCTEN` (see kernel `timer::init` and notes/abi.md); the
/// read is a plain register move, no syscall. Pair with [`cntfrq`] to turn tick deltas into seconds.
#[cfg(target_arch = "aarch64")]
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reading a system register the kernel made EL0-readable. No side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack));
    }
    t
}

/// The monotonic tick count (RISC-V): `rdtime`, which reads the `time` CSR. Readable from U-mode
/// only because the kernel sets `scounteren.TM` in its per-hart timer init, the same shape as
/// aarch64 needing `CNTKCTL_EL1.EL0VCTEN`. That claim was aspirational until 2026-07-30: the bit was
/// never set and this worked only because QEMU's OpenSBI leaves it permitted. Pair with [`cntfrq`]
/// to get seconds.
#[cfg(target_arch = "riscv64")]
pub fn now() -> u64 {
    let t: u64;
    // SAFETY: reading the time CSR the kernel made U-mode-readable. No side effects.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack));
    }
    t
}

/// The counter frequency in Hz, `CNTFRQ_EL0`: how many [`now`] ticks make a second. Constant for the
/// life of the machine (QEMU reports 62.5 MHz under TCG, the host's counter frequency under HVF).
#[cfg(target_arch = "aarch64")]
pub fn cntfrq() -> u64 {
    let f: u64;
    // SAFETY: reading a system register; EL0-readable once EL0VCTEN is set.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    f
}

/// The counter frequency in Hz (RISC-V). Unlike aarch64's `CNTFRQ_EL0`, RISC-V has **no** register
/// that reports the timebase; it lives in the device tree's `timebase-frequency` (10 MHz on QEMU
/// `virt`), which userspace cannot read. This is a real ABI gap: a complete port hands the frequency
/// to a process at start (an aux-vector entry, the way Linux passes `AT_HWCAP`). Until that exists,
/// this returns the known QEMU `virt` constant so self-timing works on the one machine we run.
#[cfg(target_arch = "riscv64")]
pub fn cntfrq() -> u64 {
    10_000_000
}

/// **Monotonic nanoseconds since boot**, from [`now`] and [`cntfrq`].
///
/// Here rather than in each program because two of them need it and the naive form is wrong:
/// `ticks * 1_000_000_000` overflows a `u64` about five minutes into a boot at 62.5 MHz, so the
/// conversion splits into whole seconds and a remainder. `user/src/clock.rs` found that the hard
/// way; `date` and the shell's `time` then wanted the same five lines, which is CLAUDE.md rule 7 at
/// the smallest size it comes in.
///
/// **This needs no capability**, and that is worth knowing when reading anything that calls it: the
/// counter is ambient (the kernel opened it to EL0), so a *duration* is measurable by any process.
/// What needs a capability is the **wall clock**, which is this plus an offset only the clock page
/// carries. See notes/clock.md.
pub fn monotonic_nanos() -> u64 {
    /// Nanoseconds in a second. Spelled here rather than taken from `clock_proto`, because this
    /// crate is the syscall runtime and depends on `abi` alone; a runtime that pulled in a wire
    /// contract to name a unit would be the wrong direction for the dependency.
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    let freq = cntfrq();
    let ticks = now();
    let secs = ticks / freq;
    let rem = ticks % freq;
    secs * NANOS_PER_SEC + rem * NANOS_PER_SEC / freq
}

/// Terminate this process. The kernel reaps the thread and frees its whole address space. Never
/// returns; the trailing spin is only there to satisfy the `-> !` type if `svc` ever came back.
pub fn exit() -> ! {
    // SAFETY: the syscall never returns; the trailing spin only satisfies the `-> !` type.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_EXIT, in("x0") 0u64, options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ecall` with SYS_EXIT traps to the kernel, which never returns to this thread. The options promise it touches neither memory nor the stack.
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_EXIT, in("a0") 0u64, options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}

/// **Die where the mistake was.** Raise a breakpoint the kernel turns into a fault, so the process
/// is killed rather than allowed to limp on: `brk #0` on aarch64, `ebreak` on riscv64.
///
/// This is the other way a program can end, and the difference from [`exit`] is not a spelling.
/// `exit` reports `EVENT_EXIT` to a supervisor and this reports `EVENT_FAULT`
/// (`kernel/src/sched.rs`, DECISIONS §26), so a supervised child that traps is legible as having
/// failed and one that exits is not. A panic must take this path or it lies about what happened.
///
/// The trailing spin never runs. It is here for the same reason [`exit`]'s is, to satisfy `-> !`
/// if the trap ever came back, and it spins rather than calling `exit` on purpose: `exit` would
/// turn an impossible situation into a clean-looking death, which is precisely the confusion the
/// paragraph above exists to prevent.
///
/// A verb, which is right for a function here: `send`, `recv`, `reap` and `exit` are all verbs, and
/// the naming tenet's noun rule is about crates, programs and modules rather than about the things
/// they do.
pub fn trap() -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill. The options promise
    // it touches neither memory nor the stack.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill. The options
    // promise it touches neither memory nor the stack.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem));
    };
    loop {
        core::hint::spin_loop();
    }
}

/// **The panic handler every nife program wants**, as a macro so it stays per-final-binary.
///
/// Write `user_rt::panic_handler!();` once at the top level of a binary and it expands to a
/// `#[panic_handler]` that calls [`trap`].
///
/// # Why a macro and not a plain item in this crate
///
/// A `#[panic_handler]` is per-final-binary: exactly one may exist in a linked program, so a
/// library that defines one forces it on every binary that links the library and collides with any
/// binary wanting its own. That constraint is real and this crate's header has recorded it since
/// milestone 19f.6. A macro keeps it: nothing is defined until a binary asks, and a program with
/// its own handler simply does not invoke this.
///
/// What the header got wrong, and what milestone 130 is fixing, is the clause after it: "each
/// binary keeps its own one-line handler; it is trivial." It stopped being one line. By the time
/// anyone counted it was fifteen, with two `unsafe` blocks and two `// SAFETY:` comments, at
/// forty-eight sites across `user/`, `crates/` and `fs_server/`, in **seven** variants. One of
/// them (`terminal_sink_caretaker`) called `exit` instead of trapping, which reports a clean death
/// for a panicking program; it was latent only because that program happens to be spawned
/// unsupervised.
///
/// So the decision not to put a *handler* in a library was right, and the inference that each
/// binary must therefore hand-roll the *trap* did not follow. The trap belongs here, in the crate
/// whose header calls itself the one place in userspace that names the two ABIs, and the macro is
/// what lets that be true without breaking the linking property.
///
/// # Examples
///
/// ```ignore
/// #![no_std]
/// #![no_main]
///
/// user_rt::panic_handler!();
///
/// #[unsafe(no_mangle)]
/// pub extern "C" fn _start() -> ! {
///     user_rt::exit()
/// }
/// ```
///
/// # BUGS
///
/// It takes no arguments and ignores the `PanicInfo`, because a program with no console cannot
/// print one. A program that *can* print (it holds a terminal endpoint) and wants the message on
/// the way down still writes its own handler; this macro is the default, not a mandate.
///
/// Named for the item it expands to, which is the one thing a reader needs it to say.
#[macro_export]
macro_rules! panic_handler {
    () => {
        #[panic_handler]
        fn panic(_: &::core::panic::PanicInfo) -> ! {
            $crate::trap()
        }
    };
}
