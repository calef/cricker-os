//! The search index: one byte layout, agreed between the builder that runs on the host and the
//! reader that runs confined.
//!
//! # Why an index at all, and why it is built on the host
//!
//! There is no directory iteration in this system. `readdir` refuses in the std PAL and the §27
//! file contract has no such verb, and the capability model argues against adding one: **enumeration
//! is authority**, and a viewer that can list a directory can discover what it was not given. So
//! "what pages exist" is not something the guest can find out by looking. It is something a builder
//! computes where there are no constraints and ships as a file, which is the same answer Unix's
//! `mandb` reached for a different reason (scanning was slow).
//!
//! # The one property the layout is designed for
//!
//! **A reader holds one 4 KiB page and never more.** That is not a budget chosen for tidiness: a
//! client of the file contract shares exactly one frame with the FS server, and a shell that had to
//! buffer a whole index would need a memory grant to search. So every section starts on a page
//! boundary and every record divides 4096 evenly, which together mean **a reader with one page in
//! hand never sees half a record**, and a lookup is a binary search over pages rather than a scan
//! over bytes.
//!
//! A lookup costs `ceil(log2(term pages)) + 1` page reads: the search narrows to a page by comparing
//! each candidate page's first term, then finishes inside that page with no further IO. For this
//! repository's corpus (see the numbers `cargo xtask manual` prints) that is single digits.
//!
//! # Layout
//!
//! ```text
//!   page 0            header (below), rest zero
//!   page_off          page records, 128 bytes each, 32 per page
//!   term_off          term records, 32 bytes each, 128 per page, sorted by term bytes
//!   post_off          postings, 4 bytes each, 1024 per page, grouped by term
//! ```
//!
//! # EXAMPLES
//!
//! The worked example lives on the `build` function, because it needs the `builder` feature to run
//! and rustdoc will only compile it when that feature is on. [`normalize`] and [`tokens`] are the
//! two functions a caller on either side reaches for, and both are exercised by the tests at the
//! foot of this file.
//!
//! # BUGS
//!
//! - **A term is truncated to [`TERM_MAX`] bytes**, so two identifiers that agree in their first 24
//!   characters share a posting list and a query for either finds both. Nothing in this corpus
//!   collides; a longer identifier set would.
//! - **Terms are ASCII-folded and split on anything that is not a letter or a digit**, so
//!   `fs_proto` indexes as `fs` and `proto` and a search for `fs_proto` finds pages that mention
//!   either. That is the same behaviour as `apropos` and it is wrong in the same way.
//! - **There is no ranking beyond occurrence count**, and no phrase search, because both want a
//!   position list and this index deliberately stores none.
//! - **The index does not record which line a term appeared on**, so a result names a page and not
//!   a place in it. Storing positions would multiply the postings section by the average term
//!   frequency, and the reader has no memory to spend on it.

/// The page size the whole layout is built around: the frame a client shares with the FS server.
pub const PAGE: usize = 4096;

/// The magic that starts a well-formed index.
pub const MAGIC: [u8; 8] = *b"CRKRMAN1";

/// Bytes in a page record.
pub const PAGE_REC: usize = 128;
/// Bytes in a term record.
pub const TERM_REC: usize = 32;
/// Bytes in a posting.
pub const POST_REC: usize = 4;

/// The longest page path an index can name.
pub const PATH_MAX: usize = 72;
/// The longest page title an index can carry.
pub const TITLE_MAX: usize = 48;
/// The longest term an index stores; longer ones are truncated to it.
pub const TERM_MAX: usize = 24;

/// What went wrong reading an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The first eight bytes are not [`MAGIC`].
    BadMagic,
    /// The header is shorter than a header.
    Truncated,
}

/// The header, parsed. Every offset is a byte offset from the start of the file and is a multiple
/// of [`PAGE`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// How many pages the index describes.
    pub pages: u32,
    /// How many distinct terms it holds.
    pub terms: u32,
    /// How many postings, across all terms.
    pub postings: u32,
    /// Byte offset of the page records.
    pub page_off: u64,
    /// Byte offset of the term records.
    pub term_off: u64,
    /// Byte offset of the postings.
    pub post_off: u64,
    /// The index's total size in bytes, which is what a caller checks a file against.
    pub total: u64,
}

