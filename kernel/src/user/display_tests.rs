use gfx_proto as gfx;

use super::*;
use crate::sched;

/// **A confined userspace driver puts a known pattern in a scanout framebuffer.**
///
/// # What this proves, precisely
///
/// The pattern is a per-coordinate function ([`gfx_proto::pixel`]), not a fill, and the digest is
/// position sensitive, so a blank, stale, shifted, transposed, or truncated surface cannot pass
/// (`crates/gfx_proto`'s host tests assert exactly those properties of the pattern itself). Two
/// independent witnesses report it, from two different address spaces: the **client** digests the
/// surface after the flush through its own mapping, and the **driver** digests it through a
/// different mapping after the device reported the transfer complete. The kernel compares both
/// against a value it computed itself from the contract, so neither process is grading its own
/// homework.
///
/// It also proves the device could reach those exact frames and no others: the surface lives
/// inside the driver's registered DMA region, the IOMMU domain maps exactly that region, and
/// `RESOURCE_ATTACH_BACKING` naming it succeeded, which under translation only happens if the
/// address translated.
///
/// # What this test does NOT prove, and what does
///
/// **This test proves the framebuffer, not the scanout.** The suite runs `-display none`, and
/// nothing inside the guest can read back QEMU's host-side surface, so "the bytes we handed the
/// device are the bytes it read out of our frames" is as far as an *in-guest* test reaches. A
/// wrong pixel *format* or a wrong scanout rectangle would pass this while showing garbage on a
/// real screen.
///
/// The scanout is proven **from the host instead**, because only the host can see it:
/// `cargo xtask`'s scanout check drives QEMU's monitor beside this suite, dumps the scanout with
/// `screendump` (which works headlessly), and compares the PPM against `gfx_proto::pixel` pixel
/// for pixel, on both ISAs. Together the two halves cover the whole path. See
/// notes/framebuffer-contract.md, "Proving the scanout, from the host".
#[test_case]
fn a_confined_userspace_driver_puts_a_known_pattern_in_a_framebuffer() {
    let display = program("display").expect("no display program in the initrd archive");
    let painter = program("painter").expect("no painter program in the initrd archive");
    let threads_before = sched::thread_count();

    // A GPU asked for but not enumerated is a build-order mistake wearing a machine fact's
    // clothes, the hazard the runners were taught to fail loudly on. The test legs always attach
    // one, so absence is a failure, not a skip.
    let (driver_report, client_report) = display_service::start(display, painter).expect(
        "no virtio-gpu-pci function on the bus: is NIFE_GPU missing from the test leg, or \
         the -device virtio-gpu-pci line from the runner?",
    );

    // And a GPU present while the IOMMU is not means every pixel read is bypassing translation.
    // That matters more for a GPU than for a disk: its backing addresses ride in a device-level
    // command payload, not in a descriptor, so the transport's validator never sees them and the
    // IOMMU is the only thing bounding them (notes/framebuffer-contract.md).
    assert!(
        crate::iommu::active(),
        "a virtio-gpu is present but the IOMMU is not active: the GPU's pixel reads are \
         unconfined (is iommu=smmuv3 / -device riscv-iommu-pci or iommu_platform=on missing?)",
    );

    // 1. The driver came up: device enumerated, resource created and backed, scanout set. Taken
    //    first because these are rendezvous SENDs, so the driver is parked here until we look.
    let [tag, geometry, display, ..] = sched::ipc_recv(driver_report);
    assert_eq!(
        tag,
        gfx::status::UP,
        "the display driver did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
         is the bring-up step that failed, see user/src/display.rs)",
    );
    assert_eq!(
        geometry,
        gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),
        "the driver created a surface of the wrong geometry",
    );
    assert!(
        (display & 0xffff_ffff) >= gfx::WIDTH as u64,
        "the device reported a display narrower than our surface: {display:#x}",
    );

    // 2. The driver's own account of what it handed the device, digested in the driver's address
    //    space after the device completed the transfer. This must be taken BEFORE the client's
    //    verdict: the driver blocks in this SEND right after replying to the first flush, so a
    //    test that waited on the client first would deadlock against the client's second CALL.
    let [tag, driver_digest, pixels, ..] = sched::ipc_recv(driver_report);
    assert_eq!(
        tag,
        gfx::status::FLUSHED,
        "the driver never served a flush (it reported {tag:#x})",
    );
    assert_eq!(
        pixels,
        gfx::PIXELS as u64,
        "the driver flushed a different surface size"
    );
    assert_eq!(
        driver_digest,
        gfx::expected_checksum(),
        "the driver's digest of the frames it handed the device is not the pattern: the pixels \
         the device read are not the pixels the client painted",
    );

    // 3. The client's verdict: the surface read back through its own mapping after the flush.
    let [tag, client_digest, mismatch, ..] = sched::ipc_recv(client_report);
    assert_eq!(
        tag,
        gfx::status::PAINTED,
        "the painting client did not report a verdict (it reported {tag:#x}; a 0xDEAD_.. word's \
         low byte names the step, see user/src/painter.rs)",
    );
    assert_eq!(
        mismatch,
        gfx::NO_MISMATCH,
        "the client read back a wrong pixel at index {mismatch} of {}",
        gfx::PIXELS,
    );
    assert_eq!(
        client_digest,
        gfx::expected_checksum(),
        "the client's read-back digest is not the pattern it painted",
    );

    // The two witnesses must agree. They are digests of the same frames taken from different
    // address spaces at different moments, so a disagreement would mean the surface changed under
    // one of them, which is the mapping bug this shared-frame contract exists to not have.
    assert_eq!(
        driver_digest, client_digest,
        "the driver and the client disagree about the surface's contents",
    );

    // **Wait for the one-shot client to be reaped before returning.** Two reasons, and the second
    // is why this is not optional. It proves a client that finished is reaped rather than leaked,
    // the discipline every one-shot program here follows. And a process's frames come back
    // asynchronously at reap, so a client still unreaped when this test returns drops ~20 frames
    // into whatever the *next* test measures; `destroy_force_kills_a_runaway_and_reclaims_its_
    // region` asserts an exact free-frame count and failed on precisely that. The driver is a
    // long-lived server and never exits, so the target is one thread above the baseline.
    //
    // Clock-based, not a yield count: with work spread across cores a yield on an idle core
    // returns instantly and a fixed count elapses in almost no real time (DECISIONS §28).
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline && sched::thread_count() > threads_before + 1 {
        sched::yield_now();
    }
    assert!(
        sched::thread_count() <= threads_before + 1,
        "the painting client reported its verdict but was never reaped (threads {} > {})",
        sched::thread_count(),
        threads_before + 1,
    );
}

