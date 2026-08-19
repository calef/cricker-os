//! **A specimen sheet for an 8-pixel-wide bitmap font**: the same text, in every font, so two of
//! them can be compared by looking rather than by reading a table of byte counts.
//!
//! The crate is a pure function from `(byte, x, y)` to ink, which is the property that lets the
//! terminal, the kernel test and the host-side scanout check agree about what a letter looks like
//! (see the crate docs). It is also what makes this possible: if the picture is a function, the
//! picture can be *printed*, and a font choice becomes something a person can look at instead of
//! something they have to take on trust.
//!
//! Run it on what ships:
//!
//! ```text
//! cargo run -p bitfont --example specimen
//! ```
//!
//! Or on a candidate, in either of the two formats a bitmap font arrives in. GNU Unifont `.hex`
//! (`CODEPOINT:BITS`, one glyph per line, most fonts on the internet) and the `.art` format this
//! tool also writes, which is a `#`/`.` picture per glyph and is the only sane way to *author* one
//! by hand:
//!
//! ```text
//! cargo run -p bitfont --example specimen -- --font unscii-8.hex --name unscii-8
//! cargo run -p bitfont --example specimen -- --font bench/font-options/hand-drawn-8x8.art
//! ```
//!
//! Two rendering modes, and both are needed for different questions. Half blocks (the default) show
//! the font at roughly its real aspect ratio, because a terminal cell is about twice as tall as it
//! is wide and one half block is therefore about square; that is the mode for "does this look
//! good". `--dots` prints one character per pixel, which is the mode for "what exactly is in this
//! glyph", and it is the format the crate's own doctest uses to pin the bit order.
//!
//! # BUGS
//!
//! Half-block output is only faithful if the terminal reading it renders U+2580 and U+2584 at full
//! cell height with no gap. Most do; a few add antialiasing that makes strokes look softer than the
//! font is. `--dots` has no such dependency and disagreeing with it is the tie-breaker.
//!
//! The `.hex` reader takes the low byte of each row and ignores 16-pixel-wide glyphs, because this
//! crate's cell is 8 wide. A 16-wide font loaded here shows its left half, which looks like a
//! clipped font rather than an error.
//!
//! Name: **provisional** (this lane, bench/font-options). "Specimen" is the printing trade's word
//! for exactly this sheet, which is the guard rail the naming tenet keeps, but the name is calef's.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One 8-pixel-wide font: 128 glyphs, each `height` rows, bit 0 the leftmost pixel (this crate's
/// convention, which is the opposite of most font files and is converted on load).
struct Font {
    name: String,
    height: usize,
    glyphs: Vec<Vec<u8>>,
}

impl Font {
    /// The font compiled into the crate, which is what ships today.
    fn shipped() -> Self {
        Font {
            name: "font8x8 (shipped)".into(),
            height: bitfont::GLYPH_H as usize,
            glyphs: (0..128u8).map(|b| bitfont::glyph(b).to_vec()).collect(),
        }
    }

    fn ink(&self, byte: u8, x: usize, y: usize) -> bool {
        if x >= 8 || y >= self.height {
            return false;
        }
        match self.glyphs.get(byte as usize) {
            Some(rows) => rows.get(y).is_some_and(|r| r >> x & 1 != 0),
            None => false,
        }
    }

    /// The bytes of `.rodata` this font's table costs: one byte per row, `height` rows, 128 glyphs.
    fn table_bytes(&self) -> usize {
        128 * self.height
    }

    /// How many of the 95 printable ASCII positions actually carry a picture. A font that is
    /// missing glyphs is not a font you can ship, however good the ones it has look.
    fn printable_drawn(&self) -> usize {
        (0x21..=0x7eu8)
            .filter(|&b| (0..self.height).any(|y| self.ink(b, 0, y) || self.row(b, y) != 0))
            .count()
    }

    fn row(&self, byte: u8, y: usize) -> u8 {
        self.glyphs
            .get(byte as usize)
            .and_then(|g| g.get(y))
            .copied()
            .unwrap_or(0)
    }
}

/// GNU Unifont `.hex`: `CODEPOINT:BITS`, hex, most significant bit leftmost. Only the basic-latin
/// block is kept, and only the 8-wide glyphs (see BUGS).
fn parse_hex(text: &str, name: &str) -> Font {
    let mut by_code: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    for line in text.lines() {
        let Some((code, bits)) = line.split_once(':') else {
            continue;
        };
        let Ok(code) = usize::from_str_radix(code.trim(), 16) else {
            continue;
        };
        if code > 0x7f {
            continue;
        }
        let bits = bits.trim();
        // Two hex digits per row for an 8-wide glyph, four for a 16-wide one (which we clip).
        let (per_row, wide) = if bits.len() % 4 == 0 && bits.len() > 32 {
            (4, true)
        } else {
            (2, false)
        };
        let rows: Vec<u8> = bits
            .as_bytes()
            .chunks(per_row)
            .filter_map(|c| u8::from_str_radix(std::str::from_utf8(&c[..2]).ok()?, 16).ok())
            .map(|b| b.reverse_bits()) // .hex is MSB-left; this crate is LSB-left.
            .collect();
        let _ = wide;
        by_code.insert(code, rows);
    }
    // The height is the commonest row count among the letters, not the largest anywhere in the
    // file. `unscii-8.hex` stores U+0000 as sixteen rows in an eight-row font, and taking the
    // maximum turned every 8x8 specimen into an 8x16 one with the bottom half blank.
    let mut tally: BTreeMap<usize, usize> = BTreeMap::new();
    for (code, rows) in &by_code {
        if (0x41..=0x7a).contains(code) {
            *tally.entry(rows.len()).or_default() += 1;
        }
    }
    let mut height = tally
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map_or(8, |(h, _)| h);
    if height == 0 {
        height = 8;
    }
    let glyphs = (0..128)
        .map(|c| {
            let mut rows = by_code.remove(&c).unwrap_or_default();
            rows.resize(height, 0);
            rows
        })
        .collect();
    Font {
        name: name.into(),
        height,
        glyphs,
    }
}

