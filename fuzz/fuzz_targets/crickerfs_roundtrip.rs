//! Fuzz the crickerfs archive **round trip**: build an image, parse it back, read every file.
//!
//! **This target asks a different question from the other three**, and the difference is the reason
//! it exists rather than a `crickerfs_parse` sibling. The other three ask "does hostile input make
//! this panic". Asking that of `Fs::parse` would be fuzzing a function Kani has already proved
//! total: `the_validation_implies_reads_slice_is_in_bounds` covers every `start_block`, `len` and
//! image length with no bound at all, and `a_short_image_is_refused_not_indexed` covers the
//! truncated case. A fuzz target there would burn CI time to rediscover a theorem.
//!
//! What is *not* proved, and cannot be by a harness of that shape, is that the writer and the reader
//! agree. `write_image` picks block placements and packs a directory; `parse` and `read` decode it.
//! Nothing checks that what went in comes out, and the last bug in this crate was exactly that
//! shape: a name over `NAME_LEN` was silently truncated, so two distinct files could land under one
//! name and the second would win. That was found by hand on 2026-08-01. This is the harness that
//! would have found it.
//!
//! **The property.** For any set of (name, contents) the writer accepts, the image it produces
//! parses, holds exactly that many files, and every name reads back byte-identical contents. Failing
//! that is a data-loss bug in an archive format, which is worse than a panic and much quieter.
//!
//! **Structured input, not bytes.** `libfuzzer-sys` derives the input through `arbitrary`, so the
//! fuzzer mutates the *file set* rather than an image's bytes. Byte mutation would spend its whole
//! budget producing images the writer never writes, which tells us nothing about whether the writer
//! and reader agree.

#![no_main]

use crickerfs::{Fs, MAX_FILES, NAME_LEN, image_size, write_image};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|files: Vec<(String, Vec<u8>)>| {
    // The writer's own limits, applied here so the target explores the accepted region rather than
    // re-testing the two rejections (`TooManyFiles`, `NameTooLong`) that the crate's unit tests
    // already pin by example. The interesting inputs are the ones that are *legal*, including the
    // awkward legal ones: an empty name, a zero-byte file, a name that is exactly NAME_LEN.
    if files.len() > MAX_FILES {
        return;
    }
    if files.iter().any(|(name, _)| name.len() > NAME_LEN) {
        return;
    }
    // The writer refuses these, and the refusal is this target's own first finding: a name with a
    // NUL in it is unrepresentable, because the padding is NUL and every reader stops at the first
    // one. Filtered here rather than asserted on, because `crickerfs`'s own host tests now pin the
    // rejection by example and re-proving it here would spend the budget on a settled question.
    if files.iter().any(|(name, _)| name.as_bytes().contains(&0)) {
        return;
    }
    // A cap on total bytes, so the fuzzer spends its time on structure rather than on memcpy. Four
    // megabytes is already far past any image this system boots.
    if files.iter().map(|(_, data)| data.len()).sum::<usize>() > 4 << 20 {
        return;
    }

    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();

    let mut image = vec![0u8; image_size(&borrowed)];
    let written = write_image(&borrowed, &mut image).expect("the limits above are the writer's");

    // `image_size` is what the disk tool allocates from, so a writer that reports having written
    // more than that sized buffer would have overrun a real caller's allocation.
    assert!(
        written <= image.len(),
        "write_image wrote {written} into a buffer image_size said needed {}",
        image.len()
    );

    let fs = Fs::parse(&image).expect("an image this crate just wrote must parse");
    assert_eq!(fs.len(), files.len(), "the directory lost or gained a file");

    // Every name reads back its own bytes. Duplicate names are legal input and the writer does not
    // reject them, so the assertion is over the *first* entry with each name, which is what `read`
    // returns; comparing against the last would be asserting a policy nothing states.
    for (name, data) in &files {
        let first = files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
            .unwrap();
        match fs.read(name) {
            Some(got) => assert_eq!(got, first, "{name:?} did not read back what was written"),
            None => panic!("{name:?} was written and cannot be read back"),
        }
        let _ = data;
    }

    // The directory iterator must agree with the header count, and every entry's name must be one
    // that was asked for. An entry naming something nobody wrote is a directory that has invented a
    // file, which is how a truncation bug looks from the reader's side.
    assert_eq!(fs.entries().count(), files.len());
    for entry in fs.entries() {
        assert!(
            files.iter().any(|(name, _)| entry.name_eq(name)),
            "the directory holds a name nothing wrote"
        );
    }
});
