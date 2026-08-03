use fs_proto::dir;
use pipeline_service::{answer, counts};

use super::*;

/// The last line `user/src/swish.rs`'s redirection role prints. Must match `REDIRECT_DONE`.
const DONE: &[u8] = b"== redirections done\n";

/// The script's transcript, run **once** and shared by every assertion below, for
/// [`pipeline_tests`]'s reason: the assertions are about one run of one script, and re-running
/// it per test would be the same measurement four times rather than four measurements.
static TRANSCRIPT: spin::Mutex<Option<([u8; 3072], usize)>> = spin::Mutex::new(None);

/// Run the script if nothing has yet, and hand back everything the shell printed. `None` when
/// this boot has no RedoxFS disk attached, which is the same skip every FS test takes.
fn transcript(out: &mut [u8; 3072]) -> Option<usize> {
    let mut cache = TRANSCRIPT.lock();
    if cache.is_none() {
        let dir = fs_service::narrow_dir(
            dir_capability_tests::blk_server_image(),
            program("fs_server")?,
            program("fs_subtree_caretaker")
                .expect("no fs_subtree_caretaker program in the initrd archive"),
            fs_proto::fixture::tree::REDIR,
            dir::ALL,
        )?;
        let Some(w) = pipeline_service::start_redirecting(dir, dir::ALL) else {
            panic!("no swish program in the initrd archive, or no memory to wire one");
        };
        let mut buf = [0u8; 3072];
        let n = pipeline_service::transcript(&w, DONE, &mut buf);
        *cache = Some((buf, n));
    }
    let (buf, n) = cache.as_ref().expect("just filled");
    out.copy_from_slice(buf);
    Some(*n)
}

/// What `wc` should report for a listing the shell **printed**, which is the same listing it
/// **wrote** with two bytes of indent removed from each line.
///
/// The prompt renders an entry as `"  name\n"` and a producer feeding a byte stream renders it
/// as `"name\n"`, because the indent is the terminal's manners and not part of the listing. So
/// the expected counts are derived here rather than hardcoded, and the derivation is the whole
/// assertion: a file that does not match this is a file with different bytes in it.
fn listing_counts(printed: &[u8]) -> (u64, u64, u64) {
    let mut lines = 0u64;
    let mut words = 0u64;
    let mut bytes = 0u64;
    for line in printed.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let name = line.strip_prefix(b"  ").unwrap_or(line);
        lines += 1;
        words += core::str::from_utf8(name)
            .expect("a listed name is not UTF-8")
            .split_ascii_whitespace()
            .count() as u64;
        bytes += name.len() as u64 + 1; // the newline the producer writes
    }
    (lines, words, bytes)
}

/// **The headline: one builtin, two destinations, the same bytes.**
///
/// `ls > out.txt` writes a listing into a file and prints nothing. The `ls` after it prints that
/// same listing (nothing between the two lines changes the directory: `>` created `out.txt`
/// *before* the first listing was read, so both see it). `wc < out.txt` then has to agree with
/// what was printed, byte for byte, once the prompt's two-space indent is taken off.
///
/// **This line spawns no process at all**, which is the part worth noticing. The shell is the
/// producer and the shell is the thing behind the file, so `ls > out.txt` is one process doing
/// two things it already knew how to do. See notes/pipes.md for why the file end is the shell's
/// and not a sink process's.
#[test_case]
fn one_builtin_two_destinations_and_the_same_bytes() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    // `>` prints nothing. A redirection that quietly also printed would mean the bytes went two
    // places, and the whole claim is that they went to exactly one.
    let redirected = answer(t, b"ls > out.txt");
    assert!(
        redirected.iter().all(|b| b.is_ascii_whitespace()),
        "`ls > out.txt` printed {:?} instead of writing it to the file",
        core::str::from_utf8(redirected).unwrap_or("<not utf-8>"),
    );

    let printed = answer(t, b"ls");
    assert!(
        printed.windows(6).any(|w| w == b"alpha\n"),
        "the listing does not hold the fixture's own files, so it proves nothing: {:?}",
        core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
    );
    assert!(
        printed.windows(8).any(|w| w == b"out.txt\n"),
        "`>` did not create the file it redirects into: {:?}",
        core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
    );

    assert_eq!(
        counts(&answer(t, b"wc < out.txt")[2..]),
        listing_counts(printed),
        "the listing that went into the file is not the listing that went to the terminal",
    );
}

