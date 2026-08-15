use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use compositor::proto::wlist;
use compositor::{Rect, SCENE, status};
use compositor_service::{
    ROLE_CAPTURE, ROLE_INPUT, ROLE_PROBE_INPUT, ROLE_PROBE_NEIGHBOUR, ROLE_PROBE_SCREEN,
    ROLE_SMALL_DAMAGE, ROLE_VICTIM, Wiring,
};

use super::*;
use crate::arch::exceptions::USER_FAULTS;
use crate::sched;

/// Spin until `cond`, bounded by wall clock rather than by a yield count: since DECISIONS §28 the
/// work a test spawns runs on *other* cores, so a yield on an idle core returns at once and a fixed
/// number of them elapses in no real time. Two seconds is far under the 60 s hang watchdog, so a
/// genuine lost wakeup still fails loudly.
fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        if cond() {
            return true;
        }
        sched::yield_now();
    }
    cond()
}

/// What the last flush asked for, and how many there have been. Written by the display stand-in
/// below; reset by each call to [`kernel_display`]. The compositors left behind by earlier tests are
/// parked in `RECV` and flush nothing, so this is not shared state in any live sense.
static LAST_FLUSH: AtomicU64 = AtomicU64::new(u64::MAX);
static FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);

/// **A display the kernel serves itself**: the rung-one contract (`INFO`, `FLUSH`) over frames the
/// kernel allocated, with no device behind it. Returns `(display endpoint, screen frames)`.
///
/// It exists to make the damage rectangle visible. A real driver honours the rectangle and says
/// nothing about it; here the flush *is* the observation, so a compositor that quietly repainted the
/// screen every frame would fail a test rather than merely be slow.
fn kernel_display() -> (sched::EpId, u64) {
    let frames = gfx_proto::SURFACE_FRAMES as u64;
    let screen = crate::memory::alloc_contiguous(frames as usize)
        .expect("no contiguous screen frames for the compositor")
        .addr();
    // SAFETY: a fresh contiguous run, direct-mapped, owned by nobody else.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(screen) as *mut u8,
            0,
            (frames * FRAME_SIZE) as usize,
        );
    }
    LAST_FLUSH.store(u64::MAX, Ordering::SeqCst);
    FLUSH_COUNT.store(0, Ordering::SeqCst);

    let ep = sched::create_endpoint();
    sched::spawn(move || {
        loop {
            let m = sched::ipc_recv_cap(ep);
            let (w0, slot) = (m[0], m[1]);
            let crate::cap::Object::Reply(caller) = sched::current_cap(slot)
                .expect("the display stand-in got no reply capability")
                .object
            else {
                panic!("the display stand-in was sent something that was not a CALL");
            };
            let (r0, r1) = match gfx_proto::op(w0) {
                gfx_proto::display::FLUSH => {
                    LAST_FLUSH.store(gfx_proto::operand(w0), Ordering::SeqCst);
                    FLUSH_COUNT.fetch_add(1, Ordering::SeqCst);
                    (0, 0)
                }
                gfx_proto::display::INFO => (
                    0,
                    gfx_proto::WIDTH as u64 | ((gfx_proto::HEIGHT as u64) << 32),
                ),
                _ => (gfx_proto::EINVAL as u64, 0),
            };
            sched::ipc_reply(caller, [r0, r1]);
            sched::delete_current_cap(slot).expect("consume the one-shot reply");
        }
    })
    .expect("could not spawn the display stand-in");
    (ep, screen)
}

/// Wait for the compositor's one status message, and check it.
fn wait_for_compositor(w: &Wiring) {
    let [tag, windows, focus, ..] = sched::ipc_recv(w.report);
    assert_eq!(
        tag,
        status::COMP_UP,
        "the compositor did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte names \
         the step, see user/src/compositor.rs)",
    );
    assert_eq!(windows, w.n as u64, "the compositor wired the wrong scene");
    assert_eq!(focus, 0, "focus should start on the bottom window");
}

/// Take a `CALL` a client parked on its report endpoint: `(the caller, the reply slot, its word)`.
fn take_call(ep: sched::EpId, want: u64) -> (u64, u64, u64) {
    let m = sched::ipc_recv_cap(ep);
    assert_eq!(
        m[0], want,
        "a client reported {:#x} where {want:#x} was expected (a 0xDEAD_.. word's low byte names \
         the step, see user/src/window.rs)",
        m[0],
    );
    let crate::cap::Object::Reply(caller) = sched::current_cap(m[1])
        .expect("a client's report was not a CALL")
        .object
    else {
        panic!("a client's report carried no reply capability");
    };
    (caller, m[1], m[2])
}

