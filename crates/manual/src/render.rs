//! The streaming renderer: markdown bytes in, styled terminal bytes out, no allocator.
//!
//! The state machine is line-oriented, which is what lets it be allocation-free. Bytes accumulate
//! in one fixed buffer until a newline completes a source line; the line is classified, and
//! whatever output it completed goes to the sink. Nothing is retained between lines except the
//! block state a paragraph or a list needs, and the one exception that has to be retained is a
//! table, because column widths are not knowable until the last row.

use crate::Sink;

/// The longest source line the renderer holds. The longest line in this repository's markdown is
/// 1835 bytes; this is the next power of two above it, so the corpus fits with room and a document
/// from elsewhere fails loudly through [`Renderer::truncated`] rather than quietly.
pub const LINE_MAX: usize = 2048;

/// How deep inline markup may nest before the renderer stops descending and emits the rest
/// literally. A bound rather than a guess: this runs in a process with one 4 KiB stack page, so an
/// unbounded recursion on adversarial input is a fault, not a slow render.
pub const MAX_DEPTH: u8 = 3;

/// Columns a table may have. Wider tables lose their right-hand columns.
pub const TABLE_COLS: usize = 8;

/// Rows a table may hold at once.
///
/// A longer table is **not** truncated: it is emitted in chunks of this many rows, each aligned to
/// its own widths. That is the failure mode worth having, because the alternative loses text. This
/// repository's largest table is 117 rows (`design/roadmap/README.md`), so the case is real rather
/// than defensive.
pub const TABLE_ROWS: usize = 48;

/// Bytes of cell text the renderer holds at once, spilling into a new chunk when it is full, on the
/// same terms as [`TABLE_ROWS`].
pub const TABLE_TEXT: usize = 8192;

/// How the renderer is asked to draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    /// The terminal's width in columns. Text wraps to it; a code block and a long word do not.
    pub width: u16,
    /// Whether to emit SGR escape sequences.
    ///
    /// **This is a capability question wearing a boolean's clothes.** `doc` writes to whatever
    /// endpoint it was handed and has no way to ask what is behind it, which is the sink contract
    /// working as designed. So the answer is not sniffed the way `isatty` sniffs it: the *shell*
    /// knows whether it wired this program to a terminal or to a pipe, and passes the answer in.
    pub color: bool,
}

impl Style {
    /// Eighty columns with colour, which is the serial console.
    pub const DEFAULT: Style = Style {
        width: 80,
        color: true,
    };
}

/// A run of output that shares one terminal attribute.
///
/// Only sequences `video_terminal` can act on are used for the things that matter (bold and the
/// eight colours); [`Attr::Emph`] and [`Attr::Strike`] deliberately use sequences it drops, because
/// they still show on the serial console's far end and the alternative was spending a colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attr {
    /// Body text.
    Plain,
    /// A heading.
    Heading,
    /// `**strong**`.
    Strong,
    /// `*emphasis*`.
    Emph,
    /// `~~strikethrough~~`.
    Strike,
    /// An inline `` `code` `` span.
    Code,
    /// A fenced code block's contents.
    Fence,
    /// A link's destination.
    Link,
    /// A list bullet or number, and a block quote's rule.
    Marker,
}

impl Attr {
    /// The escape sequence that turns this attribute on, assuming a reset came first.
    ///
    /// Each is a single-parameter sequence on purpose: `video_terminal` drops any CSI carrying more
    /// than four parameters, so a combined `\x1b[1;33m` would still work but a truecolour one would
    /// not, and keeping every sequence to one parameter means the rule never has to be remembered.
    const fn sgr(self) -> &'static [u8] {
        match self {
            Attr::Plain => b"",
            Attr::Heading | Attr::Strong => b"\x1b[1m",
            Attr::Emph => b"\x1b[4m",
            Attr::Strike => b"\x1b[9m",
            Attr::Code => b"\x1b[36m",
            Attr::Fence => b"\x1b[32m",
            Attr::Link => b"\x1b[34m",
            Attr::Marker => b"\x1b[33m",
        }
    }
}

/// How a table column is aligned, read off the delimiter row (`|---|:--:|---:|`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