/// **And the same claim for a program's output**, which is the half `>` shares with `|`.
///
/// `date` is spawned twice from the same ELF. The first time the shell prints what arrives on
/// its result endpoint; the second time it writes it into a file. `date` is not told, and the
/// byte count `wc` reports for the file has to be the length of what was printed.
#[test_case]
fn one_program_two_destinations_and_the_same_bytes() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    let direct = answer(t, b"date");
    assert!(
        direct.starts_with(b"  ") && direct.ends_with(b"\n"),
        "date printed {:?}, which is not one line through the shell",
        core::str::from_utf8(direct).unwrap_or("<not utf-8>"),
    );
    let printed = &direct[2..];

    let (lines, words, bytes) = counts(&answer(t, b"wc < date.txt")[2..]);
    assert_eq!(
        bytes as usize,
        printed.len(),
        "the same date wrote {} bytes to the terminal and {bytes} into a file: {:?}",
        printed.len(),
        core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
    );
    assert_eq!(lines, 1, "date prints one line either way");
    assert_eq!(
        words as usize,
        core::str::from_utf8(printed)
            .expect("date printed non-UTF-8")
            .split_ascii_whitespace()
            .count(),
        "the words differ between the two destinations",
    );
}

/// **`>>` is `>` with the emptying left out, and the two are measured against each other.**
///
/// The script runs the same two commands twice, changing one character:
///
/// ```text
/// echo one > trunc.txt        echo one > app.txt
/// echo two > trunc.txt        echo two >> app.txt
/// ```
///
/// so the append file must hold **exactly twice** what the truncate file holds, in all three
/// counts. Doubling is the assertion rather than `(2, 2, 8)` because a literal would be a claim
/// about what `echo` prints; this is a claim about what the operator did, and it holds whatever
/// the two lines say.
///
/// The truncate arm is load-bearing in its own right: it is what says `>` still empties the
/// file, which is the property `>>` exists to be the opposite of.
#[test_case]
fn append_keeps_what_truncate_throws_away() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    let (tl, tw, tb) = counts(&answer(t, b"wc < trunc.txt")[2..]);
    assert!(
        tb > 0,
        "the truncate arm wrote nothing, so the comparison below proves nothing",
    );
    assert_eq!(
        counts(&answer(t, b"wc < app.txt")[2..]),
        (tl * 2, tw * 2, tb * 2),
        "`>>` did not keep the first line: `>` wrote ({tl}, {tw}, {tb}) for the same two \
         commands, so append must be exactly twice that",
    );

    // **`>>` creates a name that is not there**, so it is not "open, then seek to the end":
    // there is nothing to open. On a fresh fixture that file holds exactly one copy of what
    // `echo one` writes; the assertion is stated as a whole number of copies because
    // `CRICKER_KEEP_REDOXFS=1` deliberately re-runs the suite against the image the last boot
    // left behind, and this line would then append to its own leftovers. Order-independent
    // either way, and it still fails on a `>>` that wrote a partial or a doubled line.
    assert_eq!(
        (tl, tw),
        (1, 1),
        "the truncate arm should be one line of one word; the check below assumes it",
    );
    let (fl, fw, fb) = counts(&answer(t, b"wc < fresh.txt")[2..]);
    assert!(
        fb >= tb && fb % tb == 0 && fl == fb / tb && fw == fl,
        "`>> a-name-that-is-not-there` should have created it and written one line into it, \
         and reported some whole number of those lines; got ({fl}, {fw}, {fb}) against a \
         single line of ({tl}, {tw}, {tb})",
    );
}

/// **A redirection that cannot be opened refuses the line rather than running it.**
///
/// `<` does not create, so `wc < nosuch.txt` is the filesystem's own sentence and nothing is
/// spawned. The alternative, an empty stream, would have `wc` truthfully report zero for a file
/// that does not exist, which is a number a person would believe.
///
/// And the manifest still wins over the capability: `worker 9 > out.txt` is refused for having
/// no byte stream even in a shell that could open the file, because what a `>` needs is a
/// program with bytes and not a shell with a directory.
#[test_case]
fn a_redirection_that_cannot_be_backed_is_still_refused() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    let said = answer(t, b"wc < nosuch.txt");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("no such name"),
        "`wc < nosuch.txt` must be the filesystem's refusal, not an empty stream: {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );

    let said = answer(t, b"worker 9 > out.txt");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("byte stream"),
        "`worker 9 > out.txt` should be refused for having no bytes: {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );
}