fn release(caller: u64, slot: u64) {
    sched::ipc_reply(caller, [0, 0]);
    sched::delete_current_cap(slot).expect("consume the one-shot reply");
}

/// Take client `i`'s "painted and committed" report and check the digest against the pattern the
/// contract says that window holds. Every honest client sends exactly one of these, so a test that
/// spawns a client owes it a receive: a rendezvous SEND nobody takes leaves the client parked.
fn expect_painted(w: &Wiring, i: usize) {
    let [tag, digest, id, ..] = sched::ipc_recv(w.client_report[i]);
    assert_eq!(
        tag,
        status::WIN_PAINTED,
        "window {i} reported {tag:#x} instead of painting (a 0xDEAD_.. word's low byte names the \
         step, see user/src/window.rs)",
    );
    assert_eq!(
        digest,
        compositor::expected_window_checksum(i),
        "window {i} did not paint its own window's pattern into its own surface",
    );
    assert_eq!(
        id, i as u64,
        "window {i} was told it was window {id}: the compositor published the wrong control page",
    );
}

/// The whole screen equals the picture `compositor` says `committed` windows produce. The kernel's own
/// witness, computed from the contract and read through the direct map, so no process is grading its
/// own homework.
fn assert_screen_is(w: &Wiring, committed: usize) {
    for y in 0..compositor::SCREEN_H {
        for x in 0..compositor::SCREEN_W {
            let got = w.screen_pixel(x, y);
            let want = compositor::expected_screen_pixel(committed, x, y);
            assert_eq!(
                got, want,
                "the composed screen is wrong at ({x},{y}): {got:#010x}, expected {want:#010x} \
                 with {committed} windows committed",
            );
        }
    }
}

