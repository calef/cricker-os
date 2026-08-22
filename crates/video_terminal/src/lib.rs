//! **The VT state engine: the grid a display terminal keeps** (milestone 29, the display ladder's
//! text).
//!
//! Bytes in, a character grid out, plus the rectangle that changed. Sans-IO, exactly as the
//! `line_editor` crate is: this crate holds no endpoint, makes no syscall, and has never heard of a
//! framebuffer. `user/src/display_terminal.rs` feeds it and paints what it says.
//!
//! # Why this shape
//!
//! DECISIONS §7: pure logic belongs in a crate that compiles for the host, so most tests run in
//! milliseconds. A VT engine is almost entirely pure logic (a parser and a two-dimensional array),
//! and the parts that are not (a shared surface, an IPC endpoint) are the parts a QEMU boot has to
//! prove anyway.
//!
//! It buys something specific here beyond fast tests. Because the engine is a value with no IO, the
//! **expected picture is computable by anyone holding the same script**: the terminal component runs
//! it to draw, the kernel-side test runs it to predict what should be in the framebuffer, and
//! `cargo xtask` runs it on the host to grade what QEMU is actually scanning out. Three independent
//! witnesses, one definition, no possibility of the three agreeing on a wrong answer.
//!
//! # Examples
//!
//! Bytes in, a grid out, and the rectangle that changed. Because the engine is a value with no IO,
//! the expected picture is computable by anyone holding the same bytes, which is what lets three
//! independent witnesses agree about what should be on the screen.
//!
//! ```
//! use video_terminal::Vt;
//!
//! let mut vt = Vt::new(8, 4);
//! vt.feed(b"hello\r\nworld");
//!
//! let mut row = [0u8; 8];
//! let n = vt.row_bytes(0, &mut row);
//! assert_eq!(&row[..n], b"hello   "); // the rest of the row is blanks, not garbage
//! vt.row_bytes(1, &mut row);
//! assert_eq!(&row[..5], b"world");
//! assert_eq!(vt.cursor(), (5, 1));
//!
//! // Damage is in cells and is taken, not read: the caller repaints exactly what changed and the
//! // record clears. That is the whole reason the engine reports a rectangle instead of "redraw".
//! let dirty = vt.take_damage().expect("something changed");
//! assert!(dirty.rows >= 2, "two rows were written to");
//! assert_eq!(vt.take_damage(), None); // taken once
//! ```
//!
//! **Deferred wrap** is the one subtlety in printing, and it is the reason a line that exactly fills
//! the width does not scroll the screen before anything asked it to:
//!
//! ```
//! use video_terminal::Vt;
//!
//! let mut vt = Vt::new(4, 4);
//! vt.feed(b"abcd"); // exactly fills row 0
//!
//! // The cursor stays on the last cell it wrote, with a wrap pending. It has NOT moved to row 1.
//! assert_eq!(vt.cursor(), (3, 0));
//!
//! // A CR arriving now finds the cursor on row 0, which is where a line discipline expects it. If
//! // the wrap had already happened it would be a row too low.
//! vt.feed(b"\r");
//! assert_eq!(vt.cursor(), (0, 0));
//!
//! // And when the next printable byte does arrive without an intervening CR, it wraps first.
//! let mut vt = Vt::new(4, 4);
//! vt.feed(b"abcde");
//! assert_eq!(vt.cursor(), (1, 1));
//! let mut row = [0u8; 4];
//! vt.row_bytes(1, &mut row);
//! assert_eq!(row[0], b'e');
//! ```
//!
//! The escape sequences are the ones a line discipline actually emits, plus the screen verbs any
//! program expects:
//!
//! ```
//! use video_terminal::Vt;
//!
//! let mut vt = Vt::new(8, 4);
//! vt.feed(b"one\r\ntwo\r\n");
//!
//! // CSI H homes the cursor; CSI 2J clears the screen.
//! vt.feed(b"\x1b[2J\x1b[H");
//! assert_eq!(vt.cursor(), (0, 0));
//! let mut row = [0u8; 8];
//! let n = vt.row_bytes(0, &mut row);
//! assert_eq!(&row[..n], b"        ");
//! ```
//!
//! # What it implements, and why exactly this set
//!
//! The escape sequences here are **the ones the line discipline already emits** (DECISIONS §21,
//! notes/terminal-contract.md), plus the screen verbs any program expects. That is not a guess: the
//! interoperability test in this crate runs the real `line_editor` and feeds its echo stream to this
//! parser, so the two components are checked against each other rather than against a list somebody
//! wrote down.
//!
//! - Printable bytes, with **deferred wrap** at the right margin (see [`Vt::feed`]).
//! - `CR`, `LF` (with scrolling), `BS`, `TAB`, `BEL` (ignored: there is no bell here).
//! - `CSI A/B/C/D` cursor motion, `CSI H` / `CSI f` absolute positioning.
//! - `CSI J` erase in display, `CSI K` erase in line, both with all three modes.
//! - `CSI m` (SGR): reset, bold, reverse, and the eight ANSI foreground and background colours plus
//!   the bright foregrounds.
//! - `ESC c` (RIS), a full reset.
//!
//! Anything else is **swallowed whole**, introducer and all, rather than printed as garbage.
//!
//! # What deliberately is NOT here
//!
//! No scrollback (a live grid only), no UTF-8 (the grid holds bytes; see [`bitfont::glyph`]), no
//! alternate screen, no origin mode, no scrolling regions, no tab stops other than every eight
//! columns, no mouse, and no reporting sequences at all: this engine never writes to its input,
//! which is what "sans-IO" means here and what keeps it a *value*. The honest limits are listed in
//! notes/glyphs.md.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `vt`. Refused `vt` (two letters that
//! read as *vector table* in a kernel), `virtual_terminal` (wrong twice: that is not what VT stood
//! for, and "virtual terminal" already names Linux's virtual consoles) and `screen_grid` (the crate
//! carries 63 escape-sequence references, so the grid is the output and interpreting the protocol
//! is the work). Deliberately not the program's name: this crate is named for the protocol it
//! implements (DEC's Video Terminals) and `display_terminal` for its role.

#![no_std]

pub mod keymap;
pub mod script;

// ================================================================================================
// Geometry.
// ================================================================================================

/// The widest grid this engine can hold, in cells.
///
/// Fixed rather than allocated, because the terminal component has no allocator and this crate is
/// the same code the kernel and the host run. 32 by 16 covers the current 128x64 scanout at four
/// times over; a screen bigger than that gets a bigger constant, and the terminal component asserts
/// its own geometry fits at compile time so the failure is a build error rather than a truncated
/// screen.
pub const MAX_COLS: usize = 32;
/// The tallest grid this engine can hold, in cells. See [`MAX_COLS`].
pub const MAX_ROWS: usize = 16;
/// Cells in the largest possible grid. At two bytes a cell this is 1 KiB of `.bss`.
pub const MAX_CELLS: usize = MAX_COLS * MAX_ROWS;

// ================================================================================================
// Colour.
// ================================================================================================