impl Header {
    /// Read a header out of the index's first page.
    pub fn parse(first: &[u8]) -> Result<Header, Error> {
        if first.len() < 64 {
            return Err(Error::Truncated);
        }
        if first[..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        Ok(Header {
            pages: u32(first, 12),
            terms: u32(first, 16),
            postings: u32(first, 20),
            page_off: u64v(first, 24),
            term_off: u64v(first, 32),
            post_off: u64v(first, 40),
            total: u64v(first, 48),
        })
    }
}

/// A source of index pages.
///
/// The reader asks for a page by number and gets it or does not; it never seeks, never reads a byte
/// range, and never holds two pages at once. That is the whole interface, and it is this small so
/// that the guest implementation is a single `fs_proto` `READ` and the host implementation is a
/// slice.
pub trait Pages {
    /// Fill `out` with page `index` of the file. `false` means the read failed or ran off the end.
    fn page(&mut self, index: u64, out: &mut [u8; PAGE]) -> bool;
}

/// A [`Pages`] over bytes already in memory, which is what the host and the tests use.
pub struct Slice<'a>(pub &'a [u8]);

impl Pages for Slice<'_> {
    fn page(&mut self, index: u64, out: &mut [u8; PAGE]) -> bool {
        let start = index as usize * PAGE;
        if start >= self.0.len() {
            return false;
        }
        let end = (start + PAGE).min(self.0.len());
        out[..end - start].copy_from_slice(&self.0[start..end]);
        out[end - start..].fill(0);
        true
    }
}

/// A term that was found: where its postings are and how many there are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hit {
    /// Index of this term's first posting.
    pub first: u32,
    /// How many pages mention the term.
    pub count: u16,
}

/// One page's mention of a term.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Posting {
    /// Which page record, by index.
    pub page: u16,
    /// How many times the term occurs on it.
    pub count: u16,
}

/// A page the index names.
#[derive(Clone, Copy)]
pub struct Page {
    path: [u8; PATH_MAX],
    path_len: u8,
    title: [u8; TITLE_MAX],
    title_len: u8,
}

impl Page {
    /// The page's path, as the store names it.
    pub fn path(&self) -> &[u8] {
        &self.path[..self.path_len as usize]
    }
    /// The page's title: its first level-one heading, or its path if it has none.
    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_len as usize]
    }
}

/// Fold a query word the way the builder folded the text, so the two can be compared.
///
/// Returns the length written into `out`. A word that folds to nothing (punctuation alone) returns
/// zero and is not a term.
pub fn normalize(word: &[u8], out: &mut [u8; TERM_MAX]) -> usize {
    let mut n = 0;
    for &b in word {
        if n == TERM_MAX {
            break;
        }
        if b.is_ascii_alphanumeric() {
            out[n] = b.to_ascii_lowercase();
            n += 1;
        }
    }
    n
}

/// Split text into terms the way the builder does: runs of ASCII letters and digits, folded to
/// lower case, truncated to [`TERM_MAX`], and dropped if shorter than two characters.
///
/// A one-character term is dropped because it costs a posting list per letter of the alphabet and
/// answers no question anybody asks a manual.
pub fn tokens(text: &[u8], mut f: impl FnMut(&[u8])) {
    let mut buf = [0u8; TERM_MAX];
    let mut n = 0;
    for &b in text {
        if b.is_ascii_alphanumeric() {
            if n < TERM_MAX {
                buf[n] = b.to_ascii_lowercase();
                n += 1;
            }
        } else {
            if n >= 2 {
                f(&buf[..n]);
            }
            n = 0;
        }
    }
    if n >= 2 {
        f(&buf[..n]);
    }
}