/// **A client cannot reach its neighbour's pixels, and cannot read the screen it draws into.**
///
/// The thesis content of this rung, so it is proved from four directions rather than asserted.
///
/// The attacker is given every advantage short of a capability. It is the *same binary* as the
/// honest client, with the same grants, and the kernel hands it the **exact virtual address** at
/// which its neighbour's pixels sit: one page past its own surface, which is where the kernel's
/// contiguous allocation really did put them (asserted here, so the attack cannot quietly become a
/// poke at nothing). Every client maps its surface at the same virtual address, so this is also the
/// address the neighbour uses for its own pixels. It still cannot touch them, because the boundary
/// is the mapping and not the layout.
///
/// What that proves, in order:
///
/// 1. **The refusal that needs no attack.** The attacker asks the kernel to receive on the input
///    slot it was not granted, and gets `NoSuchSlot`: "there is nothing there", not a permission
///    error from a server that consulted a list. Slot 2 is empty because the spawn literal left it
///    empty, and that emptiness is the whole of the difference between this client and a focusable
///    one.
/// 2. **The write faults**, and on aarch64 the faulting address is exactly the one it was handed.
/// 3. **Nothing was written**: the victim's witness pattern digests identically before and after,
///    from two independent readers (the kernel through the direct map, and the victim itself
///    through its own mapping after the attacker is dead). The victim is held in a `CALL` across
///    the whole attack so that "after" really is after.
/// 4. **No ambient display.** A third client, which painted into this very screen, reads the
///    address where the screen is mapped in the compositor and in the capture client, and faults.
///    It holds no mapping of the screen, so there is nothing to read and no verb to ask with.
///
/// A read fault proves the page is not mapped *at all*, which is the same reason a write cannot
/// reach it either; the two probes here are a write (integrity) and a read (confidentiality) so
/// both directions are exercised on real hardware behaviour rather than argued from one.
#[test_case]
fn a_client_holds_no_capability_for_its_neighbours_pixels_or_the_screen() {
    const ATTACKER: usize = 0;
    const VICTIM: usize = 1;
    const PEEPER: usize = 2;

    let (display, screen) = kernel_display();
    let w = compositor_service::start(3, 0, display, screen);
    wait_for_compositor(&w);

    // The victim paints and then parks in a CALL, so we can hold it there while it is attacked.
    w.spawn_client(VICTIM, ROLE_VICTIM);
    let (victim, victim_slot, reported) = take_call(w.client_report[VICTIM], status::WIN_PAINTED);
    assert_eq!(
        reported,
        compositor::expected_window_checksum(VICTIM),
        "the victim did not paint its own window's pattern",
    );
    let before = w.client_surface_digest(VICTIM);
    assert_eq!(
        before, reported,
        "the kernel and the victim disagree about the victim's surface before any attack",
    );

    // The attacker's address really is the neighbour's pixels. Without this the attack could
    // degenerate into poking an empty hole and still "pass".
    assert_eq!(
        w.neighbour_probe_phys(ATTACKER),
        w.client[VICTIM] + FRAME_SIZE,
        "the probe address is not the victim's first pixel frame: the allocation is not adjacent, \
         so this test would prove nothing about a neighbour",
    );

    let faults = USER_FAULTS.load(Ordering::Relaxed);
    w.spawn_client(ATTACKER, ROLE_PROBE_INPUT | ROLE_PROBE_NEIGHBOUR);

    let [tag, errno, ..] = sched::ipc_recv(w.client_report[ATTACKER]);
    assert_eq!(
        tag,
        status::WIN_REFUSED,
        "the attacker skipped its input probe"
    );
    assert_eq!(
        errno as i64,
        abi::Error::NoSuchSlot as i64,
        "receiving on an ungranted slot must be NoSuchSlot (-1), 'there is nothing there', not \
         {} (a permission error would mean the authority exists and was withheld)",
        errno as i64,
    );

    // It is an honest client up to the moment it is not: it paints its own window and reports, and
    // only then reaches for its neighbour's.
    expect_painted(&w, ATTACKER);

    let [tag, probe_va, ..] = sched::ipc_recv(w.client_report[ATTACKER]);
    assert_eq!(
        tag,
        status::WIN_PROBING,
        "the attacker never reached its probe"
    );
    assert_eq!(probe_va, w.neighbour_probe_va(ATTACKER));

    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "a client wrote at {probe_va:#x}, its neighbour's pixels, and was NOT stopped",
    );
    // The exact address, on both ISAs. This half used to be aarch64-only, because aarch64 had a
    // last-fault record (`FAR_EL1`, stashed for tests) and RISC-V had only a fault *count*: it
    // knew `stval` at the instant of the fault and threw it away. Milestone 19's portable
    // record keeps it on both, so "something faulted" is no longer all this ISA can say.
    assert_eq!(
        crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
        Some(probe_va),
        "something faulted, but not at the neighbour's address",
    );
    assert_eq!(
        sched::endpoint_waiting_senders(w.client_report[ATTACKER]),
        0,
        "the attacker reported past its probe: the write did not fault, so it read back what it \
         wrote into a neighbour's surface (WIN_ESCAPED)",
    );

    // The witness pattern, from the kernel and then from the victim itself.
    assert_eq!(
        w.client_surface_digest(VICTIM),
        before,
        "the victim's pixels changed while it was blocked: the attack landed",
    );
    release(victim, victim_slot);
    let [tag, after, was_before, ..] = sched::ipc_recv(w.client_report[VICTIM]);
    assert_eq!(tag, status::WIN_INTACT);
    assert_eq!(was_before, before, "the victim changed its story");
    assert_eq!(
        after, before,
        "the victim's own read-back of its surface changed after the attack",
    );

    // No ambient display: a client that draws into the screen cannot read it.
    let faults = USER_FAULTS.load(Ordering::Relaxed);
    w.spawn_client(PEEPER, ROLE_PROBE_SCREEN);
    expect_painted(&w, PEEPER);
    let [tag, screen_va, ..] = sched::ipc_recv(w.client_report[PEEPER]);
    assert_eq!(tag, status::WIN_PROBING);
    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "a window client read the composed screen at {screen_va:#x} and was NOT stopped: the \
         display is ambient",
    );
    assert_eq!(
        crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
        Some(screen_va),
        "something faulted, but not at the composed screen's address",
    );
    assert_eq!(
        sched::endpoint_waiting_senders(w.client_report[PEEPER]),
        0,
        "a client read a pixel of the screen it holds no mapping of (WIN_ESCAPED)",
    );
}

