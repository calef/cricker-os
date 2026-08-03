use super::*;
use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
use crate::sched::EpId;

/// Where the driver maps its whole DMA region. Must match user/src/display.rs `DMA_VA`.
const DMA_VA: u64 = 0x0000_0000_0090_0000;
/// Where the client maps the surface. Must match user/src/painter.rs `SURFACE_VA`.
const SURFACE_VA_CLIENT: u64 = 0x0000_0000_0060_0000;

/// The DMA region, in frames: one for the rings and control buffers, then the surface.
const DMA_FRAMES: u64 = 1 + gfx_proto::SURFACE_FRAMES as u64;

/// The driver binary's escape-attempt role; must match user/src/display.rs `ROLE_BACKING_ESCAPE`.
const ROLE_BACKING_ESCAPE: u64 = 1;

/// **Wire and spawn the display driver and the painting client.** Returns
/// `(driver report, client report)`, or `None` if no virtio-gpu function is on the bus.
///
/// One spawn site for two processes on purpose: they are only meaningful together (a driver with
/// no client serves nobody, a client with no driver blocks on its first CALL), and the endpoint
/// and the shared frames that join them are created here, in the one place that is allowed to
/// know both halves.
pub fn start(driver_image: &'static [u8], client_image: &'static [u8]) -> Option<(EpId, EpId)> {
    let (driver_report, display_ep, surface) = wire_driver(driver_image, 0, 0)?;

    // --- the client: an endpoint and the pixels. Nothing else, which is the point. ---
    let client_report = crate::sched::create_endpoint();
    let mut client_maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; gfx_proto::SURFACE_FRAMES as usize];
    for (k, m) in client_maps.iter_mut().enumerate() {
        m.va = SURFACE_VA_CLIENT + k as u64 * FRAME_SIZE;
        m.phys = surface + k as u64 * FRAME_SIZE;
    }
    crate::sched::spawn(move || {
        run(
            client_image,
            Spawn {
                arg0: 0,
                arg1: 0, // no physical address: a client has no business knowing one
                arg2: 0,
                grants: &[
                    endpoint_cap(client_report, Rights::WRITE), // slot 0: its verdict
                    endpoint_cap(display_ep, Rights::WRITE),    // slot 1: CALL the driver
                ],
                maps: &client_maps,
            },
        )
    })
    .expect("could not spawn the painting client");

    Some((driver_report, client_report))
}

/// **Spawn a driver that attacks its own confinement** (user/src/display.rs `run_backing_escape`):
/// it asks the device to read pixels out of a frame outside its grant. Returns
/// `(report endpoint, the victim frame's physical address)`, or `None` if no GPU is on the bus.
///
/// It gets exactly the honest driver's world, no more: the same confined transport, the same
/// region, the same interrupt. That is what makes it a fair test of the barrier rather than of a
/// missing capability. No client, because it never serves one.
///
/// The **kernel** picks the victim frame and hands it over in `arg2`, the same way milestone 16b's
/// confinement test picks its own escape frame: the caller has to know the exact address to look
/// for in the IOMMU's fault queue, and a driver guessing at "the frame past my region" guesses
/// wrong (the shadow page is allocated right after it, and that frame IS in the domain). The frame
/// is deliberately never freed: it is an escape target, and handing it back to the allocator while
/// a device has been told to read it is the use-after-free-by-hardware notes/dma.md warns about.
pub fn start_backing_escape(driver_image: &'static [u8]) -> Option<(EpId, u64)> {
    let victim = crate::memory::alloc()
        .expect("no victim frame for the backing-escape test")
        .addr();
    let (report, _, _) = wire_driver(driver_image, ROLE_BACKING_ESCAPE, victim)?;
    Some((report, victim))
}