/// Look a term up, reading at most `ceil(log2(term pages)) + 1` pages.
///
/// The search is over *pages*, not over records: each probe reads one page and compares the term it
/// starts with, which narrows the range by half for one read. The final page is searched in memory,
/// where it costs nothing.
pub fn lookup(h: &Header, term: &[u8], src: &mut impl Pages) -> Option<Hit> {
    let mut key = [0u8; TERM_MAX];
    let n = normalize(term, &mut key);
    if n == 0 || h.terms == 0 {
        return None;
    }
    let key = &key[..n];

    let per = PAGE / TERM_REC;
    let first = h.term_off / PAGE as u64;
    let last = (h.terms as u64 - 1) / per as u64;

    let mut buf = [0u8; PAGE];
    let mut lo = 0u64;
    let mut hi = last;
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if !src.page(first + mid, &mut buf) {
            return None;
        }
        if cmp(&buf[..TERM_REC], key) == core::cmp::Ordering::Greater {
            hi = mid - 1;
        } else {
            lo = mid;
        }
    }
    if !src.page(first + lo, &mut buf) {
        return None;
    }
    let here = (h.terms as u64 - lo * per as u64).min(per as u64) as usize;
    for i in 0..here {
        let rec = &buf[i * TERM_REC..(i + 1) * TERM_REC];
        if cmp(rec, key) == core::cmp::Ordering::Equal {
            return Some(Hit {
                first: u32(rec, TERM_MAX),
                count: u16v(rec, TERM_MAX + 4),
            });
        }
    }
    None
}

/// Read up to `out.len()` of a hit's postings, returning how many were read.
pub fn postings(h: &Header, hit: &Hit, src: &mut impl Pages, out: &mut [Posting]) -> usize {
    let mut buf = [0u8; PAGE];
    let per = PAGE / POST_REC;
    let want = (hit.count as usize).min(out.len());
    let mut done = 0;
    let mut loaded = u64::MAX;
    while done < want {
        let at = hit.first as usize + done;
        let page = h.post_off / PAGE as u64 + (at / per) as u64;
        if page != loaded {
            if !src.page(page, &mut buf) {
                return done;
            }
            loaded = page;
        }
        let off = (at % per) * POST_REC;
        out[done] = Posting {
            page: u16v(&buf, off),
            count: u16v(&buf, off + 2),
        };
        done += 1;
    }
    done
}

/// Read one page record.
pub fn page_record(h: &Header, i: u16, src: &mut impl Pages) -> Option<Page> {
    if i as u32 >= h.pages {
        return None;
    }
    let per = PAGE / PAGE_REC;
    let mut buf = [0u8; PAGE];
    if !src.page(
        h.page_off / PAGE as u64 + (i as usize / per) as u64,
        &mut buf,
    ) {
        return None;
    }
    let rec = &buf[(i as usize % per) * PAGE_REC..][..PAGE_REC];
    let mut p = Page {
        path: [0; PATH_MAX],
        path_len: rec[PATH_MAX + TITLE_MAX].min(PATH_MAX as u8),
        title: [0; TITLE_MAX],
        title_len: rec[PATH_MAX + TITLE_MAX + 1].min(TITLE_MAX as u8),
    };
    p.path.copy_from_slice(&rec[..PATH_MAX]);
    p.title
        .copy_from_slice(&rec[PATH_MAX..PATH_MAX + TITLE_MAX]);
    Some(p)
}

/// Compare a term record's term against a key.
fn cmp(rec: &[u8], key: &[u8]) -> core::cmp::Ordering {
    let n = rec[TERM_MAX + 6] as usize;
    rec[..n.min(TERM_MAX)].cmp(key)
}