/// **A one-window redraw costs one rectangle, not a screen.**
///
/// The damage rectangle is the whole reason rung one put one in the contract, and it is only worth
/// anything if the compositor honours it end to end. So this test does not measure time (which
/// under TCG would mean nothing); it observes **what was flushed** and **what was left alone**:
///
/// - the kernel plays the display, so the flush rectangle is a value it can compare against
///   `compositor::damage_to_screen`, and the count of flushes says one commit produced one flush;
/// - between the two frames the kernel **poisons every screen pixel outside** the rectangle the
///   coming commit should produce. A compositor that repainted the screen would erase the poison.
///   Finding it intact afterwards is the proof, and it is the same technique the crate's host test
///   uses in microseconds.
///
/// It also pins the startup behaviour that makes the whole picture predictable: the compositor's
/// first paint is the background over the whole screen, once, with no window drawn, because no
/// client has committed yet.
#[test_case]
fn a_one_window_redraw_costs_one_rectangle_and_not_the_screen() {
    const POISON: u32 = 0xDEAD_BEEF;

    let (display, screen) = kernel_display();
    let w = compositor_service::start(2, 0, display, screen);
    wait_for_compositor(&w);

    assert_eq!(
        FLUSH_COUNT.load(Ordering::SeqCst),
        1,
        "the compositor's startup should be exactly one flush",
    );
    assert_eq!(
        LAST_FLUSH.load(Ordering::SeqCst),
        gfx_proto::rect(0, 0, compositor::SCREEN_W, compositor::SCREEN_H),
        "the startup flush should be the whole screen",
    );
    assert_screen_is(&w, 0);

    // Window 0 paints, commits, reports, and then parks in a CALL waiting for permission to send
    // its second frame; window 1 paints and commits once.
    w.spawn_client(0, ROLE_SMALL_DAMAGE);
    expect_painted(&w, 0);
    let (client0, slot0, d0) = take_call(w.client_report[0], status::WIN_PAINTED);
    assert_eq!(d0, compositor::expected_window_checksum(0));
    w.spawn_client(1, 0);
    expect_painted(&w, 1);
    assert_screen_is(&w, 2);

    // Poison everything the coming commit must not touch.
    let want = compositor::damage_to_screen(&SCENE[0], compositor::SMALL_DAMAGE);
    assert!(
        !want.is_empty() && want.area() * 20 < Rect::screen().area(),
        "the damage rectangle under test is not small: {want:?}",
    );
    for y in 0..compositor::SCREEN_H {
        for x in 0..compositor::SCREEN_W {
            if !want.contains(x as i32, y as i32) {
                w.poison_screen_pixel(x, y, POISON);
            }
        }
    }

    let flushes = FLUSH_COUNT.load(Ordering::SeqCst);
    release(client0, slot0);
    let [tag, _, seq, ..] = sched::ipc_recv(w.client_report[0]);
    assert_eq!(tag, status::WIN_PAINTED, "the second commit never happened");
    assert_eq!(
        seq, 2,
        "the second commit should be the client's second sequence"
    );

    assert_eq!(
        FLUSH_COUNT.load(Ordering::SeqCst),
        flushes + 1,
        "one commit must be one flush",
    );
    let flushed = gfx_proto::unrect(LAST_FLUSH.load(Ordering::SeqCst));
    assert_eq!(
        flushed,
        (want.x as u32, want.y as u32, want.w, want.h),
        "the flush was not the client's damage placed on the screen",
    );

    for y in 0..compositor::SCREEN_H {
        for x in 0..compositor::SCREEN_W {
            let got = w.screen_pixel(x, y);
            if want.contains(x as i32, y as i32) {
                assert_eq!(
                    got,
                    compositor::expected_screen_pixel(2, x, y),
                    "inside the damage, ({x},{y}) was not recomposited",
                );
            } else {
                assert_eq!(
                    got, POISON,
                    "outside the damage, ({x},{y}) was overwritten: the compositor repainted more \
                     than it was asked to",
                );
            }
        }
    }
}

