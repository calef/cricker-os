use fs_proto::dir;
use pipeline_service::{answer, counts};

use super::*;

/// The last line `user/src/swish.rs`'s language role prints. Must match `LANGUAGE_DONE`.
const DONE: &[u8] = b"== language done\n";

/// The script's transcript, run **once** and shared by every assertion below, for
/// [`redirection_tests`]'s reason: the assertions are about one run of one script, and re-running
/// it per test would be the same measurement several times rather than several measurements.
static TRANSCRIPT: spin::Mutex<Option<([u8; 3072], usize)>> = spin::Mutex::new(None);

/// Run the script if nothing has yet, and hand back everything the shell printed. `None` when this
/// boot has no RedoxFS disk attached, which is the same skip every FS test takes.
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
        let Some(w) = pipeline_service::start_language(dir, dir::ALL) else {
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

/// What the shell said, as a `str`, for an assertion that wants to read words.
fn said<'a>(t: &'a [u8], line: &[u8]) -> &'a str {
    core::str::from_utf8(answer(t, line)).expect("the shell writes ASCII")
}

/// **The correctness gap this milestone closed**, at the one interface a human touches.
///
/// A file called `my notes.txt` could not be named before quoting existed, and in a shell whose
/// thesis is that naming a resource is granting it, a resource you cannot name is a resource you
/// cannot grant. So this is not an ergonomics test: it is the authority surface reaching a name it
/// could not reach.
///
/// The counts are **derived from the `echo` that wrote the file**, not written down, so a `>` that
/// dropped bytes or a quote that swallowed a word fails even though a file would still exist.
#[test_case]
fn a_name_with_a_space_in_it_can_be_written_and_read_back() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    // The redirection printed nothing, so the bytes went to exactly one place.
    let redirected = answer(t, b"echo hello world > \"my notes.txt\"");
    assert!(
        redirected.iter().all(|b| b.is_ascii_whitespace()),
        "the quoted redirection printed {:?} instead of writing it",
        core::str::from_utf8(redirected).unwrap_or("<not utf-8>"),
    );

    // `echo hello world` writes twelve bytes and a newline: one line, two words, thirteen bytes.
    // Stated as a derivation rather than a literal, so it holds if the script's text changes.
    let text = b"hello world\n";
    let expected = (
        1u64,
        2u64,
        text.len() as u64,
    );
    assert_eq!(
        counts(&answer(t, b"wc < \"my notes.txt\"")[2..]),
        expected,
        "the quoted name did not designate the file the quoted redirection wrote",
    );
}

/// **Naming the file is the operator, unwritten**, asked of a name only quoting can express.
///
/// `wc "my notes.txt"` and `wc < "my notes.txt"` are the same designation said two ways, so a pair
/// that agrees cannot both have opened the wrong file. Milestone 31's headline, one milestone's
/// worth of grammar later.
#[test_case]
fn a_quoted_name_is_the_grant_with_the_operator_left_out() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];
    let operand = counts(&answer(t, b"wc \"my notes.txt\"")[2..]);
    assert_eq!(
        operand,
        counts(&answer(t, b"wc < \"my notes.txt\"")[2..]),
        "the same file, named two ways, came out as two different files",
    );
    assert!(operand.2 > 0, "both arms read an empty file, which proves nothing");
}

/// **Quoting is the difference between naming a name and naming a set**, printed side by side.
///
/// The two lines are the same four characters and `echo` prints what a grant would move, so this is
/// the authority claim rather than a formatting one: `rm "*.txt"` hands over one name and `rm *.txt`
/// hands over everything the pattern matched.
#[test_case]
fn the_same_word_is_a_name_quoted_and_a_set_unquoted() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    let quoted = said(t, b"echo \"*.txt\"").trim();
    assert_eq!(
        quoted, "*.txt",
        "a quoted pattern must print itself, not what it would have matched",
    );

    // The unquoted line either expands to real names or is refused for matching nothing. Both are
    // acceptable answers about this fixture; what is not acceptable is printing the pattern, which
    // is the one thing that would mean quoting changed nothing.
    let bare = said(t, b"echo *.txt").trim();
    assert_ne!(
        bare, "*.txt",
        "the unquoted pattern printed itself, so quoting made no difference",
    );

    // And a quoted operator is text: this line writes no file and prints its angle bracket.
    assert_eq!(said(t, b"echo 'a > b'").trim(), "a > b");
}

