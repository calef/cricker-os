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
//! Or on a candidate, in any of the four formats a bitmap font arrives in. GNU Unifont `.hex`
//! (`CODEPOINT:BITS`, one glyph per line), Adobe `.bdf` (what Terminus, Spleen and most X11 bitmap
//! fonts ship as), the `.art` format this tool also reads, which is a `#`/`.` picture per glyph and
//! is the only sane way to *author* one by hand, and a raw character-generator ROM (`.rom`/`.bin`),
//! which is the format a font arrived in before it was a file:
//!
//! ```text
//! cargo run -p bitfont --example specimen -- --font unscii-8.hex --name unscii-8
//! cargo run -p bitfont --example specimen -- --font ter-u16n.bdf --name terminus-16
//! cargo run -p bitfont --example specimen -- --font bench/font-options/hand-drawn-8x8.art
//! cargo run -p bitfont --example specimen -- --font 81-146a.bin --name kaypro-ii
//! ```
//!
//! **Give `--font` more than once and the fonts are drawn interleaved**, sample line by sample
//! line, with the name in a left gutter. That is the mode a choice is actually made in: two
//! specimens in sections of their own is an inventory, and the eye cannot compare across a scroll.
//! `--metrics` prints the table row for `notes/glyphs.md` instead of asserting the numbers.
//!
//! ```text
//! cargo run -p bitfont --example specimen -- --metrics \
//!     --font 81-146a.bin --name kaypro-ii --font gohufont-14.bdf --name gohufont-14
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
//! clipped font rather than an error. The `.bdf` reader clips the same way, and it reads only
//! the bitmap: `SWIDTH`, `DWIDTH` and the property block are ignored, which is right for a
//! fixed-pitch cell and wrong for anything else.
//!
//! Name: **provisional** (this lane, bench/font-options). "Specimen" is the printing trade's word
//! for exactly this sheet, which is the guard rail the naming tenet keeps, but the name is calef's.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One font: 128 glyphs, each `height` rows of up to 8 pixels, bit 0 the leftmost pixel (this
/// crate's convention, which is the opposite of most font files and is converted on load).
///
/// `advance` is how far the next cell starts, which is **not** always 8. Spleen 5x8 and Terminus
/// 6x12 are narrower than this crate's cell, and drawing them on an 8-pixel pitch shows them
/// looser than they really are, which is a way to make a good font look bad by accident.
struct Font {
    name: String,
    height: usize,
    advance: usize,
    glyphs: Vec<Vec<u8>>,
}

impl Font {
    /// The font compiled into the crate, which is what ships today.
    fn shipped() -> Self {
        Font {
            name: "kaypro-style (shipped)".into(),
            height: bitfont::GLYPH_H as usize,
            advance: bitfont::GLYPH_W as usize,
            glyphs: (0..128u8).map(|b| bitfont::glyph(b).to_vec()).collect(),
        }
    }