/// **The sixteen ANSI colours**, as `0x00RRGGBB` words in the surface's own pixel format.
///
/// Indices 0..8 are the normal colours in the usual ANSI order (black, red, green, yellow, blue,
/// magenta, cyan, white) and 8..16 their bright forms. The values are the widely used xterm set
/// rather than pure primaries, because pure primaries on a 128x64 surface make a *pretty* screen and
/// a bad test: two channels at 0 and one at 255 means half the failure modes (a swapped channel, a
/// dropped shift) produce another legal colour. These have all three channels distinct in most
/// entries.
///
/// # BUGS
///
/// **That last sentence is false, measured 2026-08-19, and the argument above does not hold as
/// written.** *No* entry has three distinct channel values: every one of the sixteen is built from
/// at most two levels (`0xcd0000` is `cd,00,00`; `0xe5e5e5` is one level three times). And **eight
/// pairs are related by a channel permutation**, so a swapped channel turns one legal palette colour
/// into another legal palette colour, which is exactly the failure this palette claims to catch.
/// `(1, 2)` is red and green: swap red and green and red becomes green, undetected.
///
/// The *shape* of the argument is right and the palette does not implement it. What it does buy is
/// real but smaller: values at `0xcd` and `0xe5` rather than `0xff` mean a dropped shift or a
/// saturating write lands off-palette. **Nothing gates any of this**, which is why a false claim sat
/// in a comment; a check that every entry has three distinct channels, and that no two entries are
/// permutations of each other, is a few lines and is milestone 141's first piece.
///
/// This matters beyond tidiness because the claim is what makes the palette ugly on purpose. See
/// design/roadmap/141-a-palette-worth-looking-at.md.
pub const PALETTE: [u32; 16] = [
    0x0000_0000, // 0 black
    0x00cd_0000, // 1 red
    0x0000_cd00, // 2 green
    0x00cd_cd00, // 3 yellow
    0x0000_00ee, // 4 blue
    0x00cd_00cd, // 5 magenta
    0x0000_cdcd, // 6 cyan
    0x00e5_e5e5, // 7 white
    0x007f_7f7f, // 8 bright black (grey)
    0x00ff_5c5c, // 9 bright red
    0x0000_ff00, // 10 bright green
    0x00ff_ff00, // 11 bright yellow
    0x005c_5cff, // 12 bright blue
    0x00ff_00ff, // 13 bright magenta
    0x0000_ffff, // 14 bright cyan
    0x00ff_ffff, // 15 bright white
];

/// The foreground a reset terminal writes with.
pub const DEFAULT_FG: u8 = 7;
/// The background a reset terminal clears to. Black, so an unwritten cell is the darkest thing on
/// the screen and a blank terminal is a defined picture rather than whatever the frames held.
pub const DEFAULT_BG: u8 = 0;

// ================================================================================================
// Cells.
// ================================================================================================

/// A cell's rendition: which colours, and whether it is reversed.
///
/// One byte, packed, because the grid is a fixed array and 512 cells of `struct { u8, u8, bool }`
/// would be three times the size for no gain. Bits 0..4 are the foreground index (0..16), bits 4..7
/// the background index (0..8), bit 7 the reverse flag.
///
/// **Bold is bright, and that is a decision rather than a shortcut.** A bold weight needs a second
/// font, and in a five-column cell a bold face is a smudge; every terminal from the DEC VT onward
/// has answered SGR
/// 1 by brightening instead, which is why the palette has eight bright entries. Recorded in
/// notes/glyphs.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr(u8);

impl Attr {
    /// The rendition a reset terminal writes with.
    pub const DEFAULT: Attr = Attr(DEFAULT_FG | (DEFAULT_BG << 4));

    /// Pack a rendition: `fg` masked to 4 bits, `bg` to 3, plus the reverse flag.
    pub const fn new(fg: u8, bg: u8, reverse: bool) -> Attr {
        Attr((fg & 0x0f) | ((bg & 0x07) << 4) | if reverse { 0x80 } else { 0 })
    }

    /// The foreground palette index (bits 0..4).
    pub const fn fg(self) -> u8 {
        self.0 & 0x0f
    }

    /// The background palette index (bits 4..7).
    pub const fn bg(self) -> u8 {
        (self.0 >> 4) & 0x07
    }

    /// Whether the reverse-video bit is set. See [`colours`](Self::colours) for what it does to
    /// the actual paint colours.
    pub const fn reverse(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// The `(foreground, background)` colours this rendition actually paints with, reverse applied.
    /// One place, so the cursor and the SGR path cannot disagree about what "reversed" means.
    pub const fn colours(self) -> (u32, u32) {
        let (f, b) = (PALETTE[self.fg() as usize], PALETTE[self.bg() as usize & 7]);
        if self.reverse() { (b, f) } else { (f, b) }
    }
}

impl Default for Attr {
    fn default() -> Attr {
        Attr::DEFAULT
    }
}

/// One cell of the grid: a byte and how to paint it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character. Not UTF-8 aware: this is a byte-per-cell text-mode terminal.
    pub byte: u8,
    /// How to paint it.
    pub attr: Attr,
}

impl Cell {
    /// A blank cell in `attr`. Erasing writes **spaces in the current rendition**, not zeroes, which
    /// is what makes `CSI K` on a coloured background leave the background rather than a black gap.
    pub const fn blank(attr: Attr) -> Cell {
        Cell { byte: b' ', attr }
    }
}

impl Default for Cell {
    fn default() -> Cell {
        Cell::blank(Attr::DEFAULT)
    }
}

// ================================================================================================
// Damage.
// ================================================================================================

/// **The rectangle of cells that changed**, half-open in both axes.
///
/// A bounding box rather than a region list, the same trade `compositor::Rect` makes for the
/// compositor and for the same reason: two small changes far apart cost the box that contains both,
/// which is a few extra glyph blits here and the wrong call at desktop resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    /// Leftmost changed column.
    pub col: u32,
    /// Topmost changed row.
    pub row: u32,
    /// Width in cells.
    pub cols: u32,
    /// Height in cells.
    pub rows: u32,
}

impl CellRect {
    const fn cell(col: u32, row: u32) -> CellRect {
        CellRect {
            col,
            row,
            cols: 1,
            rows: 1,
        }
    }

    /// The bounding box of two rectangles. Public because a client that must carry damage forward
    /// across a frame the compositor has not acknowledged yet does the same accumulation
    /// (`user/src/display_terminal.rs`), and two spellings of a bounding box is one too many.
    pub const fn union(self, o: CellRect) -> CellRect {
        let col = if self.col < o.col { self.col } else { o.col };
        let row = if self.row < o.row { self.row } else { o.row };
        let (sr, or) = (self.col + self.cols, o.col + o.cols);
        let right = if sr > or { sr } else { or };
        let (sb, ob) = (self.row + self.rows, o.row + o.rows);
        let bottom = if sb > ob { sb } else { ob };
        CellRect {
            col,
            row,
            cols: right - col,
            rows: bottom - row,
        }
    }

    /// This rectangle in **pixels**, as `(x, y, w, h)`. What a `FLUSH` or a compositor damage
    /// rectangle wants.
    pub const fn to_pixels(self) -> (u32, u32, u32, u32) {
        (
            self.col * bitfont::GLYPH_W,
            self.row * bitfont::GLYPH_H,
            self.cols * bitfont::GLYPH_W,
            self.rows * bitfont::GLYPH_H,
        )
    }
}

// ================================================================================================
// The engine.
// ================================================================================================

/// The parser's state. Deliberately few: a VT's full state machine has a dozen states, and most of
/// the extra ones exist to distinguish sequences this engine swallows anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Printing.
    Ground,
    /// `ESC` has been seen.
    Esc,
    /// Inside `ESC [`, accumulating parameters.
    Csi,
    /// Inside a **string** sequence (`ESC ]`, the operating-system command, and its relatives).
    ///
    /// This state earns its place: a string sequence carries arbitrary text, so a parser without it
    /// prints the window title onto the screen. That is not hypothetical, it is what this engine did
    /// before the test caught it, and it is the reason the test feeds a title-setting sequence.
    Str,
    /// Inside a string sequence, having just seen `ESC`: a `\` ends the string (ST), anything else
    /// is part of it.
    StrEsc,
}