/// **Input reaches the focused client because that client holds a capability, and focus is the
/// compositor's decision.**
///
/// Three things are being separated here, and Unix conflates all three:
///
/// 1. **Who may deliver input.** The keystroke arrives in a ring page shared with the input source
///    alone. Any client may ring the doorbell; none of them can write that page, so none of them can
///    inject a keystroke into another client. (There is no "grab the keyboard" verb to guard,
///    because there is nothing a message could say that would do it.)
/// 2. **Who may receive it.** The focused client receives because it *holds* an input endpoint. A
///    client without one cannot be sent a keystroke by anyone, however the compositor feels about
///    it, which is the previous test's `NoSuchSlot` refusal seen from the other side.
/// 3. **Who decides.** Focus moves on a byte, in userspace, in the compositor. The kernel routes
///    the message and knows nothing about focus; this test *witnesses* the decision by reading the
///    window-list page the compositor publishes rather than by asking it.
///
/// The negative half is the interesting half: after focus moves, the unfocused client must not
/// receive the next keystroke, and `endpoint_waiting_senders` is how a test says "and then nothing
/// happened" without blocking forever on a quiet endpoint.
#[test_case]
fn input_reaches_only_the_focused_client_and_focus_is_the_compositors_call() {
    let (display, screen) = kernel_display();
    let mut w = compositor_service::start(2, 2, display, screen);
    wait_for_compositor(&w);

    w.spawn_client(0, ROLE_INPUT);
    w.spawn_client(1, ROLE_INPUT);
    for i in 0..2 {
        expect_painted(&w, i);
    }

    assert_eq!(w.focused(), 0, "focus should start on window 0");
    w.type_bytes(b"a");
    let [tag, byte, count, ..] = sched::ipc_recv(w.client_report[0]);
    assert_eq!(
        tag,
        status::WIN_INPUT,
        "the focused client got no keystroke"
    );
    assert_eq!(byte, b'a' as u64);
    assert_eq!(count, 1);

    // Focus moves, and we read the compositor's decision out of the page it publishes.
    w.type_bytes(&[compositor::proto::FOCUS_NEXT]);
    assert_eq!(
        w.focused(),
        1,
        "the compositor did not move focus, or did not publish that it had",
    );
    let record = mmu::phys_to_virt(w.wlist) + wlist::COUNT;
    // SAFETY: the window-list frame this kernel allocated, through the direct map.
    assert_eq!(unsafe { core::ptr::read_volatile(record as *const u32) }, 2);

    w.type_bytes(b"b");
    let [tag, byte, ..] = sched::ipc_recv(w.client_report[1]);
    assert_eq!(
        tag,
        status::WIN_INPUT,
        "the newly focused client got nothing"
    );
    assert_eq!(byte, b'b' as u64);
    assert_eq!(
        sched::endpoint_waiting_senders(w.client_report[0]),
        0,
        "the unfocused client received a keystroke that was not routed to it",
    );
}