/// A table held until its widths are known.
struct Table {
    /// `(start, len)` into [`Table::text`], by row and column.
    cell: [[(u16, u16); TABLE_COLS]; TABLE_ROWS],
    /// How many columns each row actually had.
    cols: [u8; TABLE_ROWS],
    text: [u8; TABLE_TEXT],
    used: usize,
    rows: usize,
    align: [Align; TABLE_COLS],
    /// Whether a delimiter row has been seen, which is what separates a real table from a
    /// paragraph that happens to start with a pipe.
    delimited: bool,
}

/// The renderer.
///
/// It is about eight kilobytes, which is twice a user process's whole stack, so a program keeps one
/// in `.bss` rather than on the stack. `user/src/doc.rs` shows the shape.
pub struct Renderer {
    style: Style,

    line: [u8; LINE_MAX],
    used: usize,
    /// Set once and never cleared: some source line did not fit.
    over: bool,

    // Output cursor state.
    col: usize,
    /// The column text starts at on a wrapped line, including the block quote rule.
    start: usize,
    /// The left margin the current block indents to, before any quote rule.
    margin: usize,
    /// Block quote depth of the block being emitted.
    quote: usize,
    attr: Attr,
    /// Whether words are being uppercased, which only a level-one heading turns on.
    upper_on: bool,
    /// A space is owed before the next word, because words are separated on output rather than
    /// copied with their source spacing.
    space: bool,
    /// Anything at all has been emitted, so a blank line would separate rather than lead.
    started: bool,
    /// A blank line is owed before the next block.
    gap: bool,

    // Block state.
    fence: Option<(u8, usize)>,
    /// The margin a list item's continuation lines align to, or `None` outside a list.
    hang: Option<usize>,

    table: Table,
}

impl Renderer {
    /// A renderer with no input yet.
    ///
    /// `const` so that a `no_std` program can put one in a `static` and never build it on the
    /// stack: this struct is about eleven kilobytes and a user process gets one 4 KiB stack page,
    /// so a non-const constructor would overflow at the assignment rather than in use.
    pub const fn new(style: Style) -> Renderer {
        Renderer {
            style,
            line: [0; LINE_MAX],
            used: 0,
            over: false,
            col: 0,
            start: 0,
            margin: BODY,
            quote: 0,
            attr: Attr::Plain,
            upper_on: false,
            space: false,
            started: false,
            gap: false,
            fence: None,
            hang: None,
            table: Table {
                cell: [[(0, 0); TABLE_COLS]; TABLE_ROWS],
                cols: [0; TABLE_ROWS],
                text: [0; TABLE_TEXT],
                used: 0,
                rows: 0,
                align: [Align::Left; TABLE_COLS],
                delimited: false,
            },
        }
    }

    /// Whether any source line was longer than [`LINE_MAX`] and lost its tail.
    ///
    /// Sticky, because the caller wants to know that the document it just printed is incomplete,
    /// not which line did it.
    pub fn truncated(&self) -> bool {
        self.over
    }

    /// Take input bytes and write whatever output they completed.
    ///
    /// Any framing is fine: one byte at a time and the whole document at once produce identical
    /// output, which is the property that lets `doc` render straight off sixteen-byte sink
    /// messages without a reassembly buffer of its own.
    pub fn feed(&mut self, bytes: &[u8], out: &mut impl Sink) {
        for &b in bytes {
            if b == b'\n' {
                let n = self.used;
                self.used = 0;
                self.line_done(n, out);
            } else if self.used < LINE_MAX {
                self.line[self.used] = b;
                self.used += 1;
            } else {
                self.over = true;
            }
        }
    }

    /// End the document: render a last line with no newline on it, close an open table, and leave
    /// the terminal's attributes as they were found.
    pub fn finish(&mut self, out: &mut impl Sink) {
        if self.used > 0 {
            let n = self.used;
            self.used = 0;
            self.line_done(n, out);
        }
        self.flush_table(out);
        // `close_line` and not `open_block`: a blank line owed to a block that never arrived is a
        // blank line nobody wants at the foot of a page.
        self.close_line(out);
    }

    // ---- line classification -------------------------------------------------------------