/// How many numeric parameters a CSI sequence may carry. `CSI 1;2;3;4 m` is already more than
/// anything here emits; a fifth is swallowed rather than growing the array, because a parameter list
/// long enough to matter belongs to a sequence this engine does not implement.
const MAX_PARAMS: usize = 4;

/// **A terminal's live grid.**
///
/// Construct with [`Vt::new`], push bytes with [`Vt::feed`], read pixels with [`Vt::pixel`], and ask
/// what changed with [`Vt::take_damage`].
pub struct Vt {
    cells: [Cell; MAX_CELLS],
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
    attr: Attr,
    /// The right margin has been reached and the *next* printable wraps. See [`Vt::feed`].
    wrap_pending: bool,
    cursor_visible: bool,
    state: State,
    params: [u16; MAX_PARAMS],
    nparams: usize,
    /// A parameter list this engine will not act on (private `?` sequences, an intermediate byte, or
    /// more parameters than fit). The sequence is still swallowed whole; only its effect is dropped.
    ignore: bool,
    dirty: Option<CellRect>,
}

impl Vt {
    /// A blank terminal of `cols` by `rows` cells, clamped to [`MAX_COLS`] and [`MAX_ROWS`].
    ///
    /// Clamped rather than refused because the alternative in a `no_std` component is a panic in a
    /// process that has no way to report one, and a terminal that is smaller than asked for is
    /// visibly wrong. The component that wires it asserts the real geometry at compile time.
    ///
    /// **`const`, so a terminal can live in `.bss`.** A user process here gets one 4 KiB page of
    /// stack (`kernel/src/user.rs`, `USER_STACK_VA`), and a grid plus the temporary a move would
    /// make is most of it. So `user/src/display_terminal.rs` keeps its `Vt` as a `static`, which needs this to
    /// be a constant expression; the clamps are spelled out rather than using `Ord::clamp`, which is
    /// not `const`.
    pub const fn new(cols: u32, rows: u32) -> Vt {
        let cols = if cols == 0 {
            1
        } else if cols > MAX_COLS as u32 {
            MAX_COLS as u32
        } else {
            cols
        };
        let rows = if rows == 0 {
            1
        } else if rows > MAX_ROWS as u32 {
            MAX_ROWS as u32
        } else {
            rows
        };
        Vt {
            cells: [Cell::blank(Attr::DEFAULT); MAX_CELLS],
            cols,
            rows,
            col: 0,
            row: 0,
            attr: Attr::DEFAULT,
            wrap_pending: false,
            cursor_visible: true,
            state: State::Ground,
            params: [0; MAX_PARAMS],
            nparams: 0,
            ignore: false,
            // A fresh terminal is entirely damage: nothing has painted its surface yet, so the
            // first present must cover the whole grid rather than the empty rectangle "nothing
            // changed" would give.
            dirty: Some(CellRect {
                col: 0,
                row: 0,
                cols,
                rows,
            }),
        }
    }

    /// The grid's width in cells.
    pub const fn cols(&self) -> u32 {
        self.cols
    }

    /// The grid's height in cells.
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// The grid's width in pixels.
    pub const fn width(&self) -> u32 {
        self.cols * bitfont::GLYPH_W
    }

    /// The grid's height in pixels.
    pub const fn height(&self) -> u32 {
        self.rows * bitfont::GLYPH_H
    }

    /// Where the cursor is, as `(col, row)`.
    pub const fn cursor(&self) -> (u32, u32) {
        (self.col, self.row)
    }

