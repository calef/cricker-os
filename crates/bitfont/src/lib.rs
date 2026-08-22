//! **A 7x8 bitmap font, and the arithmetic that turns a character into pixels** (milestone 29,
//! the display ladder's text).
//!
//! Rung one put pixels on a screen and rung two multiplexed the screen; neither could show a letter.
//! This is the piece that can, and it is deliberately the smallest thing that could: a constant
//! table of monochrome glyphs and a pure function from `(character, x, y)` to a colour.
//!
//! # Why a bitmap font, and not a scalable one
//!
//! A scalable font (TrueType, or `cosmic-text` above it) wants an allocator, floating point, a
//! rasteriser with hinting and anti-aliasing, and a font file to load from a filesystem. Every one
//! of those is a dependency a `no_std`, allocation-free userspace component would have to acquire
//! before it could draw the letter A. A bitmap font needs none of them: the glyphs are a `static`
//! in `.rodata`, drawing one is a bit test, and the whole thing is `const`-shaped enough that the
//! *expected* picture is a function three independent parties can evaluate (the terminal that
//! draws, the kernel test that checks, and the host that reads the scanout back out of QEMU). That
//! last property is what makes the text on the screen provable rather than merely plausible, and it
//! is the reason to start here even though rung three will eventually want the scalable path.
//!
//! # Examples
//!
//! The whole crate is a pure function from `(byte, x, y)` to a colour, so the expected picture can be
//! *printed*. This is the property the section above claims and it is worth showing rather than
//! asserting: a mirrored font is the classic bitmap-font bug and nearly invisible in review, because
//! half the alphabet is symmetric enough to look fine. `F` is not.
//!
//! ```
//! use bitfont::{GLYPH_H, GLYPH_W, ink};
//!
//! let art: Vec<String> = (0..GLYPH_H)
//!     .map(|y| (0..GLYPH_W).map(|x| if ink(b'F', x, y) { '#' } else { '.' }).collect())
//!     .collect();
//!
//! assert_eq!(
//!     art,
//!     [
//!         ".#####.", // the top bar runs the full five ink columns
//!         ".#.....", // the stem is on the LEFT
//!         ".#.....",
//!         ".####..", // the middle bar is one column short, which is what makes it an F
//!         ".#.....",
//!         ".#.....",
//!         ".#.....",
//!         ".......", // row 7 is the descender row, and F has none
//!     ],
//! );
//! ```
//!
//! The first and last columns are blank in **every** glyph, which is the Kaypro's geometry rather
//! than an accident of this letter: five ink columns with a one-pixel gutter each side.
//!
//! Drawing into a framebuffer is [`cell_pixel`] and nothing else. Three independent parties call it
//! for three different reasons, which is why it is a function rather than a method on a canvas: the
//! terminal to paint, the kernel test to predict, and the host-side scanout check to grade what QEMU
//! is actually displaying.
//!
//! ```
//! use bitfont::{GLYPH_H, GLYPH_W, cell_pixel};
//!
//! const WHITE: u32 = 0x00ff_ffff;
//! const BLACK: u32 = 0x0000_0000;
//!
//! // The cell at column 3, row 0 of a terminal grid, holding a space.
//! let (col, row) = (3u32, 0u32);
//! for y in 0..GLYPH_H {
//!     for x in 0..GLYPH_W {
//!         assert_eq!(cell_pixel(b' ', x, y, WHITE, BLACK), BLACK);
//!     }
//! }
//!
//! // And the grid arithmetic a fixed-pitch font buys: a pixel's cell is a division.
//! let (px, py) = (col * GLYPH_W + 1, row * GLYPH_H + 1); // column 1 is the F's stem
//! assert_eq!((px / GLYPH_W, py / GLYPH_H), (col, row));
//! assert_eq!(cell_pixel(b'F', px % GLYPH_W, py % GLYPH_H, WHITE, BLACK), WHITE);
//! ```
//!
//! Out-of-cell coordinates are not ink rather than a panic, because the callers are pixel loops, and
//! a byte with no glyph draws a visible box rather than nothing:
//!
//! ```
//! use bitfont::{ink, glyph, MISSING};
//!
//! assert!(!ink(b'F', 7, 0)); // past the cell
//! assert!(!ink(b'F', 0, 99));
//!
//! // Everything from 0x80 up is outside basic latin, so it draws the missing-glyph box. A reader
//! // sees that the text is wrong instead of seeing a gap.
//! assert_eq!(glyph(0xe9), &MISSING);
//! assert_ne!(glyph(b'F'), &MISSING);
//! ```
//!
//! # What deliberately is NOT here
//!
//! No kerning, no proportional widths, no anti-aliasing, no Unicode beyond the basic-latin block
//! (see [`glyph`] for what a byte outside it draws), and no font loading: this font is compiled in.
//! The honest limits are listed in notes/glyphs.md.
//!
//! Name: unrecorded. One of the five run-together names milestone 63 reviewed on 2026-08-01
//! (`capsh`, `lineedit`, `uheap`, `nifefs`, `bitfont`) when it deleted the rule that had
//! produced them. Three moved, `nifefs` stayed with a reason recorded, and this one stayed with
//! none.