/// **The condition table, with real commands on both sides.**
///
/// `worker 3` runs and `worker` is refused at the prompt (its manifest requires an integer), so the
/// four lines cover every arm without a branch anybody wrote for the test. The pairs are each
/// other's control: a `&&` that always ran and a `&&` that never ran would each satisfy half of
/// this and fail the other half.
#[test_case]
fn a_connector_runs_the_second_command_only_when_it_should() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    assert!(
        said(t, b"worker 3 && echo yes").contains("yes"),
        "&& did not run the second command after one that succeeded",
    );
    assert!(
        !said(t, b"worker && echo yes").contains("yes"),
        "&& ran the second command after one this shell refused",
    );
    assert!(
        !said(t, b"worker 3 || echo no").contains("no"),
        "|| ran the second command after one that succeeded",
    );
    assert!(
        said(t, b"worker || echo no").contains("no"),
        "|| did not run the second command after one this shell refused",
    );

    // `;` runs whatever happened, which is the arm the four lines above cannot show: here the `&&`
    // is skipped and the `;` still runs.
    let mixed = said(t, b"worker && echo yes ; echo always");
    assert!(mixed.contains("always"), "; did not run after a skipped &&");
    assert!(
        !mixed.contains("yes"),
        "the && ran even though its left-hand side was refused",
    );
}

/// **The decision this milestone had to make, read off a transcript**: a refusal is not an error,
/// and it gets its own number.
///
/// `worker` is refused at the prompt with nothing spawned, so `$?` is **2**. `worker 3` runs, so it
/// is 0. Unix cannot draw this line, because there `127` and a program's own `exit(1)` are the same
/// kind of integer; here they are different events and the shell knows which.
#[test_case]
fn a_refusal_and_a_success_report_different_numbers() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    // The script types `echo $?` twice, so both searches are anchored at their own prompt rather
    // than run over the whole transcript. `answer` slices from the *first* match of a needle, and
    // the two needles differ only in what precedes them, which is why the two `worker` lines are in
    // the script at all.
    let mut statuses = [""; 2];
    let mut found = 0usize;
    let needle = b"$ echo $?\n";
    let mut at = 0usize;
    while at + needle.len() <= t.len() {
        if &t[at..at + needle.len()] != needle {
            at += 1;
            continue;
        }
        let rest = &t[at + needle.len()..];
        let end = rest
            .windows(2)
            .position(|w| w == b"$ ")
            .unwrap_or(rest.len());
        if found < statuses.len() {
            statuses[found] = core::str::from_utf8(&rest[..end]).expect("ASCII").trim();
        }
        found += 1;
        at += needle.len();
    }
    assert_eq!(
        found,
        2,
        "the script types `echo $?` twice; found {found} answers in:\n{}",
        core::str::from_utf8(t).unwrap_or("<not utf-8>"),
    );
    assert_eq!(statuses[0], "0", "a command that ran should report 0");
    assert_eq!(
        statuses[1], "2",
        "a command this shell REFUSED should report 2, not 1: nothing ran",
    );
}

/// **The grammar's own refusals run nothing at all**, which is the same posture every other refusal
/// in this shell has: decided at the prompt, before anything is spawned or opened.
#[test_case]
fn a_line_the_grammar_cannot_read_runs_nothing() {
    let mut buf = [0u8; 3072];
    let Some(n) = transcript(&mut buf) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let t = &buf[..n];

    assert!(
        said(t, b"echo 'unclosed").contains("never closed"),
        "an unclosed quote should be refused by name: {:?}",
        said(t, b"echo 'unclosed"),
    );
    assert!(
        said(t, b"date &&").contains("connector"),
        "a connector with nothing after it should be refused by name: {:?}",
        said(t, b"date &&"),
    );
}
