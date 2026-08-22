//! **A window: a client of the compositor** (milestone 33, the display ladder's rung two).
//!
//! One program, several roles, because the roles differ by one act each and sharing the honest path
//! is what makes an attack a fair test rather than a different program failing for its own reasons
//! (the same reason `display` carries its own escape role).
//!
//! What an honest window holds:
//!
//! - slot 0, a **report** endpoint: its verdict goes to whoever spawned it;
//! - slot 1, the **doorbell** (WRITE): where it rings the compositor;
//! - slot 2, an **input** endpoint (READ), *only if it was granted one*. Holding this is what makes a
//!   client eligible for focus; a client without it cannot be sent a keystroke by anyone, and its
//!   attempt to receive on that slot is refused by the kernel with `NoSuchSlot`;
//! - a **control page** and a **surface**: its own geometry, its own damage rectangle, its own pixels.
//!
//! And that is all. It cannot name another client, another surface, or the screen. There is no window
//! id in any request it can make (the protocol takes none), so there is nothing to forge; and the
//! pixels of its neighbours are not in its address space, so there is nothing to reach.
//!
//! # What the roles prove
//!
//! - [`ROLE_PROBE_INPUT`]: asks the kernel to receive on the input slot it was **not** granted. The
//!   refusal is `abi::Error::NoSuchSlot` (-1), "there is nothing there", which is what
//!   no-ambient-authority reads like from the inside: not a permission error from a server that
//!   consulted a list.
//! - [`ROLE_PROBE_NEIGHBOUR`]: writes at the address where its neighbour's pixels really do live, one
//!   page past its own grant. The kernel hands it that address, so it is a perfectly informed
//!   attacker; it faults anyway, because the boundary is the mapping and not the layout.
//! - [`ROLE_PROBE_SCREEN`]: reads the composed screen. It drew into that screen, and it cannot see it:
//!   there is no ambient display here, and a screenshot is a capability.
//! - [`ROLE_CAPTURE`]: was granted a read-only mapping of the screen and of the window list, so it
//!   *can* enumerate and screenshot, and it reports a digest the kernel checks against the picture it
//!   computed itself. Then it tries to write the screen and dies, which is the other half of the
//!   grant: a screenshot tool cannot deface what it can read.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use compositor::proto::{ctl, wlist};
use compositor::status;
use user_rt::{call, exit, invoke, recv_cap, send};

/// Capability slots, by convention with `kernel/src/user/compositor_service.rs`.
const REPORT: u64 = 0;
const DOORBELL: u64 = 1;
const INPUT: u64 = 2;

/// Where the kernel maps what this process is given. Must match `compositor_service`. **Every client
/// uses the same addresses**, which is the point: two clients' surfaces are the same virtual address
/// in different address spaces, so "the neighbour's surface" is not somewhere a client can reach by
/// guessing.
const CTL_VA: u64 = 0x0000_0000_0060_0000;
const SURFACE_VA: u64 = 0x0000_0000_0061_0000;
/// The screen and the window list, read-only, and **only** for a client granted them.
const SCREEN_VA: u64 = 0x0000_0000_0070_0000;
const WLIST_VA: u64 = 0x0000_0000_0078_0000;

/// Roles, as a bitmask in `arg0`. A plain window is 0.
const ROLE_INPUT: u64 = 1 << 0;
const ROLE_PROBE_INPUT: u64 = 1 << 1;
const ROLE_PROBE_NEIGHBOUR: u64 = 1 << 2;
const ROLE_PROBE_SCREEN: u64 = 1 << 3;
const ROLE_CAPTURE: u64 = 1 << 4;
const ROLE_VICTIM: u64 = 1 << 5;
const ROLE_SMALL_DAMAGE: u64 = 1 << 6;

/// What a refusal report is about, in the second word of [`status::WIN_REFUSED`].
const WHAT_INPUT: u64 = 1;

/// Failure codes, in a `0xDEAD_...` word so a failure names its step.
const E_MAGIC: u64 = 0x01;
const E_GEOMETRY: u64 = 0x02;
const E_HELLO: u64 = 0x03;
const E_COMMIT: u64 = 0x04;
/// The spawner refused a report round trip. Defensive: the kernel-side test always replies 0, so this
/// firing means the scenario was wired differently than this role expects.
const E_REPORT: u64 = 0x05;

fn rd32(va: u64) -> u32 {
    // SAFETY: inside a page the kernel mapped into this process at spawn.
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

fn wr32(va: u64, v: u32) {
    // SAFETY: as above, and the page is writable (the control page and the surface both are).
    unsafe { core::ptr::write_volatile(va as *mut u32, v) }
}

fn px_write(i: usize, v: u32) {
    // SAFETY: `i` is below the surface's pixel count, so this is inside the mapped surface.
    unsafe { core::ptr::write_volatile((SURFACE_VA + (i * 4) as u64) as *mut u32, v) }
}

fn px_read(i: usize) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((SURFACE_VA + (i * 4) as u64) as *const u32) }
}

