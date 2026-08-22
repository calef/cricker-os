use fs_proto::dir;
use fs_proto::fixture::{navscape as nb, tree};

use super::*;
use crate::sched;

/// The `swish` binary's navigating role (`user/src/swish.rs`).
const ROLE_NAVIGATE: u64 = 1;

/// The bits every navigating shell must report whatever it was rooted in: `pwd` at its root,
/// `..` clamped, `/` naming its own root and stopping there, a listing, and the whole
/// `mkdir` / create / `rm` / `touch` sequence including the two halves of "unlink is not revoke"
/// and the two halves of `touch`'s own "create absent, leave present alone".
const ALWAYS: u64 = nb::PWD_IS_ROOT
    | nb::CLAMPED_AT_ROOT
    // The namespace half (2026-08-18): `/` is the root of your own namespace, and it stops there.
    | nb::ABSOLUTE_IS_MY_ROOT
    | nb::ABSOLUTE_CLAMPED_AT_ROOT
    | nb::LISTED
    | nb::CREATED
    | nb::MADE_DIR
    | nb::UNLINKED
    | nb::HOLDER_KEPT_READING
    | nb::NAME_GONE_AFTER_UNLINK
    | nb::UNLINK_REFUSED_A_DIRECTORY
    // The two halves of "no single call takes a subtree away", added with `RMDIR`: a directory
    // with a name in it is refused, and the same call works once the name is out.
    | nb::RMDIR_REFUSED_NON_EMPTY
    | nb::RMDIR_REMOVED_EMPTY
    // `touch`'s two-fold contract: creates an absent name, and a second call on a name that now
    // holds a body leaves it exactly as it was.
    | nb::TOUCH_CREATED
    | nb::TOUCH_PRESERVED;

/// Wire a `fs_subtree_caretaker` holding a capability to `root` and run the shell's navigation
/// script inside it. `run` keeps the names it creates distinct across runs sharing one image.
///
/// The run index and the rights ride in one word packed by `fs_proto::grant::spec`, the same
/// packing the caretaker's own grant uses: the shell is **told** what its capability carries
/// because nothing on this wire reports what a handle holds, and `OPENDIR` refuses a request
/// wider than the parent rather than narrowing it (notes/shell-navigation.md).
///
/// `None` when no RedoxFS disk is attached.
fn navigate(root: &'static str, run: u64) -> Option<u64> {
    let report = fs_service::start_granted_dir(
        dir_capability_tests::blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_subtree_caretaker")
            .expect("no fs_subtree_caretaker program in the initrd archive"),
        program("swish").expect("no swish program in the initrd archive"),
        fs_service::DirGrant {
            name: root,
            rights: dir::ALL,
            role: ROLE_NAVIGATE,
            arg: fs_proto::grant::spec(run as usize, dir::ALL),
            arg2: 0,
            // Measured, not guessed: see `spawn_fs_client`. A shell carries a path stack, a
            // parsed path and a listing buffer by value, and one page is 192 bytes short.
            stack_pages: 2,
        },
    )?;
    let [tag, verdict, ..] = sched::ipc_recv(report);
    assert_eq!(
        tag,
        fs_proto::fixture::VERDICT,
        "the shell's report is not a verdict word",
    );
    Some(verdict)
}

/// Compare a report against the exact set the configuration is specified to produce. Both
/// directions are failures and they are different ones: a bit set that should not be is a
/// shell reaching past its root, and a bit clear that should be set is a shell that could not
/// navigate at all, whose refusals therefore prove nothing.
fn assert_report(got: u64, want: u64, what: &str) {
    assert_eq!(
        got & !want,
        0,
        "the shell rooted at {what} did something it must not: {}",
        describe(got & !want),
    );
    assert_eq!(
        got & want,
        want,
        "the shell rooted at {what} could not do what its capability allows ({:#x} missing), \
         so every refusal it reported proves nothing",
        want & !got,
    );
}

