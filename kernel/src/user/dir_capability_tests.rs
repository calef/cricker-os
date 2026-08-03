use fs_proto::dir;
use fs_proto::fixture::dirscape as esc;

use super::*;
use crate::sched;

/// The binary carrying the block server's role. One `cfg`, in `fs_service`, because the boot
/// path needs the same answer this module's tests do.
pub(super) fn blk_server_image() -> &'static [u8] {
    fs_service::blk_server_image()
}

/// Wire a `fs_subtree_caretaker` holding a capability to the fixture's `sub` with exactly
/// `rights`, run the directory attacker against it, and return its verdict bitmap. `run` keeps
/// the names the attacker creates distinct, because all three runs share one image within a
/// boot and an `EEXIST` would otherwise read as a refusal.
///
/// `None` when no RedoxFS disk is attached (nothing to test; do not fail).
fn attack_a_subtree(rights: u64, run: u64) -> Option<u64> {
    let Some(report) = fs_service::start_granted_dir(
        blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_subtree_caretaker")
            .expect("no fs_subtree_caretaker program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        fs_service::DirGrant {
            name: fs_proto::fixture::tree::SUB,
            rights,
            role: 5, // ROLE_DIR_ATTACKER
            arg: run,
            arg2: 0,
            stack_pages: 0,
        },
    ) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return None;
    };
    // Both handshakes happened inside `start_granted_dir`, before this attacker existed. That
    // ordering is the fix for the startup clobber `fs_service::wait_for_caretaker` records.
    let [tag, verdict, ..] = sched::ipc_recv(report);
    assert_eq!(
        tag,
        fs_proto::fixture::VERDICT,
        "the attacker's report is not a verdict word",
    );
    Some(verdict)
}

/// Compare a verdict against the exact set the configuration is specified to produce, and say
/// what the difference means in words rather than in hex.
///
/// Both directions are failures and they are different failures, which is why this is not one
/// equality assertion: a bit set that should not be is an escape, and a bit clear that should be
/// set means the capability did not work, so every refusal it reported proves nothing.
fn assert_verdict(got: u64, want: u64, what: &str) {
    let leaked = got & !want;
    let missing = want & !got;
    assert_eq!(
        leaked,
        0,
        "the {what} directory capability leaked: {}",
        describe_dirscape(leaked),
    );
    assert_eq!(
        missing, 0,
        "the {what} directory capability could not do what it was granted ({missing:#x}), so \
         its refusals prove nothing: a capability that reaches nothing is trivially confined",
    );
}

/// Name the bits a verdict set, so a failure reads as a sentence instead of a bitmap.
fn describe_dirscape(v: u64) -> &'static str {
    if v & esc::REACHED_PARENT != 0 {
        "it opened a file that is in the granted directory's PARENT"
    } else if v & esc::REACHED_SIBLING != 0 {
        "it reached the granted directory's SIBLING"
    } else if v & esc::WALKED_UP != 0 {
        "`..` resolved to something"
    } else if v & esc::WIDENED != 0 {
        "a child carried a right its parent did not have"
    } else if v & esc::ENUMERATED_A_STRANGER != 0 {
        "a listing held a name from outside the grant"
    } else if v & esc::FORGED_HANDLE != 0 {
        "it reached something with a handle it was never given"
    } else if v & esc::CREATED != 0 {
        "it created a name through a capability with no create right"
    } else if v & esc::MADE_A_DIR != 0 {
        "it made a directory through a capability that could not"
    } else if v & esc::RENAMED != 0 {
        "it renamed a name through a capability with no remove right"
    } else if v & esc::WROTE != 0 {
        "it wrote through a capability with no write right"
    } else if v & esc::DESCENDED != 0 {
        "it descended through a capability with no descend right"
    } else if v & esc::ENUMERATED != 0 {
        "it enumerated through a capability with no enumerate right"
    } else if v & esc::REACHED_AN_UNMATCHED_NAME != 0 {
        "it reached a name in the granted directory that the grant's set does not carry"
    } else if v & esc::SET_AN_ATTR != 0 {
        "it set an extended attribute through a capability with no write right"
    } else if v & esc::READ_ATTRS != 0 {
        "it read extended attributes through a capability that could not open the file"
    } else if v & esc::GRANTED_ACCESS_FAILED != 0 {
        "the granted access itself failed, so nothing was actually proven"
    } else {
        "nothing (an empty verdict should not have failed an assertion)"
    }
}

/// **A read-only explorer: it may descend, read and list, and it reaches nothing above itself.**
///
/// This is milestone 47's keystone under attack. The attacker holds a capability to `sub` and
/// spends its life trying to name `motd` (which is in the parent), `other` and `other/secret`
/// (which are in a sibling), and `..`. All three exist on the image and the caretaker one hop
/// up can reach every one of them on any request it likes, so each refusal is a fact about the
/// capability rather than about the filesystem.
///
/// It also proves both halves of "a child can never exceed its parent": a child asked for no
/// rights can do nothing at all, and a child asked for a right this grant does not carry is
/// refused rather than quietly given something smaller.
#[test_case]
fn a_read_only_directory_capability_reaches_its_subtree_and_nothing_above_it() {
    let Some(v) = attack_a_subtree(dir::DESCEND | dir::READ | dir::ENUMERATE, 1) else {
        return;
    };
    assert_verdict(
        v,
        esc::OPENED_ITS_OWN | esc::ENUMERATED | esc::DESCENDED | esc::READ_ATTRS,
        "read-only",
    );
}

