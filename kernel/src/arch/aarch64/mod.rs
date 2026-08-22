//! aarch64 support.
//!
//! Assembly, system registers, and CPU-specific behaviour live here and nowhere
//! else. When the Raspberry Pi port happens, this is the module that gets a
//! sibling, and everything above `arch::` should be untouched. See
//! notes/portability.md and DECISIONS.md §4.

use core::arch::global_asm;

use aarch64_cpu::registers::TPIDR_EL1;
use tock_registers::interfaces::{Readable, Writeable};

use crate::println;

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

// The arm64 Image header. `_start` lands at byte 0 of the image, which is where QEMU
// begins executing. It does nothing but branch to `_boot`.
global_asm!(include_str!("image_header.s"));

// The real entry point.
global_asm!(include_str!("boot.s"));

// The exception vector table. VBAR_EL1 will point here once `init` runs.
global_asm!(include_str!("vectors.s"));

// The context switch, and where a new thread begins. Milestone 6.
global_asm!(include_str!("context.s"));

/// Point `TPIDR_EL1` at this core's per-CPU block.
///
/// `TPIDR_EL1` is a scratch system register the architecture reserves for software's own use;
/// the kernel keeps a per-core pointer in it and reads it back in one `mrs`. This is the
/// standard aarch64 per-CPU base (Linux uses `TPIDR_EL1` identically). The portable side of
/// this lives in `kernel/src/cpu.rs`; only the register touch belongs here (DECISIONS.md §4).
pub fn set_percpu(ptr: usize) {
    TPIDR_EL1.set(ptr as u64);
}

/// The logical id of the core the kernel boots on. On aarch64's `virt` the boot core is MPIDR
/// affinity 0, so it is always 0; the SMP bring-up starts every *other* core. (RISC-V's boot hart is
/// not guaranteed to be 0, so its `arch::boot_cpu_id` reads the actual hart id.)
pub fn boot_cpu_id() -> usize {
    0
}

/// Read this core's per-CPU pointer back. One instruction.
pub fn percpu() -> usize {
    TPIDR_EL1.get() as usize
}

/// **Test-only: does the per-CPU pointer name the hart we run on?** Always `true` on aarch64: the
/// pointer lives in `TPIDR_EL1`, a system register the trap frame never saves or restores, so a
/// thread migrating between cores keeps whatever `TPIDR_EL1` the destination core set at boot. The
/// RISC-V twin has to check (its per-CPU pointer is `tp`, a general register that CAN ride a stale
/// trap frame across a migration, DECISIONS §28); this exists so a portable test can call it on both.
#[cfg(test)]
pub fn percpu_matches_hart() -> bool {
    true
}

/// One PSCI call, on the conduit the machine stated. The instruction has to be a literal in `asm!`,
/// and the two forms differ in nothing else, so the body is written once here rather than twice.
///
/// Per SMCCC, x0-x3 are results and x4-x17 are scratch, so all are marked clobbered; x18-x30 are
/// preserved by the callee (the firmware).
macro_rules! psci_call {
    ($conduit:literal, $func:expr, $a1:expr, $a2:expr, $a3:expr) => {{
        let ret: i64;
        // SAFETY: a defined firmware call on the conduit `/psci` named. It starts the target core
        // and returns a status in x0; it does not touch our memory.
        unsafe {
            core::arch::asm!(
                $conduit,
                inout("x0") $func => ret,
                inout("x1") $a1 => _,
                inout("x2") $a2 => _,
                inout("x3") $a3 => _,
                lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
                lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
                lateout("x16") _, lateout("x17") _,
                options(nostack),
            );
        }
        ret
    }};
}

/// What [`psci_cpu_on`] returns when the machine never told us how to make the call. Not a PSCI
/// error code: PSCI's own space runs from -1 to -9, and inventing a tenth would be a lie about who
/// answered. Nothing reaches this in the normal path, because `smp::bring_up_secondaries` asks
/// [`can_start_secondaries`] first.
pub const PSCI_NOT_DISCOVERED: i64 = i64::MIN;

