//! **The RISC-V (rv64) architecture layer.** The second implementation of the `arch/` contract
//! (milestone 20, notes/riscv-port.md), the one that proves rule #1: the rest of the kernel calls
//! everything here through `crate::arch`, exactly the names it calls on aarch64.
//!
//! This is a scaffold. The pieces that are pure and portable-in-spirit (the per-CPU register, the
//! halt/idle/barrier primitives, the initial thread contexts) are real. The pieces that are the
//! work of later steps (MMU/Sv39, traps, the timer, SMP bring-up, the test-exit) are loud
//! `unimplemented!()` stubs, each naming the step that fills it, so nobody mistakes a stub for a
//! working port. What is proved *today* is that the boundary is complete: a second architecture
//! compiles and links against the whole kernel with no change above `arch/`.

use core::arch::{asm, global_asm};

pub mod context;
pub mod exceptions;
pub mod interrupts;
pub mod irq;
pub mod mmu;
pub mod semihosting;
pub mod timer;

// The saved thread context and how a new one is faked (the Rust half of context.s). Re-exported
// flat so `crate::arch::{Context, switch_to}` names them regardless of architecture.
pub use context::{Context, switch_to};

// The S-mode entry (_start), the .bss zeroing, and the stack handoff to `kernel_main`.
global_asm!(include_str!("boot.s"));

// The context switch and the two first-run trampolines (the asm half of context.rs).
global_asm!(include_str!("context.s"));

// The S-mode trap vector (the asm half of exceptions.rs): save the frame, dispatch, restore, sret.
global_asm!(include_str!("trap.s"));

use core::sync::atomic::{AtomicUsize, Ordering};

/// **The per-hart trap stash**, the thing `sscratch` points at in both U- and S-mode. RISC-V's `tp`
/// is a general register that U-mode owns, so a trap from U-mode arrives with the user's `tp` and the
/// kernel must recover its own per-CPU pointer from a *hart-private* source. That source is this
/// struct: `sscratch` holds `&TRAP_STASH[hart]`, and `trap.s` reads the kernel `tp` from `percpu` and
/// the kernel stack from `kernel_sp`. One global `KERNEL_TP` could not do this once there is more than
/// one hart (every hart would reload hart 0's pointer); an array indexed by hart, reached through the
/// per-hart `sscratch`, is what makes the trap path SMP-correct.
///
/// `#[repr(C)]` and the field order are load-bearing: `trap.s` accesses these by fixed byte offset
/// (0, 8, 16, 24), checked below. Each hart touches only its own entry, and only during its own trap,
/// so the `AtomicUsize`s are for interior mutability through the shared static, not cross-core
/// synchronization (the asm reads/writes them plainly).
#[repr(C)]
struct TrapStash {
    /// The current thread's kernel-stack top; where a U-mode trap lands. Set on every return to
    /// U-mode (trap.s `trap_return`), so it always names the thread about to run in U-mode.
    kernel_sp: AtomicUsize,
    /// This hart's `PerCpu` pointer: the kernel `tp` the trap entry restores.
    percpu: AtomicUsize,
    /// Two scratch words the trap entry uses to free registers before it has a stack.
    scratch0: AtomicUsize,
    scratch1: AtomicUsize,
}

impl TrapStash {
    const fn new() -> Self {
        Self {
            kernel_sp: AtomicUsize::new(0),
            percpu: AtomicUsize::new(0),
            scratch0: AtomicUsize::new(0),
            scratch1: AtomicUsize::new(0),
        }
    }
}

// trap.s hardcodes these offsets; keep them honest.
const _: () = {
    assert!(core::mem::offset_of!(TrapStash, kernel_sp) == 0);
    assert!(core::mem::offset_of!(TrapStash, percpu) == 8);
    assert!(core::mem::offset_of!(TrapStash, scratch0) == 16);
    assert!(core::mem::offset_of!(TrapStash, scratch1) == 24);
};

/// One trap stash per hart, indexed like `cpu::PERCPU`. A static so it exists before any allocator,
/// which the very first trap (and every secondary's bring-up) needs.
static TRAP_STASH: [TrapStash; crate::cpu::MAX_CPUS] =
    [const { TrapStash::new() }; crate::cpu::MAX_CPUS];