fn die(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    exit();
}

/// Ring the doorbell. Content-free by design: the compositor learns what changed from our control
/// page, not from this message, because a shared endpoint's messages cannot be trusted.
fn ring(op: u64) -> i64 {
    let (r0, _) = call(DOORBELL, compositor::proto::req(op, 0), 0);
    r0 as i64
}

/// Set our damage rectangle and bump our sequence, then commit. The compositor reads the rectangle as
/// untrusted input and clips it to our surface; the reply comes after the pixels are on the screen.
fn commit(seq: &mut u32, damage: compositor::Rect) {
    wr32(CTL_VA + ctl::DAMAGE_X, damage.x as u32);
    wr32(CTL_VA + ctl::DAMAGE_Y, damage.y as u32);
    wr32(CTL_VA + ctl::DAMAGE_W, damage.w);
    wr32(CTL_VA + ctl::DAMAGE_H, damage.h);
    *seq += 1;
    // The sequence bump must be visible after the pixels and the rectangle it describes, or the
    // compositor could composite a frame we have not finished writing. A release fence, portable
    // across both ISAs, rather than arch-specific asm in a userspace program.
    //
    // PAIR: `serve_frame` in user/src/compositor.rs, which reads `SEQ` and then the four damage
    // fields. Milestone 43's audit found that reader had no fence at all (finding 7), and **this is
    // the one publish in the subsystem the IPC rendezvous does not cover**: the doorbell is shared,
    // so `serve_frame` rescans *every* client's control page on any client's `COMMIT`, and only the
    // caller is blocked. A second window mid-`commit` on another core is read with nothing ordering
    // the compositor's own loads. See notes/memory-ordering.md.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    wr32(CTL_VA + ctl::SEQ, *seq);
    if ring(compositor::proto::COMMIT) != 0 {
        die(E_COMMIT);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, neighbour_va: u64, _arg2: u64) -> ! {
    // `HELLO` first, for one reason: its reply cannot arrive until the compositor is serving the
    // doorbell, and the compositor publishes every control page before it starts serving. So the
    // rendezvous *is* the synchronization, and no client polls for a valid control page.
    if ring(compositor::proto::HELLO) != 0 {
        die(E_HELLO);
    }

    if rd32(CTL_VA + ctl::MAGIC) != ctl::MAGIC_VALUE {
        die(E_MAGIC);
    }
    let id = rd32(CTL_VA + ctl::ID);
    let w = rd32(CTL_VA + ctl::WIDTH);
    let h = rd32(CTL_VA + ctl::HEIGHT);
    // We are *told* our geometry rather than assuming it: at rung two the compositor really does
    // choose the size, and a client that assumed the constants would paint the wrong shape the first
    // time a scene changed. It is still checked against the largest surface the contract allows,
    // because a control page claiming more than that describes memory we were not granted, and the
    // first thing that would happen is a fault while painting our own window.
    // **Checked arithmetic, because `w * h * 4` in `u32` wraps and the check is against values
    // from a shared page** (milestone 43, notes/shared-page-audit.md finding 4). `w = h = 0x10000`
    // multiplied out to exactly 0, sailed through the bound it was written to fail, and the paint
    // loop below would then write 2^32 pixels past a two-frame surface. `display_terminal.rs`
    // validates the same geometry by division for the same reason; this is the multiplication
    // spelled so it cannot lie.
    let bytes = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(4))
        .unwrap_or(u32::MAX);
    if w == 0 || h == 0 || bytes > compositor::MAX_SURFACE_BYTES {
        die(E_GEOMETRY);
    }

    // Paint. Every pixel is a function of the coordinates and of *which window we are*, so a surface
    // painted by the wrong client, at the wrong offset, or not at all is visibly wrong (crates/compositor
    // asserts those properties on the host).
    for y in 0..h {
        for x in 0..w {
            px_write((y * w + x) as usize, compositor::window_pixel(id, x, y));
        }
    }

    let mut seq = 0u32;
    commit(&mut seq, compositor::Rect::new(0, 0, w, h));

    // Read our own surface back and digest it: the client-side witness that what we painted is what
    // is in the frames, taken after the compositor read them.
    let digest = compositor::surface_checksum(w, h, px_read);

    // --- The refusal that needs no attack: ask for something we hold no capability for. ---
    if role & ROLE_PROBE_INPUT != 0 {
        // SAFETY: `svc`/`ecall`. The slot is empty, so the kernel refuses; nothing happens.
        let r = unsafe { invoke(INPUT, abi::endpoint::RECV, 0, 0, 0) };
        send(REPORT, status::WIN_REFUSED, r as u64, WHAT_INPUT);
    }

    if role & ROLE_VICTIM != 0 {
        // Report the digest and **wait**: the kernel replies once the attacker beside us has done its
        // worst, and we then re-read our own surface. A pattern that survives an attack is the witness
        // that matters, and taking it from inside the victim's own address space is what makes it a
        // witness rather than an inference.
        let (r0, _) = call(REPORT, status::WIN_PAINTED, digest);
        if r0 as i64 != 0 {
            die(E_REPORT);
        }
        let after = compositor::surface_checksum(w, h, px_read);
        send(REPORT, status::WIN_INTACT, after, digest);
        exit();
    }

    send(REPORT, status::WIN_PAINTED, digest, id as u64);

    if role & ROLE_CAPTURE != 0 {
        // We hold a read-only mapping of the screen and of the window list. That mapping IS the
        // screenshot capability and the enumeration capability; there is no verb for either, which is
        // why this block calls nothing and asks nobody. After our own commit, so what we see includes
        // our own window and is a complete screen rather than a half-composed one.
        let shot = compositor::surface_checksum(compositor::SCREEN_W, compositor::SCREEN_H, |i| {
            // SAFETY: inside the read-only screen mapping the kernel gave this role.
            unsafe { core::ptr::read_volatile((SCREEN_VA + (i * 4) as u64) as *const u32) }
        });
        let count = rd32(WLIST_VA + wlist::COUNT);
        let focused = rd32(WLIST_VA + wlist::FOCUSED);
        send(
            REPORT,
            status::WIN_CAPTURED,
            shot,
            focused as u64 | ((count as u64) << 32),
        );
    }

    if role & ROLE_SMALL_DAMAGE != 0 {
        // A second frame with a *small* damage rectangle, to prove a one-window redraw does not cost a
        // whole screen. The kernel poisons the screen outside this rectangle between the two commits;
        // the poison must survive.
        let (r0, _) = call(REPORT, status::WIN_PAINTED, digest);
        if r0 as i64 != 0 {
            die(E_REPORT);
        }
        commit(&mut seq, compositor::SMALL_DAMAGE);
        send(REPORT, status::WIN_PAINTED, digest, seq as u64);
    }

    // --- The probes that are expected to be fatal. Reported first, so a test knows the attempt
    // happened even though nothing can report afterwards. ---
    if role & ROLE_PROBE_NEIGHBOUR != 0 {
        send(REPORT, status::WIN_PROBING, neighbour_va, 0);
        // SAFETY: deliberately not safe. This is the milestone's central claim under test: the
        // address is where a neighbour's pixels physically are, one page past our own grant, and it is
        // not mapped here. The store must fault. If it does not, the report below fires and the test
        // fails loudly rather than passing quietly.
        unsafe { core::ptr::write_volatile(neighbour_va as *mut u32, 0xBAD0_BAD0) };
        // SAFETY: as above; only reached if the write did not fault, which is a broken system.
        let seen = unsafe { core::ptr::read_volatile(neighbour_va as *const u32) };
        send(REPORT, status::WIN_ESCAPED, neighbour_va, seen as u64);
    }

    if role & ROLE_PROBE_SCREEN != 0 {
        send(REPORT, status::WIN_PROBING, SCREEN_VA, 0);
        // SAFETY: deliberately not safe. We painted into this screen and we are not mapped to read it.
        let seen = unsafe { core::ptr::read_volatile(SCREEN_VA as *const u32) };
        send(REPORT, status::WIN_ESCAPED, SCREEN_VA, seen as u64);
    }

    if role & ROLE_CAPTURE != 0 {
        // The screen mapping is read-only, so a screenshot tool cannot deface the screen. Last,
        // because it is fatal.
        send(REPORT, status::WIN_PROBING, SCREEN_VA, 1);
        // SAFETY: deliberately not safe; a write to a read-only mapping must fault.
        unsafe { core::ptr::write_volatile(SCREEN_VA as *mut u32, 0xBAD0_BAD0) };
        send(REPORT, status::WIN_ESCAPED, SCREEN_VA, 1);
    }

    if role & ROLE_INPUT != 0 {
        // Focus routing: we hold an input endpoint, so keystrokes can reach us. A client without this
        // capability cannot be sent one, however the compositor feels about it.
        let mut count = 0u64;
        loop {
            let (w0, reply_slot, bytes) = recv_cap(INPUT);
            // Answer first: the compositor is blocked in CALL, the terminal contract's driver-half
            // rendezvous, and it is the flow control for a fast source.
            // SAFETY: `svc`/`ecall`; the kernel validated the Reply capability and consumes it.
            unsafe { invoke(reply_slot, abi::reply::REPLY, 0, 0, 0) };
            // Clamp to the eight bytes one word carries. `proto::len` is a full 32-bit field, so
            // an unclamped count shifts past 63 at `k == 8` and spins up to 2^32 times.
            // `display_terminal.rs` and `line_editor.rs` both clamp here; this was the one that
            // did not (milestone 43, notes/shared-page-audit.md finding 4).
            let n = line_editor::proto::len(w0).min(8);
            for k in 0..n {
                count += 1;
                send(REPORT, status::WIN_INPUT, (bytes >> (8 * k)) & 0xff, count);
            }
        }
    }

    exit();
}

user_rt::panic_handler!();