/// PSCI `CPU_ON`: start a secondary core. Returns 0 on success, a negative error otherwise.
///
/// PSCI (Power State Coordination Interface) is the firmware call standard for turning ARM cores on
/// and off. Arguments follow the SMC calling convention: the function id in x0, then the target
/// core's `MPIDR_EL1[39:0]` affinity value, the PHYSICAL entry address it begins at (MMU off), and a
/// context word that arrives in the new core's x0.
///
/// **The conduit and the function id are read from the machine** (milestone 100). They used to be
/// `hvc #0` and `0xC400_0003`, compiled in, which was QEMU `virt`'s answer and nothing else's. The
/// `/psci` node states both: `method` says whether the firmware listens at EL2 (`hvc`) or EL3
/// (`smc`), and `cpu_on` publishes the id, which PSCI 0.1 machines had to because the standard id
/// space did not exist yet. `isa::init` parses the node; this reads what it recorded.
///
/// # BUGS
///
/// - **The `smc` path has never executed.** `crates/isa`'s host tests decode a real QEMU dump that
///   states `smc` (`virt,virtualization=on`), so the *reading* is exercised on a genuine tree; that
///   configuration enters the kernel at EL2 and this kernel expects EL1, so nothing here boots it.
///   The `hvc` path is exercised on every test run. Choosing the wrong one of the two is an
///   undefined-instruction trap rather than an error code, which is why it is read rather than
///   defaulted, and why this note is here rather than in a tracker.
pub fn psci_cpu_on(target_mpidr: u64, entry: u64, context: u64) -> i64 {
    let Some((conduit, func)) = isa::psci() else {
        return PSCI_NOT_DISCOVERED;
    };
    let func = func as u64;
    match conduit {
        ::isa::aarch64::Conduit::Hvc => psci_call!("hvc #0", func, target_mpidr, entry, context),
        ::isa::aarch64::Conduit::Smc => psci_call!("smc #0", func, target_mpidr, entry, context),
    }
}

/// Can this machine start a secondary core at all? Asked once by `smp::bring_up_secondaries`.
///
/// False on a machine whose device tree has no `/psci` node, or one whose node did not say enough
/// to make the call. Both are real: a uniprocessor board has no reason to carry the node, and a
/// board whose cores are released from a spin-table carries a different mechanism instead.
pub fn can_start_secondaries() -> bool {
    isa::psci().is_some()
}

/// Print how this machine starts a core, or why it cannot. One line, on every boot, beside the SMP
/// count; the same discipline milestone 60's ISA summary set.
pub fn print_bring_up_mechanism() {
    let Some(psci) = isa::psci_record() else {
        println!("  smp: the device tree has no /psci node, so no core can be started here");
        return;
    };
    match (psci.conduit, psci.cpu_on) {
        (Some(conduit), Some(cpu_on)) => println!(
            "  smp: psci over {}, CPU_ON {cpu_on:#010x} ({}, from the device tree)",
            conduit.name(),
            if psci.cpu_on_from_property {
                "the node's own id"
            } else {
                "the standard id"
            },
        ),
        (None, _) => {
            println!(
                "  smp: /psci states no usable `method`, and hvc-versus-smc cannot be guessed"
            );
        }
        (_, None) => {
            println!("  smp: /psci is 0.1 and published no CPU_ON id, so there is no call to make");
        }
    }
}

/// Bring the CPU into a state where the kernel can safely run.
///
/// Right now that means one thing: install the exception vectors, so that a fault
/// produces a report instead of a silent death. Note the ordering constraint in
/// `main.rs`: the console has to come up first, because the fault handler's whole
/// job is to *print*.
pub fn init() {
    exceptions::init();
}

/// Park this core forever.
///
/// **`wfi`, not `wfe`, and the difference is not academic.**
///
/// `wfe` waits for an *event*: an `sev` from another core, or a lock release. QEMU's
/// emulation treats it as little more than a hint, so `loop { wfe() }` keeps translating
/// and executing, and a halted kernel burns **99.7% of a host CPU core**. We discovered
/// this the way you'd expect: eleven abandoned QEMU processes cooking the laptop overnight
/// at a combined 729%.
///
/// `wfi` waits for an *interrupt*, and QEMU implements it as an actual vCPU halt: the host
/// thread sleeps. An idle kernel becomes genuinely idle.
///
/// It is also the more correct instruction for what we mean. We are not waiting for an
/// event from a sibling core. We are idling until something interrupts us, of which there
/// is currently nothing, which is exactly the point.
pub fn halt() -> ! {
    loop {
        aarch64_cpu::asm::wfi();
    }
}

