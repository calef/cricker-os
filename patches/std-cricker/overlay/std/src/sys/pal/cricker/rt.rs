//! The std runtime contract, and the syscall glue that meets it.
//!
//! This is the PAL's twin of `crates/user_rt`: the same `svc #0` / `ecall` instructions, the
//! same register convention, deliberately re-stated here because std cannot depend on an
//! out-of-tree crate. The ABI *constants* are not re-stated: `abi.rs` next door is generated
//! verbatim from `crates/abi/src/lib.rs` by `cargo xtask std-src`, so the numbers cannot drift.
//! Only these few asm wrappers are hand-copied; if `user_rt`'s change, change these.
//!
//! # The std slot convention
//!
//! A std program's loader owes it, per the out-of-band-contract rule of notes/abi.md §4:
//!
//! - **slot 0**: an untyped budget. The global allocator draws heap pages from it lazily via
//!   `untyped::MAP`, at [`HEAP_BASE`], capped at [`HEAP_MAX`].
//! - **slot 1**: an endpoint with WRITE. `stdout` and `stderr` SEND here, 16 bytes per message
//!   (w0 = byte count, w1|w2 = the bytes, little-endian). Interleaving of out and err is the
//!   phase-one price of one endpoint; milestone 28's terminal contract owns fixing it.
//!
//! And two more, granted only to a std program that is given the network (milestone 27 phase two;
//! the net PAL in `sys/net` binds them, DECISIONS §25):
//!
//! - **slot 2**: the `Stack` endpoint with WRITE. `std::net` speaks the netstack socket contract
//!   (`netproto`) over it: `CALL`s carry a socket id and control words, `SEND_CAP` delegates a
//!   per-socket shared frame. A program not given the network leaves this slot empty, and every
//!   `TcpStream`/`UdpSocket` operation returns `Unsupported` rather than blocking.
//! - **slot 3**: an untyped budget the net PAL mints and maps each socket's shared frame from.
//!
//! And one more, granted only to a std program that is given a **directory** (milestone 27 phase
//! two, the FS-service half; the fs PAL in `sys/fs` binds it, DECISIONS §27):
//!
//! - **slot 4**: the FS-service endpoint with WRITE. That endpoint **is** the directory
//!   capability: the server it reaches is bound to one directory node, and every name `std::fs`
//!   sends is resolved under that directory. There is no global namespace to reach past it, so a
//!   path that tries to leave the directory is refused before it ever reaches the wire. The grant
//!   comes with one page the loader maps at [`FS_PAGE`], the contract's unit of transfer. A
//!   program left this slot empty gets `Unsupported` from every `std::fs` call, which is what "no
//!   ambient filesystem" feels like from inside a process.
//!
//! Programs that never allocate, print, open a socket, or open a file never touch the slots they
//! do not use.

pub const UNTYPED_SLOT: u64 = 0;
pub const STDOUT_SLOT: u64 = 1;
pub const STACK_SLOT: u64 = 2;
pub const NET_UNTYPED_SLOT: u64 = 3;
pub const FS_DIR_SLOT: u64 = 4;

/// Where the loader maps the page a std program shares with its FS server: 4096 bytes, one file
/// block, carrying a name out on `OPEN` and file bytes both ways on `READ`/`WRITE`. Clear of the
/// program image (0x40_0000), its stack (below 0x50_0000), the net PAL's per-socket frames
/// (0x1000_0000 upward, one page per socket id), and the heap (0x4000_0000). The kernel-side
/// wiring maps the same physical frame the FS server holds; see `fs_service` in kernel/src/user.rs.
pub const FS_PAGE: u64 = 0x1100_0000;

/// Where the heap lives: 1 GiB, clear of the program image (0x40_0000), stacks, shared pages,
/// and the initrd window (0x2000_0000). Same value as `user_rt::heap::DEFAULT_BASE`.
pub const HEAP_BASE: u64 = 0x4000_0000;

/// The heap's growth cap. Generous because the untyped budget is the real, per-program limit
/// (`untyped::MAP` returns OutOfMemory when it is spent); this only bounds the VA range.
pub const HEAP_MAX: u64 = 256 * 1024 * 1024;

use super::abi;

/// Invoke a capability. See `crates/user_rt::invoke`, of which this is a verbatim twin.
#[cfg(target_arch = "aarch64")]
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
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

/// Invoke a capability (RISC-V): `ecall`, number in `a7`, args in `a0..a4`, result in `a0`.
#[cfg(target_arch = "riscv64")]
pub unsafe fn invoke(cap: u64, method: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    let ret: i64;
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

/// SEND three words on the endpoint in `slot`. Blocks until a receiver takes them.
pub fn send(slot: u64, w0: u64, w1: u64, w2: u64) -> i64 {
    unsafe { invoke(slot, abi::endpoint::SEND, w0, w1, w2) }
}

/// `CALL` the endpoint in `slot`: send two words and block until the server replies through the
/// one-shot Reply capability the kernel mints. Returns the two reply words. A verbatim twin of
/// `user_rt::call`; the net PAL (`sys/net`) drives the socket contract with it.
///
/// On a syscall-level failure (an empty slot, wrong rights) the kernel returns a negative value
/// in the first result register, which a caller distinguishes from a server reply by reading it
/// as `i64` (the net server never replies a negative word).
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

/// Give up the CPU (`SYS_YIELD`); the timed sleep loop is built on this.
pub fn yield_now() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("svc #0", in("x8") abi::SYS_YIELD, options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ecall", in("a7") abi::SYS_YIELD, options(nostack, nomem));
    }
}

/// Terminate this process (`SYS_EXIT`). The kernel reaps the thread and frees the address space.
pub fn exit(code: i64) -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") abi::SYS_EXIT,
            in("x0") code as u64,
            options(nostack, nomem),
        );
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") abi::SYS_EXIT,
            in("a0") code as u64,
            options(nostack, nomem),
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Fault on purpose (`brk` / `ebreak`): the kernel kills the process and reports where. This is
/// `abort()` on an OS whose failure story is "a fault the kernel attributes", and it is what
/// `panic!` reaches after printing, since the target is panic=abort.
pub fn abort() -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem));
    }
    loop {
        core::hint::spin_loop();
    }
}

/// The monotonic tick count: the one ambient readable this ABI grants (notes/abi.md, "the one
/// ambient thing"). aarch64 `CNTVCT_EL0`; RISC-V `rdtime`.
pub fn now() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let t: u64;
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack));
        }
        t
    }
    #[cfg(target_arch = "riscv64")]
    {
        let t: u64;
        unsafe {
            core::arch::asm!("rdtime {}", out(reg) t, options(nomem, nostack));
        }
        t
    }
}

/// Ticks per second. aarch64 reports it in `CNTFRQ_EL0`; RISC-V has no architectural register
/// for the timebase, so this is the QEMU `virt` constant, the same honest gap `user_rt::cntfrq`
/// records (10 MHz until the ABI grows an aux-vector-style handoff).
pub fn cntfrq() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let f: u64;
        unsafe {
            core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
        }
        f
    }
    #[cfg(target_arch = "riscv64")]
    {
        10_000_000
    }
}