    fn line_done(&mut self, n: usize, out: &mut impl Sink) {
        // Work on a copy of the bounds rather than the buffer, so the classifier can borrow the
        // line while the emitters borrow `self` mutably.
        let mut end = n;
        if end > 0 && self.line[end - 1] == b'\r' {
            end -= 1;
        }

        if let Some((ch, len)) = self.fence {
            let (i, _) = indent_of(&self.line[..end]);
            if is_closing(&self.line[i..end], ch, len) {
                self.fence = None;
                self.gap = true;
            } else {
                self.code_line(end, out);
            }
            return;
        }

        // A block quote's `>` markers are stripped here and turned into a rule at line start, so
        // every classifier below sees the quoted line exactly as it would see an unquoted one.
        let (mut i, _) = indent_of(&self.line[..end]);
        let mut quote = 0;
        while i < end && self.line[i] == b'>' {
            quote += 1;
            i += 1;
            if i < end && self.line[i] == b' ' {
                i += 1;
            }
            let (j, _) = indent_of(&self.line[i..end]);
            i += j;
        }
        let ind = if quote > 0 { 0 } else { i };
        let body = i..end;

        if body.is_empty() {
            self.flush_table(out);
            self.close_line(out);
            self.hang = None;
            self.gap = self.started;
            return;
        }

        if quote != self.quote {
            self.flush_table(out);
            self.close_line(out);
            self.quote = quote;
        }

        if let Some(f) = fence_at(&self.line[body.clone()]) {
            self.flush_table(out);
            self.close_line(out);
            self.fence = Some(f);
            return;
        }

        if self.line[body.start] == b'|' {
            self.take_table_row(body, out);
            return;
        }
        self.flush_table(out);

        if let Some(level) = heading_at(&self.line[body.clone()]) {
            self.heading(level, body, out);
            return;
        }

        if is_rule(&self.line[body.clone()]) {
            self.rule(out);
            return;
        }

        if let Some(marker) = marker_at(&self.line[body.clone()]) {
            self.list_item(ind, marker, body, out);
            return;
        }

        // Everything else is paragraph text, which includes a continuation line of the paragraph
        // or list item above it. That is why a list's `hang` survives until a blank line: the
        // indent it set is what a continuation aligns to.
        if self.hang.is_none() {
            self.margin = BODY + ind.min(MAX_INDENT);
        }
        // A continuation line of the paragraph above leaves the cursor mid-line, so this is the
        // test for "am I starting a block" without a flag to keep in step.
        if self.col == 0 {
            self.open_block(out);
        }
        self.inline(body, Attr::Plain, 0, out);
        self.space = true;
    }

    // ---- block emitters ------------------------------------------------------------------

    fn heading(&mut self, level: u8, body: core::ops::Range<usize>, out: &mut impl Sink) {
        self.open_block(out);
        self.hang = None;
        self.margin = if level <= 2 {
            0
        } else {
            BODY + 2 * (level as usize - 3)
        };
        let text = body.start + level as usize + 1..body.end;
        // Level one is the page's own name and is uppercased, which is the one thing this borrows
        // from a man page's layout: a reader scanning a screenful finds the title without reading
        // it. Levels below it keep their case, because they are sentences here and shouting them
        // costs more than it buys.
        self.upper(level == 1);
        self.inline(text, Attr::Heading, 0, out);
        self.upper(false);
        self.close_line(out);
        self.gap = true;
    }

    fn rule(&mut self, out: &mut impl Sink) {
        self.open_block(out);
        self.hang = None;
        self.margin = BODY;
        self.line_start(out);
        self.set(Attr::Marker, out);
        let n = (self.style.width as usize).saturating_sub(self.start);
        let dashes = [b'-'; 64];
        let mut left = n;
        while left > 0 {
            let take = left.min(64);
            out.put(&dashes[..take]);
            left -= take;
        }
        self.col += n;
        self.close_line(out);
        self.gap = true;
    }

    fn code_line(&mut self, end: usize, out: &mut impl Sink) {
        self.open_block(out);
        self.margin = CODE;
        self.line_start(out);
        self.set(Attr::Fence, out);
        // Verbatim: a code block is the one thing in a document whose line breaks are its meaning,
        // so it is never wrapped and never re-spaced. A line wider than the terminal runs over,
        // which is what every pager does with code too.
        let vis = visible(&self.line[..end]);
        out.put(&self.line[..end]);
        self.col += vis;
        self.close_line(out);
    }