#![no_std]

pub mod glyphs;

/// A glyph cell's width in pixels.
///
/// The font is a **fixed-pitch** 7x8, which is what lets a terminal grid be `x / GLYPH_W` and
/// nothing more.
///
/// Seven, not eight, and the reason is the Kaypro II's video board: it shifted out a zero, five
/// bits of character generator, and a zero, so the cell is five ink columns with a one-pixel gutter
/// on each side. Every glyph here keeps that gutter (asserted below), which is where the
/// inter-character gap comes from, so the advance is 7 and there is no tracking to add.
///
/// The narrower cell is also what makes the terminal usable: 128 / 7 is **18 columns** where 128 / 8
/// was 16. The scanout is not a whole multiple of 7, so the two rightmost pixels of a full-width
/// surface are outside the grid and stay background; see `user/src/display_terminal.rs`.
pub const GLYPH_W: u32 = 7;

/// A glyph cell's height in pixels. Eight rows, of which the last carries only the descenders
/// (`g j p q y , ;`) and the underscore, so consecutive text rows do not collide.
pub const GLYPH_H: u32 = 8;

/// **The glyph for a byte this font has no picture for**: a hollow box, the convention every
/// terminal uses for "I cannot draw this".
///
/// It exists so that [`glyph`] is *total*. A font that returned `None`, or blank, for an unmapped
/// byte would make a mojibake bug look like a spacing bug, and the whole point of drawing something
/// visible is that a wrong byte in the grid shows up as a wrong picture rather than as nothing.
pub static MISSING: [u8; 8] = [0x3e, 0x22, 0x22, 0x22, 0x22, 0x22, 0x3e, 0x00];

/// **The rows of the glyph for `byte`**, top row first, bit 0 the leftmost pixel.
///
/// Bytes `0x00..=0x7f` come from the drawn table in [`glyphs`]; the control codes in there
/// are blank, which is correct because a terminal never puts a control code in a cell (the VT engine
/// consumes them). Everything from `0x80` up draws [`MISSING`].
///
/// **The grid is bytes, not `char`s, and that is an honest limit**: this font covers basic latin, so
/// a UTF-8 decoder above it would have nothing to draw for what it decoded. When there is a font
/// with the coverage to justify one, the decoder goes in the VT engine and this signature becomes
/// `char`. Recorded in notes/glyphs.md rather than half-built.
pub fn glyph(byte: u8) -> &'static [u8; 8] {
    match glyphs::BASIC.get(byte as usize) {
        Some(g) => g,
        None => &MISSING,
    }
}

/// Is the pixel at `(x, y)` **inside this cell** part of the glyph's ink?
///
/// Out-of-cell coordinates are not ink rather than a panic: the callers are pixel loops, and a
/// bounds check they can rely on is cheaper than one each.
pub fn ink(byte: u8, x: u32, y: u32) -> bool {
    if x >= GLYPH_W || y >= GLYPH_H {
        return false;
    }
    glyph(byte)[y as usize] >> x & 1 != 0
}