/// The physical hart id OpenSBI handed the kernel on (`a0` at `_start`), stashed by boot.s. The
/// logical cpu id equals the physical hart id on RISC-V (so the PLIC context `2*hart+1`, the timer,
/// and IPIs all line up), and this is which hart is the boot cpu. QEMU's boot hart is usually 0 but
/// the spec does not require it, so the SMP bring-up reads this rather than assuming.
#[unsafe(no_mangle)]
static BOOT_HARTID: AtomicUsize = AtomicUsize::new(0);

/// The physical hart id the kernel booted on (see [`BOOT_HARTID`]).
pub fn boot_hartid() -> usize {
    BOOT_HARTID.load(Ordering::Relaxed)
}

/// The logical id of the hart the kernel boots on. On RISC-V the logical cpu id equals the physical
/// hart id, and QEMU's boot hart is not guaranteed to be 0, so this reads the id OpenSBI handed us at
/// `_start` (stashed by boot.s). The SMP bring-up starts every *other* hart. The aarch64 twin is
/// always 0.
pub fn boot_cpu_id() -> usize {
    boot_hartid()
}

/// Set this hart's per-CPU pointer. RISC-V's `tp` (thread pointer) is the analog of aarch64's
/// `TPIDR_EL1`, but a general register, so this also arms the per-hart trap path: it records the
/// pointer in this hart's [`TrapStash`] and points `sscratch` at that stash, so `trap.s` can recover
/// the kernel `tp` after a U-mode round trip. See `crate::percpu` and trap.s.
pub fn set_percpu(ptr: usize) {
    // Set `tp` first so `cpu::id()` (which reads `tp`) resolves this hart's index into TRAP_STASH.
    // SAFETY: writes a general register the kernel reserves for per-CPU data. No memory effect.
    unsafe { asm!("mv tp, {}", in(reg) ptr, options(nomem, nostack, preserves_flags)) };
    let stash = &TRAP_STASH[crate::cpu::id()];
    stash.percpu.store(ptr, Ordering::Relaxed);
    let stash_ptr = stash as *const TrapStash as usize;
    // SAFETY: `sscratch` now names this hart's stash; trap.s reads it as `&TrapStash` on every trap.
    unsafe {
        asm!("csrw sscratch, {}", in(reg) stash_ptr, options(nomem, nostack, preserves_flags))
    };
}

/// Read this hart's per-CPU pointer (the value last handed to [`set_percpu`]).
pub fn percpu() -> usize {
    let tp: usize;
    // SAFETY: reads a general register. No side effects.
    unsafe { asm!("mv {}, tp", out(reg) tp, options(nomem, nostack, preserves_flags)) };
    tp
}

/// Start a secondary hart. The name is aarch64's (`psci_cpu_on`); the mechanism is the SBI HSM
/// (Hart State Management) extension's `sbi_hart_start(hartid, start_addr, opaque)`. The firmware
/// starts the target hart at `entry` (a physical address) in S-mode with paging off, `a0` = its hart
/// id and `a1` = `context`. Returns the SBI error (0 = success; a hart QEMU did not create, when
/// `-smp` is smaller than MAX_CPUS, returns a nonzero error rather than hanging). See boot.s
/// `secondary_boot`.
pub fn psci_cpu_on(target_hart: u64, entry: u64, context: u64) -> i64 {
    const SBI_HSM_EID: usize = 0x0048_534D; // "HSM"
    const SBI_HART_START_FID: usize = 0;
    let error: i64;
    // SAFETY: an SBI call. a7 = extension, a6 = function, a0..a2 = (hartid, start_addr, opaque). The
    // firmware returns the error in a0 and clobbers a1; nothing else.
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_HSM_EID,
            in("a6") SBI_HART_START_FID,
            inout("a0") target_hart => error,
            inout("a1") entry => _,
            in("a2") context,
            options(nostack),
        );
    }
    error
}