/// The shared half of both spawns: find the GPU, build the DMA region, route the interrupt,
/// register the confined transport, and spawn `driver_image` at `role` with `arg2`. Returns
/// `(report endpoint, display endpoint, the surface's physical base)`.
fn wire_driver(driver_image: &'static [u8], role: u64, arg2: u64) -> Option<(EpId, EpId, u64)> {
    let d = crate::pci::find_gpu_device()?;

    // The DMA region: contiguous, because the surface must be one run of physical frames for the
    // device's backing to be a single memory entry and for the IOMMU domain to cover it as one
    // range. Zeroed, so neither a stale descriptor nor a stale pixel is ever visible to the
    // device or to the client.
    let dma = crate::memory::alloc_contiguous(DMA_FRAMES as usize)
        .expect("no contiguous DMA region for the display driver")
        .addr();
    // SAFETY: a fresh contiguous run of frames, reachable through the direct map, owned by
    // nobody else.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(dma) as *mut u8,
            0,
            (DMA_FRAMES * FRAME_SIZE) as usize,
        );
    }
    let surface = dma + FRAME_SIZE; // page 1 onward: the frames the client also maps

    // The device's interrupt, routed to an endpoint so the driver's WAIT receives it as a
    // message (milestone 9a).
    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(d.intid, irq_ep);
    crate::arch::irq::enable(d.intid);

    // Register the transport: the kernel keeps the registers and the two DMA-critical powers,
    // and confines the device in hardware to exactly this region plus the shadow page.
    let vid = crate::virtio::register(
        crate::virtio::Transport::pci(&d),
        dma,
        DMA_FRAMES * FRAME_SIZE,
        Some(d.rid), // the PCIe requester id the IOMMU keys its tables on
    );

    let display_ep = crate::sched::create_endpoint(); // client WRITE (CALL) -> driver READ
    let driver_report = crate::sched::create_endpoint();

    // --- the driver: the confined transport, the interrupt, the whole DMA region, and the
    // display endpoint's serving half. ---
    let mut driver_maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; DMA_FRAMES as usize];
    for (k, m) in driver_maps.iter_mut().enumerate() {
        m.va = DMA_VA + k as u64 * FRAME_SIZE;
        m.phys = dma + k as u64 * FRAME_SIZE;
    }
    crate::sched::spawn(move || {
        run(
            driver_image,
            Spawn {
                arg0: role, // 0 = the display driver; 1 = the escape attempt
                arg1: dma,  // the DMA region's PHYSICAL base: descriptors speak physical
                arg2,       // the escape role's victim frame; unused (0) by the display driver
                grants: &[
                    endpoint_cap(driver_report, Rights::WRITE), // slot 0: status
                    irq_cap(d.intid),                           // slot 1: the completion IRQ
                    virtio_cap(vid),                            // slot 2: the confined transport
                    endpoint_cap(display_ep, Rights::READ),     // slot 3: serve clients
                ],
                maps: &driver_maps,
            },
        )
    })
    .expect("could not spawn the display driver");

    Some((driver_report, display_ep, surface))
}

/// **Wire and spawn the display driver alone**, with no client: `(report endpoint, display
/// endpoint, the surface's physical base)`, or `None` if no virtio-gpu is on the bus.
///
/// For rung two (milestone 33). The compositor takes `painter`'s place at this seam exactly as the
/// contract promised it would, so what it needs from rung one is a display endpoint to CALL and the
/// frames the device scans out. Nothing about the driver changes, which is the claim
/// notes/framebuffer-contract.md made when it said routing was by endpoint.
pub fn start_driver(driver_image: &'static [u8]) -> Option<(EpId, EpId, u64)> {
    wire_driver(driver_image, 0, 0)
}

/// Where the display terminal maps the page an application writes text into. Must match
/// `user/src/display_terminal.rs` `OUT_VA`.
const OUT_VA_TERM: u64 = 0x0000_0000_0068_0000;

/// What the kernel keeps after wiring a display terminal onto the scanout.
pub struct TerminalWiring {
    /// The display driver's status endpoint.
    pub driver_report: EpId,
    /// The terminal's status endpoint.
    pub term_report: EpId,
    /// The endpoint the terminal serves. The kernel holds WRITE, so it can play **both** classes
    /// of sender: an application (`OP_WRITE`) and an input source (`OP_BYTES`).
    pub term: EpId,
    /// The application's output page, so the kernel can put the bytes of an `OP_WRITE` there.
    pub out: u64,
    /// The scanout frames, so the kernel can read the picture back through the direct map and
    /// grade it against a value it computed itself.
    pub surface: u64,
}

