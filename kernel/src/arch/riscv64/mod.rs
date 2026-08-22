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
#[cfg(feature = "fastpath_pad")]
mod fastpath_pad;
pub mod interrupts;
pub mod iommu;
pub mod irq;
pub mod isa;
pub mod mmu;
pub mod semihosting;
pub mod timer;

// The saved thread context and how a new one is faked (the Rust half of context.s). Re-exported
// flat so `crate::arch::{Context, switch_to}` names them regardless of architecture.
pub use context::{Context, switch_to};
// E3's padding sled (milestone 134); see kernel/src/fastpath_pad.rs.
#[cfg(feature = "fastpath_pad")]
pub use fastpath_pad::fastpath_pad_body;

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
        asm!("csrw sscratch, {}", in(reg) stash_ptr, options(nomem, nostack, preserves_flags));
    };
}

/// Read this hart's per-CPU pointer (the value last handed to [`set_percpu`]).
pub fn percpu() -> usize {
    let tp: usize;
    // SAFETY: reads a general register. No side effects.
    unsafe { asm!("mv {}, tp", out(reg) tp, options(nomem, nostack, preserves_flags)) };
    tp
}

/// **Test-only: does `tp` name the hart we are physically running on?** RISC-V keeps the kernel
/// per-CPU pointer in `tp`, so `cpu::id()` is only right if `tp` is. The independent ground truth is
/// `sscratch`, which points at this hart's [`TrapStash`], is set once per hart by [`set_percpu`], and
/// never migrates. A mismatch means a preempted kernel thread resumed on a different hart still
/// carrying the stale `tp` from its origin hart, the SMP migration bug `trap.s`'s S-mode `tp`
/// handling fixes (DECISIONS §28). The two reads run under masked interrupts so a preemption between
/// them cannot split them across harts and manufacture a false mismatch. The aarch64 twin is a
/// constant `true`: there the per-CPU pointer is `TPIDR_EL1`, a system register the trap frame never
/// carries, so it cannot go stale on migration.
#[cfg(test)]
pub fn percpu_matches_hart() -> bool {
    let was_enabled = crate::arch::interrupts::disable();
    let sscratch: usize;
    // SAFETY: reads a CSR; `sscratch` holds `&TRAP_STASH[hart]` in S-mode (trap.s keeps it so).
    unsafe { asm!("csrr {}, sscratch", out(reg) sscratch, options(nomem, nostack)) };
    // `cpu::id()` reads `tp`; taken here, under the same mask, so both name one instant on one hart.
    let hart_from_tp = crate::cpu::id();
    crate::arch::interrupts::restore(was_enabled);
    let base = TRAP_STASH.as_ptr() as usize;
    let hart_from_sscratch = (sscratch - base) / core::mem::size_of::<TrapStash>();
    hart_from_sscratch == hart_from_tp
}

/// Start a secondary hart. The name is aarch64's (`psci_cpu_on`); the mechanism is the SBI HSM
/// (Hart State Management) extension's `sbi_hart_start(hartid, start_addr, opaque)`. The firmware
/// starts the target hart at `entry` (a physical address) in S-mode with paging off, `a0` = its hart
/// id and `a1` = `context`. Returns the SBI error (0 = success; a hart QEMU did not create, when
/// `-smp` is smaller than `MAX_CPUS`, returns a nonzero error rather than hanging). See boot.s
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

/// Can this machine start a secondary hart at all? Asked once by `smp::bring_up_secondaries`.
///
/// **Always true here, and the asymmetry with aarch64 is the point.** RISC-V has exactly one
/// mechanism: SBI HSM `hart_start`, an `ecall` to the firmware that is already under us by
/// construction (this kernel is entered in S-mode, so something is running in M-mode). There is no
/// conduit to choose and no function id to look up, which is why milestone 100 is an aarch64
/// milestone with a RISC-V half rather than the other way round. Whether the firmware *implements*
/// HSM is a separate question and one `isa::init` already answers: `SBI_REQUIRED` includes it, so a
/// firmware without it stops the boot before this is reached.
pub fn can_start_secondaries() -> bool {
    true
}