    fn list_item(
        &mut self,
        ind: usize,
        marker: (usize, bool),
        body: core::ops::Range<usize>,
        out: &mut impl Sink,
    ) {
        let (mlen, _ordered) = marker;
        self.open_block(out);
        self.hang = None;
        self.margin = BODY + ind.min(MAX_INDENT);
        self.line_start(out);
        self.set(Attr::Marker, out);
        // A bullet is normalised to `-` whichever of `-`, `*` and `+` the author typed, because
        // three spellings of one thing on one screen reads as three different things.
        let wrote = if self.line[body.start] == b'*' || self.line[body.start] == b'+' {
            out.put(b"- ");
            2
        } else {
            out.put(&self.line[body.start..body.start + mlen]);
            mlen
        };
        self.col += wrote;
        // Continuation lines align under the item's text, not under its bullet, which is what makes
        // a wrapped list item readable as one item.
        self.margin += wrote;
        self.hang = Some(self.margin);
        self.inline(body.start + mlen..body.end, Attr::Plain, 0, out);
        self.space = true;
    }

    // ---- tables --------------------------------------------------------------------------

    fn take_table_row(&mut self, body: core::ops::Range<usize>, out: &mut impl Sink) {
        // A delimiter row is what promotes a run of pipe-led lines to a table. Until one arrives
        // the rows are held; if the block ends without one they are emitted as ordinary text, so a
        // paragraph that begins with a pipe is not silently eaten.
        if is_delimiter(&self.line[body.clone()]) {
            self.read_align(body);
            self.table.delimited = true;
            return;
        }
        // A table that outgrows the buffer spills into a second aligned chunk rather than losing
        // its tail. `body.len()` is an upper bound on the bytes this row will store, since trimming
        // and the dropped trailing cell only ever remove some.
        if self.table.rows >= TABLE_ROWS || self.table.used + (body.end - body.start) > TABLE_TEXT {
            self.flush_table(out);
            // No blank line between the chunks: they are one table, and a gap would say otherwise.
            self.gap = false;
        }
        self.close_line(out);
        let row = self.table.rows;
        let mut col = 0;
        let mut i = body.start + 1;
        while i <= body.end && col < TABLE_COLS {
            // A `\|` is a pipe in the cell's text, not a column boundary. `notes/scripts.md` has
            // one (`--arch aarch64\| riscv64`), and reading it as a separator gave that table a
            // third column nobody wrote and squeezed the other two to pay for it.
            let mut j = i;
            while j < body.end && !(self.line[j] == b'|' && (j == i || self.line[j - 1] != b'\\')) {
                j += 1;
            }
            let cell = trim(&self.line[i..j.min(body.end)]);
            // A trailing pipe produces one empty cell that nobody typed; drop it rather than
            // widening every table by a blank column.
            if !(cell.is_empty() && j >= body.end) {
                let start = self.table.used;
                let mut n = 0;
                let mut k = 0;
                while k < cell.len() && start + n < TABLE_TEXT {
                    // The backslash of an escape is the escape, not the text.
                    if cell[k] == b'\\' && k + 1 < cell.len() && cell[k + 1] == b'|' {
                        k += 1;
                        continue;
                    }
                    self.table.text[start + n] = cell[k];
                    n += 1;
                    k += 1;
                }
                self.table.used += n;
                self.table.cell[row][col] = (start as u16, n as u16);
                col += 1;
            }
            if j >= body.end {
                break;
            }
            i = j + 1;
        }
        self.table.cols[row] = col as u8;
        self.table.rows += 1;
    }