/// Name a bit, so a failure reads as a sentence.
fn describe(v: u64) -> &'static str {
    if v & nb::WALKED_UP != 0 {
        "`..` climbed out of its root"
    } else if v & (nb::REACHED_SECRET | nb::REACHED_INNER) != 0 {
        "it opened a file that exists only in the OTHER shell's root"
    } else if v & (nb::ABSOLUTE_REACHED_SECRET | nb::ABSOLUTE_REACHED_INNER) != 0 {
        "an absolute path reached a file that exists only in the OTHER shell's root, so `/` is \
         rooted in something wider than what this shell holds"
    } else if v & (nb::SAW_SECRET | nb::SAW_INNER) != 0 {
        "its listing held a name from the other shell's root"
    } else if v & nb::NAVIGATION_FAILED != 0 {
        "the navigation itself failed, so nothing was proven"
    } else if v & nb::DESCENDED != 0 {
        "it descended into a directory that is not in its root"
    } else {
        "something no other bit describes"
    }
}

/// **A shell navigates the subtree it holds, and cannot climb out of it.**
///
/// `pwd` renders `/` at its root because that is the root of the only namespace it has; `..`
/// there is refused with nothing sent, because `..` is a pop of the stack of capabilities the
/// shell descended through and at the root there is nothing to pop; an absolute path is refused
/// as *unnameable*, since there is no namespace to root one in. Then the verbs that change the
/// tree, including the one this milestone insists on separating: `rm` removes the **name**, the
/// handle the shell still holds keeps reading the bytes, and the name really is gone.
#[test_case]
fn a_shell_navigates_its_own_subtree_and_clamps_at_its_root() {
    let Some(v) = navigate(tree::SUB, 4) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    assert_report(
        v,
        ALWAYS
            | nb::REACHED_INNER
            | nb::ABSOLUTE_REACHED_INNER
            | nb::SAW_INNER
            | nb::DESCENDED
            | nb::RETURNED,
        "sub",
    );
}

/// **The headline: every shell has its own root, and neither can name the other's files.**
///
/// Two shells, two subtrees of one image, one script. Each tries to open `sub/inner` and
/// `other/secret` and reports which it reached, and it is **told nothing about which subtree it
/// was rooted in**, so the property is read off the pair rather than claimed by either. The
/// listings are checked the same way, because a listing is a rendering of authority: a name from
/// the other shell's root appearing in one would be an escape even though nothing was opened.
///
/// Not by policy. The FS server can reach both directories on any request it likes, and the
/// caretakers one hop up hold the whole image root. What stops each shell is that **no
/// capability reaching the other subtree exists in its cspace**, which is why the two runs are
/// sequential and it costs nothing: they are separate processes with separate roots, and being
/// alive at the same instant would prove no more than this does (they share one page with the
/// FS server, so the harness runs them in turn).
#[test_case]
fn two_shells_with_different_roots_cannot_name_each_others_files() {
    let Some(a) = navigate(tree::SUB, 5) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let b = navigate(tree::OTHER, 6).expect("the service was wired for the first shell");

    assert_report(
        a,
        ALWAYS
            | nb::REACHED_INNER
            | nb::ABSOLUTE_REACHED_INNER
            | nb::SAW_INNER
            | nb::DESCENDED
            | nb::RETURNED,
        "sub",
    );
    // The second holds a subtree with no child directory in it, so it cannot descend, and that
    // difference is the point: the same script against a different capability does different
    // things, and neither shell's world contains the other's.
    assert_report(
        b,
        ALWAYS | nb::REACHED_SECRET | nb::ABSOLUTE_REACHED_SECRET | nb::SAW_SECRET,
        "other",
    );

    // Stated once more as the crossing, because that is the sentence the milestone makes and an
    // exact-set assertion is easy to read as a list of unrelated facts.
    assert_eq!(
        (a & nb::REACHED_SECRET, b & nb::REACHED_INNER),
        (0, 0),
        "a shell named a file in the other shell's root",
    );
    assert_ne!(a & nb::REACHED_INNER, 0);
    assert_ne!(b & nb::REACHED_SECRET, 0);

    // **And once more with a leading slash**, which is the namespace half's whole claim: `/` is
    // the root of *your* namespace, so the same absolute token typed in two shells opens two
    // different files, and neither can name the other's. A `/` rooted in anything global would
    // set both of these bits in both reports.
    assert_eq!(
        (
            a & nb::ABSOLUTE_REACHED_SECRET,
            b & nb::ABSOLUTE_REACHED_INNER
        ),
        (0, 0),
        "an absolute path named a file in the other shell's root",
    );
    assert_ne!(a & nb::ABSOLUTE_REACHED_INNER, 0);
    assert_ne!(b & nb::ABSOLUTE_REACHED_SECRET, 0);
}