/// **Focus routes a keystroke into one terminal's grid and not its neighbour's** (milestone 29's
/// text increment meeting rung two).
///
/// Two display terminals, side by side, each a compositor client with exactly a window client's
/// authority. The keystroke routing this rung already proved is now **visible in the picture**,
/// which is a stronger statement than the endpoint-level one and a different kind of evidence:
/// an `A` typed while terminal 0 has focus appears in terminal 0's grid, a TAB moves focus, and
/// a `B` appears in terminal 1's. Neither letter appears in the other window, and the kernel
/// checks that by comparing every pixel of the composed screen against the two engines it ran
/// itself.
///
/// # Why this is the capability claim and not a policy claim
///
/// Terminal 1 receives the `B` because it **holds an input endpoint**, not because it asked and
/// not because the compositor consulted a list (DECISIONS §33). The compositor's whole part in
/// it is choosing *which* of the capabilities it holds to use. A client granted no input endpoint
/// has an empty cspace slot and cannot be sent a keystroke by anyone, which the neighbouring test
/// in this module proves by value (`NoSuchSlot`); this one proves the other half, that a
/// keystroke routed to one holder does not land in another's memory.
///
/// # And the terminal is a client, unchanged at both seams
///
/// `compositor` cannot tell a display terminal from the `window` client that paints a coordinate
/// pattern: same grants, same control page, same doorbell, same `COMMIT`. Neither `compositor` nor
/// `gfx_proto` needed a line changed to carry text. That is the seam claim made twice in one
/// milestone, once at each rung.
///
/// Uses the kernel's display stand-in rather than the GPU, deliberately: this test is about
/// routing and composition, the device is proved elsewhere in this file, and a third `display`
/// against the same physical device would put the scanout's picture in a race with the
/// host-side check that reads it.
#[test_case]
fn focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours() {
    let (display, screen) = kernel_display();
    let mut w = compositor_service::start(2, 2, display, screen);
    wait_for_compositor(&w);

    // Both terminals up. Each negotiated its geometry out of the control page the compositor
    // published, so a window whose size the client did not choose is the *normal* case here.
    let mut terms = [video_terminal::Vt::new(1, 1), video_terminal::Vt::new(1, 1)];
    let mut clients = [None, None];
    for i in 0..2 {
        let c = w.spawn_terminal(i);
        let [tag, dims, mode, ..] = sched::ipc_recv(w.client_report[i]);
        assert_eq!(
            tag,
            video_terminal::status::TERM_UP,
            "terminal {i} did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
             names the step, see user/src/display_terminal.rs)",
        );
        assert_eq!(mode, video_terminal::status::MODE_WINDOW);
        let (cols, rows) = ((dims & 0xffff_ffff) as u32, (dims >> 32) as u32);
        assert_eq!(
            (cols, rows),
            (SCENE[i].w / bitfont::GLYPH_W, SCENE[i].h / bitfont::GLYPH_H),
            "terminal {i} sized itself to a grid its window cannot hold",
        );
        terms[i] = video_terminal::script::window(i, cols, rows);
        clients[i] = Some(c);
    }

    // Each terminal prints its own banner. Different text per window, so an OP_WRITE delivered
    // to the wrong terminal is a wrong picture rather than a duplicate one.
    for (i, c) in clients.iter().enumerate() {
        c.as_ref()
            .unwrap()
            .print(video_terminal::script::WINDOW_BANNER[i]);
    }

    // Type at the focused terminal, move focus with TAB, type at the next one. `type_bytes`
    // writes the input ring and rings the doorbell, which is exactly what a keyboard driver
    // does; the authority it exercises is the ring mapping, and no client has one.
    assert_eq!(w.focused(), 0, "focus should start on the bottom window");
    w.type_bytes(video_terminal::script::WINDOW_TYPED[0]);
    assert_eq!(w.focused(), 0, "typing must not move focus");
    w.type_bytes(&[compositor::proto::FOCUS_NEXT]);
    assert_eq!(
        w.focused(),
        1,
        "TAB did not move focus: the compositor's own policy decision, published in the window \
         list so it can be witnessed rather than asked for",
    );
    w.type_bytes(video_terminal::script::WINDOW_TYPED[1]);

    // The picture. Every pixel of the composed screen, against the two engines the kernel ran
    // itself: window content from `video_terminal`, placement and stacking from `compositor`.
    for y in 0..compositor::SCREEN_H {
        for x in 0..compositor::SCREEN_W {
            let got = w.screen_pixel(x, y);
            let want =
                compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| terms[i].pixel(sx, sy));
            assert_eq!(
                got, want,
                "the composed screen is wrong at ({x},{y}): {got:#010x}, expected {want:#010x}",
            );
        }
    }

    // **And the letters really are distinguishable**, so the comparison above has teeth: a
    // compositor that sent both keystrokes to both terminals, or the wrong one to each, would
    // have to produce a different picture. Asserted rather than assumed, because if the two
    // scripts ever became the same text this test would keep passing while proving nothing.
    let swapped = [
        video_terminal::script::window(1, terms[0].cols(), terms[0].rows()),
        video_terminal::script::window(0, terms[1].cols(), terms[1].rows()),
    ];
    assert!(
        (0..compositor::SCREEN_H).any(|y| (0..compositor::SCREEN_W).any(|x| {
            compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| swapped[i].pixel(sx, sy))
                != compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| {
                    terms[i].pixel(sx, sy)
                })
        })),
        "the two terminals show the same thing: this test cannot tell mis-routed input from \
         correct input",
    );
}

