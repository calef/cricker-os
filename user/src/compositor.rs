//! **The compositor: one screen, several mutually distrusting clients** (milestone 33, the display
//! ladder's rung two).
//!
//! This process is rung one's *client*, unchanged at that seam: it holds the display endpoint and the
//! scanout frames exactly as `painter` did, and `display` cannot tell the difference. What it adds is
//! everything above the seam, and it holds these capabilities and nothing else:
//!
//! - slot 0, a **report** endpoint: how it tells its spawner it came up;
//! - slot 1, the **display** endpoint (WRITE): `gfx FLUSH(damage)` to whoever serves the screen;
//! - slot 2, the **doorbell** (READ): where every client rings. One endpoint for all of them;
//! - slots 3.., one **input endpoint per focusable client** (WRITE): where a keystroke goes when that
//!   client has focus. Focus is *which of these capabilities the compositor uses*, and a client that
//!   holds no such endpoint cannot be sent input at all.
//!
//! Plus mappings: the scanout, the window-list page it publishes, the input ring it shares with the
//! input source, and each client's control page and surface. It holds no device, no interrupt, no DMA
//! authority, and no physical address.
//!
//! # Why the doorbell can be shared, and why there is no authorization code here
//!
//! The full argument is in `compositor`'s crate docs and notes/compositor.md; the short version is that
//! a shared endpoint carries no sender identity, so **nothing in a message is trusted**. The two verbs
//! are content-free (`HELLO`, `COMMIT`), every per-client fact is read from that client's own control
//! page, and every privileged answer travels through memory the holder was granted rather than
//! through a reply. So this program never asks "may you?" about anything. It cannot leak the screen to
//! a client that asks for it, because handing over the screen is not something it can do.
//!
//! # What it does not do
//!
//! No window management (the scene is `compositor::SCENE`, fixed), no alpha, no GPU (rung four), no text
//! (rung three). And one structural limit worth reading before believing anything else here: this
//! process has **one blocking wait point**, because a thread can only be parked in one `RECV` and
//! this kernel has no wait-any primitive and no threads sharing an address space. That is the reason
//! the design routes everything through one doorbell plus shared memory rather than one endpoint per
//! source, and the reason a client that stops answering can stall the compositor. See
//! notes/compositor.md, "The primitive this rung wants and does not have".
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46), replacing `compd`.
//! Refused `compd` (the `-d` claim). Sharing the crate's name is deliberate: the crate is this
//! program's logic and the program is its authority (DECISIONS §63).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use compositor::proto::{ctl, ring, wlist};
use compositor::{Rect, SCENE};
use gfx_proto as gfx;
use user_rt::{call, invoke, recv_cap, send};

/// Capability slots, by convention with `kernel/src/user/compositor_service.rs`.
const REPORT: u64 = 0;
const DISPLAY: u64 = 1;
const DOORBELL: u64 = 2;
/// The first per-client input endpoint. Client `i` (for `i < focusable`) is at `INPUT + i`.
const INPUT: u64 = 3;

/// Where the kernel maps what this process is given. Must match `compositor_service`.
const SCREEN_VA: u64 = 0x0000_0000_0080_0000;
const WLIST_VA: u64 = 0x0000_0000_0081_0000;
const RING_VA: u64 = 0x0000_0000_0082_0000;
const CLIENT_BASE: u64 = 0x0000_0000_0090_0000;
const CLIENT_STRIDE: u64 = 0x0000_0000_0010_0000;

/// Bring-up failures, reported in a `0xDEAD_...` word so a failure names its step instead of hanging.
const E_TOO_MANY_WINDOWS: u64 = 0x01;
const E_FLUSH: u64 = 0x02;

fn ctl_va(i: usize) -> u64 {
    CLIENT_BASE + i as u64 * CLIENT_STRIDE
}

fn surface_va(i: usize) -> u64 {
    ctl_va(i) + 0x1000
}

fn rd32(va: u64) -> u32 {
    // SAFETY: `va` is inside a page the kernel mapped read/write into this process at spawn.
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

fn wr32(va: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(va as *mut u32, v) }
}

