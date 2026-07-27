//! Device drivers.
//!
//! A driver never reaches into a kernel global. It is handed what it needs
//! (a base address, later a DMA allocator, later an interrupt registration) and
//! knows nothing about the rest of the kernel. That rule is cheap now and is what
//! keeps the microkernel door open later. See DECISIONS.md §4.

// The GIC interrupt-controller driver. Still un-gated: portable code (sched, smp, syscall, user)
// names `drivers::gic` directly, an interrupt-controller coupling the RISC-V port must resolve at
// the traps step (its PLIC analog). It is pure MMIO Rust, so it compiles on riscv and is simply dead
// there until then. Gating it, and abstracting that coupling, is the interrupts step's work.
pub mod gic;

// The PL011 UART, aarch64's `virt` console. Used only by the console (via a compile-time alias), so
// it gates cleanly. RISC-V's `virt` has an NS16550 instead.
#[cfg(target_arch = "riscv64")]
pub mod ns16550;
#[cfg(target_arch = "aarch64")]
pub mod pl011;