    /// Show or hide the block cursor. A hidden cursor is what a terminal that is only *printing*
    /// wants, and it is what makes a picture independent of where the last write left off.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        if self.cursor_visible != visible {
            self.cursor_visible = visible;
            self.damage_cell(self.col, self.row);
        }
    }

    /// The cell at `(col, row)`. Out of range gives a default blank rather than a panic, because the
    /// callers are pixel loops.
    pub fn cell(&self, col: u32, row: u32) -> Cell {
        if col >= self.cols || row >= self.rows {
            return Cell::default();
        }
        self.cells[(row * self.cols + col) as usize]
    }

    /// **The pixel at `(x, y)` of the terminal's surface.** The whole of rendering, as a pure
    /// function of the grid.
    ///
    /// The cursor is a **block**, drawn by swapping the cell's own two colours. Drawing it here
    /// rather than as a separate overlay is what keeps the picture a function of the state: a test
    /// that predicts the screen predicts the cursor too, and a cursor left in the wrong place is a
    /// failure rather than a cosmetic difference nobody notices.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        let (col, row) = (x / bitfont::GLYPH_W, y / bitfont::GLYPH_H);
        let cell = self.cell(col, row);
        let mut attr = cell.attr;
        if self.cursor_visible && col == self.col && row == self.row && col < self.cols {
            attr = Attr::new(attr.fg(), attr.bg(), !attr.reverse());
        }
        let (fg, bg) = attr.colours();
        bitfont::cell_pixel(
            cell.byte,
            x % bitfont::GLYPH_W,
            y % bitfont::GLYPH_H,
            fg,
            bg,
        )
    }

    /// What has changed since the last [`Vt::take_damage`], in cells.
    pub const fn damage(&self) -> Option<CellRect> {
        self.dirty
    }

    /// What has changed since the last call, in cells, clearing the record.
    pub fn take_damage(&mut self) -> Option<CellRect> {
        self.dirty.take()
    }

    /// Copy row `row`'s bytes into `out`, returning how many were written. For tests and for a
    /// caller that wants the text rather than the pixels; there is no `String` in `no_std`.
    pub fn row_bytes(&self, row: u32, out: &mut [u8]) -> usize {
        let n = (self.cols as usize).min(out.len());
        for (i, o) in out.iter_mut().enumerate().take(n) {
            *o = self.cell(i as u32, row).byte;
        }
        n
    }

    /// **Push bytes through the parser.**
    ///
    /// # Deferred wrap, which is the one subtlety in printing
    ///
    /// Writing into the last column does **not** move the cursor to the next row. It leaves the
    /// cursor on that last cell and arms a pending wrap, and the *next* printable byte does the
    /// wrap first. Every real terminal does this, and the reason is visible in a line discipline's
    /// echo: a line that exactly fills the width would otherwise scroll the screen before anything
    /// asked it to, and a `CR` arriving right after the last character would find the cursor a row
    /// too low. Any cursor motion, `CR`, or `LF` cancels the pending wrap.
    pub fn feed(&mut self, bytes: &[u8]) {
        let before = (self.col, self.row);
        for &b in bytes {
            self.byte(b);
        }
        if (self.col, self.row) != before && self.cursor_visible {
            // The cursor is painted, so moving it dirties both the cell it left and the one it
            // arrived at. Doing it once per `feed` rather than per byte keeps a long print from
            // dirtying a trail of cells it also overwrote.
            self.damage_cell(before.0, before.1);
            self.damage_cell(self.col, self.row);
        }
    }

    fn byte(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Esc => self.esc(b),
            State::Csi => self.csi(b),
            State::Str => match b {
                0x07 => self.state = State::Ground, // BEL, the xterm terminator
                0x1b => self.state = State::StrEsc,
                _ => {}
            },
            State::StrEsc => {
                // `ESC \` is the standard string terminator; an ESC followed by anything else was
                // part of the string after all.
                self.state = if b == b'\\' {
                    State::Ground
                } else {
                    State::Str
                };
            }
        }
    }

    fn ground(&mut self, b: u8) {
        match b {
            0x1b => {
                self.state = State::Esc;
            }
            b'\r' => {
                self.col = 0;
                self.wrap_pending = false;
            }
            b'\n' => {
                self.wrap_pending = false;
                self.line_feed();
            }
            0x08 => {
                // Backspace does not wrap back to the previous row. A line discipline never asks it
                // to (it only backs up within the line it echoed), and a terminal that did would
                // have to remember whether the previous row ended in a wrap.
                self.wrap_pending = false;
                self.col = self.col.saturating_sub(1);
            }
            b'\t' => {
                self.wrap_pending = false;
                // Every eight columns, and never past the last one: a tab at the right margin stops
                // there rather than wrapping, which is what the fixed-tab-stop terminals do.
                self.col = ((self.col / 8 + 1) * 8).min(self.cols - 1);
            }
            0x07 => {} // BEL: there is no bell on a framebuffer, and a visual bell is policy
            0x00..=0x1f | 0x7f => {} // every other control code: consumed, never drawn
            _ => self.print(b),
        }
    }

    fn print(&mut self, b: u8) {
        if self.wrap_pending {
            self.wrap_pending = false;
            self.col = 0;
            self.line_feed();
        }
        let (col, row) = (self.col, self.row);
        self.put(
            col,
            row,
            Cell {
                byte: b,
                attr: self.attr,
            },
        );
        if self.col + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.col += 1;
        }
    }

    /// Down one row, scrolling the grid up if that would fall off the bottom.
    fn line_feed(&mut self) {
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        // Scroll: every row moves, so the whole grid is damage. Honest rather than clever; a real
        // terminal with a scrolling accelerator still repaints the screen it exposes.
        let cols = self.cols as usize;
        let used = cols * self.rows as usize;
        self.cells.copy_within(cols..used, 0);
        for c in &mut self.cells[used - cols..used] {
            *c = Cell::blank(self.attr);
        }
        self.damage_all();
    }

    fn esc(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.state = State::Csi;
                self.params = [0; MAX_PARAMS];
                self.nparams = 0;
                self.ignore = false;
            }
            b'c' => self.reset(),
            // The string introducers: OSC (`]`), DCS (`P`), SOS/PM/APC (`X`, `^`, `_`). Their
            // payload is arbitrary text, so it has to be *consumed*, not returned to Ground.
            b']' | b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
            // Anything else: the introducer and this byte are both consumed. A two-byte escape this
            // engine does not speak must not leave its final byte to be printed as a letter, which is
            // the classic "stray letter on the screen" bug.
            _ => {}
        }
    }

    fn csi(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                if self.nparams == 0 {
                    self.nparams = 1;
                }
                if self.nparams <= MAX_PARAMS {
                    let p = &mut self.params[self.nparams - 1];
                    *p = p.saturating_mul(10).saturating_add((b - b'0') as u16);
                }
            }
            b';' => {
                self.nparams += 1;
                if self.nparams > MAX_PARAMS {
                    self.ignore = true;
                    self.nparams = MAX_PARAMS;
                }
            }
            // A private-use introducer (`?`, `<`, `=`, `>`) or an intermediate byte: this engine
            // implements none of those sequences, so it swallows the whole thing rather than acting
            // on a parameter list that means something else.
            0x20..=0x2f | 0x3c..=0x3f => self.ignore = true,
            0x40..=0x7e => {
                let ignore = self.ignore;
                self.state = State::Ground;
                if !ignore {
                    self.csi_final(b);
                }
            }
            _ => {
                // A control code inside a sequence: real terminals execute it. Nothing that reaches
                // this engine does that, so it is dropped along with the sequence, which fails in the
                // direction of drawing nothing rather than drawing garbage.
                self.ignore = true;
            }
        }
    }

    /// The first parameter, defaulting to `d` when absent or zero (the ANSI convention: `CSI 0 A`
    /// and `CSI A` both mean one row).
    fn param(&self, i: usize, d: u32) -> u32 {
        match self.params.get(i) {
            Some(&0) | None => d,
            Some(&p) => p as u32,
        }
    }

    fn csi_final(&mut self, b: u8) {
        match b {
            b'A' => {
                self.wrap_pending = false;
                self.row = self.row.saturating_sub(self.param(0, 1));
            }
            b'B' => {
                self.wrap_pending = false;
                self.row = (self.row + self.param(0, 1)).min(self.rows - 1);
            }
            b'C' => {
                self.wrap_pending = false;
                self.col = (self.col + self.param(0, 1)).min(self.cols - 1);
            }
            b'D' => {
                self.wrap_pending = false;
                self.col = self.col.saturating_sub(self.param(0, 1));
            }
            // CUP. Parameters are **one-based** on the wire and zero-based here, which is the
            // off-by-one every terminal implementation has had at least once.
            b'H' | b'f' => {
                self.wrap_pending = false;
                self.row = (self.param(0, 1) - 1).min(self.rows - 1);
                self.col = (self.param(1, 1) - 1).min(self.cols - 1);
            }
            b'J' => self.erase_display(self.param(0, 0)),
            b'K' => self.erase_line(self.param(0, 0)),
            b'm' => self.sgr(),
            // Every other final byte: the sequence is consumed and nothing happens. That includes
            // the device-report sequences, deliberately: this engine has no way to answer one and a
            // half-answered query is worse than an unanswered one.
            _ => {}
        }
    }

    /// `CSI n J`: 0 erases from the cursor to the end of the screen, 1 to its start, 2 all of it.
    /// Note that `CSI 2 J` does **not** move the cursor; that is why a line discipline's ^L sends
    /// `CSI 2J` followed by `CSI H`.
    fn erase_display(&mut self, mode: u32) {
        let here = self.row * self.cols + self.col;
        let end = self.rows * self.cols;
        let (from, to) = match mode {
            0 => (here, end),
            1 => (0, here + 1),
            _ => (0, end),
        };
        for i in from..to.min(end) {
            self.cells[i as usize] = Cell::blank(self.attr);
        }
        if to > from {
            self.damage_all();
        }
    }

    /// `CSI n K`: 0 erases from the cursor to the end of the line, 1 to its start, 2 the whole line.
    fn erase_line(&mut self, mode: u32) {
        let (from, to) = match mode {
            0 => (self.col, self.cols),
            1 => (0, self.col + 1),
            _ => (0, self.cols),
        };
        for c in from..to.min(self.cols) {
            let row = self.row;
            self.put(c, row, Cell::blank(self.attr));
        }
    }

    /// `CSI ... m`: the rendition. An empty parameter list is `CSI 0 m`, a reset, which is the one
    /// place the "absent means zero" default differs from the cursor sequences' "absent means one".
    fn sgr(&mut self) {
        let n = self.nparams.max(1);
        for i in 0..n {
            let p = self.params.get(i).copied().unwrap_or(0);
            match p {
                0 => self.attr = Attr::DEFAULT,
                // Bold brightens rather than thickening: see [`Attr`].
                1 => self.attr = Attr::new(self.attr.fg() | 8, self.attr.bg(), self.attr.reverse()),
                7 => self.attr = Attr::new(self.attr.fg(), self.attr.bg(), true),
                22 => {
                    self.attr = Attr::new(self.attr.fg() & 7, self.attr.bg(), self.attr.reverse());
                }
                27 => self.attr = Attr::new(self.attr.fg(), self.attr.bg(), false),
                30..=37 => {
                    // Setting a colour keeps the bold bit, the way a terminal does: `ESC[1m ESC[31m`
                    // is bright red, not dark red.
                    let bright = self.attr.fg() & 8;
                    self.attr =
                        Attr::new((p as u8 - 30) | bright, self.attr.bg(), self.attr.reverse());
                }
                39 => self.attr = Attr::new(DEFAULT_FG, self.attr.bg(), self.attr.reverse()),
                40..=47 => {
                    self.attr = Attr::new(self.attr.fg(), p as u8 - 40, self.attr.reverse());
                }
                49 => self.attr = Attr::new(self.attr.fg(), DEFAULT_BG, self.attr.reverse()),
                90..=97 => {
                    self.attr = Attr::new((p as u8 - 90) | 8, self.attr.bg(), self.attr.reverse());
                }
                // Everything else (underline, blink, 256-colour, truecolour) is dropped. A cell here
                // has no bit for them, and drawing the *text* in the wrong style is better than not
                // drawing it.
                _ => {}
            }
        }
    }

    /// `ESC c`: back to power-on. Clears the grid in the *default* rendition rather than the current
    /// one, which is the difference between a reset and an erase.
    fn reset(&mut self) {
        self.attr = Attr::DEFAULT;
        self.cells = [Cell::blank(Attr::DEFAULT); MAX_CELLS];
        self.col = 0;
        self.row = 0;
        self.wrap_pending = false;
        self.damage_all();
    }

    fn put(&mut self, col: u32, row: u32, cell: Cell) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let at = (row * self.cols + col) as usize;
        if self.cells[at] != cell {
            self.cells[at] = cell;
            self.damage_cell(col, row);
        }
    }

    fn damage_cell(&mut self, col: u32, row: u32) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let r = CellRect::cell(col, row);
        self.dirty = Some(match self.dirty {
            Some(d) => d.union(r),
            None => r,
        });
    }

    fn damage_all(&mut self) {
        self.dirty = Some(CellRect {
            col: 0,
            row: 0,
            cols: self.cols,
            rows: self.rows,
        });
    }
}