/// The `.art` format: `@ 41` (a hex code point) followed by that many rows of `#` (ink) and
/// anything else (paper). Blank lines and `;` comments are ignored. This is the format for a font
/// somebody draws, because hexadecimal is not a medium a person can see a letter in.
fn parse_art(text: &str, name: &str) -> Font {
    let mut by_code: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut current: Option<usize> = None;
    let mut height = 0usize;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim_start().starts_with(';') || line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix('@') {
            let code = rest.split_whitespace().next().unwrap_or("");
            current = usize::from_str_radix(code, 16).ok();
            if let Some(c) = current {
                by_code.entry(c).or_default();
            }
            continue;
        }
        let Some(code) = current else { continue };
        let mut row = 0u8;
        for (x, ch) in line.chars().take(8).enumerate() {
            if ch == '#' {
                row |= 1 << x;
            }
        }
        let rows = by_code.entry(code).or_default();
        rows.push(row);
        height = height.max(rows.len());
    }
    let glyphs = (0..128)
        .map(|c| {
            let mut rows = by_code.remove(&c).unwrap_or_default();
            rows.resize(height, 0);
            rows
        })
        .collect();
    Font {
        name: name.into(),
        height,
        glyphs,
    }
}

/// The sample text, which is the same for every font on purpose: a scroll down two specimens is
/// then a fair comparison rather than two different sentences.
///
/// It is chosen for the places 8x8 fonts fail. `Il1|` and `O0` are the confusions that make a
/// terminal font unreadable in a password or a hex dump; `rn` against `m` is the one that makes
/// prose wrong rather than ugly; `g q y p j` are the descenders, which is where an 8-row cell runs
/// out of room; and the last two lines are ordinary prose and ordinary code, because a font that
/// looks good on a pangram and bad in a sentence is bad.
const SAMPLE: &[&str] = &[
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "abcdefghijklmnopqrstuvwxyz",
    "0123456789  Il1|  O0  rn m  8B 5S 2Z",
    "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~",
    "jagged pyqj gravity: quips, quays;",
    "A backup somebody depends on either",
    "works on a Tuesday or it does not.",
    "if x >= GLYPH_W { return false; }",
];

/// The glyphs worth looking at one pixel at a time. `F` is first because it is the one the crate's
/// doctest pins: it is asymmetric in x, so a mirrored table (the classic bitmap-font bug) cannot
/// hide in it the way it hides in half the alphabet.
const DETAIL: &str = "FAagMW01";

/// Two pixel rows per output row, which is about square in a terminal cell and is therefore the
/// mode that shows what the font looks like rather than what is in it.
fn half_blocks(font: &Font, text: &str) -> String {
    let mut out = String::new();
    for band in (0..font.height).step_by(2) {
        for byte in text.bytes() {
            for x in 0..8usize {
                let top = font.ink(byte, x, band);
                let bottom = font.ink(byte, x, band + 1);
                out.push(match (top, bottom) {
                    (true, true) => '\u{2588}',
                    (true, false) => '\u{2580}',
                    (false, true) => '\u{2584}',
                    (false, false) => ' ',
                });
            }
        }
        out.push('\n');
    }
    out
}

/// One character per pixel, side by side, which is the format an argument about a single glyph has
/// to be settled in.
fn dots(font: &Font, text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        let _ = write!(out, "  {:?}    ", byte as char);
    }
    out.push('\n');
    for y in 0..font.height {
        for byte in text.bytes() {
            for x in 0..8usize {
                out.push(if font.ink(byte, x, y) { '#' } else { '.' });
            }
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut name: Option<String> = None;
    let mut want_dots = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--font" => path = args.next(),
            "--name" => name = args.next(),
            "--dots" => want_dots = true,
            other => {
                eprintln!("specimen: unknown argument {other:?}");
                eprintln!("usage: specimen [--font FILE.hex|FILE.art] [--name NAME] [--dots]");
                std::process::exit(2);
            }
        }
    }

    let font = match &path {
        None => Font::shipped(),
        Some(p) => {
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| {
                eprintln!("specimen: {p}: {e}");
                std::process::exit(1);
            });
            let name = name.clone().unwrap_or_else(|| p.clone());
            if p.ends_with(".art") {
                parse_art(&text, &name)
            } else {
                parse_hex(&text, &name)
            }
        }
    };

    println!("=== {} ===", font.name);
    println!(
        "8x{} cell, {} bytes of table for 128 glyphs, {} of the 94 printable positions drawn\n",
        font.height,
        font.table_bytes(),
        font.printable_drawn(),
    );
    for line in SAMPLE {
        print!("{}", half_blocks(&font, line));
        println!();
    }
    if want_dots {
        println!("{}", dots(&font, DETAIL));
    }
}