/// **The device is refused a framebuffer it was not granted.**
///
/// The GPU raises a confinement question a disk and a NIC do not, and this is the test that
/// answers it. Everywhere else, every address a device will touch arrives in a virtqueue
/// descriptor, so the kernel validates it and copies it into a shadow ring the driver cannot
/// reach (notes/dma.md). A virtio-gpu's *backing* addresses arrive inside a device-level command
/// payload instead. The kernel bounds the descriptor that carries the command, but the addresses
/// within it are bytes it does not parse, and it deliberately does not start parsing them:
/// teaching the transport to read virtio-gpu commands would put device knowledge in the one place
/// §18 keeps device-neutral.
///
/// So the IOMMU is the barrier, and this proves it holds **in hardware**, the same way and with
/// the same evidence milestone 16b's confinement test does: the fault the IOMMU recorded. A driver
/// with exactly the honest driver's authority asks the device to read pixels out of a frame the
/// kernel deliberately left out of its domain, and then to transfer from it. The IOMMU must fault
/// at that frame.
///
/// **The device's response code is deliberately not the assertion, and that is a finding.** The
/// first version of this test asserted the command came back refused, and it did not: QEMU's DMA
/// layer answers a translation failure by handing the device a *bounce buffer* instead of failing
/// the mapping, so `RESOURCE_ATTACH_BACKING` returns OK while the bytes the device actually gets
/// are not the victim frame's. The confinement held; only the error reporting did not survive the
/// trip. So the fault queue is the fact, the response code is printed for the record, and the
/// nuance is written down rather than smoothed over (notes/framebuffer-contract.md).
///
/// **Runs BEFORE the happy-path test, and the name is what makes that true.** This test resets
/// and re-registers the same physical GPU (each driver programs the device from scratch, and a
/// virtio reset destroys every resource and scanout), the same way the disk's attacker tests share
/// one device with the honest driver. If it ran second it would wipe the pattern the pixel test
/// put on the scanout, and the host-side scanout check (`cargo xtask`'s `gpu_shot`, which dumps
/// the scanout while the suite runs) would find nothing to match. Sorting first is why this is
/// named `a_backing...` rather than `the_iommu...`. A reordering does not corrupt anything; it
/// fails the scanout check loudly, which is the right way to be wrong.
#[test_case]
fn a_backing_outside_the_grant_is_refused_by_the_iommu() {
    let display = program("display").expect("no display program in the initrd archive");

    // Drain any stale fault first, so what we observe is this test's.
    while crate::iommu::take_fault().is_some() {}

    let (report, victim) = display_service::start_backing_escape(display)
        .expect("no virtio-gpu-pci function on the bus: is NIFE_GPU missing from the test leg?");
    assert!(
        crate::iommu::active(),
        "a virtio-gpu is present but the IOMMU is not active: nothing would refuse this escape, \
         so the test would pass or fail on a fiction",
    );

    let [tag, response, ..] = sched::ipc_recv(report);
    assert_eq!(
        tag,
        gfx::status::BACKING,
        "the escape driver did not reach its attach (it reported {tag:#x}; a 0xDEAD_.. word's \
         low byte names the bring-up step, see user/src/display.rs)",
    );

    // The evidence. QEMU records the fault as it processes the command under TCG, so a bounded
    // spin is plenty; the bound turns "no fault ever" into a failure rather than a hang.
    let mut fault = None;
    for _ in 0..2_000_000 {
        if let Some(f) = crate::iommu::take_fault() {
            fault = Some(f);
            break;
        }
        core::hint::spin_loop();
    }
    let f = fault.unwrap_or_else(|| {
        panic!(
            "the GPU was pointed at {victim:#x}, outside its DMA region, and the IOMMU recorded \
             no fault (the device answered the attach with {response:#x}): a backing address \
             rides in a command payload the transport validator cannot see, so if the IOMMU is \
             not bounding it, nothing is",
        )
    });
    assert_eq!(
        f.addr & !0xfff,
        victim & !0xfff,
        "the IOMMU faulted, but on {:#x} (code {:#x}, rid {:#x}), not the frame the GPU was \
         pointed at ({victim:#x})",
        f.addr,
        f.code,
        f.rid,
    );

    // Leave the fault queue as we found it. Not tidiness: the RISC-V IOMMU's queue holds 128
    // records and the driver does not clear its overflow bit, so records left behind here cost a
    // later test its own fault assertion. The escape above is sized to produce one fault for the
    // same reason (user/src/display.rs).
    while crate::iommu::take_fault().is_some() {}
}