/// What the display terminal reports to whoever spawned it.
///
/// **Status, not contract**: no client can ask for any of this, and it is here rather than in the
/// terminal contract (`line_editor::proto`) because it is about the *component*, not about the terminal
/// a program talks to. The engine above is sans-IO and this module is three constants; nothing in
/// the engine reads them.
pub mod status {
    /// The terminal is up: `send(REPORT, TERM_UP, cols | rows << 32, mode)`. Sent once, after the
    /// blank grid has been presented, so a spawner that sees it knows the geometry was negotiated
    /// and the first picture reached the screen.
    ///
    /// **One report, ever**, for the reason `compositor::status::COMP_UP` gives: a status `SEND` is a
    /// rendezvous, so a component that narrated every frame would block until its spawner listened,
    /// and a spawner that stopped listening would wedge everything behind it. What is on the screen
    /// is observable where it belongs: in the frames, and at the display endpoint.
    pub const TERM_UP: u64 = 0x7E7_0001;

    /// The terminal drives a display endpoint directly (`gfx FLUSH`), owning the whole scanout.
    pub const MODE_DISPLAY: u64 = 0;
    /// The terminal is a compositor client, owning one window (`compose COMMIT`).
    pub const MODE_WINDOW: u64 = 1;

    /// The keyboard driver is up: `send(REPORT, KBD_UP, buffers posted, 0)`. The device is
    /// enumerated, the event queue is programmed through the confined transport, and every
    /// device-writable buffer is posted, so a spawner that sees this knows a key pressed from here
    /// on has somewhere to land. Also one report, ever, and for the same reason [`TERM_UP`] is.
    pub const KBD_UP: u64 = 0x7E7_0002;
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::string::String;
    use std::vec::Vec;

    /// A terminal's rows as strings, for assertions that read like the screen.
    fn rows(vt: &Vt) -> Vec<String> {
        (0..vt.rows())
            .map(|r| {
                let mut buf = [0u8; MAX_COLS];
                let n = vt.row_bytes(r, &mut buf);
                String::from_utf8_lossy(&buf[..n]).into_owned()
            })
            .collect()
    }

    fn vt(cols: u32, rows: u32) -> Vt {
        let mut vt = Vt::new(cols, rows);
        vt.take_damage();
        vt
    }

    /// **A rendition names two palette entries, and reverse swaps which is ink.** Every other test
    /// compares one `Attr` against another, which cannot see a packer that lost a field or a
    /// `colours` that returns a constant: both sides move together. These are the numbers.
    #[test]
    fn a_rendition_resolves_to_the_palette_entries_it_names() {
        assert_eq!(Attr::DEFAULT.fg(), DEFAULT_FG);
        assert_eq!(Attr::DEFAULT.bg(), DEFAULT_BG);
        assert!(!Attr::DEFAULT.reverse());
        assert_eq!(Attr::DEFAULT.colours(), (PALETTE[7], PALETTE[0]));
        // A background that is not 7, so a mask that widened to "always 7" is visible.
        assert_eq!(Attr::new(3, 4, false).colours(), (PALETTE[3], PALETTE[4]));
        assert_eq!(
            Attr::new(3, 4, true).colours(),
            (PALETTE[4], PALETTE[3]),
            "reverse swaps ink and paper, it does not pick different colours"
        );
    }

    /// **The damage box's arithmetic, at the edges where a bounding box is decided.** A rectangle
    /// that already contains the other must be its own union either way round, which is the case
    /// that reads both operands' far edges; the cell-by-cell unions elsewhere only ever grow the
    /// second operand's, so half of `union` was never consulted.
    #[test]
    fn a_damage_box_contains_a_rectangle_it_already_covered() {
        let big = CellRect {
            col: 1,
            row: 2,
            cols: 4,
            rows: 3,
        };
        let inside = CellRect {
            col: 2,
            row: 3,
            cols: 1,
            rows: 1,
        };
        assert_eq!(big.union(inside), big);
        assert_eq!(inside.union(big), big);
        // Cells are 7 by 8, so a rect at (1, 2) starts at (7, 16) and is 28 by 24. The two axes
        // differ on purpose: a single constant used for both would pass whatever it was.
        assert_eq!(big.to_pixels(), (7, 16, 28, 24));
    }

    /// The grid's size in cells and in pixels, as numbers rather than as the expression that
    /// computes them. 6 by 3 because 6*7, 6+7 and 6/7 are three different answers.
    #[test]
    fn the_grid_reports_its_size_in_cells_and_in_pixels() {
        let t = Vt::new(6, 3);
        assert_eq!((t.cols(), t.rows()), (6, 3));
        assert_eq!(t.width(), 42);
        assert_eq!(t.height(), 24);
    }

    /// **The pixel is a pure function of the grid**, so it can be pinned exactly. `L` at cell
    /// (1, 1) in green, which starts at pixel (7, 8) because the cell is 7 by 8: its stem is the
    /// second column of the cell (the first is the font's gutter), its top row holds nothing else,
    /// and its foot reaches the last ink column on the baseline. Those four pixels separate every
    /// way the cell-versus-glyph coordinate split can go wrong (a mixed-up divide and remainder
    /// agree at cell (0, 0), which is where every other pixel assertion sits), and the two axes
    /// use different divisors so a single constant cannot satisfy both.
    #[test]
    fn a_pixel_names_its_cell_and_its_place_inside_the_glyph() {
        let mut t = vt(4, 3);
        t.set_cursor_visible(false);
        t.feed(b"\x1b[2;2H\x1b[32mL");
        let (green, black) = (PALETTE[2], PALETTE[0]);
        assert_eq!(
            t.pixel(7, 8),
            black,
            "the cell's first column is the font's gutter"
        );
        assert_eq!(t.pixel(8, 8), green, "the stem is the column after it");
        assert_eq!(
            t.pixel(12, 8),
            black,
            "and the L's top row is stem and nothing else"
        );
        assert_eq!(
            t.pixel(12, 14),
            green,
            "its foot reaches the last ink column on the baseline"
        );
    }