    fn read_align(&mut self, body: core::ops::Range<usize>) {
        let mut col = 0;
        let mut i = body.start + 1;
        while i <= body.end && col < TABLE_COLS {
            let mut j = i;
            while j < body.end && self.line[j] != b'|' {
                j += 1;
            }
            let spec = trim(&self.line[i..j.min(body.end)]);
            if !spec.is_empty() {
                let left = spec[0] == b':';
                let right = spec[spec.len() - 1] == b':';
                self.table.align[col] = match (left, right) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                };
                col += 1;
            }
            if j >= body.end {
                break;
            }
            i = j + 1;
        }
    }

    fn flush_table(&mut self, out: &mut impl Sink) {
        if self.table.rows == 0 {
            return;
        }
        let rows = self.table.rows;
        let delimited = self.table.delimited;
        self.table.rows = 0;
        self.table.used = 0;
        self.table.delimited = false;

        let ncols = (0..rows)
            .map(|r| self.table.cols[r] as usize)
            .max()
            .unwrap_or(0);
        if ncols == 0 {
            return;
        }

        self.open_block(out);

        // Natural widths first, then shrink the widest column until the row fits. Shrinking the
        // widest is the rule that keeps a table of one long prose column and three short ones
        // readable: the prose loses characters and the labels do not.
        let mut w = [0usize; TABLE_COLS];
        for r in 0..rows {
            let here = self.table.cols[r] as usize;
            for (c, width) in w.iter_mut().enumerate().take(here) {
                let (s, l) = self.table.cell[r][c];
                let vis = visible(&self.table.text[s as usize..(s + l) as usize]);
                if vis > *width {
                    *width = vis;
                }
            }
        }
        let avail = (self.style.width as usize).saturating_sub(BODY + 2 * self.quote);
        // Each column costs its width plus the three bytes of " | " that follow it, less the last.
        let overhead = 3 * (ncols - 1);
        let mut total: usize = w[..ncols].iter().sum::<usize>() + overhead;
        while total > avail {
            let (widest, _) = w[..ncols]
                .iter()
                .enumerate()
                .max_by_key(|&(_, v)| *v)
                .unwrap_or((0, &0));
            if w[widest] <= MIN_CELL {
                break;
            }
            w[widest] -= 1;
            total -= 1;
        }

        self.margin = BODY;
        for r in 0..rows {
            self.line_start(out);
            for (c, width) in w.iter().enumerate().take(ncols) {
                let width = *width;
                if c > 0 {
                    self.set(Attr::Marker, out);
                    out.put(b" | ");
                    self.col += 3;
                }
                let (s, l) = if c < self.table.cols[r] as usize {
                    self.table.cell[r][c]
                } else {
                    (0, 0)
                };
                let attr = if r == 0 && delimited {
                    Attr::Strong
                } else {
                    Attr::Plain
                };
                // The attribute is set before the cell's bytes are touched, because emitting them
                // borrows the table arena and `set` borrows all of `self`. A cell is written as one
                // unit and never wrapped: a wrapped cell would not line up with its column, which
                // is the only thing a table is for.
                self.set(attr, out);
                let (lo, hi) = (s as usize, (s + l) as usize);
                let mut n = 0;
                let mut k = lo;
                while k < hi && n < width {
                    let step = char_len(&self.table.text[..hi], k);
                    out.put(&self.table.text[k..k + step]);
                    k += step;
                    n += 1;
                }
                self.col += n;
                pad(out, width - n);
                self.col += width - n;
            }
            self.close_line(out);
            if r == 0 && delimited {
                self.line_start(out);
                self.set(Attr::Marker, out);
                for (c, width) in w.iter().enumerate().take(ncols) {
                    if c > 0 {
                        out.put(b"-+-");
                        self.col += 3;
                    }
                    let dashes = [b'-'; 64];
                    let mut left = *width;
                    while left > 0 {
                        let take = left.min(64);
                        out.put(&dashes[..take]);
                        left -= take;
                    }
                    self.col += *width;
                }
                self.close_line(out);
            }
        }
        self.gap = true;
    }

    // ---- inline --------------------------------------------------------------------------

    /// Render one line's worth of inline markup.
    ///
    /// `base` is the attribute plain text takes, so a heading's words come out as a heading without
    /// the inline scanner knowing what a heading is.
    fn inline(&mut self, r: core::ops::Range<usize>, base: Attr, depth: u8, out: &mut impl Sink) {
        let (start, end) = (r.start, r.end);
        let mut i = start;
        let mut word = i;
        while i < end {
            let b = self.line[i];
            if b == b' ' || b == b'\t' {
                self.emit(word..i, base, out);
                self.space = true;
                while i < end && (self.line[i] == b' ' || self.line[i] == b'\t') {
                    i += 1;
                }
                word = i;
                continue;
            }
            // A code span is found first and consumed whole, which is what keeps `*` and `[` inside
            // one from being read as markup. That ordering is the single most load-bearing thing in
            // this scanner for this corpus: it is 11,281 code spans, many of them full of the very
            // characters the other rules look for.
            if b == b'`'
                && let Some(close) = find(&self.line[i + 1..end], b'`').map(|k| i + 1 + k)
            {
                self.emit(word..i, base, out);
                self.unit(i + 1..close, Attr::Code, out);
                i = close + 1;
                word = i;
                continue;
            }
            if b == b'*' && depth < MAX_DEPTH {
                let strong = i + 1 < end && self.line[i + 1] == b'*';
                let open = i + if strong { 2 } else { 1 };
                if open < end
                    && self.line[open] != b' '
                    && let Some(close) = closer(&self.line[..end], open, b'*', strong)
                {
                    self.emit(word..i, base, out);
                    let a = if strong { Attr::Strong } else { Attr::Emph };
                    self.inline(open..close, a, depth + 1, out);
                    i = close + if strong { 2 } else { 1 };
                    word = i;
                    continue;
                }
            }
            if b == b'~'
                && i + 1 < end
                && self.line[i + 1] == b'~'
                && depth < MAX_DEPTH
                && let Some(close) = closer(&self.line[..end], i + 2, b'~', true)
            {
                self.emit(word..i, base, out);
                self.inline(i + 2..close, Attr::Strike, depth + 1, out);
                i = close + 2;
                word = i;
                continue;
            }
            if (b == b'[' || (b == b'!' && i + 1 < end && self.line[i + 1] == b'['))
                && depth < MAX_DEPTH
            {
                let image = b == b'!';
                let lb = if image { i + 1 } else { i };
                if let Some(rb) = find(&self.line[lb..end], b']').map(|k| lb + k)
                    && rb + 1 < end
                    && self.line[rb + 1] == b'('
                    && let Some(rp) = find(&self.line[rb + 2..end], b')').map(|k| rb + 2 + k)
                {
                    self.emit(word..i, base, out);
                    if image {
                        self.unit_bytes(b"[image:", Attr::Marker, out);
                        self.space = true;
                    }
                    self.inline(lb + 1..rb, base, depth + 1, out);
                    if image {
                        self.unit_bytes(b"]", Attr::Marker, out);
                    } else {
                        // The destination is shown rather than hidden, because a terminal has no
                        // way to follow a link and a reader who cannot see where one points has
                        // been given a worse document than the source.
                        self.space = true;
                        self.unit_wrapped(rb + 2..rp, Attr::Link, out);
                    }
                    i = rp + 1;
                    word = i;
                    continue;
                }
            }
            i += 1;
        }
        self.emit(word..end, base, out);
    }

    /// Emit a run of source bytes as one word.
    fn emit(&mut self, r: core::ops::Range<usize>, attr: Attr, out: &mut impl Sink) {
        if r.start >= r.end {
            return;
        }
        self.unit(r, attr, out);
    }

    /// Place one unbreakable run at the cursor, wrapping first if it does not fit.
    fn unit(&mut self, r: core::ops::Range<usize>, attr: Attr, out: &mut impl Sink) {
        let vis = visible(&self.line[r.clone()]);
        self.before_unit(vis, out);
        self.set(attr, out);
        // The uppercase pass copies through a small stack buffer rather than allocating, which is
        // the only reason a heading can shout without a heap.
        if self.upper_on {
            let mut k = r.start;
            while k < r.end {
                let take = (r.end - k).min(64);
                let mut buf = [0u8; 64];
                buf[..take].copy_from_slice(&self.line[k..k + take]);
                buf[..take].make_ascii_uppercase();
                out.put(&buf[..take]);
                k += take;
            }
        } else {
            out.put(&self.line[r]);
        }
        self.col += vis;
        self.space = false;
    }

    /// The same, for bytes that are not in the line buffer.
    fn unit_bytes(&mut self, bytes: &[u8], attr: Attr, out: &mut impl Sink) {
        let vis = visible(bytes);
        self.before_unit(vis, out);
        self.set(attr, out);
        out.put(bytes);
        self.col += vis;
        self.space = false;
    }

    /// A run that may be broken across lines if it is longer than the terminal, which is what a
    /// long URL needs and what a word does not want.
    fn unit_wrapped(&mut self, r: core::ops::Range<usize>, attr: Attr, out: &mut impl Sink) {
        let width = self.style.width as usize;
        let mut k = r.start;
        while k < r.end {
            let room = width.saturating_sub(self.col.max(self.start)).max(1);
            let mut take = 0;
            let mut n = 0;
            while k + take < r.end && n < room {
                take += char_len(&self.line[..r.end], k + take);
                n += 1;
            }
            self.unit(k..k + take, attr, out);
            k += take;
            if k < r.end {
                self.newline(out);
            }
        }
    }

    fn before_unit(&mut self, vis: usize, out: &mut impl Sink) {
        if self.col > self.start && self.space {
            if self.col + 1 + vis > self.style.width as usize {
                self.newline(out);
            } else {
                self.set(Attr::Plain, out);
                out.put(b" ");
                self.col += 1;
            }
        }
        if self.col == 0 {
            self.line_start(out);
        }
        self.space = false;
    }

    // ---- cursor --------------------------------------------------------------------------

    /// Open a line: the margin, then one `|` rule per level of block quote.
    fn line_start(&mut self, out: &mut impl Sink) {
        let margin = self.hang.unwrap_or(self.margin);
        pad(out, margin);
        self.col = margin;
        for _ in 0..self.quote {
            self.set(Attr::Marker, out);
            out.put(b"| ");
            self.col += 2;
        }
        self.start = self.col;
        self.started = true;
    }

    fn newline(&mut self, out: &mut impl Sink) {
        // The attribute is reset *before* the newline, never after, because a terminal that is left
        // bold at end of line paints the rest of the row and the next prompt with it.
        self.set(Attr::Plain, out);
        out.put(b"\n");
        self.col = 0;
        self.space = false;
    }

    /// End whatever line is open, leaving any blank line owed to the next block still owed.
    fn close_line(&mut self, out: &mut impl Sink) {
        if self.col > 0 {
            self.newline(out);
        }
        self.space = false;
    }

    /// Begin a block: end the open line and spend the blank line owed to it.
    ///
    /// The separation from [`Renderer::close_line`] is what stops a blank source line and the block
    /// after it from each emitting a gap, which is one blank line too many and was the first thing
    /// the tests caught.
    fn open_block(&mut self, out: &mut impl Sink) {
        self.close_line(out);
        if self.gap && self.started {
            out.put(b"\n");
        }
        self.gap = false;
    }

    fn set(&mut self, attr: Attr, out: &mut impl Sink) {
        if !self.style.color || attr == self.attr {
            self.attr = attr;
            return;
        }
        if self.attr != Attr::Plain {
            out.put(b"\x1b[0m");
        }
        out.put(attr.sgr());
        self.attr = attr;
    }

    fn upper(&mut self, on: bool) {
        self.upper_on = on;
    }
}