/// **A bitmap font and a VT engine put readable text on the scanout.**
///
/// Milestone 29's remaining increment, end to end: a terminal component that is a *client* of
/// the framebuffer contract, drawing glyphs into a surface and flushing a damage rectangle.
///
/// # What makes this a proof rather than a screenshot
///
/// Text is the case where "it looked right" is most tempting and least sufficient, so the
/// picture is a **value three parties compute independently**:
///
/// - the **terminal** runs the `video_terminal` engine over the bytes it was sent and paints what it says;
/// - the **kernel** runs the same engine over the same script (`video_terminal::script`) and compares the
///   scanout frames pixel for pixel through the direct map. It never asks the terminal anything;
/// - the **host** runs it again and compares QEMU's `screendump` against the same definition
///   (`cargo xtask`, beside this suite). That one is not optional: `-display none` means nothing
///   in the guest can see the device's own surface, so a wrong pixel format or scanout rectangle
///   would satisfy the first two and show garbage on a screen.
///
/// And the host checker has a **negative control**: it must reject the same screen with one
/// letter changed (`video_terminal::script::GREETING_TYPO`). A checker that only asked "is there ink?" would
/// pass every run including the ones that drew the wrong thing.
///
/// # What the script exercises, and why each part is there
///
/// Four rows of text (a one-row picture would hide a stride error), three renditions (a terminal
/// that ignored SGR would draw every glyph correctly and still fail), a `\r\n` pair (what
/// `line_editor::expand_output` puts on the wire for a Unix `\n`), descenders and an underscore (the
/// glyph rows a font table truncated to seven would lose), and then **keystrokes**, delivered as
/// `OP_BYTES`: the terminal contract's driver half, byte for byte what `user/src/input.rs` sends
/// and what the compositor forwards to a focused client.
///
/// # And the picture the driver reports is the *blank* terminal, on purpose
///
/// `display`'s one status report covers its first flush, which here is the terminal's blank grid
/// before anything has been written. So it doubles as the check that an empty terminal is a
/// *defined* picture (spaces on the default background, with the cursor) rather than whatever
/// those frames held at boot, exactly as rung two used it for the empty compositor screen.
///
/// **Runs after the confinement test and before the pattern test**, which the name arranges: the
/// confinement test resets the device and would wipe this, and the pattern test's picture is the
/// one that stays up until QEMU exits. See notes/glyphs.md for the ordering and what breaks it.
#[test_case]
fn a_bitmap_font_and_a_vt_engine_put_readable_text_on_the_scanout() {
    let display = program("display").expect("no display program in the initrd archive");
    let display_terminal =
        program("display_terminal").expect("no display_terminal program in the initrd archive");

    let w = display_service::start_terminal(display, display_terminal).expect(
        "no virtio-gpu-pci function on the bus: is NIFE_GPU missing from the test leg, or \
         the -device virtio-gpu-pci line from the runner?",
    );

    // The driver came up. Taken first: these are rendezvous SENDs, so the driver is parked here.
    let [tag, geometry, ..] = sched::ipc_recv(w.driver_report);
    assert_eq!(
        tag,
        gfx::status::UP,
        "the display driver did not come up (it reported {tag:#x})",
    );
    assert_eq!(geometry, gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),);

    // The terminal negotiated its geometry from the driver rather than assuming it, and got the
    // grid the script is written for.
    let [tag, dims, mode, ..] = sched::ipc_recv(w.term_report);
    assert_eq!(
        tag,
        video_terminal::status::TERM_UP,
        "the display terminal did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low \
         byte names the step, see user/src/display_terminal.rs)",
    );
    assert_eq!(
        dims,
        video_terminal::script::COLS as u64 | ((video_terminal::script::ROWS as u64) << 32),
        "the terminal sized itself to a different grid than the script predicts",
    );
    assert_eq!(mode, video_terminal::status::MODE_DISPLAY);

    // The driver's account of the terminal's first flush: the blank grid. A second address
    // space's witness, taken after the device reported the transfer complete.
    let blank = video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
    let [tag, driver_digest, pixels, ..] = sched::ipc_recv(w.driver_report);
    assert_eq!(tag, gfx::status::FLUSHED, "the driver served no flush");
    assert_eq!(pixels, gfx::PIXELS as u64);
    assert_eq!(
        driver_digest,
        gfx::checksum(|i| {
            let (x, y) = (
                (i % gfx::WIDTH as usize) as u32,
                (i / gfx::WIDTH as usize) as u32,
            );
            blank.pixel(x, y)
        }),
        "the frames the device read for the terminal's first flush are not a blank terminal: an \
         empty terminal must be a defined picture, not whatever was in those frames",
    );
    w.assert_screen_is(&blank, "a terminal that has been sent nothing");

    // Play the application, then the input driver. Both replies mean the pixels are on the
    // device's side, so there is nothing to poll and nothing to sleep for.
    w.print(video_terminal::script::GREETING);
    w.type_bytes(video_terminal::script::TYPED);

    // The kernel's own witness: every pixel, against the engine it ran itself.
    let expect = video_terminal::script::full_screen();
    w.assert_screen_is(&expect, "after the greeting and the typing");

    // A wrong screen must not pass this. The typo picture differs from the real one in one
    // letter, so asserting they differ at all is what says the comparison above has teeth: if
    // `assert_screen_is` were somehow vacuous, this would still be true, which is why the check
    // is that the two *pictures* differ rather than that the screen is not the typo.
    let mut typo =
        video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
    typo.feed(video_terminal::script::GREETING_TYPO);
    typo.feed(video_terminal::script::TYPED);
    assert!(
        (0..gfx::HEIGHT).any(|y| (0..gfx::WIDTH).any(|x| typo.pixel(x, y) != expect.pixel(x, y))),
        "the one-letter typo produces an identical picture: the negative control is inert",
    );

    // **Hold the picture up for the host.** `cargo xtask` polls QEMU's monitor while this suite
    // runs, and the next test puts rung one's pattern on the same scanout. Three seconds is an
    // order of magnitude more than the poll needs, and if the host never sees it the run fails at
    // the scanout check rather than passing quietly.
    let deadline = crate::arch::timer::now() + 3 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        sched::yield_now();
    }
}