/// The scanout, as a slice. The compositor's whole output.
///
/// # Safety
/// The kernel maps exactly `SCREEN_PIXELS` pixels at [`SCREEN_VA`], read/write, shared with the
/// display driver. No other thread in this process exists, so there is no aliasing question.
fn screen() -> &'static mut [u32] {
    // SAFETY: see above.
    unsafe { core::slice::from_raw_parts_mut(SCREEN_VA as *mut u32, compositor::SCREEN_PIXELS) }
}

/// Client `i`'s surface, as a slice. The pixels the client writes and this process only reads.
fn source(i: usize) -> &'static [u32] {
    // SAFETY: the kernel maps `SCENE[i].frames()` frames at `surface_va(i)`, which is at least
    // `pixels()` words. Read-only use here by discipline: the client owns these pixels.
    unsafe { core::slice::from_raw_parts(surface_va(i) as *const u32, SCENE[i].pixels()) }
}

fn die(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    user_rt::exit();
}

/// **Publish every client's control page and the window list.** After this a client may read its own
/// geometry out of its own memory, which is how a per-client answer is delivered on a protocol whose
/// messages carry no identity.
fn publish(n: usize, focus: u32) {
    for (i, win) in SCENE.iter().take(n).enumerate() {
        let c = ctl_va(i);
        wr32(c + ctl::ID, i as u32);
        wr32(c + ctl::WIDTH, win.w);
        wr32(c + ctl::HEIGHT, win.h);
        wr32(c + ctl::STRIDE, win.stride());
        wr32(c + ctl::STATUS, ctl::STATUS_OK);
        wr32(c + ctl::ACKED, 0);
        // MAGIC last: a client that sees it knows every field above is already written.
        //
        // **The store order is not what makes that true, and an earlier version of this comment said
        // it was.** `wr32` is `write_volatile`, which guarantees the access happens and guarantees
        // no ordering whatsoever; on aarch64 the interconnect may make MAGIC visible before the
        // fields above it. What actually makes the check sound is the rendezvous: `publish` is
        // called once, before this program enters its serve loop, so a client's `HELLO` reply cannot
        // arrive until every control page is written. Both clients say so at their end
        // (`user/src/window.rs`, `user/src/display_terminal.rs`) and this end did not.
        //
        // MAGIC stays last anyway, because it costs nothing and is the right shape if this ever
        // publishes again while clients run. If it does, this needs a release fence and every client
        // needs an acquire one, the way `clock_proto` does it. See notes/memory-ordering.md.
        wr32(c + ctl::MAGIC, ctl::MAGIC_VALUE);
    }

    wr32(WLIST_VA + wlist::COUNT, n as u32);
    wr32(WLIST_VA + wlist::FOCUSED, focus);
    for (i, win) in SCENE.iter().take(n).enumerate() {
        let r = WLIST_VA + wlist::RECORDS + i as u64 * wlist::RECORD_STRIDE;
        wr32(r, win.origin_x as u32);
        wr32(r + 4, win.origin_y as u32);
        wr32(r + 8, win.w);
        wr32(r + 12, win.h);
    }
}

/// Put `damage` on the screen: `gfx FLUSH`, the same request `painter` made, through the same
/// endpoint. The driver copies those pixels into the device's resource and shows them.
fn flush(damage: Rect) {
    let Some((x, y, w, h)) = damage.as_flush_rect() else {
        return; // nothing on screen changed; asking the driver to flush nothing is a refused request
    };
    // The pixels must be visible to whoever reads them next: the display driver in another address
    // space, and through it a device. A release fence is the portable way to say that (it lowers to
    // `dmb ish` on aarch64 and `fence` on RISC-V), and it belongs here rather than in arch code
    // because this is a userspace program (DECISIONS rule 1).
    //
    // PAIR: `barrier()` in user/src/display.rs, which the driver issues before it moves the
    // virtqueue index and again before it notifies the device. The `call(DISPLAY, FLUSH, ...)` below
    // is what orders these pixels against the driver *reading* them (the driver is blocked in
    // `recv_cap`); this fence is what covers the driver-to-device leg not being ours to see.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let (r0, _) = call(
        DISPLAY,
        gfx::req(gfx::display::FLUSH, gfx::rect(x, y, w, h)),
        0,
    );
    if r0 as i64 != 0 {
        // The driver refuses a rectangle outside the surface rather than clamping it, so this is our
        // arithmetic being wrong, not the driver being difficult. Fail loudly.
        die(E_FLUSH);
    }
}