/// **Three clients' surfaces become one screen, and the host confirms it.**
///
/// The end-to-end picture, with a real virtio-gpu under it (rung one's driver, unchanged: the
/// compositor takes `painter`'s place at that seam and `display` cannot tell). Four witnesses, which is
/// the point, because a compositor's output is exactly the thing one digest cannot be trusted about:
///
/// 1. **the driver**, digesting the frames it handed the device after the device said it had them.
///    Its one report is the compositor's *startup* frame, which is the background alone, so it is
///    also the check that an empty screen is a defined picture rather than whatever was in RAM;
/// 2. **the kernel**, reading the scanout frames through the direct map and comparing every pixel
///    against `compositor::expected_screen_pixel`, a value it computed itself;
/// 3. **a capture client in its own address space**, which holds a read-only mapping of the screen
///    (that mapping being the screenshot capability) and digests what it sees;
/// 4. **the host**, through QEMU's monitor, comparing `screendump`'s PPM against the same
///    definition. This one is not optional: `-display none` means nothing in the guest can see the
///    device's own surface, so a wrong pixel format or scanout rectangle would pass all three
///    in-guest witnesses and show garbage on a screen. `cargo xtask` runs it beside this suite.
///
/// The capture client also proves the *shape* of the grant twice over: it enumerates the windows
/// out of the read-only page the compositor publishes (there is no enumerate verb to call), and its
/// attempt to **write** the screen faults, because a thing that may look at the screen may not draw
/// on it.
#[test_case]
fn three_clients_compose_into_one_scanout_and_the_host_sees_it() {
    let display = program("display").expect("no display program in the initrd archive");
    let (driver_report, display, screen) = display_service::start_driver(display).expect(
        "no virtio-gpu-pci function on the bus: is NIFE_GPU missing from the test leg, or \
         the -device virtio-gpu-pci line from the runner?",
    );
    assert!(
        crate::iommu::active(),
        "a virtio-gpu is present but the IOMMU is not active: the GPU's pixel reads are \
         unconfined (notes/framebuffer-contract.md)",
    );
    let [tag, geometry, ..] = sched::ipc_recv(driver_report);
    assert_eq!(
        tag,
        gfx_proto::status::UP,
        "the display driver did not come up (it reported {tag:#x})",
    );
    assert_eq!(
        geometry,
        gfx_proto::WIDTH as u64 | ((gfx_proto::HEIGHT as u64) << 32),
    );

    let w = compositor_service::start(3, 0, display, screen);
    wait_for_compositor(&w);

    // The driver's own account of the compositor's first frame. Taken here and not later because
    // this is a rendezvous SEND: the driver is parked in it, and a test that spawned clients first
    // would deadlock the driver against the compositor's next flush.
    let [tag, driver_digest, pixels, ..] = sched::ipc_recv(driver_report);
    assert_eq!(
        tag,
        gfx_proto::status::FLUSHED,
        "the driver served no flush"
    );
    assert_eq!(pixels, gfx_proto::PIXELS as u64);
    assert_eq!(
        driver_digest,
        compositor::expected_screen_checksum(0),
        "the frames the device read for the compositor's first flush are not the background: an \
         empty screen must be a defined picture",
    );

    // In order, so that the capture client below is looking at a finished screen.
    for i in 0..2 {
        w.spawn_client(i, 0);
        expect_painted(&w, i);
    }

    let faults = USER_FAULTS.load(Ordering::Relaxed);
    w.spawn_client(2, ROLE_CAPTURE);
    expect_painted(&w, 2);

    // The screenshot, taken through a read-only mapping by a process that holds one.
    let [tag, shot, listed, ..] = sched::ipc_recv(w.client_report[2]);
    assert_eq!(
        tag,
        status::WIN_CAPTURED,
        "the capture client reported nothing"
    );
    assert_eq!(
        shot,
        compositor::expected_screen_checksum(3),
        "a client holding the screen's read-only mapping digested a different screen than the \
         kernel computed from the contract",
    );
    assert_eq!(
        listed,
        3u64 << 32,
        "the window list it enumerated does not say three windows with focus on the first",
    );

    // The kernel's own witness, pixel for pixel.
    assert_screen_is(&w, 3);

    // The other half of a read-only grant: it cannot deface what it may read.
    let [tag, va, which, ..] = sched::ipc_recv(w.client_report[2]);
    assert_eq!(tag, status::WIN_PROBING);
    assert_eq!(which, 1, "the capture client skipped its write probe");
    assert!(
        wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
        "a client wrote to the screen through a read-only mapping at {va:#x} and was NOT stopped",
    );
    assert_eq!(
        sched::endpoint_waiting_senders(w.client_report[2]),
        0,
        "the capture client survived writing to the screen (WIN_ESCAPED)",
    );
    // And the write did not land, which is the thing that would otherwise corrupt the picture the
    // host is about to check.
    assert_screen_is(&w, 3);

    // **Hold the picture up for the host.** `cargo xtask` polls QEMU's monitor about every 250 ms
    // while this suite runs, and the next test in the file re-registers the device (which destroys
    // the scanout), so a composed screen that vanished immediately could be missed. Three seconds is
    // an order of magnitude more than the poll needs and is nothing against the per-test ceiling.
    // If the host never sees it, the run fails at the scanout check rather than passing quietly.
    let deadline = crate::arch::timer::now() + 3 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        sched::yield_now();
    }
}
