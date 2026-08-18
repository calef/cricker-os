use fs_proto::dir;
use fs_proto::fixture::{VERDICT, globscape as gb, rm as rr, tree};
use sink_proto::eof;

use super::*;
use crate::sched;

/// The `swish` binary's globbing role (`user/src/swish.rs`).
const ROLE_GLOB: u64 = 2;

/// The most messages one `rm` run may send before the harness stops reading, as
/// [`rm_program_tests`] bounds it and for the same reason.
const MAX_MESSAGES: usize = 64;

/// The set `gl-*.txt` matches in the fixture, which `fs_proto`'s host test pins against the
/// matcher. Stated literally here because building a grant is the one thing the kernel can do
/// without an enumeration, and proved to *be* the expansion over there.
const MATCHED: [(&[u8], bool); 2] = [
    (tree::GLOB_ONE.as_bytes(), false),
    (tree::GLOB_TWO.as_bytes(), false),
];

/// **The two phases are one test**, and the order is load-bearing rather than incidental: the
/// shell expands the pattern over the fixture as staged, and the second phase then removes what
/// it matched. Run the other way round, the expansion would find nothing and the agreement would
/// be an agreement about the empty set.
///
/// Splitting them into two `#[test_case]`s would make that ordering a property of the harness,
/// which is exactly the order-coupled fixture DECISIONS §27 spent a day on.
#[test_case]
fn what_a_shell_shows_is_what_a_set_grant_takes_away() {
    let Some(shown) = shell_expanded() else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    assert_shell_agreed(shown);
    a_set_capability_cannot_name_a_stranger();
    the_set_grant_removes_exactly_the_match();
}

/// Phase one: a shell rooted in `globset` runs `echo gl-*.txt` and plans `rm gl-*.txt`, and
/// reports whether the two produced the same names.
///
/// The rights it is granted are what expansion actually costs: `ENUMERATE` to list the
/// directory, `DESCEND` because the shell walks a path with `OPENDIR`, and `READ` so a listing
/// is not the only thing it could ever do. There is no `REMOVE` in it at all, which is the point
/// of `echo` being the half that demonstrates this: **showing the authority costs none of it.**
fn shell_expanded() -> Option<u64> {
    let report = fs_service::start_granted_dir(
        dir_capability_tests::blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_subtree_caretaker")
            .expect("no fs_subtree_caretaker program in the initrd archive"),
        program("swish").expect("no swish program in the initrd archive"),
        fs_service::DirGrant {
            name: tree::GLOBSET,
            rights: dir::ENUMERATE | dir::DESCEND | dir::READ,
            role: ROLE_GLOB,
            // The shell is told what its capability carries, for notes/shell-navigation.md's
            // reason: nothing on this wire reports what a handle holds.
            arg: fs_proto::grant::spec(0, dir::ENUMERATE | dir::DESCEND | dir::READ),
            arg2: 0,
            // **Four, and the number is a measurement**, as the navigating role's two were.
            // Two overflowed by 256 bytes, presenting as a data abort on the shell's own `sp`
            // and then as the 60 s lost-wakeup watchdog, because the test was still waiting for
            // a report from a process that had died.
            //
            // The cost is a name set travelling **by value** through a chain of frames a debug
            // build does not collapse: the expander holds one, `Expansion` carries one into
            // `plan`, `designate` returns one, and the `Endowment` that comes back carries one
            // more. That measurement is also what set `nameset::MAX_NAMES`: sixteen names did
            // not fit in the four pages this wiring maps, and `CLIENT_EXTRA_STACK`'s note is
            // right that the answer to that is smaller frames rather than a bigger number.
            stack_pages: 6,
        },
    )?;
    let [tag, verdict, ..] = sched::ipc_recv(report);
    assert_eq!(tag, VERDICT, "the shell's report is not a verdict word");
    Some(verdict)
}

fn assert_shell_agreed(v: u64) {
    assert_eq!(
        v & gb::GLOB_FAILED,
        0,
        "the shell could not plan the grant at all, so nothing it reported means anything",
    );
    let want = gb::EXPANDED
        | gb::AGREED
        | gb::EXCLUDED_A_STRANGER
        | gb::NO_MATCH_REFUSED
        | gb::PATTERN_IN_PATH_REFUSED
        | gb::TEXT_UNTOUCHED;
    assert_eq!(
        v, want,
        "the globbing witness reported {v:#x}, wanted {want:#x}",
    );
}

/// Run `rm` once behind a `fs_nameset_caretaker` holding [`MATCHED`] inside `globset`.
///
/// `name` empty means [`fs_proto::grant::WHOLE_NAMESPACE`]: the operand is the set, and `rm`
/// learns it by enumerating the capability it was handed.
fn run_rm(name: &str, flags: u64) -> (u64, u64) {
    let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
    let report = fs_service::start_granted_set(
        dir_capability_tests::blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_nameset_caretaker")
            .expect("no fs_nameset_caretaker program in the initrd archive"),
        program("rm").expect("no rm program in the initrd archive"),
        fs_service::SetGrant {
            dir: tree::GLOBSET,
            names: &MATCHED,
            // **`REMOVE` and nothing else.** `rm *.txt` takes names out of one directory; it may
            // not read them, write them, create beside them or walk under them. Listing the set
            // is not on this ladder at all, because the caretaker answers that from the grant.
            rights: dir::REMOVE,
            role: fs_proto::grant::spec(name.len(), flags),
            arg: lo,
            arg2: hi,
            stack_pages: 6,
        },
    )
    .expect("the FS service was wired by the shell phase");

    for _ in 0..MAX_MESSAGES {
        let [w0, w1, w2, _, _] = sched::ipc_recv(report);
        // The sink contract's end of stream, which is what `rm` ends with since 2026-08-17; the
        // verdict rides in the two words `OP_EOF` leaves free. The shell phase above still reports
        // a `VERDICT`, because that one is a witness's bitmap and not a byte stream.
        if w0 == eof() {
            return (w1, w2);
        }
        assert!(w0 <= 16, "neither a verdict nor a text frame: {w0:#x}");
    }
    panic!("rm sent {MAX_MESSAGES} messages and never a verdict");
}

/// **The attacker, and it is `rm` itself.** Told to remove a name that exists, sits one
/// directory entry away from the two it was granted, and that the caretaker one hop up could
/// remove on any request it liked.
///
/// It gets `ENOENT`: in this scope there is no such name. Nothing consulted a permission, and
/// nothing in `rm` decided not to try, which is what makes this a fact about the capability.
fn a_set_capability_cannot_name_a_stranger() {
    let (status, removed) = run_rm(tree::GLOB_MISS, 0);
    assert_eq!(
        status,
        rr::status(2), // ENOENT
        "a name outside the set must not be nameable through a set capability",
    );
    assert_eq!(removed, 0);
}

/// **And the grant works**, which is what stops the refusal above being equally true of a
/// capability that reaches nothing. Two names in, two names removed, and the two the pattern did
/// not match are still on the disk, asserted from the host by
/// `xtask::redoxfs_glob_grant_took_exactly_the_match`.
fn the_set_grant_removes_exactly_the_match() {
    let (status, removed) = run_rm("", 0);
    assert_eq!(status, rr::OK, "the set grant reported a failure");
    assert_eq!(
        removed,
        MATCHED.len() as u64,
        "a set grant must remove every name in it, and only those",
    );
}