// ---- free helpers ------------------------------------------------------------------------

/// The left margin body text takes. Two rather than a man page's five, because the other terminal
/// this system has is 32 columns wide and five of them is a sixth of the screen.
const BODY: usize = 2;
/// The margin a fenced code block takes, two in from body text so it reads as set apart without a
/// border character that a 32-column terminal cannot spare.
const CODE: usize = 4;
/// How far a source indent is honoured before it is clamped. A deeply indented list would otherwise
/// push text off a narrow screen.
const MAX_INDENT: usize = 8;
/// The narrowest a table column may be squeezed before the renderer gives up and lets the row
/// overflow, because a two-character column tells a reader nothing.
const MIN_CELL: usize = 3;

fn pad(out: &mut impl Sink, n: usize) {
    let spaces = [b' '; 64];
    let mut left = n;
    while left > 0 {
        let take = left.min(64);
        out.put(&spaces[..take]);
        left -= take;
    }
}

/// Visible width: bytes, less UTF-8 continuation bytes, which occupy no column of their own.
fn visible(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

/// How many bytes the character starting at `i` occupies.
fn char_len(bytes: &[u8], i: usize) -> usize {
    let mut n = 1;
    while i + n < bytes.len() && (bytes[i + n] & 0xC0) == 0x80 {
        n += 1;
    }
    n
}

fn find(hay: &[u8], b: u8) -> Option<usize> {
    hay.iter().position(|&x| x == b)
}

fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && (s[a] == b' ' || s[a] == b'\t') {
        a += 1;
    }
    while b > a && (s[b - 1] == b' ' || s[b - 1] == b'\t') {
        b -= 1;
    }
    &s[a..b]
}