    fn ink(&self, byte: u8, x: usize, y: usize) -> bool {
        if x >= self.advance || y >= self.height {
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

    /// Characters and rows this font fits on the display ladder's 128x64 scanout, which is the
    /// number that decides whether a bigger cell is affordable rather than merely nicer.
    fn grid(&self) -> (usize, usize) {
        (SCANOUT_W / self.advance, SCANOUT_H / self.height)
    }

    /// How many of the 95 printable ASCII positions actually carry a picture. A font that is
    /// missing glyphs is not a font you can ship, however good the ones it has look.
    fn printable_drawn(&self) -> usize {
        (0x21..=0x7eu8)
            .filter(|&b| (0..self.height).any(|y| self.row(b, y) != 0))
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
    let advance = 8;
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
        advance,
        glyphs,
    }
}

/// Adobe `.bdf`, which is what Terminus, Spleen, gohufont and every X11 bitmap font ship as.
///
/// Two things have to be right or the font arrives scrambled, and neither is optional. **The rows
/// are most-significant-bit-first, padded to whole bytes**, so this crate's leftmost-is-bit-0
/// convention needs every byte reversed. And **a glyph is positioned by its own bounding box, not
/// by the cell**: `BBX w h xoff yoff` places the bitmap relative to the baseline, which sits
/// `FONTBOUNDINGBOX`'s height plus its y-offset down from the top of the cell. Ignore that and
/// ascenders and descenders land on different baselines, which is exactly the class of bug this
/// tool exists to make visible.
fn parse_bdf(text: &str, name: &str) -> Font {
    let mut cell_w = 8usize;
    let mut cell_h = 0usize;
    let mut cell_yoff = 0i32;
    let mut by_code: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut code: Option<usize> = None;
    let mut bbx = (0i32, 0i32, 0i32, 0i32);
    let mut reading = false;
    let mut rows: Vec<u8> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("FONTBOUNDINGBOX") => {
                let v: Vec<i32> = it.filter_map(|t| t.parse().ok()).collect();
                if v.len() == 4 {
                    cell_w = v[0].clamp(1, 8) as usize;
                    cell_h = v[1].max(0) as usize;
                    cell_yoff = v[3];
                }
            }
            Some("ENCODING") => {
                code = it
                    .next()
                    .and_then(|t| t.parse::<i32>().ok())
                    .map(|c| c as usize);
            }
            Some("BBX") => {
                let v: Vec<i32> = it.filter_map(|t| t.parse().ok()).collect();
                if v.len() == 4 {
                    bbx = (v[0], v[1], v[2], v[3]);
                }
            }
            Some("BITMAP") => {
                reading = true;
                rows.clear();
            }
            Some("ENDCHAR") => {
                reading = false;
                if let Some(c) = code.filter(|&c| c < 128) {
                    let mut cell = vec![0u8; cell_h];
                    let baseline = cell_h as i32 + cell_yoff;
                    let top = baseline - (bbx.1 + bbx.3);
                    for (i, byte) in rows.iter().enumerate() {
                        let y = top + i as i32;
                        if y < 0 || y as usize >= cell_h {
                            continue;
                        }
                        let shift = bbx.2.clamp(-8, 8);
                        let placed = if shift >= 0 {
                            byte >> shift as u32
                        } else {
                            byte << (-shift) as u32
                        };
                        cell[y as usize] |= placed.reverse_bits();
                    }
                    by_code.insert(c, cell);
                }
                code = None;
            }
            Some(tok) if reading => {
                if let Ok(b) = u8::from_str_radix(&tok[..2.min(tok.len())], 16) {
                    rows.push(b);
                }
            }
            _ => {}
        }
    }

    let height = cell_h.max(1);
    let advance = cell_w;
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
        advance,
        glyphs,
    }
}

/// The `.art` format: `@ 41` (a hex code point) followed by that many rows of `#` (ink) and
/// anything else (paper). Blank lines and `;` comments are ignored. This is the format for a font
/// somebody draws, because hexadecimal is not a medium a person can see a letter in.
///
/// **The advance is the width of the widest picture row**, not a constant, because the shipped font
/// is 7 columns and the earlier hand-drawn candidate is 8. Taking it from the file means a drawn
/// font is shown at its own pitch, which is the same rule the `.bdf` reader already follows.
fn parse_art(text: &str, name: &str) -> Font {
    let mut by_code: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut current: Option<usize> = None;
    let mut height = 0usize;
    let mut advance = 0usize;
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
        let mut wide = 0usize;
        for (x, ch) in line.chars().take(8).enumerate() {
            wide = x + 1;
            if ch == '#' {
                row |= 1 << x;
            }
        }
        advance = advance.max(wide);
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
        advance,
        glyphs,
    }
}

