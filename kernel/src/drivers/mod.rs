//! Device drivers.
//!
//! A driver never reaches into a kernel global. It is handed what it needs
//! (a base address, later a DMA allocator, later an interrupt registration) and
//! knows nothing about the rest of the kernel. That rule is cheap now and is what
//! keeps the microkernel door open later. See DECISIONS.md §4.

pub mod pl011;
