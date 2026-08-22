//! **The framebuffer contract** (milestone 29, the display ladder's first rung).
//!
//! Written down as code in one place so the three parties cannot drift: the display driver
//! (`user/src/display.rs`, which owns the virtio-gpu device), the client that draws
//! (`user/src/painter.rs`, which owns no device at all), and the kernel-side test that checks the
//! result. The prose contract is notes/framebuffer-contract.md; this crate is its machine-readable
//! half, the same split `fs_proto` makes for the filesystem and `line_editor::proto` for the terminal.
//!
//! # The shape
//!
//! ```text
//!   virtio-gpu ──virtio──►┌────────────────┐◄──display IPC──┐ client
//!    (PCIe, behind        │ display driver │                │ (holds a surface + an endpoint,
//!     the IOMMU)          └────────────────┘                │  no device, no DMA authority)
//!                              ▲     ▲
//!                              │     └── the surface: N frames of pixels, mapped in BOTH
//!                              └──────── the rings + request buffers, mapped in the DRIVER ONLY
//! ```
//!
//! **Control by message, pixels by shared frame** (DECISIONS §10), the same division the block and
//! file contracts make. A client CALLs the driver with an opcode and a rectangle; the pixels
//! themselves never ride in a message. They live in a run of frames the kernel maps into both
//! address spaces at spawn, and which the client may write at any time without asking.
//!
//! # Who can do what, and why the split is the point
//!
//! The driver holds the `Virtio` capability, the device's interrupt, and a mapping of the whole DMA
//! region. The client holds an endpoint and the surface frames, and that is all: it cannot program a
//! queue, cannot ring the device, cannot see the descriptor rings (they are in a page it is not
//! mapped), and cannot name a physical address. So the worst a hostile client can do is draw
//! nonsense, which is exactly the authority a thing that draws should have. That separation is what
//! rung two (the compositor, milestone 33) is built on, which is why it is drawn this early: the
//! compositor takes the client's place and multiplexes many surfaces into one, and it should not
//! have to invent a contract to do it.
//!
//! # Examples
//!
//! The test pattern is the part of this contract worth demonstrating, because its whole value is
//! **negative**: it is built so that the ways a framebuffer actually fails are all visible, and a
//! solid fill would catch none of them. A buffer shifted by a single pixel is the case that a
//! checksum over a fill, or a "is it all blue?" check, cannot see at all:
//!
//! ```
//! use gfx_proto::{PIXELS, checksum, expected_checksum, first_mismatch, pixel_at};
//!
//! // A client that painted the surface correctly.
//! let good: [u32; PIXELS] = core::array::from_fn(pixel_at);
//! assert_eq!(checksum(|i| good[i]), expected_checksum());
//! assert_eq!(first_mismatch(|i| good[i]), None);
//!
//! // The same pixels, off by one: a stride bug, or a driver that transferred from the wrong offset.
//! let shifted: [u32; PIXELS] = core::array::from_fn(|i| pixel_at(i + 1));
//! assert_ne!(checksum(|i| shifted[i]), expected_checksum());
//! assert_eq!(first_mismatch(|i| shifted[i]), Some(0)); // and it names where
//!
//! // A device that ignored the transfer entirely.
//! assert_ne!(checksum(|_| 0), expected_checksum());
//! ```
//!
//! A damage rectangle is one word, so a flush is one message with no shared-memory preamble, and a
//! rectangle that leaves the surface is **refused rather than clamped**: a clamp would silently
//! absorb a client's coordinate bug.
//!
//! ```
//! use gfx_proto::{HEIGHT, WIDTH, rect, rect_in_surface, unrect};
//!
//! let w = rect(4, 8, 16, 32);
//! assert_eq!(unrect(w), (4, 8, 16, 32));
//! assert!(rect_in_surface(4, 8, 16, 32));
//!
//! assert!(!rect_in_surface(0, 0, WIDTH + 1, HEIGHT)); // one pixel too wide
//! assert!(!rect_in_surface(0, 0, WIDTH, 0)); // empty is not a flush
//! ```
//!
//! # What deliberately is NOT here
//!
//! No fonts, no glyph cache, no VT state, no input. Rung one puts pixels in a scanout and proves
//! they got there; the terminal on top is a later increment and slots in as another client of this
//! same contract (see notes/framebuffer-contract.md, "The seams left open").
//!
//! Name: unrecorded, and it is one of two `*_proto` crates that are. The suffix is settled and
//! checked (milestone 46, 2026-07-30), so its seven siblings are `recorded`: the rule plus the
//! service the stem names produces the whole name. It does not here. `gfx` is an **abbreviation**,
//! which is the first of the three failure modes the naming tenet lists for crate names, and the
//! rule that picks `<graphics>_proto` does not pick which word goes in front of the underscore.
//! Nothing weighs `gfx` against `graphics`, and the sibling case is instructive: milestone 63
//! expanded `credcli` and spelled the service `credentialer` in full while leaving `cred`. So the
//! research is still owed here, not just the ruling. Introduced 2026-07-29 with virtio-gpu
//! enumeration.

