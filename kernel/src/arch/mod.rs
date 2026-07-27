//! The hardware abstraction boundary.
//!
//! Everything below this module is architecture-specific. Everything above it
//! should be portable. The rest of the kernel talks to the hardware only through
//! what is re-exported here.
//!
//! This is the single most important structural rule in the codebase, and the one
//! that is easiest to erode by accident. If you find yourself writing `asm!` or
//! touching a system register outside `arch/`, that's the bug. See
//! notes/portability.md.

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

// The second architecture (milestone 20, notes/riscv-port.md). Its presence here, dispatched by
// the same `cfg` and re-exported flat through the same surface, is the proof that rule #1 holds: a
// new ISA is a new directory, not a diff across the kernel.
#[cfg(target_arch = "riscv64")]
mod riscv64;

#[cfg(target_arch = "riscv64")]
pub use riscv64::*;