/// Send a reschedule inter-processor interrupt to `target_hart` via the SBI IPI extension. The
/// firmware sets the target hart's `sip.SSIP`, so it takes a supervisor software interrupt
/// (`scause` = 1) and drains its inbox. The RISC-V analog of an aarch64 reschedule SGI; used by
/// `arch::irq::send_reschedule` (and so by `sched::place_on` when it hands a thread to another hart).
pub fn sbi_send_ipi(target_hart: usize) {
    const SBI_IPI_EID: usize = 0x0073_5049; // "sPI"
    const SBI_SEND_IPI_FID: usize = 0;
    let mask = 1usize << target_hart; // hart_mask, relative to base 0
    // SAFETY: an SBI call. a7 = extension, a6 = function, a0 = hart bitmap, a1 = mask base. The
    // firmware returns in a0/a1 (ignored); nothing else is touched.
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_IPI_EID,
            in("a6") SBI_SEND_IPI_FID,
            inout("a0") mask => _,
            inout("a1") 0usize => _,
            options(nostack),
        );
    }
}

/// Shoot down a virtual-address translation on the harts in `hart_mask`, via the SBI RFENCE
/// extension. The firmware IPIs those harts and each executes `sfence.vma start, ...` for the range.
/// RISC-V has no hardware TLB broadcast (aarch64's `tlbi ..., is` does), so a kernel page-table change
/// must be pushed to the other harts this way or a migrated thread faults on a stale translation. See
/// `mmu::flush_tlb`.
pub fn sbi_remote_sfence_vma(hart_mask: usize, start: usize, size: usize) {
    const SBI_RFENCE_EID: usize = 0x5246_4E43; // "RFNC"
    const SBI_REMOTE_SFENCE_VMA_FID: usize = 1;
    // SAFETY: an SBI call. a7/a6 = extension/function, a0 = hart bitmap, a1 = mask base (0), a2/a3 =
    // the address range. The firmware returns in a0/a1 (ignored); nothing else is touched.
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_RFENCE_EID,
            in("a6") SBI_REMOTE_SFENCE_VMA_FID,
            inout("a0") hart_mask => _,
            inout("a1") 0usize => _,
            in("a2") start,
            in("a3") size,
            options(nostack),
        );
    }
}

/// Bring this hart's architecture state up. On RISC-V that is the trap vector (`stvec`): unlike
/// aarch64's `VBAR_EL1` this is the only per-hart install a secondary needs here, since the timer and
/// interrupt unmasking are separate steps in `secondary_main`. The primary sets `stvec` directly in
/// its boot tour; this is the path a secondary takes to the same place.
pub fn init() {
    exceptions::init();
}

/// Stop this hart forever, cheaply. `wfi` parks the hart until an interrupt; with nothing left to
/// wake it, that is the rest of time at zero host CPU. The same discipline as aarch64: `wfi`, never
/// a spin. See CLAUDE.md, "Never leave QEMU running".
pub fn halt() -> ! {
    loop {
        // SAFETY: wait-for-interrupt is always safe; it only affects when the next instruction runs.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

/// Park until the next interrupt (the scheduler's idle primitive).
pub fn wait_for_interrupt() {
    // SAFETY: as `halt`, but returns when an interrupt arrives.
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

/// This core's current stack pointer, for the stack-overflow canary check (stack.rs).
pub fn current_sp() -> u64 {
    let sp: u64;
    // SAFETY: reads a register. No side effects.
    unsafe { asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    sp
}

/// A DMA write memory barrier: order all prior stores before any device sees a later one. RISC-V's
/// `fence ow, ow` orders outer (device/IO) writes; the plain `fence` here is the conservative full
/// barrier, matching aarch64's `dsb sy`. Tightened when a real DMA driver lands.
pub fn dma_wmb() {
    // SAFETY: a fence has no memory effect of its own; it only constrains ordering.
    unsafe { asm!("fence", options(nostack, preserves_flags)) };
}

/// Make the instruction fetcher aware of code just written as data. Where aarch64 needs a
/// clean/invalidate loop over cache lines, RISC-V has one instruction: `fence.i` synchronizes this
/// hart's instruction stream with its prior stores. It has no address range (it covers everything),
/// so `va`/`len` are ignored; a multi-hart port will additionally need to fence the other harts. See
/// notes/riscv-port.md, leak #3.
pub fn sync_icache(va: u64, len: usize) {
    let _ = (va, len);
    // SAFETY: `fence.i` only orders instruction fetch against prior stores on this hart.
    unsafe { asm!("fence.i", options(nostack, preserves_flags)) };
}