fn indent_of(s: &[u8]) -> (usize, usize) {
    let mut i = 0;
    let mut cols = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        cols += if s[i] == b'\t' { 4 } else { 1 };
        i += 1;
    }
    (i, cols)
}

/// `# ` through `###### `, and nothing else: a `#` with no space after it is a comment in half the
/// code in this repository's fenced blocks and is not a heading anywhere.
fn heading_at(s: &[u8]) -> Option<u8> {
    let mut n = 0;
    while n < s.len() && s[n] == b'#' {
        n += 1;
    }
    if (1..=6).contains(&n) && n < s.len() && s[n] == b' ' {
        Some(n as u8)
    } else {
        None
    }
}

/// An opening fence: three or more backticks or tildes.
fn fence_at(s: &[u8]) -> Option<(u8, usize)> {
    let c = *s.first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = s.iter().take_while(|&&b| b == c).count();
    if n >= 3 { Some((c, n)) } else { None }
}

/// A closing fence: the same character, at least as long, and nothing else on the line.
fn is_closing(s: &[u8], ch: u8, len: usize) -> bool {
    let n = s.iter().take_while(|&&b| b == ch).count();
    n >= len && trim(&s[n..]).is_empty()
}

/// Three or more of `-`, `*` or `_`, with only spaces between them.
fn is_rule(s: &[u8]) -> bool {
    let c = match s.first() {
        Some(&c) if c == b'-' || c == b'*' || c == b'_' => c,
        _ => return false,
    };
    let mut n = 0;
    for &b in s {
        if b == c {
            n += 1;
        } else if b != b' ' && b != b'\t' {
            return false;
        }
    }
    n >= 3
}

