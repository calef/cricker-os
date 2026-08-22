//! **The client that draws** (milestone 29, the display ladder's rung one).
//!
//! This process paints a pattern into a framebuffer and asks for it to be shown. It holds:
//!
//! - slot 0, a **report** endpoint: its verdict goes to whoever spawned it;
//! - slot 1, a **display** endpoint (WRITE): it CALLs the driver here;
//! - slot 2, an **untyped**: the budget the page tables for its own mappings come out of;
//! - slots 3.., the **surface**: `gfx::SURFACE_FRAMES` `Frame` capabilities, one per page, which it
//!   maps itself (milestone 108; before that the kernel mapped them in at spawn and the client held
//!   nothing that said so).
//!
//! And that is all. It holds no `Virtio` capability, no interrupt, no device mapping, and it cannot
//! name a physical address; it has never heard of virtio-gpu. The pixels are the only memory it
//! shares with anything. So the worst a hostile version of this program can do is draw nonsense: it
//! cannot program a queue, cannot ring a doorbell, and cannot aim a device at a byte it does not
//! already own. That is the separation rung two (the compositor) is built on, and the reason this
//! trivial program is a separate binary rather than a function inside the driver.
//!
//! # What it proves
//!
//! It paints [`gfx_proto::pixel`], a per-coordinate function rather than a fill, then flushes, then
//! **reads the whole surface back and digests it**. The digest and the index of the first wrong pixel
//! go to the report endpoint, where the kernel test compares them against a value it computed itself
//! from the contract. So a pass means the pattern this process wrote is the pattern that was in the
//! frames the device read, byte for byte, and the driver agreed about it from its own address space.
//! See notes/framebuffer-contract.md for what that does and does not prove about the screen.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use gfx_proto as gfx;
use user_rt::{call, exit, send};

/// Capability slots, by convention with `kernel/src/user/display_service.rs`.
const REPORT: u64 = 0;
const DISPLAY: u64 = 1;
/// The untyped this program spends on the page tables the surface needs.
const BUDGET: u64 = 2;
/// The first of `gfx::SURFACE_FRAMES` consecutive slots holding the scanout, a `Frame` per page.
const SURFACE_FRAME: u64 = 3;

/// Where this program puts the surface. **Its choice, not the kernel's**: it holds the frames and
/// maps them (milestone 108).
const SURFACE_VA: u64 = 0x0000_0000_0060_0000;

/// Failure codes, reported in a `0xDEAD_...` word so a failure names its step.
const E_INFO: u64 = 0x01;
const E_GEOMETRY: u64 = 0x02;
const E_FLUSH: u64 = 0x03;
const E_PARTIAL_FLUSH: u64 = 0x04;
/// A surface frame would not map: this program was handed the pixels it could not put anywhere.
const E_SURFACE: u64 = 0x05;

fn px_write(i: usize, v: u32) {
    // SAFETY: `i` is below `gfx::PIXELS`, so this is inside the mapped surface; it is mapped
    // read/write and shared only with the driver.
    unsafe { core::ptr::write_volatile((SURFACE_VA + (i * 4) as u64) as *mut u32, v) };
}

fn px_read(i: usize) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((SURFACE_VA + (i * 4) as u64) as *const u32) }
}

fn die(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    // Exit so the kernel reaps this one-shot client rather than leaving it spinning on a run queue.
    exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_arg0: u64, _arg1: u64, _arg2: u64) -> ! {
    // **Put the surface in our own address space** (milestone 108). One `MAP` per page, because a
    // `Frame` names a page: the run is contiguous in physics and in virtual memory, and the object
    // has no way to say so. See notes/frames.md's BUGS.
    for k in 0..gfx::SURFACE_FRAMES as u64 {
        if !user_rt::map_frame(SURFACE_FRAME + k, SURFACE_VA + k * 4096, true, BUDGET) {
            die(E_SURFACE);
        }
    }

    // Ask the driver how big the surface is, rather than only trusting the compile-time constants.
    // Rung two hands a client a surface it did not choose, so the runtime question is the one that
    // will still make sense then.
    let (r0, geometry) = call(DISPLAY, gfx::req(gfx::display::INFO, 0), 0);
    if r0 as i64 != 0 {
        die(E_INFO);
    }
    let (w, h) = ((geometry & 0xffff_ffff) as u32, (geometry >> 32) as u32);
    if w != gfx::WIDTH || h != gfx::HEIGHT {
        die(E_GEOMETRY);
    }

    // Paint. Every pixel is a function of its coordinates, so no accident produces this buffer:
    // zeroed memory, a fill, a loop that never advanced, and a transposed or shifted write all fail
    // the digest below (crates/gfx_proto asserts exactly those properties on the host).
    for y in 0..h {
        for x in 0..w {
            px_write((y * w + x) as usize, gfx::pixel(x, y));
        }
    }

    // Show it. One whole-surface damage rectangle: the driver copies the pixels into the host
    // resource and puts that resource on the scanout, and does not reply until the device has
    // completed both.
    let (r0, _) = call(
        DISPLAY,
        gfx::req(gfx::display::FLUSH, gfx::rect(0, 0, w, h)),
        0,
    );
    if r0 as i64 != 0 {
        die(E_FLUSH);
    }

    // A rectangle that runs one pixel off the right edge must be **refused, not clamped**: a clamp
    // would hide a client's coordinate bug, and this client is the only thing that can check the
    // driver keeps that promise. Cheap, and it is the one negative case the contract has.
    let (r0, _) = call(
        DISPLAY,
        gfx::req(gfx::display::FLUSH, gfx::rect(1, 0, w, h)),
        0,
    );
    if r0 as i64 != gfx::EINVAL {
        die(E_PARTIAL_FLUSH);
    }

    // Read the whole surface back and digest it. This is the client half of the pixel proof: the
    // bytes are read after the device consumed them, through this process's own mapping, and the
    // digest is position sensitive, so a surface that came back shifted, mirrored, truncated, or
    // stale cannot match.
    let digest = gfx::checksum(px_read);
    let mismatch = gfx::first_mismatch(px_read).map_or(gfx::NO_MISMATCH, |i| i as u64);
    send(REPORT, gfx::status::PAINTED, digest, mismatch);

    // One-shot: reported, so exit and be reaped rather than spin.
    exit();
}

user_rt::panic_handler!();