/// A raw **character-generator ROM**, which is the format a bitmap font arrived in before it was a
/// file: a chip on the video board, addressed by character code and scanline, whose data lines feed
/// a shift register directly. There is no header and no metadata, so everything below is either
/// derived from the bytes or is a fact about the machine's wiring, and the two are marked apart.
///
/// Three things are derived from the bytes, and each is checkable by looking at the ROM:
///
/// - **The used half.** A 2K part often carries a 1K font, and the unused half is uniform. Kaypro's
///   `81-146a` is blank for its first 1024 bytes because the hardware selects that half during
///   blanking, so the leading uniform 1K-aligned blocks are skipped.
/// - **Rows per glyph**, which is then the used length over 128.
/// - **Inversion.** A font is mostly paper, so if most bits in the used region are set, the part
///   stores ink as zero. MAME says the same thing declaratively with `ROMREGION_INVERT`.
///
/// One thing is a fact about the wiring and is only **inferred** here: how many of the eight data
/// lines reach the shift register. If the top bits are paper in every byte of the font, they are
/// not connected, and the glyph is the remaining span plus one blank gutter column on each side.
/// That gutter is Kaypro's (`kaypro_v.cpp` shifts out `0`, five bits, `0`), not a universal truth.
///
/// # BUGS
///
/// The gutter is assumed to be one column on each side, because that is what the one machine this
/// was written against does. A chargen wired with two blank columns, or with the glyph left-aligned
/// rather than right-aligned, comes out at the wrong pitch and looks loose rather than wrong, which
/// is the hardest kind of error to see. Pass `--advance` to override it.
///
/// Rows are assumed to be contiguous per glyph, low address is the top scanline, and the code point
/// is the ROM index. A machine whose video RAM permutes the code (Kaypro's own `chr ^ 0x80`, which
/// is what puts its font in the second half) is handled by the used-half skip and by nothing else.
fn parse_chargen(bytes: &[u8], name: &str, advance_override: Option<usize>) -> Font {
    // Skip leading 1K blocks that carry no picture at all.
    let mut start = 0;
    while start + 1024 < bytes.len() {
        let block = &bytes[start..start + 1024];
        if block.iter().all(|&b| b == block[0]) {
            start += 1024;
        } else {
            break;
        }
    }
    let used = &bytes[start..];

    // A font is mostly paper. If most bits are set, ink is zero and the part is stored inverted.
    let set: u32 = used.iter().map(|b| b.count_ones()).sum();
    let inverted = set * 2 > used.len() as u32 * 8;
    let norm: Vec<u8> = used
        .iter()
        .map(|&b| if inverted { !b } else { b })
        .collect();

    let height = (norm.len() / 128).max(1);

    // Which data lines are wired: the highest bit that is ever ink, counting from the left.
    let union = norm.iter().fold(0u8, |a, &b| a | b);
    let ink_bits = 8 - union.leading_zeros() as usize;
    let ink_bits = ink_bits.clamp(1, 8);
    let advance = advance_override.unwrap_or((ink_bits + 2).min(8));
    // Place the glyph one column in from the left, converting MSB-left to this crate's bit 0-left.
    let glyphs = (0..128)
        .map(|c| {
            (0..height)
                .map(|r| {
                    let byte = norm.get(c * height + r).copied().unwrap_or(0);
                    let mut row = 0u8;
                    for i in 0..ink_bits {
                        // Bit `ink_bits-1-i` is the i'th pixel from the left of the glyph.
                        if byte >> (ink_bits - 1 - i) & 1 != 0 {
                            let x = i + usize::from(advance > ink_bits);
                            if x < 8 {
                                row |= 1 << x;
                            }
                        }
                    }
                    row
                })
                .collect()
        })
        .collect();

    Font {
        name: name.into(),
        height,
        advance,
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
            for x in 0..font.advance {
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
            for x in 0..font.advance {
                out.push(if font.ink(byte, x, y) { '#' } else { '.' });
            }
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

/// What the shapes measure, over the 52 letters: the row of the table in `notes/glyphs.md`.
///
/// Ink per letter is weight. The two sigmas are consistency, which is what the eye reads as rhythm
/// rather than as any one glyph: the left-edge sigma says whether the letters start in the same
/// place, and the width sigma whether they are the same size. Cap, x-height and descender are the
/// vertical proportions, with the baseline taken from the bottom of `x` and the descender counted
/// as the rows a `g` reaches below it.
///
/// # BUGS
///
/// A font whose `x` or `g` is missing reports a baseline of zero and a nonsense descender rather
/// than refusing, because every candidate so far draws all 94 printable positions and a font that
/// does not is already disqualified by the `printable_drawn` column.
fn metrics(font: &Font) -> String {
    let letters: Vec<u8> = (b'A'..=b'Z').chain(b'a'..=b'z').collect();
    let extent = |byte: u8| -> Option<(usize, usize, usize, usize, usize)> {
        let mut min_x = usize::MAX;
        let mut max_x = 0;
        let mut min_y = usize::MAX;
        let mut max_y = 0;
        let mut ink = 0;
        for y in 0..font.height {
            for x in 0..font.advance {
                if font.ink(byte, x, y) {
                    ink += 1;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        (ink > 0).then_some((min_x, max_x, min_y, max_y, ink))
    };

    let seen: Vec<_> = letters.iter().filter_map(|&b| extent(b)).collect();
    let n = seen.len().max(1) as f64;
    let mean = |v: &[f64]| v.iter().sum::<f64>() / n;
    let sigma = |v: &[f64]| {
        let m = mean(v);
        (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n).sqrt()
    };

    let ink: Vec<f64> = seen.iter().map(|e| e.4 as f64).collect();
    let left: Vec<f64> = seen.iter().map(|e| e.0 as f64).collect();
    let width: Vec<f64> = seen.iter().map(|e| (e.1 - e.0 + 1) as f64).collect();

    let cap = extent(b'H').map_or(0, |e| e.3 - e.2 + 1);
    let x_ext = extent(b'x');
    let x_height = x_ext.map_or(0, |e| e.3 - e.2 + 1);
    let baseline = x_ext.map_or(0, |e| e.3);
    let descender = extent(b'g').map_or(0, |e| e.3.saturating_sub(baseline));

    format!(
        "| `{}` | {:.1} | {:.2} | {:.2} | {} | {} | {} |",
        font.name,
        mean(&ink),
        sigma(&left),
        sigma(&width),
        cap,
        x_height,
        descender,
    )
}

/// Load one font, picking the reader by extension. A character-generator ROM is bytes rather than
/// text, so it is read as bytes and everything else is read as UTF-8.
fn load(path: &str, name: Option<String>, advance: Option<usize>) -> Font {
    let name = name.unwrap_or_else(|| path.to_string());
    if path.ends_with(".rom") || path.ends_with(".bin") {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("specimen: {path}: {e}");
            std::process::exit(1);
        });
        return parse_chargen(&bytes, &name, advance);
    }
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("specimen: {path}: {e}");
        std::process::exit(1);
    });
    let mut font = if path.ends_with(".art") {
        parse_art(&text, &name)
    } else if path.ends_with(".bdf") {
        parse_bdf(&text, &name)
    } else {
        parse_hex(&text, &name)
    };
    if let Some(a) = advance {
        font.advance = a;
    }
    font
}

/// The one-line summary a font is judged on before anyone looks at it: the cell, what the table
/// costs, how much of ASCII is actually drawn, and the grid it gives on the display ladder's
/// scanout, which is the number that decides whether a taller cell is affordable.
fn summary(font: &Font) -> String {
    let (cols, rows) = font.grid();
    format!(
        "{}x{} cell, {} B table, {}/94 printable drawn, {cols}x{rows} on {}x{}",
        font.advance,
        font.height,
        font.table_bytes(),
        font.printable_drawn(),
        SCANOUT_W,
        SCANOUT_H,
    )
}

/// The scanout the grid is measured against, which is `gfx_proto::WIDTH` and `HEIGHT` on the
/// display ladder today. Written here rather than depended on, because this example is a bench
/// tool and a dependency on the protocol crate would make it part of the shipping graph.
const SCANOUT_W: usize = 128;
const SCANOUT_H: usize = 64;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut paths: Vec<String> = Vec::new();
    let mut names: Vec<Option<String>> = Vec::new();
    let mut advance: Option<usize> = None;
    let mut want_dots = false;
    let mut want_metrics = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--font" => {
                paths.push(args.next().unwrap_or_default());
                names.push(None);
            }
            "--name" => {
                // Names the most recent --font, so several pairs can be given on one line.
                if let Some(slot) = names.last_mut() {
                    *slot = args.next();
                } else {
                    let _ = args.next();
                }
            }
            "--advance" => advance = args.next().and_then(|v| v.parse().ok()),
            "--dots" => want_dots = true,
            "--metrics" => want_metrics = true,
            other => {
                eprintln!("specimen: unknown argument {other:?}");
                eprintln!(
                    "usage: specimen [--font FILE.hex|.bdf|.art|.rom [--name NAME]]... \
                     [--advance N] [--dots] [--metrics]"
                );
                std::process::exit(2);
            }
        }
    }

    let fonts: Vec<Font> = if paths.is_empty() {
        vec![Font::shipped()]
    } else {
        paths
            .iter()
            .zip(names)
            .map(|(p, n)| load(p, n, advance))
            .collect()
    };

    // The gutter carries the font's name on the first row of its block, so two fonts can be read
    // adjacently without a heading between every pair of lines. Comparison is the whole point: a
    // specimen in a section of its own is an inventory, not a comparison.
    let gutter = fonts
        .iter()
        .map(|f| f.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;

    for font in &fonts {
        println!("=== {} ===\n{}", font.name, summary(font));
    }
    println!();

    if want_metrics {
        println!(
            "| Font | Ink per letter | Left edge sigma | Width sigma | Cap | x-height | Descender |"
        );
        println!("|---|---|---|---|---|---|---|");
        for font in &fonts {
            println!("{}", metrics(font));
        }
        println!();
    }

    for line in SAMPLE {
        for font in &fonts {
            let block = half_blocks(font, line);
            for (i, row) in block.lines().enumerate() {
                let label = if i == 0 { font.name.as_str() } else { "" };
                println!("{label:<gutter$}{row}");
            }
        }
        println!();
    }
    if want_dots {
        for font in &fonts {
            println!("--- {} ---", font.name);
            println!("{}", dots(font, DETAIL));
        }
    }
}