/// **Drain the input ring and route what is in it.** The ring is a page shared with the input source
/// alone, so a keystroke's authority is a mapping no client has: this is why input cannot be forged
/// over the shared doorbell.
///
/// `FOCUS_NEXT` moves focus, which is the compositor's decision and nobody else's. Every other byte
/// goes to the focused client as `line_editor::proto::OP_BYTES`, the terminal contract's driver half
/// verbatim, so a terminal is a client of this compositor without either contract changing.
fn drain_input(focusable: usize, focus: &mut u32) {
    let mut head = rd32(RING_VA + ring::HEAD);
    let tail = rd32(RING_VA + ring::TAIL);
    // **The acquire half of `kbd::ring_publish`'s release** (milestone 43,
    // notes/shared-page-audit.md finding 7). The producer fences before it stores the tail, with a
    // comment saying the bytes must be visible before the tail that advertises them; that orders
    // the producer's stores and does nothing whatever for this reader. `rd32` is a plain volatile
    // load, and on aarch64 the byte loads below may be satisfied before the tail load above, so
    // without this the compositor can act on a fresh tail and stale bytes. The kernel's stand-in
    // for this same ring (`kernel/src/user/keyboard_service.rs`, `take_typed`) already fences
    // here; the userspace reader of the same contract did not.
    // PAIR: `kbd::ring_publish`'s release fence in the input source (milestone 43,
    // notes/shared-page-audit.md finding 7), which fences before it stores the tail loaded above.
    core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
    while head != tail {
        let at = RING_VA + ring::BYTES + (head % ring::CAPACITY) as u64;
        // SAFETY: inside the ring page, which the kernel mapped read/write.
        let byte = unsafe { core::ptr::read_volatile(at as *const u8) };
        head = head.wrapping_add(1);

        if byte == compositor::proto::FOCUS_NEXT {
            if focusable > 0 {
                *focus = (*focus + 1) % focusable as u32;
                wr32(WLIST_VA + wlist::FOCUSED, *focus);
            }
            continue;
        }
        if (*focus as usize) < focusable {
            // A CALL, not a SEND, because that is what the terminal contract's driver half does: the
            // client answers, and the answer is the flow control that keeps a fast source from
            // outrunning a slow client. It also means a focused client that stops answering stalls
            // this loop, which is the honest cost of one wait point (see the module note).
            let w0 = line_editor::proto::req(line_editor::proto::OP_BYTES, 1);
            let _ = call(INPUT + *focus as u64, w0, byte as u64);
        }
        // A byte with no focusable client to receive it is dropped. Not silently: there is nobody
        // holding an input capability, so there is nowhere for it to go, which is the correct
        // behaviour rather than a queue that grows forever.
    }
    wr32(RING_VA + ring::HEAD, head);
}