fn u16v(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64v(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

// ---- the builder, host only ----------------------------------------------------------------

#[cfg(feature = "builder")]
mod builder {
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// One page offered to the builder.
    pub struct Source<'a> {
        /// The path the store will know the page by. Truncated to [`PATH_MAX`].
        pub path: &'a str,
        /// The page's title. Truncated to [`TITLE_MAX`].
        pub title: &'a str,
        /// The page's markdown source, which is what gets tokenised.
        pub text: &'a [u8],
    }

    /// Build an index over these pages.
    ///
    /// This is the only allocating function in the crate and it is behind the `builder` feature for
    /// that reason: it runs in `xtask` on a host with a heap, never on the guest.
    ///
    /// # EXAMPLES
    ///
    /// ```
    /// use manual::index;
    ///
    /// let bytes = index::build(&[
    ///     index::Source { path: "notes/glob.md", title: "Globbing", text: b"a pattern and a name" },
    ///     index::Source { path: "notes/ipc.md", title: "IPC", text: b"a name and an endpoint" },
    /// ]);
    ///
    /// let mut src = index::Slice(&bytes);
    /// let h = index::Header::parse(&bytes[..index::PAGE]).unwrap();
    ///
    /// // Both pages say "name", so the term has two postings.
    /// let hit = index::lookup(&h, b"name", &mut src).unwrap();
    /// assert_eq!(hit.count, 2);
    ///
    /// // And one of them is the one whose title says so.
    /// let mut post = [index::Posting { page: 0, count: 0 }; 4];
    /// let n = index::postings(&h, &hit, &mut src, &mut post);
    /// let first = index::page_record(&h, post[0].page, &mut src).unwrap();
    /// assert_eq!(n, 2);
    /// assert_eq!(first.title(), b"Globbing");
    /// ```
    pub fn build(pages: &[Source<'_>]) -> Vec<u8> {
        // term -> (page index -> occurrences). A BTreeMap because the term table must come out
        // sorted and this way it is sorted by construction rather than by a pass nobody can see.
        let mut terms: BTreeMap<Vec<u8>, BTreeMap<u16, u32>> = BTreeMap::new();
        for (i, p) in pages.iter().enumerate() {
            tokens(p.text, |t| {
                *terms
                    .entry(t.to_vec())
                    .or_default()
                    .entry(i as u16)
                    .or_default() += 1;
            });
        }

        let post_count: usize = terms.values().map(|m| m.len()).sum();
        let page_off = PAGE;
        let term_off = page_off + round_up(pages.len() * PAGE_REC);
        let post_off = term_off + round_up(terms.len() * TERM_REC);
        let total = post_off + round_up(post_count * POST_REC);
        let mut out = vec![0u8; total];

        out[..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&1u32.to_le_bytes());
        out[12..16].copy_from_slice(&(pages.len() as u32).to_le_bytes());
        out[16..20].copy_from_slice(&(terms.len() as u32).to_le_bytes());
        out[20..24].copy_from_slice(&(post_count as u32).to_le_bytes());
        out[24..32].copy_from_slice(&(page_off as u64).to_le_bytes());
        out[32..40].copy_from_slice(&(term_off as u64).to_le_bytes());
        out[40..48].copy_from_slice(&(post_off as u64).to_le_bytes());
        out[48..56].copy_from_slice(&(total as u64).to_le_bytes());

        for (i, p) in pages.iter().enumerate() {
            let rec = page_off + i * PAGE_REC;
            let path = p.path.as_bytes();
            let n = path.len().min(PATH_MAX);
            out[rec..rec + n].copy_from_slice(&path[..n]);
            let title = p.title.as_bytes();
            let m = title.len().min(TITLE_MAX);
            out[rec + PATH_MAX..rec + PATH_MAX + m].copy_from_slice(&title[..m]);
            out[rec + PATH_MAX + TITLE_MAX] = n as u8;
            out[rec + PATH_MAX + TITLE_MAX + 1] = m as u8;
        }

        let mut post = 0usize;
        for (i, (term, byname)) in terms.iter().enumerate() {
            let rec = term_off + i * TERM_REC;
            let n = term.len().min(TERM_MAX);
            out[rec..rec + n].copy_from_slice(&term[..n]);
            out[rec + TERM_MAX..rec + TERM_MAX + 4].copy_from_slice(&(post as u32).to_le_bytes());
            out[rec + TERM_MAX + 4..rec + TERM_MAX + 6]
                .copy_from_slice(&(byname.len() as u16).to_le_bytes());
            out[rec + TERM_MAX + 6] = n as u8;
            for (page, count) in byname {
                let at = post_off + post * POST_REC;
                out[at..at + 2].copy_from_slice(&page.to_le_bytes());
                out[at + 2..at + 4]
                    .copy_from_slice(&(*count).min(u16::MAX as u32).to_le_bytes()[..2]);
                post += 1;
            }
        }

        out
    }

    /// The first level-one heading in a markdown document, which is what a page is called.
    pub fn title_of(text: &[u8]) -> Option<&str> {
        let mut start = 0;
        while start < text.len() {
            let end = text[start..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|k| start + k)
                .unwrap_or(text.len());
            let line = &text[start..end];
            if line.starts_with(b"# ") {
                return core::str::from_utf8(&line[2..]).ok().map(str::trim);
            }
            start = end + 1;
        }
        None
    }

    fn round_up(n: usize) -> usize {
        n.div_ceil(PAGE) * PAGE
    }
}

#[cfg(feature = "builder")]
pub use builder::{Source, build, title_of};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_folds_the_way_the_text_did() {
        // The one property that makes a lookup possible at all: the builder's tokeniser and the
        // reader's query normaliser must agree, byte for byte, on what a term is.
        let mut buf = [0u8; TERM_MAX];
        let n = normalize(b"Capability!", &mut buf);
        assert_eq!(&buf[..n], b"capability");

        let mut seen: [[u8; TERM_MAX]; 4] = [[0; TERM_MAX]; 4];
        let mut lens = [0usize; 4];
        let mut i = 0;
        tokens(b"Capability, a `Frame`. x", |t| {
            if i < 4 {
                seen[i][..t.len()].copy_from_slice(t);
                lens[i] = t.len();
                i += 1;
            }
        });
        // Two terms out of four words: `a` and `x` are single characters, and a per-letter posting
        // list answers no question anybody asks a manual.
        assert_eq!(i, 2);
        assert_eq!(&seen[0][..lens[0]], b"capability");
        assert_eq!(&seen[1][..lens[1]], b"frame");
    }

    #[test]
    fn a_term_longer_than_the_record_folds_to_its_prefix() {
        let long = b"averyveryverylongidentifiername";
        let mut buf = [0u8; TERM_MAX];
        assert_eq!(normalize(long, &mut buf), TERM_MAX);
    }

    #[test]
    fn a_header_that_is_not_one_is_refused() {
        assert_eq!(Header::parse(&[0u8; 64]), Err(Error::BadMagic));
        assert_eq!(Header::parse(&[0u8; 8]), Err(Error::Truncated));
    }

    #[cfg(feature = "builder")]
    #[test]
    fn every_term_the_builder_wrote_is_found_by_the_reader() {
        use alloc::vec::Vec;

        // Enough terms to make the term table span several pages, because the binary search over
        // pages is the part that a single-page index would never exercise. `TERM_REC` is 32 bytes
        // and a page holds 128 records, so this is four pages of terms.
        let mut text = alloc::string::String::new();
        let mut want: Vec<alloc::string::String> = Vec::new();
        for i in 0..500 {
            let t = alloc::format!("term{i:04}");
            text.push_str(&t);
            text.push(' ');
            want.push(t);
        }
        let bytes = build(&[Source {
            path: "notes/x.md",
            title: "X",
            text: text.as_bytes(),
        }]);
        let h = Header::parse(&bytes[..PAGE]).unwrap();
        assert_eq!(h.terms as usize, want.len());

        let mut src = Slice(&bytes);
        for t in &want {
            let hit = lookup(&h, t.as_bytes(), &mut src)
                .unwrap_or_else(|| panic!("the reader cannot find {t}, which the builder wrote"));
            assert_eq!(hit.count, 1);
        }
        assert!(lookup(&h, b"absent", &mut src).is_none());
    }

    #[cfg(feature = "builder")]
    #[test]
    fn a_record_never_straddles_a_page() {
        // The property the whole layout exists for: a reader that holds one page never sees half a
        // record. It is a claim about the offsets, so it is checked on the offsets.
        let bytes = build(&[Source {
            path: "a",
            title: "A",
            text: b"one two three",
        }]);
        let h = Header::parse(&bytes[..PAGE]).unwrap();
        for off in [h.page_off, h.term_off, h.post_off] {
            assert_eq!(off % PAGE as u64, 0, "section at {off} is not page aligned");
        }
        for rec in [PAGE_REC, TERM_REC, POST_REC] {
            assert_eq!(PAGE % rec, 0, "{rec}-byte records do not divide a page");
        }
        assert_eq!(h.total as usize, bytes.len());
    }

    #[cfg(feature = "builder")]
    #[test]
    fn a_title_is_the_first_level_one_heading() {
        assert_eq!(title_of(b"intro\n\n# The title\n\n## no\n"), Some("The title"));
        assert_eq!(title_of(b"## only a section\n"), None);
    }
}