    /// A column past the last one is off the grid, not the next row's first cell. The rows of the
    /// grid are contiguous, so an out-of-range column that is not rejected reads a real cell.
    #[test]
    fn a_column_past_the_margin_is_not_the_next_rows_first_cell() {
        let mut t = vt(4, 2);
        t.feed(b"\x1b[2;1Hab");
        assert_eq!(t.cell(4, 0), Cell::default());
    }

    /// **Tabs stop every eight columns and never past the last one.** Nothing else feeds a tab, and
    /// a line discipline does not emit one, but a program's output does.
    #[test]
    fn a_tab_advances_to_the_next_stop_and_stops_at_the_margin() {
        let mut t = vt(20, 2);
        t.feed(b"\t");
        assert_eq!(t.cursor(), (8, 0));
        t.feed(b"\t");
        assert_eq!(t.cursor(), (16, 0), "the stop is measured from where it is");
        t.feed(b"\t");
        assert_eq!(
            t.cursor(),
            (19, 0),
            "a tab at the right margin stops there rather than wrapping"
        );
    }

    /// Four parameters is the limit and the limit is **legal**: `CSI 1;2;3;4 m` acts, and only a
    /// fifth is dropped. Every other test here uses two or three, so the limit itself was never
    /// judged from the inside.
    #[test]
    fn the_fourth_parameter_is_legal_and_the_fifth_is_not() {
        let mut t = vt(8, 1);
        t.feed(b"\x1b[0;1;32;7ma");
        assert_eq!(t.cell(0, 0).attr, Attr::new(10, DEFAULT_BG, true));
        t.feed(b"\x1b[0;1;2;3;31mb");
        assert_eq!(
            t.cell(1, 0).attr,
            Attr::new(10, DEFAULT_BG, true),
            "a sequence with too many parameters is swallowed, not half-applied"
        );
    }

    /// `CSI n J` from a cursor that is **not on the first row**, in the mode that erases backwards.
    /// Both directions read `row * cols + col`, and at row 0 that arithmetic cannot be wrong.
    #[test]
    fn erase_in_display_starts_from_where_the_cursor_actually_is() {
        let mut t = vt(4, 3);
        t.feed(b"abcd\r\nefgh\r\nijkl\x1b[2;3H\x1b[J");
        assert_eq!(rows(&t), ["abcd", "ef  ", "    "]);

        let mut t = vt(4, 3);
        t.set_cursor_visible(false);
        t.feed(b"abcd\r\nefgh\r\nijkl");
        t.take_damage();
        t.feed(b"\x1b[2;3H\x1b[1J");
        assert_eq!(
            rows(&t),
            ["    ", "   h", "ijkl"],
            "mode 1 erases through the cursor's own cell and no further"
        );
        assert!(t.damage().is_some(), "erasing the screen is damage");
    }

    /// The rest of SGR: the bright foregrounds as their own sequences, and the three switches that
    /// turn something *off*. A terminal that only ever set attributes would pass every other test
    /// here and leave a line reversed forever.
    #[test]
    fn sgr_has_bright_colours_and_switches_that_turn_things_off() {
        let mut t = vt(8, 1);
        t.feed(b"\x1b[92ma");
        assert_eq!(t.cell(0, 0).attr, Attr::new(10, DEFAULT_BG, false));
        t.feed(b"\x1b[1mb");
        assert_eq!(
            t.cell(1, 0).attr.fg(),
            10,
            "bold on an already-bright colour is idempotent, not a toggle"
        );
        t.feed(b"\x1b[7;41mc\x1b[27md\x1b[39me\x1b[49mf");
        assert_eq!(t.cell(2, 0).attr, Attr::new(10, 1, true));
        assert_eq!(t.cell(3, 0).attr, Attr::new(10, 1, false), "SGR 27");
        assert_eq!(t.cell(4, 0).attr, Attr::new(DEFAULT_FG, 1, false), "SGR 39");
        assert_eq!(
            t.cell(5, 0).attr,
            Attr::new(DEFAULT_FG, DEFAULT_BG, false),
            "SGR 49"
        );
    }

    /// Text lands in the grid, `CR` returns to column 0, and `LF` goes down without returning.
    /// The `\r\n` pair is what `line_editor::expand_output` produces from a Unix `\n`, so a terminal
    /// that treated `LF` as `CRLF` would look right on that stream and wrong on every other.
    #[test]
    fn printing_moves_the_cursor_the_way_a_terminal_does() {
        let mut t = vt(8, 3);
        t.feed(b"hi");
        assert_eq!(t.cursor(), (2, 0));
        t.feed(b"\n");
        assert_eq!(t.cursor(), (2, 1), "LF alone must not return the carriage");
        t.feed(b"\r");
        assert_eq!(t.cursor(), (0, 1));
        t.feed(b"there");
        assert_eq!(rows(&t), ["hi      ", "there   ", "        "]);
        // Backspace steps back within the row and stops at the margin.
        t.feed(b"\x08\x08");
        assert_eq!(t.cursor(), (3, 1));
        t.feed(b"\r\x08");
        assert_eq!(
            t.cursor(),
            (0, 1),
            "backspace must not wrap to the row above"
        );
    }

    /// **Deferred wrap.** Filling the last column leaves the cursor on it; the next printable wraps.
    /// A terminal that wrapped eagerly would scroll a full-width line before anything asked it to.
    #[test]
    fn the_right_margin_wraps_late_not_early() {
        let mut t = vt(4, 3);
        t.feed(b"abcd");
        assert_eq!(t.cursor(), (3, 0), "the cursor stays on the last column");
        assert_eq!(rows(&t)[1], "    ", "nothing has moved to the next row yet");
        t.feed(b"e");
        assert_eq!(t.cursor(), (1, 1));
        assert_eq!(rows(&t), ["abcd", "e   ", "    "]);

        // And a CR arriving right after a full line finds the cursor on the same row.
        let mut t = vt(4, 3);
        t.feed(b"abcd\rX");
        assert_eq!(rows(&t)[0], "Xbcd");
        assert_eq!(rows(&t)[1], "    ");
    }

    /// A line feed on the bottom row scrolls, and scrolling exposes a blank row rather than the
    /// row that used to be there.
    #[test]
    fn the_bottom_row_scrolls() {
        let mut t = vt(4, 3);
        t.feed(b"one\r\ntwo\r\nsix\r\n");
        assert_eq!(rows(&t), ["two ", "six ", "    "]);
        assert_eq!(t.cursor(), (0, 2), "the cursor stays on the bottom row");
        t.feed(b"new\r\nend");
        assert_eq!(rows(&t), ["six ", "new ", "end "]);
        // Scrolling is whole-screen damage, honestly reported.
        assert_eq!(
            t.damage(),
            Some(CellRect {
                col: 0,
                row: 0,
                cols: 4,
                rows: 3
            }),
        );

        // The same claim with nothing else in the frame. Above, the printing and the cursor
        // between them already dirty the whole grid, so a scroll that reported no damage at all
        // would still add up to the right rectangle. A bare LF on the bottom row moves every row
        // and writes no cell, which is the only shape that can tell the two apart.
        let mut t = vt(4, 3);
        t.set_cursor_visible(false);
        t.feed(b"ab\r\ncd\r\nef");
        t.take_damage();
        t.feed(b"\r\n");
        assert_eq!(rows(&t), ["cd  ", "ef  ", "    "]);
        assert_eq!(
            t.take_damage(),
            Some(CellRect {
                col: 0,
                row: 0,
                cols: 4,
                rows: 3
            }),
            "a scroll moves every row, so the whole grid is damage",
        );
    }