#![no_std]

// ================================================================================================
// The surface: geometry and pixel format.
// ================================================================================================

/// The surface's width in pixels.
///
/// **Not square, on purpose.** A square surface makes a stride bug, a transposition, and an x/y swap
/// all invisible: the byte count comes out the same and the checksum of a transposed square can
/// still be built out of the right pixels. 128x64 makes every one of those a size or content
/// mismatch. The lower bound is the device's: QEMU refuses a scanout smaller than 16 in either
/// dimension.
pub const WIDTH: u32 = 128;
/// The surface's height in pixels. See [`WIDTH`] for why this is not equal to it.
pub const HEIGHT: u32 = 64;

/// Bytes per pixel. One 32-bit word, in the format [`FORMAT`] names.
pub const BYTES_PER_PIXEL: u32 = 4;

/// Bytes per row. No padding: a virtio-gpu 2D resource is tightly packed, so the stride is exactly
/// the row's pixels. Named anyway, because every framebuffer bug in history has been a stride bug
/// and the name is where the assumption becomes visible.
pub const STRIDE: u32 = WIDTH * BYTES_PER_PIXEL;

/// The surface's size in bytes.
pub const SURFACE_BYTES: u32 = STRIDE * HEIGHT;

/// The surface's size in 4 KiB frames. The kernel maps exactly this many contiguous frames into both
/// the driver and the client, and it is what makes the framebuffer's memory story a *grant* rather
/// than a special case: those frames sit inside the driver's registered DMA region, so the same
/// validator and the same IOMMU domain that confine a disk's descriptors confine the GPU's pixel
/// reads. See notes/framebuffer-contract.md, "The memory story".
pub const SURFACE_FRAMES: u32 = SURFACE_BYTES.div_ceil(4096);

/// `VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM`, the virtio-gpu 2D format code for this surface.
///
/// Byte 0 is blue, byte 1 green, byte 2 red, byte 3 ignored, which on a little-endian machine reads
/// as the `u32` `0x00RRGGBB`. Both ISAs we target are little-endian here, so [`pixel`] builds that
/// word directly and a single-byte store order never enters the picture.
pub const FORMAT: u32 = 2;

/// The number of pixels in the surface.
pub const PIXELS: usize = (WIDTH as usize) * (HEIGHT as usize);

// Two facts about the geometry that must hold at build time, not at test time, because getting either
// wrong is a device refusal or a wasted frame rather than a wrong answer: QEMU refuses a scanout
// smaller than 16 in either dimension, and a surface that does not fill whole frames would hand the
// client a partial page.
const _: () = assert!(
    WIDTH >= 16 && HEIGHT >= 16,
    "QEMU refuses a scanout smaller than 16 in either dimension",
);
const _: () = assert!(
    SURFACE_BYTES.is_multiple_of(4096),
    "the surface must fill whole frames: it is granted and mapped a frame at a time",
);

/// The byte offset of pixel `(x, y)` from the start of the surface.
pub const fn offset_of(x: u32, y: u32) -> usize {
    (y * STRIDE + x * BYTES_PER_PIXEL) as usize
}

// ================================================================================================
// The test pattern.
// ================================================================================================