/// **Wire and spawn the display driver with a terminal on the whole scanout** (milestone 29's
/// remaining increment). `None` if no virtio-gpu function is on the bus.
///
/// The terminal takes `painter`'s place at the display seam with **exactly `painter`'s
/// authority**: a report endpoint, the display endpoint, and the surface frames. It holds no
/// device, no interrupt, and no physical address, and `display` cannot tell it from the client that
/// drew a test pattern. That is the answer to "did the framebuffer contract need changing to
/// carry text?", and it is an answer made of a spawn literal rather than an argument.
///
/// What it adds over `painter`'s wiring is two things, and both are the terminal contract's, not
/// the framebuffer's: an endpoint it **serves** (the terminal contract's IPC half), and a page an
/// application writes bytes into (DECISIONS §10's control-by-message, bulk-by-shared-page split).
pub fn start_terminal(
    driver_image: &'static [u8],
    term_image: &'static [u8],
) -> Option<TerminalWiring> {
    // A scanout that is not a whole number of character cells would leave a strip the terminal
    // never paints. A build error, not a runtime surprise.
    const _: () = assert!(
        gfx_proto::WIDTH.is_multiple_of(bitfont::GLYPH_W)
            && gfx_proto::HEIGHT.is_multiple_of(bitfont::GLYPH_H),
        "the scanout is not a whole number of character cells",
    );
    // And the script's geometry is the scanout's, checked here rather than trusted, because the
    // script is what three independent parties predict the picture from.
    const _: () = assert!(
        gfx_proto::WIDTH / bitfont::GLYPH_W == video_terminal::script::COLS
            && gfx_proto::HEIGHT / bitfont::GLYPH_H == video_terminal::script::ROWS,
        "video_terminal::script's geometry and the scanout's have drifted apart",
    );

    let (driver_report, display_ep, surface) = wire_driver(driver_image, 0, 0)?;

    let out = crate::memory::alloc()
        .expect("no output-page frame for the display terminal")
        .addr();
    // SAFETY: a fresh frame, direct-mapped, owned by nobody yet.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(out) as *mut u8, 0, FRAME_SIZE as usize);
    };

    let term_report = crate::sched::create_endpoint();
    let term = crate::sched::create_endpoint();

    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; gfx_proto::SURFACE_FRAMES as usize + 1];
    for (k, m) in maps
        .iter_mut()
        .take(gfx_proto::SURFACE_FRAMES as usize)
        .enumerate()
    {
        m.va = SURFACE_VA_CLIENT + k as u64 * FRAME_SIZE;
        m.phys = surface + k as u64 * FRAME_SIZE;
    }
    maps[gfx_proto::SURFACE_FRAMES as usize] = Mapping {
        va: OUT_VA_TERM,
        phys: out,
        flags: Flags::user_data(),
    };

    crate::sched::spawn(move || {
        run(
            term_image,
            Spawn {
                arg0: video_terminal::status::MODE_DISPLAY,
                arg1: 0, // no physical address: a terminal has no business knowing one
                arg2: 0,
                grants: &[
                    endpoint_cap(term_report, Rights::WRITE), // slot 0: status
                    endpoint_cap(display_ep, Rights::WRITE),  // slot 1: CALL the driver
                    endpoint_cap(term, Rights::READ),         // slot 2: serve the terminal
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the display terminal");

    Some(TerminalWiring {
        driver_report,
        term_report,
        term,
        out,
        surface,
    })
}

impl TerminalWiring {
    /// **Play the application**: put `text` in the output page and `OP_WRITE` it.
    ///
    /// Returns when the terminal has drawn it and the display driver has put it on the scanout,
    /// because that is what the terminal contract says an `OP_WRITE` reply means (the bytes are
    /// on the console's side). So a test needs no polling and no sleep between writes.
    pub fn print(&self, text: &[u8]) {
        super::term_print(self.out, self.term, text);
    }

    /// **Play the input driver**: `OP_BYTES` these keystrokes, eight to a message.
    ///
    /// Byte for byte the framing `user/src/input.rs` sends and the compositor forwards
    /// (DECISIONS §33), which is the point: the display terminal is fed by the same driver half
    /// as the serial one, so neither contract had to grow anything to carry a keystroke to a
    /// screen.
    pub fn type_bytes(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut w1 = 0u64;
            for (k, &b) in chunk.iter().enumerate() {
                w1 |= (b as u64) << (8 * k);
            }
            let w0 = line_editor::proto::req(line_editor::proto::OP_BYTES, chunk.len() as u64);
            assert_eq!(
                crate::sched::ipc_call(self.term, [w0, w1])[0],
                0,
                "the terminal refused a keystroke",
            );
        }
    }

    /// A scanout pixel, read by the **kernel** through the direct map: a witness that belongs to
    /// no process in userspace.
    pub fn screen_pixel(&self, x: u32, y: u32) -> u32 {
        let at = mmu::phys_to_virt(self.surface) + (y * gfx_proto::WIDTH + x) as u64 * 4;
        // SAFETY: inside the scanout frames this kernel allocated.
        unsafe { core::ptr::read_volatile(at as *const u32) }
    }

    /// **The scanout holds exactly the picture `expect` describes.** Compared pixel for pixel
    /// rather than by digest, so a failure names a coordinate.
    pub fn assert_screen_is(&self, expect: &video_terminal::Vt, what: &str) {
        for y in 0..gfx_proto::HEIGHT {
            for x in 0..gfx_proto::WIDTH {
                let (got, want) = (self.screen_pixel(x, y), expect.pixel(x, y));
                assert_eq!(
                    got,
                    want,
                    "{what}: the framebuffer is wrong at ({x},{y}) [cell ({},{})]: {got:#010x}, \
                     expected {want:#010x}",
                    x / bitfont::GLYPH_W,
                    y / bitfont::GLYPH_H,
                );
            }
        }
    }
}