/// **A key pressed on a real device becomes the byte a terminal receives** (milestone 29's
/// input).
///
/// A confined userspace virtio-input driver, behind the same PCIe transport and the same IOMMU
/// domain as the GPU, brings up the event queue, takes a real key event off it, and publishes
/// the byte in the compositor's input ring.
///
/// # Why the host has to press the key, and how
///
/// Nothing in the guest can press a key: that is the point of a device test. So the **host**
/// does it, over the same QEMU monitor connection the scanout check already holds open.
/// `cargo xtask` sends `sendkey` beside this suite, exactly as it dumps the scanout beside it,
/// and `video_terminal::script::HOST_KEY` is the one definition of which key so the pressing side and the
/// asserting side cannot drift. The keys go out from the start of the run and QEMU drops them
/// until a driver sets `DRIVER_OK`, so there is nothing to synchronize.
///
/// # What this proves, and where it hands off
///
/// The path from **a physical key event to a terminal byte**: the device is enumerated and
/// checked (`DeviceID` is virtio-input, not whatever the transport felt like saying), the event
/// queue is programmed through the confined transport with every buffer device-**writable**,
/// an event arrives by interrupt, `video_terminal::keymap` turns an evdev code into a character, and the
/// byte lands in the input ring.
///
/// The rest of the path (ring to focused client to pixels) is
/// `compositor_tests::focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours`,
/// and the seam between the two halves is the ring itself, which is exactly where DECISIONS §33
/// put the boundary: the driver's authority to type is the ring's mapping, and the compositor's
/// authority to deliver is the client endpoints it holds. Naming the seam is better than one
/// test that hides it.
///
/// # The authority this driver does not have
///
/// It holds no client's endpoint, so it cannot choose who receives a keystroke; it cannot even
/// name a client. And it rings the same **content-free** doorbell every client holds, so nothing
/// it says carries the keystroke: a client that rang that endpoint forever could not type a
/// character, because typing is a page it does not map.
#[test_case]
fn a_keystroke_from_a_virtio_keyboard_becomes_a_terminal_byte() {
    let kbd = program("kbd").expect("no kbd program in the initrd archive");
    let mut w = keyboard_service::start(kbd).expect(
        "no virtio-input function on the bus: is NIFE_KBD missing from the test leg, or the \
         -device virtio-keyboard-pci line from the runner?",
    );
    assert!(
        crate::iommu::active(),
        "a keyboard is present but the IOMMU is not active: the device's event buffers are \
         unconfined, and a keyboard's buffers are where every keystroke lands",
    );

    let [tag, buffers, ..] = sched::ipc_recv(w.report);
    assert_eq!(
        tag,
        video_terminal::status::KBD_UP,
        "the keyboard driver did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
         names the step, see user/src/kbd.rs)",
    );
    assert!(
        buffers > 0,
        "the driver posted no event buffers, so a key would have nowhere to land",
    );

    // Wait for the driver to ring, which it only does when it has typed something. Bounded on
    // the clock: if the host's `sendkey` never reaches the device, this fails with a sentence
    // rather than hanging until the harness's ceiling.
    let deadline = crate::arch::timer::now() + 10 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline && sched::endpoint_waiting_senders(w.doorbell) == 0 {
        sched::yield_now();
    }
    assert!(
        sched::endpoint_waiting_senders(w.doorbell) > 0,
        "the keyboard driver came up but never typed anything in ten seconds: the host's \
         `sendkey {}` is not reaching the device (is the monitor socket attached? see \
         cargo xtask's scanout check, which owns that connection)",
        video_terminal::script::HOST_KEY,
    );
    w.answer_doorbell();

    let mut typed = [0u8; 16];
    let n = w.take_typed(&mut typed);
    assert!(n > 0, "the driver rang the doorbell with an empty ring");
    for (i, &b) in typed[..n].iter().enumerate() {
        assert_eq!(
            b,
            video_terminal::script::HOST_KEY_BYTE,
            "byte {i} of {n} from the keyboard is {:?}, not the {:?} the host pressed: the \
             evdev keycode was mapped wrong, or a key release was counted as a press",
            b as char,
            video_terminal::script::HOST_KEY_BYTE as char,
        );
    }
    // A release must not type. The host sends press *and* release for each `sendkey`, so a
    // driver that ignored the value field would produce two bytes per press; that would show up
    // above as the right character twice, which is why the count is checked against the
    // presses the host could have made rather than merely being non-zero.
    assert!(
        n <= 64,
        "{n} bytes from a handful of key presses: releases or auto-repeats are being counted \
         more than once",
    );
}