/// **Serve one frame.** Read every client's control page, composite whatever changed, flush exactly
/// that, and nothing more.
///
/// The caller is blocked in `CALL` throughout, which is what makes reading a client's pixels safe
/// without a lock: the client that rang cannot be writing while we read. A client that rang *for*
/// another client's damage gains nothing, since it is still only its own control page it can write.
fn serve_frame(
    n: usize,
    focusable: usize,
    focus: &mut u32,
    last_seq: &mut [u32],
    committed: &mut [bool],
) {
    drain_input(focusable, focus);

    let mut damage = compositor::EMPTY;
    for i in 0..n {
        let c = ctl_va(i);
        let seq = rd32(c + ctl::SEQ);
        if seq == last_seq[i] {
            continue; // an idle client costs nothing
        }
        last_seq[i] = seq;
        committed[i] = true;

        // **The acquire half of the client's release** (milestone 43,
        // notes/shared-page-audit.md finding 7). Both clients (`window.rs`, `display_terminal.rs`)
        // put a fence between their pixel and rectangle stores and the sequence store, each with a
        // comment saying the sequence must become visible last "or the compositor could composite
        // a frame we have not finished writing". That is the release side, and it was the only
        // side there was: nothing here ordered the rectangle and pixel loads *after* the sequence
        // load, so on a weakly-ordered machine this could read a fresh sequence beside the
        // previous frame's pixels. A one-sided fence is not a fence.
        //
        // The same shape, in the clock page's seqlock, is milestone 80's finding; it is recorded
        // here because it is a different page, a different pair, and the reader's half rather than
        // the writer's.
        // PAIR: each client's release fence in user/src/window.rs and user/src/display_terminal.rs
        // (milestone 43), between the pixel/rectangle stores and the `ctl::SEQ` store loaded above.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        // The client's rectangle is **untrusted input**. Clip it to the surface it owns; say so in its
        // own control page if it was out of bounds (per-client feedback through the only channel that
        // can carry it), and carry on. The worst a lie can do is make us re-copy the liar's pixels.
        let asked = Rect::new(
            rd32(c + ctl::DAMAGE_X) as i32,
            rd32(c + ctl::DAMAGE_Y) as i32,
            rd32(c + ctl::DAMAGE_W),
            rd32(c + ctl::DAMAGE_H),
        );
        let inside = asked.intersect(&SCENE[i].bounds());
        wr32(
            c + ctl::STATUS,
            if inside == asked {
                ctl::STATUS_OK
            } else {
                ctl::STATUS_CLIPPED
            },
        );
        damage = damage.union(&compositor::damage_to_screen(&SCENE[i], asked));
        wr32(c + ctl::ACKED, seq);
    }

    if damage.is_empty() {
        return;
    }
    paint(n, damage, committed);
}

/// Composite `damage` and put it on the screen. A client that has never committed contributes an
/// **empty** source, so it is not drawn: before anyone has painted, the screen is the background, not
/// whatever the surfaces happened to hold.
fn paint(n: usize, damage: Rect, committed: &[bool]) {
    let mut srcs: [&[u32]; compositor::MAX_WINDOWS] = [&[]; compositor::MAX_WINDOWS];
    for (i, s) in srcs.iter_mut().enumerate().take(n) {
        if committed[i] {
            *s = source(i);
        }
    }
    compositor::composite(screen(), &srcs[..n], n, damage);
    flush(damage);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(windows: u64, focusable: u64, _arg2: u64) -> ! {
    let n = windows as usize;
    let focusable = focusable as usize;
    if n > SCENE.len() || n > compositor::MAX_WINDOWS || focusable > n {
        die(E_TOO_MANY_WINDOWS);
    }
    let mut focus: u32 = 0;
    let mut last_seq = [0u32; compositor::MAX_WINDOWS];
    let mut committed = [false; compositor::MAX_WINDOWS];

    publish(n, focus);

    // **Paint the background once, over the whole screen.** Every frame after this is damage-only, so
    // without this pass the parts of the screen no window ever damages would keep whatever the frames
    // held at boot. A real compositor does the same thing for the same reason, and it is also the one
    // full-screen flush in the program's life. Nobody has committed, so no window is drawn: the screen
    // a spawner sees before its clients run is exactly the background, which is a picture a test can
    // predict rather than "whatever was in those frames".
    paint(n, Rect::screen(), &committed);

    send(REPORT, compositor::status::COMP_UP, n as u64, focus as u64);

    loop {
        // One wait point, and everything arrives here: a client's HELLO or COMMIT, and the input
        // source's ring. `recv_cap`'s second value is the kernel-minted one-shot Reply naming the
        // caller, which is how a reply reaches whoever rang without this program ever knowing who
        // that was.
        let (w0, reply_slot, _) = recv_cap(DOORBELL);
        let r0: i64 = match compositor::proto::op(w0) {
            compositor::proto::HELLO => 0,
            compositor::proto::COMMIT => {
                serve_frame(n, focusable, &mut focus, &mut last_seq, &mut committed);
                0
            }
            _ => compositor::proto::EBADOP,
        };
        // SAFETY: `svc`/`ecall`; the kernel validated the Reply capability and consumes it here.
        unsafe { invoke(reply_slot, abi::reply::REPLY, r0 as u64, 0, 0) };
    }
}

user_rt::panic_handler!();