/// **The pixel at `(x, y)`: a function of the coordinates, not a fill.**
///
/// A solid fill proves almost nothing. Zeroed memory, a device that ignored the transfer, a client
/// that wrote one pixel and a loop that never advanced, and a buffer written by the wrong process all
/// pass a "is it all blue?" check. So each channel here is a different, coordinate-dependent
/// function, chosen so that the failures a framebuffer actually has are all visible:
///
/// - **red** rises with `x` five times faster than with `y`, so a transposed or x/y-swapped buffer
///   is wrong at almost every pixel;
/// - **green** rises with `y` and, half as fast, with `x`, so a row-stride error shifts it;
/// - **blue** is `x xor 2y` multiplied by an odd number, which is non-monotonic in both axes, so an
///   off-by-one in either direction does not land on a neighbouring value.
///
/// No pixel value is repeated along a row or a column for the whole surface, and no channel is
/// constant, which is what makes [`checksum`] able to catch a shifted, mirrored, truncated, or stale
/// buffer rather than only a blank one.
pub const fn pixel(x: u32, y: u32) -> u32 {
    let r = (x.wrapping_mul(5).wrapping_add(y)) & 0xff;
    let g = (y.wrapping_mul(3).wrapping_add(x >> 1)) & 0xff;
    let b = ((x ^ (y << 1)).wrapping_mul(37)) & 0xff;
    (r << 16) | (g << 8) | b
}

/// The pixel at linear index `i` (row-major), the same value [`pixel`] gives for that coordinate.
pub const fn pixel_at(i: usize) -> u32 {
    let x = (i % WIDTH as usize) as u32;
    let y = (i / WIDTH as usize) as u32;
    pixel(x, y)
}

/// **A position-sensitive digest of the whole surface**, FNV-1a over the pixel words in row-major
/// order.
///
/// `read(i)` yields pixel `i`. It is a closure rather than a slice because the callers read the
/// surface through a mapped page with volatile loads (userspace) or compute it from [`pixel_at`]
/// (the kernel's expected value), and neither is a `&[u32]`.
///
/// FNV-1a is chosen for being a few lines, having no table, and mixing position into the result: the
/// same multiset of pixels in a different order gives a different digest, which is the property that
/// makes a rotated or mirrored surface fail. It is not a cryptographic hash and nothing here needs
/// one; the measured-boot path (`crates/measured_boot`) is where that requirement lives.
pub fn checksum(mut read: impl FnMut(usize) -> u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    for i in 0..PIXELS {
        let w = read(i);
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3); // FNV-1a 64-bit prime
        }
    }
    h
}

/// The digest [`checksum`] gives for a surface holding exactly the pattern. The kernel test computes
/// this independently of any process, so a driver or client that reports a digest is reporting
/// against a value neither of them produced.
pub fn expected_checksum() -> u64 {
    checksum(pixel_at)
}

/// The linear index of the first pixel that is not the pattern, or `None` if every one matches.
/// Reported by the client so a failure names a coordinate instead of only disagreeing on a digest.
pub fn first_mismatch(mut read: impl FnMut(usize) -> u32) -> Option<usize> {
    (0..PIXELS).find(|&i| read(i) != pixel_at(i))
}

/// What [`first_mismatch`] reports on the wire when there was none. `PIXELS` is out of range for a
/// real index, so it cannot be confused with pixel 0.
pub const NO_MISMATCH: u64 = PIXELS as u64;

// ================================================================================================
// The display IPC: what a client may ask the driver for.
// ================================================================================================

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `fs_proto` and `line_editor::proto` use, so all three contracts read alike.
pub const OP_SHIFT: u32 = 56;

/// Build a request's first word from an opcode and a 56-bit operand.
pub const fn req(op: u64, operand: u64) -> u64 {
    (op << OP_SHIFT) | (operand & ((1 << OP_SHIFT) - 1))
}

/// The opcode of a request word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// The operand of a request word.
pub const fn operand(w0: u64) -> u64 {
    w0 & ((1 << OP_SHIFT) - 1)
}