    /// Cursor sequences, with the parameter defaults ANSI specifies: an absent or zero parameter is
    /// one, and `CUP`'s parameters are one-based on the wire.
    #[test]
    fn the_cursor_sequences_use_ansi_defaults() {
        let mut t = vt(8, 4);
        t.feed(b"\x1b[3;5H");
        assert_eq!(t.cursor(), (4, 2), "CSI H is row;col and one-based");
        t.feed(b"\x1b[A");
        assert_eq!(t.cursor(), (4, 1), "an absent parameter means one");
        t.feed(b"\x1b[0B");
        assert_eq!(t.cursor(), (4, 2), "a zero parameter means one too");
        t.feed(b"\x1b[2D");
        assert_eq!(t.cursor(), (2, 2));
        t.feed(b"\x1b[99C");
        assert_eq!(t.cursor(), (7, 2), "motion clamps to the grid");
        t.feed(b"\x1b[99A");
        assert_eq!(t.cursor(), (7, 0));
        t.feed(b"\x1b[H");
        assert_eq!(t.cursor(), (0, 0), "CSI H with no parameters is home");
        // The far edges. A clamp that is one too generous parks the cursor off the grid, where
        // every subsequent write is silently discarded by `put`.
        t.feed(b"\x1b[99B");
        assert_eq!(t.cursor(), (0, 3), "downward motion clamps to the last row");
        t.feed(b"\x1b[99;99H");
        assert_eq!(t.cursor(), (7, 3), "CUP clamps on both axes");
    }

    /// Erasing, in all three modes of both verbs, and the property that makes `CSI K` useful to a
    /// line discipline: it erases **to the end of the line** and leaves the cursor alone.
    #[test]
    fn erasing_clears_what_it_says_and_no_more() {
        let mut t = vt(6, 2);
        t.feed(b"abcdef\r\nghijkl");
        t.feed(b"\x1b[2;3H\x1b[K");
        assert_eq!(rows(&t), ["abcdef", "gh    "]);
        assert_eq!(t.cursor(), (2, 1), "erase in line must not move the cursor");
        t.feed(b"\x1b[1;4H\x1b[1K");
        assert_eq!(
            rows(&t),
            ["    ef", "gh    "],
            "mode 1 erases through the cursor"
        );
        t.feed(b"\x1b[2K");
        assert_eq!(rows(&t), ["      ", "gh    "]);

        let mut t = vt(6, 2);
        t.feed(b"abcdef\r\nghijkl\x1b[1;4H\x1b[J");
        assert_eq!(
            rows(&t),
            ["abc   ", "      "],
            "erase to the end of the screen"
        );
        let mut t = vt(6, 2);
        t.feed(b"abcdef\r\nghijkl\x1b[2J");
        assert_eq!(rows(&t), ["      ", "      "]);
        assert_eq!(t.cursor(), (5, 1), "CSI 2J does not home the cursor");
    }

    /// **Erasing writes spaces in the current rendition**, so clearing on a coloured background
    /// leaves the background. A terminal that erased to black would leave a gap in a coloured line,
    /// and that is exactly the redraw a line discipline does on every backspace.
    #[test]
    fn erasing_keeps_the_current_background() {
        let mut t = vt(4, 1);
        t.feed(b"\x1b[44mxy\x1b[K");
        for c in 0..4 {
            assert_eq!(t.cell(c, 0).attr.bg(), 4, "column {c} lost its background");
        }
        assert_eq!(t.cell(3, 0).byte, b' ');
    }

    /// SGR: the colours, the reverse flag, and the two rules a terminal actually needs. Bold is a
    /// bright foreground, and setting a colour afterwards keeps the brightness.
    #[test]
    fn sgr_sets_colour_brightness_and_reverse() {
        let mut t = vt(8, 1);
        t.feed(b"\x1b[31ma");
        assert_eq!(t.cell(0, 0).attr, Attr::new(1, DEFAULT_BG, false));
        t.feed(b"\x1b[1mb");
        assert_eq!(t.cell(1, 0).attr.fg(), 9, "bold must brighten the colour");
        t.feed(b"\x1b[32mc");
        assert_eq!(
            t.cell(2, 0).attr.fg(),
            10,
            "a new colour keeps the bold bit"
        );
        t.feed(b"\x1b[22md");
        assert_eq!(t.cell(3, 0).attr.fg(), 2);
        t.feed(b"\x1b[7;44me");
        assert_eq!(t.cell(4, 0).attr, Attr::new(2, 4, true));
        t.feed(b"\x1b[0mf");
        assert_eq!(t.cell(5, 0).attr, Attr::DEFAULT, "SGR 0 resets everything");
        t.feed(b"\x1b[mg");
        assert_eq!(t.cell(6, 0).attr, Attr::DEFAULT, "an empty SGR is a reset");
    }

    /// **A sequence this engine does not implement is swallowed whole.** The failure mode this
    /// prevents is the one everybody has seen: an unimplemented escape whose final byte lands on the
    /// screen as a stray letter, corrupting a line that was otherwise fine.
    #[test]
    fn an_unknown_sequence_leaves_nothing_on_the_screen() {
        let mut t = vt(8, 1);
        t.feed(b"a\x1b[?25lb\x1b[38;5;120mc\x1b]0;title\x07d\x1bZe");
        assert_eq!(
            rows(&t)[0],
            "abcde   ",
            "an unimplemented sequence left bytes on the screen",
        );
        // A sequence split across two feeds is still one sequence: the parser is a state machine
        // across calls, which is what a byte-at-a-time IPC path needs.
        let mut t = vt(8, 1);
        t.feed(b"x\x1b");
        t.feed(b"[2");
        t.feed(b"Cy");
        assert_eq!(rows(&t)[0], "x  y    ");

        // A string sequence ends at ST (`ESC \`) as well as at BEL, and an ESC inside it that is
        // not ST is part of the string. Only the BEL terminator is exercised above, so the whole
        // ESC-in-a-string state was reachable but never distinguished from Ground.
        let mut t = vt(8, 1);
        t.feed(b"a\x1b]0;title\x1b\\b");
        assert_eq!(rows(&t)[0], "ab      ");
        let mut t = vt(8, 1);
        t.feed(b"a\x1b]x\x1bZy\x1b\\b");
        assert_eq!(
            rows(&t)[0],
            "ab      ",
            "an ESC that is not ST stays in the string"
        );

        // Control codes with no meaning here are consumed. Drawn instead, they are the font's
        // blanks or its missing-glyph box, in the middle of a line that was otherwise fine.
        let mut t = vt(8, 1);
        t.feed(b"a\x01\x02\x1f\x7fb");
        assert_eq!(rows(&t)[0], "ab      ");
    }

