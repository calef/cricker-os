//! aarch64 support.
//!
//! Assembly, system registers, and CPU-specific behaviour live here and nowhere
//! else. When the Raspberry Pi port happens, this is the module that gets a
//! sibling, and everything above `arch::` should be untouched. See
//! notes/portability.md and DECISIONS.md §4.

use aarch64_cpu::registers::TPIDR_EL1;
use core::arch::global_asm;
use tock_registers::interfaces::{Readable, Writeable};

pub mod exceptions;
pub mod interrupts;
pub mod mmu;
pub mod semihosting;
pub mod timer;

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

/// Read this core's per-CPU pointer back. One instruction.
pub fn percpu() -> usize {
    TPIDR_EL1.get() as usize
}

/// PSCI `CPU_ON`: start a secondary core. Returns 0 on success, a negative PSCI error otherwise.
///
/// PSCI (Power State Coordination Interface) is the firmware call standard for turning ARM cores
/// on and off. On QEMU `virt` the conduit is `hvc` (the `/psci` node's `method`), so we trap to
/// the emulated firmware with `hvc #0`. Arguments follow the SMC calling convention: the function
/// id in x0, then the target core's MPIDR, the PHYSICAL entry address it begins at (MMU off), and
/// a context word that arrives in the new core's x0. `0xC400_0003` is `PSCI_CPU_ON` (64-bit).
///
/// TODO(portability): the conduit (`hvc` vs `smc`), the function id, and the CPU list all live in
/// the device tree (`/psci`, `/cpus`). A portable version reads them instead of hardcoding QEMU
/// virt's values, the way we insist everywhere else that the machine describe itself. See
/// notes/device-tree.md and DECISIONS.md §11.
pub fn psci_cpu_on(target_mpidr: u64, entry: u64, context: u64) -> i64 {
    const PSCI_CPU_ON: u64 = 0xC400_0003;
    let ret: i64;
    // SAFETY: a defined firmware call. It starts the target core and returns a status in x0; it
    // does not touch our memory. Per SMCCC, x0-x3 are results and x4-x17 are scratch, so all are
    // marked clobbered; x18-x30 are preserved.
    unsafe {
        core::arch::asm!(
            "hvc #0",
            inout("x0") PSCI_CPU_ON => ret,
            inout("x1") target_mpidr => _,
            inout("x2") entry => _,
            inout("x3") context => _,
            lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
            lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
            lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
            lateout("x16") _, lateout("x17") _,
            options(nostack),
        );
    }
    ret
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
