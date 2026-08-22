//! The virtio-blk driver as its own binary (parity C).
//!
//! On aarch64 the block driver is a *role* of the PL011-wired `hello` multiplexer (milestone 9
//! predates the dedicated-binary split). This is its portable analog, the 19f pattern: the whole
//! driver logic is the shared `virtio` module, and this file is only the skeleton a dedicated
//! binary needs (an `_start` that dispatches the role, a panic handler, the runtime re-exports).
//! The kernel's `virtio_service` spawns it with the same role numbers and the same capability
//! slots as hello's roles, so the kernel side is byte-identical across the two shapes.
//!
//! The three roles are the honest driver and the two attackers the DMA-confinement tests spawn
//! (a descriptor aimed at kernel memory, and the indirect-descriptor escape). The attackers ride
//! in the same binary for the same reason they ride in hello: they differ from the honest driver
//! by one descriptor, and sharing the setup code is what makes the attack a fair test.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as "always
//! right, and always the majority". An abbreviation is allowed where it is the ordinary name of the
//! thing, which `blk` is.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

// The virtio module names `crate::{check, invoke, send}`; in `hello` those are its helpers, here
// they are `user_rt`'s (same signatures) plus the local `check`.
pub use user_rt::{invoke, send};

/// Role numbers, matching `kernel/src/user/virtio_service.rs` (and hello's dispatch).
const VIRTIO_BLK: u64 = 3;
const VIRTIO_ATTACK: u64 = 8;
const VIRTIO_ATTACK_INDIRECT: u64 = 13;
const VIRTIO_BLK_WRITE: u64 = 30;
const VIRTIO_BLK_WRITE_ABANDON: u64 = 31;
const VIRTIO_NET: u64 = 40;
/// The block server (milestone 32 phase 2): serves blocks over blk IPC to the FS server.
const VIRTIO_BLK_SERVER: u64 = 32;

/// A failed sanity check is a fault, not a wrong answer: panic, and the handler traps.
pub fn check(ok: bool) {
    if !ok {
        panic!();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, _arg2: u64) -> ! {
    match role {
        VIRTIO_BLK => virtio::run(dma_phys),
        VIRTIO_ATTACK => virtio::run_attack(dma_phys),
        VIRTIO_ATTACK_INDIRECT => virtio::run_attack_indirect(dma_phys),
        VIRTIO_BLK_WRITE => virtio::run_write(dma_phys),
        VIRTIO_BLK_WRITE_ABANDON => virtio::run_write_abandon(dma_phys),
        VIRTIO_NET => virtio::run_net(dma_phys),
        VIRTIO_BLK_SERVER => virtio::run_blk_server(dma_phys),
        _ => panic!(),
    }
}

user_rt::panic_handler!();