    /// The cursor is part of the picture, so moving it is damage; and a hidden cursor is not.
    #[test]
    fn the_cursor_is_painted_and_therefore_damages() {
        let mut t = vt(4, 2);
        let (fg, bg) = Attr::DEFAULT.colours();
        // Under the cursor, a blank cell shows the *background* colour as its ink field.
        assert_eq!(t.pixel(0, 0), fg, "the block cursor should invert its cell");
        assert_eq!(t.pixel(bitfont::GLYPH_W, 0), bg, "and only its own cell");

        t.take_damage();
        t.feed(b"\x1b[1;3H");
        let d = t.damage().expect("moving the cursor must be damage");
        assert!(
            d.col == 0 && d.cols >= 3,
            "both cells must be in the damage"
        );

        t.set_cursor_visible(false);
        assert_eq!(
            t.pixel(2 * bitfont::GLYPH_W, 0),
            bg,
            "hidden means not drawn"
        );
    }

    /// Damage is the bounding box of what changed, and writing a cell the value it already holds is
    /// not a change. Without that, a terminal that repainted the same line would flush the screen
    /// every frame and the damage rectangle would be decoration.
    #[test]
    fn damage_is_a_bounding_box_of_real_changes() {
        let mut t = vt(8, 4);
        t.set_cursor_visible(false);
        t.take_damage();
        assert_eq!(t.damage(), None, "a terminal at rest reports no damage");

        t.feed(b"\x1b[2;3Hab");
        assert_eq!(
            t.take_damage(),
            Some(CellRect {
                col: 2,
                row: 1,
                cols: 2,
                rows: 1
            }),
        );
        assert_eq!(t.damage(), None, "taking the damage clears it");

        t.feed(b"\x1b[2;3Hab");
        assert_eq!(t.damage(), None, "rewriting identical cells is not damage");

        t.feed(b"\x1b[1;1HZ\x1b[4;8HY");
        let d = t.take_damage().unwrap();
        assert_eq!(
            (d.col, d.row, d.cols, d.rows),
            (0, 0, 8, 4),
            "two far-apart changes cost the box that contains both",
        );
        assert_eq!(d.to_pixels(), (0, 0, 56, 32));
    }

    /// `ESC c` is a reset and not an erase: it clears in the *default* rendition and homes the
    /// cursor, where `CSI 2J` does neither.
    #[test]
    fn ris_resets_the_rendition_too() {
        let mut t = vt(4, 2);
        t.feed(b"\x1b[41;33mab\x1bc");
        assert_eq!(t.cursor(), (0, 0));
        assert_eq!(t.cell(0, 0).attr, Attr::DEFAULT);
        assert_eq!(rows(&t), ["    ", "    "]);
    }

    /// **The engine parses what the line discipline emits**, checked against the real component
    /// rather than against a list of sequences somebody wrote down.
    ///
    /// This is the interoperability claim milestone 28's contract makes and milestone 29 relies on:
    /// the display terminal's VT engine is fed the same echo stream the serial console gets, so a
    /// sequence `line_editor` emits and this engine does not understand would be a hole between two
    /// components that are otherwise separately correct. Feeding the actual editor closes it, and it
    /// keeps closing it if `line_editor` changes its redraw strategy.
    #[test]
    fn it_understands_the_line_disciplines_echo() {
        struct Echo(Vec<u8>);
        impl line_editor::Sink for Echo {
            fn put(&mut self, bytes: &[u8]) {
                self.0.extend_from_slice(bytes);
            }
        }

        // Type "hello", back up two (^B), insert "XY", delete forward (^D), kill to end (^K), then
        // Enter. That is the editing that produces CSI D, CSI C, CSI K and a mid-line redraw, which
        // is the whole set a display terminal has to understand to show an edited line correctly.
        let mut ld = line_editor::LineDisc::new();
        let mut echo = Echo(Vec::new());
        let mut event = line_editor::Event::None;
        for &b in b"hello\x02\x02XY\x04\x0b\r" {
            event = ld.feed(b, &mut echo);
        }
        assert_eq!(event, line_editor::Event::Line);
        assert!(
            echo.0.windows(3).any(|w| w == b"\x1b[K"),
            "the discipline stopped emitting CSI K: this test is no longer interoperability",
        );
        assert!(
            echo.0.windows(3).any(|w| w == b"\x1b[D"),
            "the discipline stopped emitting cursor motion: likewise",
        );

        let mut t = vt(16, 3);
        t.set_cursor_visible(false);
        t.feed(&echo.0);
        // Whatever redraw strategy the discipline chose, the screen must show the line it completed.
        let line = String::from_utf8_lossy(ld.line()).into_owned();
        assert_eq!(line, "helXY");
        assert_eq!(
            rows(&t)[0].trim_end(),
            line,
            "the grid disagrees with the line the discipline assembled",
        );

        // And the ^L repaint (CSI 2J, CSI H, then the prompt and the line) drives this engine
        // correctly: the screen is cleared and the line is back at the top.
        let mut echo = Echo(Vec::new());
        for &b in b"redraw\x0c" {
            ld.feed(b, &mut echo);
        }
        assert!(echo.0.windows(4).any(|w| w == b"\x1b[2J"));
        t.feed(&echo.0);
        assert_eq!(
            rows(&t)[0].trim_end(),
            "redraw",
            "the ^L repaint did not land at the top of a cleared screen",
        );
        assert_eq!(rows(&t)[1].trim_end(), "", "the old screen survived a ^L");
    }

    /// Each compositor window shows its own banner and its own typed text, so a test that mixed
    /// two windows up, or drew one twice, cannot pass. `script::window` is what the guest builds;
    /// covering it here means a change to the shared script fails on the host in milliseconds
    /// rather than in QEMU minutes later.
    #[test]
    fn each_window_shows_its_own_banner_and_its_own_typing() {
        let a = script::window(0, script::COLS, script::ROWS);
        let b = script::window(1, script::COLS, script::ROWS);
        let (ra, rb) = (rows(&a), rows(&b));
        assert_eq!(ra[0].trim_end(), "term0");
        assert_eq!(rb[0].trim_end(), "term1");
        assert_ne!(
            ra, rb,
            "two windows rendering identically would pass a mixed-up test"
        );
        assert!(
            ra.iter().any(|r| r.contains('A')) && rb.iter().any(|r| r.contains('B')),
            "each window must show what was typed at it, not at its neighbour",
        );
    }

    /// The script the milestone-29 tests drive is a fair one: it uses more than one row, more than
    /// one rendition, and leaves a picture that a blank screen, a shifted screen, or a screen
    /// missing its last write cannot match. Asserted here so the QEMU test is not the first thing to
    /// find out otherwise.
    #[test]
    fn the_demo_script_produces_a_picture_worth_checking() {
        // Drive `script::full_screen()` rather than rebuilding it here. The guest test renders
        // exactly that, so reconstructing the setup by hand let the two witnesses diverge: this
        // test fed GREETING and stopped, while the screen being graded in QEMU also had TYPED on
        // it. `script.rs` exists so every party agrees, which only works if every party calls it.
        let t = script::full_screen();
        let seen = rows(&t);
        assert_eq!(seen[0].trim_end(), "nife");
        assert!(
            seen.iter().filter(|r| !r.trim().is_empty()).count() >= 3,
            "the script fills one row: a stride bug would be invisible",
        );
        let attrs: Vec<Attr> = (0..t.rows())
            .flat_map(|r| (0..t.cols()).map(move |c| (c, r)))
            .map(|(c, r)| t.cell(c, r).attr)
            .collect();
        assert!(
            attrs.iter().any(|a| *a != Attr::DEFAULT),
            "the script never changes rendition: the colour path is untested on the machine",
        );
        assert!(
            (0..t.rows()).any(|r| (0..t.cols()).any(|c| t.cell(c, r).byte != b' ')),
            "the script drew nothing",
        );
    }
}
