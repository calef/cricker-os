//! What the renderer must get right, and the two properties that make it usable at all.
//!
//! The interesting tests here are the last two. `framing_does_not_matter` is what lets `doc` render
//! straight off sixteen-byte sink messages with no reassembly buffer, and `every_word_survives`
//! is the closed-corpus argument in executable form: this renderer was written instead of taking
//! `pulldown-cmark`, and the justification for that is that the input set is *this repository*, so
//! the conformance claim is checkable directly against it rather than against a spec suite.

use manual::{Renderer, Sink, Style};

struct Buf(Vec<u8>);

impl Sink for Buf {
    fn put(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

fn plain(src: &str, width: u16) -> String {
    render(
        src,
        Style {
            width,
            color: false,
        },
    )
}

fn render(src: &str, style: Style) -> String {
    let mut out = Buf(Vec::new());
    let mut r = Renderer::new(style);
    r.feed(src.as_bytes(), &mut out);
    r.finish(&mut out);
    String::from_utf8(out.0).unwrap()
}

#[test]
fn headings_shout_once() {
    // Level one is the page's name and is uppercased; level two keeps its case, because a document
    // whose every heading shouts has told the reader nothing about which one is the title.
    assert_eq!(
        plain("# Title\n\n## Section\n\nBody.\n", 80),
        "TITLE\n\nSection\n\n  Body.\n"
    );
}

#[test]
fn a_hash_without_a_space_is_not_a_heading() {
    // Half the fenced code in this repository is shell, and shell comments start with `#`. This is
    // the rule that keeps `#!/bin/sh` and `# a comment` out of the heading path.
    assert_eq!(plain("#nope\n", 80), "  #nope\n");
}

#[test]
fn paragraphs_rewrap() {
    // Source line breaks are not output line breaks: a paragraph is re-flowed to the terminal's
    // width, which is the whole reason to render markdown rather than print it.
    let out = plain("one two\nthree four five\n", 12);
    assert_eq!(out, "  one two\n  three four\n  five\n");
}

#[test]
fn code_blocks_are_verbatim() {
    let out = plain("```sh\n  ls   -l\n```\n", 80);
    assert_eq!(out, "      ls   -l\n");
}

#[test]
fn a_code_span_protects_its_contents() {
    // 11,281 code spans in this corpus, many of them full of the characters the other inline rules
    // look for. If this ordering were wrong, `*ptr` inside backticks would open emphasis and eat
    // the rest of the line.
    let out = render(
        "a `*ptr [x](y)` b\n",
        Style {
            width: 80,
            color: true,
        },
    );
    assert!(out.contains("\x1b[36m*ptr [x](y)"));
    assert!(!out.contains("\x1b[4m"));
}

#[test]
fn underscores_are_never_emphasis() {
    // The corpus writes `snake_case` identifiers in running prose constantly. CommonMark would read
    // `__rust_alloc` as an opened strong span; here it is text, and that is a deliberate narrowing
    // recorded in the renderer's BUGS.
    let out = render(
        "the __rust_alloc symbol and fs_proto\n",
        Style {
            width: 80,
            color: true,
        },
    );
    assert!(!out.contains('\x1b'));
}

#[test]
fn emphasis_needs_a_closer_that_is_not_a_space() {
    let styled = render(
        "a *b* c\n",
        Style {
            width: 80,
            color: true,
        },
    );
    assert!(styled.contains("\x1b[4mb"));
    // A lone asterisk mid-sentence has no closer and must not swallow the rest of the line.
    let stray = render(
        "2 * 3 = 6\n",
        Style {
            width: 80,
            color: true,
        },
    );
    assert!(!stray.contains('\x1b'));
}

#[test]
fn list_markers_are_normalised_and_hang() {
    // Three spellings of a bullet on one screen read as three different things, so they are one
    // spelling on output. The continuation aligns under the text, not under the bullet.
    let out = plain("* one two three\n+ four\n- five\n", 14);
    assert_eq!(out, "  - one two\n    three\n  - four\n  - five\n");
}

#[test]
fn ordered_lists_keep_their_numbers() {
    assert_eq!(plain("1. one\n2. two\n", 80), "  1. one\n  2. two\n");
}

#[test]
fn block_quotes_get_a_rule() {
    assert_eq!(plain("> quoted\n", 80), "  | quoted\n");
}

#[test]
fn tables_align_to_their_widest_cell() {
    let out = plain("| a | bbbb |\n|---|---|\n| cc | d |\n", 80);
    assert_eq!(out, "  a  | bbbb\n  ---+-----\n  cc | d   \n");
}

#[test]
fn a_pipe_line_without_a_delimiter_is_still_printed() {
    // A paragraph that happens to start with a pipe must not be silently eaten by the table path.
    let out = plain("| not a table\n", 80);
    assert!(out.contains("not a table"));
}

#[test]
fn links_show_where_they_point() {
    // A terminal cannot follow a link, so a reader who cannot see the destination has been handed a
    // worse document than the source.
    let out = plain("see [the note](notes/glob.md) for more\n", 80);
    assert_eq!(out, "  see the note notes/glob.md for more\n");
}

#[test]
fn an_attribute_never_survives_a_newline() {
    // A terminal left bold at end of line paints the next row and the prompt with it. Every line
    // this renderer emits ends reset.
    let out = render(
        "# Title\n\n**strong text that wraps past the edge**\n",
        Style {
            width: 20,
            color: true,
        },
    );
    for line in out.split('\n') {
        if line.contains('\x1b') {
            assert!(
                line.ends_with("\x1b[0m") || !line.contains("\x1b["),
                "line: {line:?}"
            );
        }
    }
}

#[test]
fn framing_does_not_matter() {
    // The property `doc` depends on: input arrives as sixteen-byte sink messages, and a renderer
    // that produced different output for different chunkings would need a reassembly buffer, which
    // is a memory grant the program can otherwise do without.
    let src = "# T\n\ntext with **bold** and `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> q\n\n- item\n";
    let whole = plain(src, 40);
    for chunk in [1usize, 3, 16, 17, 4096] {
        let mut out = Buf(Vec::new());
        let mut r = Renderer::new(Style {
            width: 40,
            color: false,
        });
        for piece in src.as_bytes().chunks(chunk) {
            r.feed(piece, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(
            String::from_utf8(out.0).unwrap(),
            whole,
            "chunk size {chunk}"
        );
    }
}

#[test]
fn a_document_with_no_trailing_newline_still_ends() {
    assert_eq!(plain("# T", 80), "T\n");
}

#[test]
fn an_overlong_line_is_reported_rather_than_hidden() {
    let long = "x".repeat(manual::LINE_MAX + 10);
    let mut out = Buf(Vec::new());
    let mut r = Renderer::new(Style {
        width: 80,
        color: false,
    });
    r.feed(long.as_bytes(), &mut out);
    r.finish(&mut out);
    assert!(r.truncated());
}

/// Every letter and digit in the repository's own markdown reaches the rendered output, in order.
///
/// This is the claim that replaces a `CommonMark` conformance suite. A renderer that silently ate a
/// construct (an HTML block, a reference link, an indented continuation, a table row) would drop
/// the characters inside it, and a subsequence check finds that on real files rather than on
/// invented ones. Subsequence rather than equality because the renderer legitimately *adds*
/// characters (an image's `image:` marker) and legitimately *joins* what markup split
/// (`**L**inkable` is two runs in the source and one word on screen).
///
/// It runs at a terminal four thousand columns wide, because a real one truncates table cells to
/// their column width, and that is a formatting choice the BUGS section records rather than a
/// parsing failure. The number was found rather than picked: at 1000 columns this repository's
/// widest table still shrinks, which is what a documentation service's honest caveat looks like
/// from the inside.
#[test]
fn every_character_survives() {
    // Runtime, not env!: the compile-time form bakes the absolute path into the binary, and a
    // cached artifact built before a checkout moves (the 2026-08-15 cricker-os -> nife directory
    // rename) then reads a directory that no longer exists and checks zero files. Cargo sets the
    // variable at run time too, and that one is always the live path.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this for tests");
    let root = std::path::Path::new(&manifest)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut checked = 0;
    for dir in ["notes", "design/decisions", "design/roadmap"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            // A fence's info string (the `bash` in a ```bash line) is a language tag rather than
            // content, so it is not offered as evidence. Nothing else is excluded: a table longer
            // than the buffer spills into another chunk rather than losing rows, which is exactly
            // the behaviour this test exists to hold in place.
            let want: Vec<char> = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("```"))
                .flat_map(|l| l.chars().chain(core::iter::once('\n')))
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect();
            let have: Vec<char> = plain(&src, 4000)
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect();
            let mut i = 0;
            for c in &have {
                if i < want.len() && want[i] == *c {
                    i += 1;
                }
            }
            assert!(
                i == want.len(),
                "{}: rendering dropped text near {:?}",
                path.display(),
                want[i.saturating_sub(30)..(i + 30).min(want.len())]
                    .iter()
                    .collect::<String>()
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "only checked {checked} files");
}