/// The client's requests.
pub mod display {
    /// `CALL(req(INFO, 0), 0)` -> `(0, width | height << 32)`. Ask the surface's geometry rather
    /// than assuming it. The constants in this crate are the compile-time contract; this is the
    /// runtime one, and it is what lets rung two hand a client a surface whose size the client did
    /// not choose.
    pub const INFO: u64 = 1;

    /// `CALL(req(FLUSH, 0), rect)` -> `(0, 0)`, or a negative errno.
    ///
    /// "The pixels in this rectangle of my surface changed; get them onto the screen." The driver
    /// does the two device commands that means (`TRANSFER_TO_HOST_2D` to copy the guest bytes into
    /// the host resource, then `RESOURCE_FLUSH` to put that resource on the scanout) and replies when
    /// the device has completed them. `rect` is [`super::rect`]-packed; an empty or out-of-surface
    /// rectangle is refused rather than clamped, because a clamp hides a client's coordinate bug.
    pub const FLUSH: u64 = 2;
}

/// Pack a damage rectangle into one 56-bit word: `x`, `y`, `w`, `h`, 14 bits each. 14 bits caps a
/// coordinate at 16383, which is past any scanout this rung will drive and keeps the whole rectangle
/// in the operand field so a flush is one message with no shared-memory preamble.
pub const fn rect(x: u32, y: u32, w: u32, h: u32) -> u64 {
    ((x as u64) & 0x3fff)
        | (((y as u64) & 0x3fff) << 14)
        | (((w as u64) & 0x3fff) << 28)
        | (((h as u64) & 0x3fff) << 42)
}

/// Unpack a rectangle packed by [`rect`], as `(x, y, w, h)`.
pub const fn unrect(v: u64) -> (u32, u32, u32, u32) {
    (
        (v & 0x3fff) as u32,
        ((v >> 14) & 0x3fff) as u32,
        ((v >> 28) & 0x3fff) as u32,
        ((v >> 42) & 0x3fff) as u32,
    )
}

/// Is this rectangle wholly inside the surface, and not empty? The driver checks every incoming
/// flush against this before it touches the device.
pub const fn rect_in_surface(x: u32, y: u32, w: u32, h: u32) -> bool {
    w > 0 && h > 0 && x + w <= WIDTH && y + h <= HEIGHT
}

/// The errno a refused request replies with (`-EINVAL`), the same negative-is-an-error convention
/// `fs_proto` sets.
pub const EINVAL: i64 = -22;

// ================================================================================================
// What each process reports to whoever spawned it.
// ================================================================================================

/// The driver's and the client's status messages. Both go to a report endpoint the spawner holds, not
/// to each other: neither process is anyone's supervisor, and the kernel-side test is the one party
/// that gets to compare the two accounts.
pub mod status {
    /// The driver's first message: `send(REPORT, UP, width | height << 32, scanouts)`. The device is
    /// enumerated, the resource is created and backed, and the scanout is set. A spawner that never
    /// sees this knows bring-up, not drawing, is what failed.
    pub const UP: u64 = 0xD15_0001;

    /// The driver's message after it serves its first flush: `send(REPORT, FLUSHED, digest, pixels)`,
    /// where `digest` is [`super::checksum`] over the bytes the driver handed the device.
    ///
    /// This is the **driver-side witness**: a second, independent account of the surface's contents,
    /// taken in a different address space from the client that wrote them and after the device
    /// reported the transfer complete. It is status, not part of the client contract: a client cannot
    /// ask for it and rung two's compositor can ignore it entirely.
    pub const FLUSHED: u64 = 0xD15_0002;

    /// The client's verdict: `send(REPORT, PAINTED, digest, first_mismatch)`, where `digest` is
    /// [`super::checksum`] over the surface as the client reads it back after the flush, and
    /// `first_mismatch` is a pixel index or [`super::NO_MISMATCH`].
    pub const PAINTED: u64 = 0xD15_0003;

    /// The escape attempt's result: `send(REPORT, BACKING, response, 0)`, where `response` is the
    /// device's reply code to a `RESOURCE_ATTACH_BACKING` naming memory outside the driver's grant.
    ///
    /// This is the one hazard a GPU adds that a disk and a NIC do not have. A virtio-gpu's backing
    /// addresses travel in a *device-level command payload*, not in a virtqueue descriptor, so the
    /// transport's shadow-ring validator never sees them: it bounds the descriptor that carries the
    /// command, and the addresses inside that command are opaque to it. The IOMMU is what bounds
    /// them, and this reports whether it did. See notes/framebuffer-contract.md.
    pub const BACKING: u64 = 0xD15_0004;
}