/// **The same attacker against a capability carrying every right**, which is what makes the run
/// above mean anything.
///
/// Without it, a `fs_subtree_caretaker` that answered no to everything would pass the read-only
/// test perfectly, and so would a grant that reached nothing at all. Here the same requests
/// through the same code succeed: it writes, it creates, it makes a directory. And it *still*
/// reaches nothing above itself, which is the point: the widening is exactly the axes that were
/// widened.
#[test_case]
fn a_full_directory_capability_does_everything_inside_and_nothing_outside() {
    let Some(v) = attack_a_subtree(dir::ALL, 2) else {
        return;
    };
    assert_verdict(
        v,
        esc::OPENED_ITS_OWN
            | esc::ENUMERATED
            | esc::DESCENDED
            | esc::CREATED
            | esc::WROTE
            | esc::RENAMED
            | esc::MADE_A_DIR
            | esc::READ_ATTRS
            | esc::SET_AN_ATTR,
        "full",
    );
}

/// **Milestone 47's motivating sentence, made a test**: a program handed a directory to write
/// into can add to it and write to what it added, and it cannot walk into a subdirectory or find
/// out what else is in there.
///
/// Three rungs withheld at once (`DESCEND`, `ENUMERATE`, `REMOVE`), and two of them are the
/// interesting cases:
///
/// - `DESCEND` withheld while `CREATE` is held: `mkdir` needs both, so this capability can make
///   a file and cannot make a directory. A directory it could not have walked into would be a
///   way to mint a capability out of a right that was withheld.
/// - `REMOVE` withheld while `CREATE` is held is **"add to this, destroy nothing"** exactly, and
///   it is what `RENAME` exists to make falsifiable: this attacker creates a name and then tries
///   to move it, through the same code the full run below moves it with, and cannot. Before that
///   verb existed nothing on the wire consulted `REMOVE` at all, so the rung was a claim rather
///   than a rule.
#[test_case]
fn an_append_only_directory_capability_adds_and_cannot_walk_or_list() {
    let Some(v) = attack_a_subtree(dir::READ | dir::WRITE | dir::CREATE, 3) else {
        return;
    };
    assert_verdict(
        v,
        esc::OPENED_ITS_OWN | esc::CREATED | esc::WROTE | esc::READ_ATTRS | esc::SET_AN_ATTR,
        "append-only",
    );
}

/// **A name-set capability carries its files' attributes and still designates only its names**
/// (milestone 61, the third caretaker).
///
/// The other two caretakers are covered by the runs above and by the per-file attacker.
/// `fs_nameset_caretaker` is the one that inspects a name on **every** request, so teaching it
/// four verbs whose operand is an *attribute* name rather than a directory name is where this
/// milestone could most easily have gone wrong, in either direction:
///
/// - Filter the attribute name against the set, and a program behind `rm *.txt` cannot read what
///   is attached to a file the pattern did match. `READ_ATTRS` and `SET_AN_ATTR` catch that.
/// - Stop filtering the directory names, and the set stops being a set. `REACHED_AN_UNMATCHED`
///   `_NAME` catches that, and the witness holds `dir::READ`, which `rm`'s own witness does not,
///   so the naming question is asked here through the verbs `rm` never sends.
///
/// The set is one name inside the `sub` fixture, so `deeper` is one directory entry away and the
/// caretaker one hop up holds a capability that could open it.
#[test_case]
fn a_name_set_capability_reads_its_attributes_and_still_names_only_its_set() {
    let Some(report) = fs_service::start_granted_set(
        blk_server_image(),
        program("fs_server").expect("no fs_server program in the initrd archive"),
        program("fs_nameset_caretaker")
            .expect("no fs_nameset_caretaker program in the initrd archive"),
        program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        fs_service::SetGrant {
            dir: fs_proto::fixture::tree::SUB,
            // Exactly one name, and `deeper` deliberately left out of it.
            names: &[(fs_proto::fixture::tree::INNER.as_bytes(), false)],
            rights: dir::READ | dir::WRITE | dir::DESCEND,
            role: 6, // ROLE_SET_ATTRS
            arg: 0,
            arg2: 0,
            stack_pages: 0,
        },
    ) else {
        crate::println!("    (no RedoxFS disk attached; skipping)");
        return;
    };
    let [tag, v, ..] = sched::ipc_recv(report);
    assert_eq!(
        tag,
        fs_proto::fixture::VERDICT,
        "the name-set witness's report is not a verdict word",
    );
    assert_verdict(
        v,
        esc::OPENED_ITS_OWN | esc::READ_ATTRS | esc::SET_AN_ATTR,
        "name-set",
    );
}
