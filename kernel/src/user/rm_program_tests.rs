use fs_proto::dir;
use fs_proto::fixture::{VERDICT, rm as rr, tree};
use grant_plan::rmopt;

use super::*;
use crate::sched;

/// The most messages one run may send before the harness stops reading. A `rm` that never sent
/// its verdict would otherwise hang the boot; this turns it into a failed assertion.
const MAX_MESSAGES: usize = 64;

/// What one run of the program reported.
struct Outcome {
    /// Its exit status: 0, or the errno of the first failure (`fs_proto::fixture::rm`).
    status: u64,
    /// How many names it removed.
    removed: u64,
    /// **How many lines of text it printed before the verdict.** Zero is the interesting value:
    /// `rm(1)` is silent on success, and a `SEND` blocks until somebody receives it, so a line
    /// that was emitted cannot be missed by looking after the fact.
    printed: usize,
}

/// Run `rm` once inside a `fs_subtree_caretaker` holding [`tree::RMTREE`] with exactly
/// `rights`, told to remove `name` with `flags`.
///
/// `None` when no RedoxFS disk is attached (nothing to test; do not fail).
fn run_rm(rights: u64, name: &str, flags: u64) -> Option<Outcome> {
    assert!(
        fs_proto::grant::fits(name.as_bytes()),
        "the operand rides in two argument words",
    );
    let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
    let report = fs_service::start_granted_dir(
        dir_capability_tests::blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_subtree_caretaker")
            .expect("no fs_subtree_caretaker program in the initrd archive"),
        program("rm").expect("no rm program in the initrd archive"),
        fs_service::DirGrant {
            name: tree::RMTREE,
            rights,
            // `rm` is started with a **grant's** three words rather than a role and a number:
            // the spec (the operand's length, and the options where a caretaker's rights ride),
            // then the two words of name.
            role: fs_proto::grant::spec(name.len(), flags),
            arg: lo,
            arg2: hi,
            // Measured the way `spawn_fs_client` asks for: the recursion is real stack, each
            // level holding a listing buffer by value, and this program has no allocator.
            stack_pages: 6,
        },
    )?;

    // `printed` is the enumeration index rather than a counter, and the two coincide exactly:
    // the verdict arm returns *before* the increment, so on the iteration that sees it, the
    // index is the number of text frames that arrived first. That is the quantity being
    // reported, so this is not merely appeasing the lint.
    for (printed, _) in (0..MAX_MESSAGES).enumerate() {
        let [w0, w1, w2, _, _] = sched::ipc_recv(report);
        if w0 == VERDICT {
            return Some(Outcome {
                status: w1,
                removed: w2,
                printed,
            });
        }
        // A text frame: its first word is a byte count, which cannot collide with the verdict
        // word. That is what makes "it printed nothing" an assertion rather than a hope.
        assert!(w0 <= 16, "neither a verdict nor a text frame: {w0:#x}");
    }
    panic!("rm sent {MAX_MESSAGES} messages and never a verdict");
}

/// **The headline: `rm -r` removes the subtree it was granted, and a narrower capability
/// cannot begin.**
///
/// Three runs against one tree, in order, because they are one argument:
///
/// 1. `rm rm-doomed` with the full grant is **`EISDIR` and nothing removed**. A directory is a
///    refusal, never a silent escalation to recursive removal, which is `rm(1)`'s behaviour and
///    is what makes `-r` mean something.
/// 2. `rm -r rm-doomed` through a capability carrying only `REMOVE` **cannot even look**: the
///    `OPENDIR` is `ENOENT`, because a naming right withheld answers "in this scope there is no
///    such name" (DECISIONS §47). The tree is still there. Nothing in the program decided this;
///    the capability to walk was never handed over.
/// 3. The same command through a capability carrying `ENUMERATE｜DESCEND｜REMOVE` takes the
///    whole tree, bottom-up, five names.
///
/// Runs 1 and 2 are what make run 3 more than "a loop deleted some files": the *same binary*,
/// the *same operand*, and the outcome is decided by what was in the cspace.
#[test_case]
fn rm_r_takes_the_subtree_it_was_granted_and_a_narrower_grant_cannot_begin() {
    let Some(refused) = run_rm(dir::REMOVE_TREE, tree::RM_DOOMED, 0) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    assert_eq!(
        refused.status,
        rr::status(dir::EISDIR),
        "`rm` of a directory must refuse, not quietly recurse",
    );
    assert_eq!(refused.removed, 0);
    assert!(
        refused.printed > 0,
        "a failure is a diagnostic AND an exit status; this one printed nothing",
    );

    // The narrow grant: it may take names out of `rmtree` and may not walk into anything.
    let blind = run_rm(dir::REMOVE, tree::RM_DOOMED, rmopt::RECURSIVE)
        .expect("the service was wired by the first run");
    assert_eq!(
        blind.status,
        rr::status(2), // ENOENT
        "a capability that may not descend must not learn that the subtree is there",
    );
    assert_eq!(
        blind.removed, 0,
        "a `rm -r` that could not walk must not have removed anything on the way",
    );

    // And the same line through the grant that carries the walk.
    let done = run_rm(
        dir::REMOVE_TREE,
        tree::RM_DOOMED,
        rmopt::RECURSIVE | rmopt::VERBOSE,
    )
    .expect("the service was wired by the first run");
    assert_eq!(done.status, rr::OK, "the removal reported a failure");
    assert_eq!(
        done.removed, 5,
        "two files, a leaf, the directory holding it, and the top: five names",
    );
    assert_eq!(
        done.printed, 5,
        "`-v` prints one line per name removed, and nothing else",
    );
}

/// **`-f` is idempotency, and silence is the default.** Both are `rm(1)`'s, checked against the
/// man page rather than remembered.
///
/// - `rm rm-nothing` on a name that is not there is a diagnostic and a non-zero status.
/// - `rm -f rm-nothing` is **neither**: "if the file does not exist, do not display a diagnostic
///   message or modify the exit status". That is what makes a script re-runnable, and "absence
///   is the desired state" is not a lie about failure.
/// - `rm rm-solo` on a name that *is* there removes it and **prints nothing at all**, which is
///   why `-v` exists. Without this run the `-f` claim above is equally true of a program that
///   never prints.
///
/// Its names are its own, so this run and the recursive one above cannot depend on each other's
/// order.
#[test_case]
fn rm_f_ignores_a_name_that_is_not_there_and_success_says_nothing() {
    let Some(loud) = run_rm(dir::REMOVE_TREE, tree::RM_MISSING, 0) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    assert_eq!(loud.status, rr::status(2), "a missing name is ENOENT");
    assert!(loud.printed > 0, "and it says so");

    let quiet = run_rm(dir::REMOVE_TREE, tree::RM_MISSING, rmopt::FORCE)
        .expect("the service was wired by the first run");
    assert_eq!(
        quiet.status,
        rr::OK,
        "`-f` must not modify the exit status for a name that is not there",
    );
    assert_eq!(quiet.printed, 0, "`-f` suppresses the diagnostic too");
    assert_eq!(quiet.removed, 0, "there was nothing to remove");

    let did =
        run_rm(dir::REMOVE_TREE, tree::RM_SOLO, 0).expect("the service was wired by the first run");
    assert_eq!(did.status, rr::OK);
    assert_eq!(did.removed, 1);
    assert_eq!(
        did.printed, 0,
        "success prints nothing; `-v` is the option that changes that",
    );
}