/// Wait for one interrupt, then return. **The idle thread's whole body.**
///
/// When every other thread is blocked (all waiting on I/O, say), the scheduler runs the idle
/// thread, which parks the CPU here until *something* interrupts: the timer, or the device a
/// blocked driver is waiting on. The handler may wake a thread; when `wfi` returns, the idle
/// thread yields and the scheduler picks up whatever became runnable.
///
/// `wfi`, not `wfe`, for the reason in `halt`: QEMU implements `wfi` as a real vCPU halt (the
/// host thread sleeps), so an idle kernel is genuinely idle. See notes/scheduler and CLAUDE.md.
pub fn wait_for_interrupt() {
    aarch64_cpu::asm::wfi();
}

/// This core's current stack pointer. Reading `sp` is arch-specific (rule 1), so the stack-overflow
/// canary check (stack.rs) goes through here rather than embedding an `asm!` in portable code.
pub fn current_sp() -> u64 {
    let sp: u64;
    // SAFETY: reads a register. No side effects.
    unsafe {
        core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    };
    sp
}

/// `SPSel`, the register that says which stack pointer the name `sp` currently means at EL1:
/// bit 0 set is `SP_EL1`, clear is `SP_EL0`. Reading a system register is arch-specific (rule 1),
/// so the trap-frame sanity test (`el1_runs_on_sp_el1`, user/tests.rs) goes through here rather
/// than embedding an `asm!` in portable code, the same shape as [`current_sp`]. Test-only because
/// nothing but that test asks, and aarch64-only on purpose: RISC-V does not bank the stack pointer
/// by privilege level, so there is no analogous register to read (notes/riscv-parity-scope.md).
#[cfg(test)]
pub fn spsel() -> u64 {
    let spsel: u64;
    // SAFETY: reading SPSel has no side effects.
    unsafe { core::arch::asm!("mrs {}, spsel", out(reg) spsel, options(nostack, nomem)) };
    spsel
}

/// Order all prior normal-memory writes before the next device (MMIO) write.
///
/// The kernel builds a virtio descriptor ring in normal memory, then rings the device with an MMIO
/// write. The device is a **separate observer** that reads that ring by DMA, so the ring stores
/// must be globally visible before the "go" signal lands, or the device reads stale bytes. A `dsb`
/// guarantees it. On QEMU DMA is coherent and the notify is processed synchronously, so this is
/// effectively free; on real hardware it is load-bearing. Arch-specific by rule 1, so it lives here
/// rather than in the transport (kernel/src/virtio.rs).
pub fn dma_wmb() {
    aarch64_cpu::asm::barrier::dsb(aarch64_cpu::asm::barrier::SY);
}

/// Make the instruction fetcher aware of code we just wrote as data, over `[va, va+len)`.
///
/// The D-cache and the I-cache are **not coherent** on aarch64. This is not a QEMU quirk, it is the
/// architecture: the assumption is that writing code is rare and paying for coherence on every store
/// is not worth it, so the loader has to say so explicitly, and every loader on every ARM machine
/// does exactly this. `dc cvau` cleans the data cache to the point of unification, `ic ivau`
/// invalidates the instruction cache, and the barriers make the two agree. Get it wrong and the CPU
/// executes whatever was in that page *before* the program landed there.
///
/// Arch-specific by rule 1 (RISC-V does this with a single `fence.i`), so it lives here rather than
/// in the ELF loader (kernel/src/user.rs). See notes/riscv-port.md, leak #3.
pub fn sync_icache(va: u64, len: usize) {
    const LINE: u64 = 64; // conservative: the real size is in CTR_EL0

    let mut p = va & !(LINE - 1);
    let end = va + len as u64;

    // SAFETY: cache maintenance on a mapped, readable range is always sound.
    unsafe {
        while p < end {
            core::arch::asm!("dc cvau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", options(nostack));

        let mut p = va & !(LINE - 1);
        while p < end {
            core::arch::asm!("ic ivau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", "isb", options(nostack));
    }
}