/// A list marker, returning `(marker length including its trailing space, ordered)`.
fn marker_at(s: &[u8]) -> Option<(usize, bool)> {
    if s.len() >= 2 && (s[0] == b'-' || s[0] == b'*' || s[0] == b'+') && s[1] == b' ' {
        return Some((2, false));
    }
    let mut n = 0;
    while n < s.len() && s[n].is_ascii_digit() {
        n += 1;
    }
    if n > 0 && n + 1 < s.len() && (s[n] == b'.' || s[n] == b')') && s[n + 1] == b' ' {
        return Some((n + 2, true));
    }
    None
}

/// A table's delimiter row: pipes, dashes, colons and spaces, with at least one dash.
fn is_delimiter(s: &[u8]) -> bool {
    let mut dashes = 0;
    for &b in s {
        match b {
            b'-' => dashes += 1,
            b'|' | b':' | b' ' | b'\t' => {}
            _ => return false,
        }
    }
    dashes >= 1
}

/// Find the closing delimiter for emphasis opened at `open`.
///
/// The rule is deliberately narrower than `CommonMark`'s: a closer must not be preceded by a space,
/// which is what stops a stray `*` mid-sentence from swallowing the rest of a paragraph. And `_` is
/// **not** an emphasis delimiter in this renderer at all, because this repository writes
/// `snake_case` identifiers in running prose constantly (`fs_proto`, `line_editor`,
/// `__rust_alloc`) and treating them as markup would misrender far more than it would style.
fn closer(s: &[u8], open: usize, ch: u8, double: bool) -> Option<usize> {
    let mut i = open;
    while i < s.len() {
        if s[i] == ch && (!double || (i + 1 < s.len() && s[i + 1] == ch)) {
            if i > open && s[i - 1] != b' ' {
                return Some(i);
            }
            i += if double { 2 } else { 1 };
            continue;
        }
        i += 1;
    }
    None
}