/// `VIRTIO_GPU_RESP_OK_NODATA`, the device's "command accepted" reply. Named here because the escape
/// test's assertion is about this exact value: the device must NOT return it for a backing outside
/// the grant.
pub const RESP_OK_NODATA: u64 = 0x1100;

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    /// The geometry is self-consistent, and the surface is a whole number of frames the kernel can
    /// hand out. A mismatch here would be a contract two processes read differently.
    #[test]
    fn the_geometry_agrees_with_itself() {
        assert_eq!(STRIDE, 512);
        assert_eq!(SURFACE_BYTES, 512 * 64);
        assert_eq!(
            SURFACE_BYTES % 4096,
            0,
            "the surface should fill its frames"
        );
        assert_eq!(SURFACE_FRAMES, 8);
        assert_eq!(PIXELS, 8192);
        assert_eq!(
            offset_of(0, 1),
            STRIDE as usize,
            "row 1 starts one stride in"
        );
        assert_eq!(offset_of(WIDTH - 1, HEIGHT - 1) + 4, SURFACE_BYTES as usize);
        assert_ne!(WIDTH, HEIGHT, "a square surface hides transposition bugs");
    }

    /// **The pattern is not a fill, and it is not symmetric.** These are the properties the pixel
    /// test leans on, so they are asserted rather than assumed: a bug cannot produce this buffer by
    /// accident, and a transposed or shifted buffer cannot match it.
    #[test]
    fn the_pattern_cannot_be_produced_by_accident() {
        // Not constant: no channel is the same everywhere.
        let first = pixel(0, 0);
        assert!(
            (0..PIXELS).any(|i| pixel_at(i) != first),
            "the pattern is a solid fill",
        );

        // Not symmetric in x and y, so a transposition is wrong nearly everywhere.
        let transposed_matches = (0..HEIGHT)
            .flat_map(|y| (0..HEIGHT).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(x, y) == pixel(y, x))
            .count();
        assert!(
            transposed_matches < (HEIGHT * HEIGHT / 8) as usize,
            "the pattern is close to symmetric: a transposed surface would still mostly match",
        );

        // Distinct along a row and along a column, so a one-pixel shift changes the value.
        for x in 0..WIDTH - 1 {
            assert_ne!(pixel(x, 7), pixel(x + 1, 7), "row 7 repeats at x={x}");
        }
        for y in 0..HEIGHT - 1 {
            assert_ne!(pixel(9, y), pixel(9, y + 1), "column 9 repeats at y={y}");
        }

        // The X byte of B8G8R8X8 is left zero, so a device that reads it sees a defined value.
        assert_eq!(pixel(31, 17) >> 24, 0, "the unused byte should be zero");
    }

    /// The checksum catches the four ways a framebuffer goes wrong that a length check does not:
    /// blank, stale, shifted, and one wrong pixel.
    #[test]
    fn the_checksum_catches_a_wrong_surface() {
        let good = expected_checksum();

        let blank = checksum(|_| 0);
        assert_ne!(good, blank, "a zeroed surface matched the pattern");

        let fill = checksum(|_| pixel(0, 0));
        assert_ne!(good, fill, "a solid fill matched the pattern");

        // Shifted by one pixel: every byte is a real pattern byte, in the wrong place.
        let shifted = checksum(|i| pixel_at((i + 1) % PIXELS));
        assert_ne!(good, shifted, "a surface shifted by one pixel matched");

        // Shifted by one whole row, the classic stride bug.
        let row = checksum(|i| pixel_at((i + WIDTH as usize) % PIXELS));
        assert_ne!(good, row, "a surface shifted by one row matched");

        // Exactly one pixel wrong, in the middle.
        let one = checksum(|i| if i == PIXELS / 2 { 0 } else { pixel_at(i) });
        assert_ne!(good, one, "a surface with one wrong pixel matched");
    }

    /// `first_mismatch` finds the pixel a digest can only disagree about, and says nothing when the
    /// surface is right.
    #[test]
    fn first_mismatch_names_the_pixel() {
        assert_eq!(first_mismatch(pixel_at), None);
        assert_eq!(
            first_mismatch(|i| if i == 4242 { 1 } else { pixel_at(i) }),
            Some(4242)
        );
        assert_eq!(
            NO_MISMATCH, PIXELS as u64,
            "the sentinel must not be an index"
        );
    }

    /// A surface built the way the client builds it (write every pixel through a byte buffer, read it
    /// back as words) digests to the expected value. This is the round trip the two userspace
    /// programs make, done on the host where a failure is a millisecond instead of a QEMU boot.
    #[test]
    fn a_byte_buffer_painted_with_the_pattern_digests_correctly() {
        let mut buf = vec![0u8; SURFACE_BYTES as usize];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let o = offset_of(x, y);
                buf[o..o + 4].copy_from_slice(&pixel(x, y).to_le_bytes());
            }
        }
        let digest = checksum(|i| {
            let o = i * 4;
            u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
        });
        assert_eq!(digest, expected_checksum());
    }

    /// The request packing round-trips, and the rectangle guard refuses what it should. The driver
    /// refuses rather than clamps, so these are the cases that must come back false.
    #[test]
    fn requests_and_rectangles_round_trip() {
        let w0 = req(display::FLUSH, rect(3, 5, 120, 50));
        assert_eq!(op(w0), display::FLUSH);
        assert_eq!(unrect(operand(w0)), (3, 5, 120, 50));

        assert!(
            rect_in_surface(0, 0, WIDTH, HEIGHT),
            "the whole surface is legal"
        );
        assert!(
            !rect_in_surface(0, 0, 0, 10),
            "an empty rectangle is refused"
        );
        assert!(
            !rect_in_surface(0, 0, WIDTH, 0),
            "a zero-height rectangle is refused"
        );
        assert!(
            !rect_in_surface(1, 0, WIDTH, HEIGHT),
            "a rectangle past the right edge"
        );
        assert!(
            !rect_in_surface(0, 1, WIDTH, HEIGHT),
            "a rectangle past the bottom edge"
        );
        assert!(
            !rect_in_surface(WIDTH, 0, 1, 1),
            "a rectangle starting off the surface"
        );
    }
    /// Milestone 85: the pattern's whole point is that a wrong buffer is wrong at almost every
    /// pixel, but no test pinned a single pixel's value, so every `&` in the channel math could
    /// become `|` and every shift could reverse with the checksum happily agreeing with itself.
    /// Five hand-computed values, chosen so each operator's mutation changes at least one:
    /// (0,0) is the zero anchor, (1,0) and (0,1) isolate the red and green shifts, (7,3) makes
    /// the xor visible (7 xor 6 is 1), and (300,200) overflows every channel so the masks bite.
    #[test]
    fn the_pattern_is_the_pattern() {
        assert_eq!(pixel(0, 0), 0x00_0000);
        assert_eq!(pixel(1, 0), 0x05_0025);
        assert_eq!(pixel(0, 1), 0x01_034A);
        assert_eq!(pixel(7, 3), 0x26_0C25);
        assert_eq!(pixel(300, 200), 0xA4_EE2C);
        assert_eq!(pixel_at(WIDTH as usize + 1), pixel(1, 1));
    }

    /// FNV-1a's xor is what lets a bit CLEAR change the digest; an or there only accumulates set
    /// bits and collides exactly where it matters. Two surfaces differing in one low bit of one
    /// pixel must digest differently.
    #[test]
    fn the_checksum_sees_a_one_bit_change() {
        let a = checksum(pixel_at);
        let b = checksum(|i| if i == 0 { pixel_at(0) ^ 1 } else { pixel_at(i) });
        assert_ne!(a, b);
    }

    /// The refusal errno is the wire contract's negative-is-an-error convention; a deleted minus
    /// sign turns every refusal into a success code of 22.
    #[test]
    fn the_refusal_is_negative() {
        assert_eq!(EINVAL, -22);
    }
}