/// Print how this machine starts a hart. One line, on every boot, beside the SMP count; the aarch64
/// twin has something to say here because it read `/psci`, and this one says what is fixed.
pub fn print_bring_up_mechanism() {
    crate::println!("  smp: sbi hsm hart_start (the only mechanism RISC-V defines)");
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

/// The SBI RFENCE extension id, "RFNC" in ASCII. Both remote-fence calls below live in it.
const SBI_RFENCE_EID: usize = 0x5246_4E43;

/// **What the RFENCE calls mean by "the whole thing".** SBI defines a remote fence as covering
/// everything when `size` is all-ones (OpenSBI's `SBI_TLB_FLUSH_ALL`), and separately when `start`
/// and `size` are *both* zero. The two are not the same request and the difference matters here:
/// for [`sbi_remote_sfence_vma_asid`], all-ones is the one that reaches
/// `sfence.vma x0, asid` (every address, that ASID), while `0, 0` makes OpenSBI fall back to
/// `sfence.vma` with no operands and throw the entire TLB away on every target hart, which is
/// precisely the sledgehammer milestone 58 removed. Passing the wrong one would still be *correct*
/// and would silently undo the milestone on every other hart.
const SBI_RFENCE_ALL: usize = usize::MAX;

/// **How many remote RFENCEs this boot has issued** (milestone 130's follow-on, bench builds only).
///
/// Every SBI RFENCE is an `ecall` into firmware, so it is the single most expensive thing on an
/// otherwise local TLB path, and whether one is issued at all is decided by a *mask*
/// (`smp::online_harts_mask()`). On a single-hart boot the correct answer is **zero**: there is
/// nobody to shoot down, and both call sites skip the call when the mask names no other hart.
///
/// This counter exists because a reading in notes/benchmarks.md (2026-08-15) inferred that a
/// `map_new` regression was remote RFENCEs fired against an over-reporting mask, and recorded
/// honestly that **nothing had counted a fence**. This counts them.
///
/// `feature = "bench"` rather than always-on: a relaxed increment is cheap but this is the TLB
/// shootdown path, and a diagnostic has no business on it in a shipping kernel. The bench build is
/// its own kernel binary, so the instrument and the shipping path stay separate objects.
#[cfg(feature = "bench")]
static REMOTE_FENCES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record one issued RFENCE. See [`REMOTE_FENCES`].
#[cfg(feature = "bench")]
fn note_remote_fence() {
    REMOTE_FENCES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// Remote RFENCEs issued so far this boot. Bench builds only; see [`REMOTE_FENCES`].
#[cfg(feature = "bench")]
pub fn remote_fence_count() -> u64 {
    REMOTE_FENCES.load(core::sync::atomic::Ordering::Relaxed)
}

/// Shoot down a virtual-address translation on the harts in `hart_mask`, via the SBI RFENCE
/// extension. The firmware IPIs those harts and each executes `sfence.vma start, ...` for the range.
/// RISC-V has no hardware TLB broadcast (aarch64's `tlbi ..., is` does), so a kernel page-table change
/// must be pushed to the other harts this way or a migrated thread faults on a stale translation. See
/// `mmu::flush_tlb`.
pub fn sbi_remote_sfence_vma(hart_mask: usize, start: usize, size: usize) {
    const SBI_REMOTE_SFENCE_VMA_FID: usize = 1;
    #[cfg(feature = "bench")]
    note_remote_fence();
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

/// **Discharge every translation tagged with `asid` on the harts in `hart_mask`**: the remote half
/// of `mmu::flush_asid`, and the instruction that makes an ASID safe to hand to a new address space
/// (milestone 58). Each target hart executes `sfence.vma x0, asid`.
///
/// # Why this has to exist at all
///
/// `sfence.vma` is a **local** instruction. It orders and invalidates for the hart that runs it and
/// says nothing about any other, which is the single largest difference between the two ISAs'
/// TLB maintenance: aarch64's `tlbi aside1is` broadcasts across the inner-shareable domain in
/// hardware and needs no software protocol. So the ASID reuse contract (`crates/asid`: flush, then
/// the number may tag someone else) is one instruction there and a distributed protocol here.
///
/// # What is ordered, and by whom
///
/// The acknowledgement the contract needs is **the return of this call**. SBI's RFENCE functions are
/// synchronous: OpenSBI queues the request, sends the IPI, and spins in `sbi_tlb_sync` until every
/// target hart has drained it, so by the time `ecall` returns no target holds an entry wearing this
/// tag. Linux depends on the same guarantee (its `flush_tlb_mm` does no waiting of its own), which
/// is the reason to believe it rather than a reading of ours.
///
/// The IPI is delivered as an **M-mode** software interrupt, so a target hart with S-mode interrupts
/// masked still services it. That is not a detail: without it, any code that disables interrupts and
/// spins would deadlock the flusher, and the kernel disables interrupts routinely.
///
/// # BUGS
///
/// - **A firmware that implemented RFENCE asynchronously would break this silently**, and S-mode has
///   no way to detect it: the failure is a stale translation on another hart, arbitrarily far from
///   the cause. The SBI spec's wording is "instructs the remote harts to execute", which OpenSBI
///   reads as synchronous and a different implementation might not. `isa::the_firmware_implements_what_the_kernel_calls`
///   checks the extension is present; nothing checks it is synchronous, because nothing can.
/// - **`hart_mask` is a bitmap of hart ids relative to base 0**, so it only reaches harts 0..63 on
///   rv64. Fine for `MAX_CPUS`, wrong for a machine with more harts than that, which is the same
///   limitation `smp::bring_up_secondaries` documents for logical-id-equals-hart-id.
pub fn sbi_remote_sfence_vma_asid(hart_mask: usize, asid: u16) {
    const SBI_REMOTE_SFENCE_VMA_ASID_FID: usize = 2;
    #[cfg(feature = "bench")]
    note_remote_fence();
    // SAFETY: an SBI call. a7/a6 = extension/function, a0 = hart bitmap, a1 = mask base (0), a2/a3 =
    // the address range (all of it), a4 = the ASID. The firmware returns in a0/a1 (ignored).
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_RFENCE_EID,
            in("a6") SBI_REMOTE_SFENCE_VMA_ASID_FID,
            inout("a0") hart_mask => _,
            inout("a1") 0usize => _,
            in("a2") 0usize,
            in("a3") SBI_RFENCE_ALL,
            in("a4") asid as usize,
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
    // The other harts' instruction fetch is not ordered by anything above, and RISC-V has no
    // broadcast form: a thread scheduled onto another hart can fetch stale bytes for code this
    // hart just wrote. TCG never shows this (its icache is perfectly coherent), and the U74 did,
    // on first-silicon day (2026-08-14): init's freshly built child hung on the board and nowhere
    // else. Push fence.i to every other online hart via SBI RFENCE, the mechanism the TLB
    // shootdown already uses one FID over.
    let others = crate::smp::online_harts_mask() & !(1 << crate::cpu::id());
    if others != 0 {
        sbi_remote_fence_i(others);
    }
}

/// Execute `fence.i` on the harts in `hart_mask`, via the SBI RFENCE extension: the cross-hart
/// half of [`sync_icache`], because `fence.i` is hart-local and instruction memory written on one
/// hart is not otherwise ordered against another hart's fetch.
pub fn sbi_remote_fence_i(hart_mask: usize) {
    const SBI_REMOTE_FENCE_I_FID: usize = 0;
    // SAFETY: an SBI call. a7/a6 = extension/function, a0 = hart bitmap, a1 = mask base (0). The
    // firmware returns in a0/a1 (ignored); nothing else is touched.
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_RFENCE_EID,
            in("a6") SBI_REMOTE_FENCE_I_FID,
            inout("a0") hart_mask => _,
            inout("a1") 0usize => _,
            options(nostack),
        );
    }
}
