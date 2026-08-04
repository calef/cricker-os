//! **The manual**: markdown in, styled terminal bytes out, and the index that says what exists.
//!
//! Milestone 40. This crate is the pure half of the documentation service. It holds three things
//! and no others: a streaming markdown renderer ([`Renderer`]), the byte layout of a search index
//! ([`index`]), and the query that reads one. There is no IO here, no syscall, no endpoint and no
//! allocator on the path a confined program takes. `user/src/doc.rs` is the program; this is what
//! it computes.
//!
//! # The renderer is a stream, not a parser
//!
//! [`Renderer::feed`] takes whatever bytes arrived and writes whatever output they completed. It
//! never holds a document, never builds a tree, and never allocates. That shape is not an
//! optimisation, it is what the program's capabilities already are: `doc` receives its input as
//! [`sink_proto`](../sink_proto/index.html) messages of sixteen bytes each and writes its output
//! the same way, so a renderer that needed the whole document would need somewhere to put it, and
//! somewhere to put it is a memory grant the program can otherwise do without.
//!
//! The cost is stated rather than hidden: a source line longer than [`LINE_MAX`] is truncated, and
//! a table wider than [`TABLE_COLS`] loses its right-hand columns. A table *longer* than
//! [`TABLE_ROWS`] does not lose rows; it spills into a second aligned chunk, because losing text is
//! the one failure mode a documentation service cannot have. See `BUGS`.
//!
//! # What it renders
//!
//! ATX headings, paragraphs with word wrapping, fenced code blocks, block quotes, bullet and
//! ordered lists with nesting by indent, tables with computed column widths, thematic breaks, and
//! the inline set: `**strong**`, `*emphasis*`, `` `code` ``, `~~strike~~`, `[text](dest)` and
//! `![alt](dest)`. That set was chosen by counting: it is every construct that appears in this
//! repository's own 328 markdown files, which is the corpus the viewer exists to serve.
//!
//! # EXAMPLES
//!
//! Render a document to plain text at eighty columns:
//!
//! ```
//! use manual::{Renderer, Sink, Style};
//!
//! struct Buf(String);
//! impl Sink for Buf {
//!     fn put(&mut self, bytes: &[u8]) {
//!         self.0.push_str(core::str::from_utf8(bytes).unwrap());
//!     }
//! }
//!
//! let mut out = Buf(String::new());
//! let mut r = Renderer::new(Style { width: 80, color: false });
//! r.feed(b"# Title\n\nSome **strong** text.\n", &mut out);
//! r.finish(&mut out);
//! assert_eq!(out.0, "TITLE\n\n  Some strong text.\n");
//! ```
//!
//! The same document with colour on, which is what `doc` emits when it is writing to a terminal:
//!
//! ```
//! # use manual::{Renderer, Sink, Style};
//! # struct Buf(String);
//! # impl Sink for Buf {
//! #     fn put(&mut self, bytes: &[u8]) { self.0.push_str(core::str::from_utf8(bytes).unwrap()); }
//! # }
//! let mut out = Buf(String::new());
//! let mut r = Renderer::new(Style { width: 80, color: true });
//! r.feed(b"a **b** c\n", &mut out);
//! r.finish(&mut out);
//! assert!(out.0.contains("\x1b[1mb"));
//! ```
//!
//! # BUGS
//!
//! - **A source line longer than [`LINE_MAX`] is truncated**, and the truncation is silent in the
//!   output. The longest line in this repository is 1835 bytes (a table row in `notes/scripts.md`),
//!   which is why the limit is what it is; a document from somewhere else may lose text. The
//!   renderer records it, so a caller that wants to know can ask [`Renderer::truncated`].
//! - **Setext headings (`Title` over `=====`) are not recognised.** Nothing in this repository uses
//!   one, and `---` on its own line is far more often a thematic break here, so treating it as a
//!   heading would misrender 64 real lines to catch zero.
//! - **Reference links (`[text][label]`) render as their literal text.** There is exactly one in
//!   the corpus.
//! - **No HTML.** An HTML tag is passed through as text, which is what it looks like in a terminal
//!   anyway. There are 53 in the corpus and every one of them is either inside a code span or a
//!   placeholder like `<name>` that a reader wants to see literally.
//! - **Emphasis is rendered as underline (SGR 4), which `video_terminal` silently drops** because
//!   its SGR handler implements only 0, 1, 7, 22, 27 and the colour ranges. On the serial console,
//!   where the far end is the host's terminal, it shows. So emphasis is visible on one of the two
//!   terminals this system has, and the choice was between that and spending a colour on it.
//! - **Width is counted in characters, not columns.** A UTF-8 continuation byte counts as zero, so
//!   ASCII and Latin text wrap correctly and a wide CJK character is counted as one column when it
//!   occupies two. There is no CJK in the corpus and no font that could draw it.

#![no_std]

#[cfg(feature = "builder")]
extern crate alloc;

pub mod index;
mod render;

pub use render::{Attr, LINE_MAX, MAX_DEPTH, Renderer, Style, TABLE_COLS, TABLE_ROWS, TABLE_TEXT};

/// Somewhere rendered bytes go.
///
/// The same shape as [`line_editor::Sink`](../line_editor/trait.Sink.html), and deliberately not
/// that trait: this crate has no dependencies, and a renderer that needed the line discipline crate
/// to describe "bytes leave here" would have taken one for a four-line definition. A caller that
/// holds both can implement this over that in one line.
///
/// **An implementation may be called with an empty slice** and must tolerate it.
pub trait Sink {
    /// Take these bytes. There is no partial acceptance and no error: a sink that cannot take them
    /// drops them, which is the same choice `sink_proto` makes for a destination that has gone.
    fn put(&mut self, bytes: &[u8]);
}
