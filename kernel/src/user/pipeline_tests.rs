use super::*;

/// The last line `user/src/swish.rs`'s pipeline role prints. Must match `PIPELINE_DONE` there.
const DONE: &[u8] = b"== pipelines done\n";

/// The script's transcript, run **once** and shared by every assertion below.
///
/// Cached because running it is expensive (a shell, an init service, and several spawned
/// programs) and because the assertions are about one run of one script: re-running it per test
/// would not be four independent measurements, it would be the same measurement four times.
static TRANSCRIPT: spin::Mutex<Option<([u8; 3072], usize)>> = spin::Mutex::new(None);

/// Run the script if nothing has yet, and hand back everything the shell printed.
fn transcript(out: &mut [u8; 3072]) -> usize {
    let mut cache = TRANSCRIPT.lock();
    if cache.is_none() {
        let Some(w) = pipeline_service::start() else {
            panic!("no swish program in the initrd archive, or no memory to wire one");
        };
        let mut buf = [0u8; 3072];
        let n = pipeline_service::transcript(&w, DONE, &mut buf);
        *cache = Some((buf, n));
    }
    let (buf, n) = cache.as_ref().expect("just filled");
    out.copy_from_slice(buf);
    *n
}

use pipeline_service::{answer, counts};

/// **The headline: the same `date`, two destinations, the same bytes.**
///
/// `date` is spawned twice from the same ELF with the same argument. The first time its output
/// slot holds the shell's result endpoint and the shell prints what arrives; the second time it
/// holds an endpoint into `wc`, and `wc` counts what arrives. Neither `date` was told which, and
/// there is no message on the sink contract that could have told it.
///
/// The assertion is the byte count, which is the one number that has to agree. It is compared
/// against the *observed* first arm rather than a constant, so it holds whether or not this
/// boot has a clock and whatever `date` decides to say.
///
/// The word and line counts are checked too, because a byte count alone would pass for a `wc`
/// that had simply counted the right number of wrong bytes.
#[test_case]
fn one_program_two_destinations_and_the_same_bytes() {
    let mut buf = [0u8; 3072];
    let n = transcript(&mut buf);
    let t = &buf[..n];

    // Arm one: the shell's own result endpoint. It prints two spaces, then date's bytes.
    let direct = answer(t, b"date");
    assert!(
        direct.starts_with(b"  ") && direct.ends_with(b"\n"),
        "date printed {:?}, which is not one line through the shell",
        core::str::from_utf8(direct).unwrap_or("<not utf-8>"),
    );
    let printed = &direct[2..];

    // Arm two: an endpoint into `wc`.
    let (lines, words, bytes) = counts(&answer(t, b"date | wc")[2..]);
    assert_eq!(
        bytes as usize,
        printed.len(),
        "the same date wrote {} bytes to the shell and {bytes} into a pipe: {:?}",
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

/// **A builtin can lead a pipeline, because the shell can be a sink's client too.**
///
/// `echo hello world | wc` is a real two-party pipeline with only one spawned process in it:
/// the shell writes the bytes into the endpoint itself. That costs no new mechanism, and it is
/// what the sink contract being register-only buys (notes/sink-protocol.md): being a writer
/// needs one capability and nothing else.
///
/// Twelve bytes: `hello world` plus the newline the shell adds, which is what `echo` prints.
#[test_case]
fn the_shell_can_be_the_producer() {
    let mut buf = [0u8; 3072];
    let n = transcript(&mut buf);
    let t = &buf[..n];
    assert_eq!(
        counts(&answer(t, b"echo hello world | wc")[2..]),
        (1, 2, 12),
        "the shell's own bytes did not arrive at wc intact",
    );
}

/// **Three stages, so a middle one is a real middle.**
///
/// The second `wc` counts the first `wc`'s output, which is only possible because `wc` writes
/// the same contract it reads. `1 2 12\n` is seven bytes, one line, three words.
#[test_case]
fn a_pipeline_composes_past_two_stages() {
    let mut buf = [0u8; 3072];
    let n = transcript(&mut buf);
    let t = &buf[..n];
    assert_eq!(
        counts(&answer(t, b"echo hello world | wc | wc")[2..]),
        (1, 3, 7),
        "the middle stage's output did not reach the last one",
    );
}

/// **The refusals happen at the prompt, with nothing spawned**, which is the half of this that
/// Unix cannot do. Each of these is a fact the manifest knows and a shell can therefore act on
/// before it moves any authority.
#[test_case]
fn the_line_is_refused_before_anything_is_spawned() {
    let mut buf = [0u8; 3072];
    let n = transcript(&mut buf);
    let t = &buf[..n];

    // A reader with nothing to read. On Unix this is a shell that appears to hang. The sentence
    // names all three ways to feed it, and the first of them is milestone 31's operand form, so
    // this doubles as the check that the refusal tells a person what to type next.
    let said = answer(t, b"wc");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("name a file"),
        "`wc` alone should be refused for having no input, not run: {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );

    // A program whose answer is a register, on the left of a pipe.
    let said = answer(t, b"worker 9 | wc");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("byte stream"),
        "`worker 9 | wc` should be refused for having no bytes: {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );

    // And one that reads nothing, on the right of one.
    let said = answer(t, b"date | date");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("no input"),
        "`date | date` should be refused: date reads nothing. got {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );
}

/// **`>` needs a filesystem this shell was granted none of, and it says so** rather than
/// running the command and printing the output to the terminal.
///
/// This is the **negative control** for [`redirection_tests`], which runs the same ELF from the
/// same archive with one more capability in it and writes the file. Neither behaviour is a
/// branch in the shell: the refusal is a fact about a cspace with nothing in slot 4, and that is
/// the sentence milestone 31 wrote and this milestone had to keep true. See notes/pipes.md.
#[test_case]
fn a_redirection_a_shell_cannot_back_is_refused_rather_than_dropped() {
    let mut buf = [0u8; 3072];
    let n = transcript(&mut buf);
    let t = &buf[..n];
    let said = answer(t, b"date > report.txt");
    assert!(
        core::str::from_utf8(said)
            .unwrap_or("")
            .contains("no such capability"),
        "a redirection this shell cannot back must be refused, not silently dropped: {:?}",
        core::str::from_utf8(said).unwrap_or("<not utf-8>"),
    );
}