/// **The colour a cell shows at `(x, y)`**: `fg` where the glyph has ink, `bg` where it does not.
///
/// This is the whole of glyph rendering, and it is a pure function on purpose. The terminal calls it
/// to paint, the kernel test calls it to predict, and the host-side scanout check calls it to grade
/// what QEMU is actually displaying, so none of the three can be wrong in a way the others agree
/// with.
pub fn cell_pixel(byte: u8, x: u32, y: u32, fg: u32, bg: u32) -> u32 {
    if ink(byte, x, y) { fg } else { bg }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// **The bit order, asserted against a shape that would survive being wrong.**
    ///
    /// A mirrored font is the classic bitmap-font bug and it is nearly invisible in review: half the
    /// alphabet is symmetric enough to look fine. So the assertion is made with `F`, which is
    /// asymmetric in x, and spelled out as the picture rather than as hex.
    #[test]
    fn bit_zero_is_the_leftmost_pixel() {
        let art: Vec<std::string::String> = (0..GLYPH_H)
            .map(|y| {
                (0..GLYPH_W)
                    .map(|x| if ink(b'F', x, y) { '#' } else { '.' })
                    .collect()
            })
            .collect();
        assert_eq!(
            art,
            [
                ".#####.", // the top bar runs the full five ink columns
                ".#.....", // and the stem is on the LEFT
                ".#.....", //
                ".####..", // the middle bar is one column short, which is what makes it an F
                ".#.....", //
                ".#.....", //
                ".#.....", //
                ".......", // row 7 is the descender row, and F has none
            ],
            "the font is mirrored, or the rows are upside down",
        );
    }

    /// Every printable character has a picture, and no two share one.
    ///
    /// A table that was truncated, shifted by one, or transcribed with a dropped row would show up
    /// here as a hole or a collision, and nowhere else until it was on a screen.
    #[test]
    fn every_printable_glyph_is_present_and_distinct() {
        assert!(!glyph(b' ').iter().any(|&r| r != 0), "space must be blank",);
        let mut seen: Vec<(u8, [u8; 8])> = Vec::new();
        for byte in 0x21..=0x7eu8 {
            let g = *glyph(byte);
            assert!(
                g.iter().any(|&r| r != 0),
                "{:?} ({byte:#04x}) has no glyph",
                byte as char,
            );
            if let Some((other, _)) = seen.iter().find(|(_, o)| *o == g) {
                panic!(
                    "{:?} and {:?} have the same glyph: the table is shifted or duplicated",
                    *other as char, byte as char,
                );
            }
            seen.push((byte, g));
        }
        assert_eq!(seen.len(), 0x7e - 0x21 + 1);
    }

    /// A byte with no picture draws the missing-glyph box, and that box is nobody's letter. Total,
    /// so a mojibake bug is visible rather than blank.
    #[test]
    fn an_unmapped_byte_draws_a_box_that_is_not_a_letter() {
        assert_eq!(glyph(0x80), &MISSING);
        assert_eq!(glyph(0xff), &MISSING);
        for byte in 0x20..=0x7eu8 {
            assert_ne!(
                *glyph(byte),
                MISSING,
                "{:?} is indistinguishable from the missing glyph",
                byte as char,
            );
        }
        // Hollow: a filled box would be a solid block, which is a legitimate thing a terminal draws.
        assert!(!ink(0x80, 3, 3), "the missing glyph should be hollow");
        assert!(ink(0x80, 1, 1), "the missing glyph should have a left edge");
        assert!(ink(0x80, 5, 1), "and a right edge");
    }

    /// Control codes are blank. The VT engine consumes them, so one reaching a cell is a bug; if it
    /// ever does, a blank is the failure that does the least damage to the rest of the line.
    #[test]
    fn control_codes_are_blank() {
        for byte in (0x00..0x20u8).chain(core::iter::once(0x7f)) {
            assert!(
                !glyph(byte).iter().any(|&r| r != 0),
                "{byte:#04x} is a control code with ink",
            );
        }
    }

    /// `cell_pixel` is the two-colour choice and nothing else, and it is defined everywhere a pixel
    /// loop might ask, including outside the cell.
    #[test]
    fn a_cell_is_foreground_ink_over_a_background() {
        const FG: u32 = 0x00ff_ffff;
        const BG: u32 = 0x0000_2040;
        let mut lit = 0;
        for y in 0..GLYPH_H {
            for x in 0..GLYPH_W {
                let got = cell_pixel(b'A', x, y, FG, BG);
                assert_eq!(got, if ink(b'A', x, y) { FG } else { BG });
                lit += u32::from(got == FG);
            }
        }
        let cell = GLYPH_W * GLYPH_H;
        assert!(
            (8..cell - 8).contains(&lit),
            "'A' lit {lit} of {cell} pixels, which is a blank or a solid block",
        );
        assert_eq!(cell_pixel(b'A', GLYPH_W, 0, FG, BG), BG, "outside the cell");
        assert_eq!(cell_pixel(b'A', 0, GLYPH_H, FG, BG), BG);
        assert_eq!(
            cell_pixel(b' ', 3, 3, FG, BG),
            BG,
            "a space must show no ink at all",
        );
    }

    /// **The gutter columns are clear in every glyph**, which is the Kaypro's geometry and is what
    /// separates one character from the next.
    ///
    /// Its board shifted out a zero, five bits, and a zero; the ROM could not have inked column 0
    /// or column 6 if it wanted to. Here nothing enforces it but this test, so a glyph drawn one
    /// column too wide would run into its neighbour and read as a rendering bug rather than as a
    /// font that broke its own rule.
    #[test]
    fn every_glyph_keeps_the_gutter_columns_clear() {
        let spilling: Vec<char> = (0x20..=0x7eu8)
            .chain(core::iter::once(0x80))
            .filter(|&b| (0..GLYPH_H).any(|y| ink(b, 0, y) || ink(b, GLYPH_W - 1, y)))
            .map(|b| b as char)
            .collect();
        assert_eq!(
            spilling,
            [],
            "these glyphs ink the gutter, so they touch their neighbours",
        );
        // And the five that remain are really used: a font that had quietly become four columns
        // wide would pass the check above.
        assert!(
            (0x20..=0x7eu8).any(|b| (0..GLYPH_H).any(|y| ink(b, GLYPH_W - 2, y))),
            "no glyph uses the fifth ink column",
        );
    }

    /// **The five ink columns really are five**, and the baseline really is row 6.
    ///
    /// Consistency is what makes a bitmap font read as words rather than as letters, and the two
    /// things a drawn font drifts on are where a letter starts and where it sits. Both are checked
    /// against the whole alphabet rather than against a sample.
    #[test]
    fn the_letters_share_a_baseline_and_a_left_edge() {
        for byte in (b'a'..=b'z').chain(b'A'..=b'Z') {
            let left = (0..GLYPH_W).find(|&x| (0..GLYPH_H).any(|y| ink(byte, x, y)));
            // Column 1 for a letter with a body, column 2 for the narrow ones (`i j l t f`),
            // which are centred in the cell the way a fixed-pitch font centres them. Anything
            // further right is a letter that has drifted.
            assert!(
                matches!(left, Some(1 | 2)),
                "{:?} starts at {left:?}, not in the first two ink columns",
                byte as char,
            );
            assert!(
                (0..GLYPH_W).any(|x| ink(byte, x, 6)),
                "{:?} has nothing on the baseline (row 6)",
                byte as char,
            );
        }
        // Exactly the glyphs that belong below the baseline, and each by one row. More would be a
        // drawing slip; fewer would be a `g` that sits on the baseline like an `o`. `_` is the
        // underscore, which *is* row 7, and `|` is deliberately full height so it cannot be read
        // as an `I` or an `l`.
        let descending: Vec<char> = (0x21..=0x7eu8)
            .filter(|&b| (0..GLYPH_W).any(|x| ink(b, x, GLYPH_H - 1)))
            .map(|b| b as char)
            .collect();
        assert_eq!(descending, [',', ';', '_', 'g', 'j', 'p', 'q', 'y', '|']);
    }
}
