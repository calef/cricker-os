//! **The spawn protocol: the wire half of the shell-to-init grant expression.**
//!
//! When the shell resolves a `run` into an [`Endowment`](crate::Endowment), it does not build the
//! child itself: init holds the initrd and is the ELF loader (the parser stays in one place, out of
//! the shell). So the shell tells init what to spawn and, crucially, *delegates the capabilities it
//! grants* over the same endpoint. This module is that contract's word layout, the capability-shell
//! analogue of `line_editor::proto`.
//!
//! It is a **userspace** protocol, not kernel ABI. The kernel routes these words the way it routes
//! any IPC (DECISIONS §10, §12, §21); it never reads them. Adding a field is a change here, not to
//! the syscall surface.
//!
//! # The exchange
//!
//! The shell owns the sequence; init serves it in a loop.
//!
//! 1. **Request.** The shell `SEND`s three words on the spawn endpoint: the program id, the
//!    integer argument, and the memory-grant page count. See [`request`] / [`prog_id`] /
//!    [`arg`] / [`mem_pages`].
//! 2. **The directory grant, if the request announced one** ([`Wiring::dir`], milestone 31 phase 3):
//!    [`GRANT_WORDS`] plain `SEND`s carrying the caretaker's `START` words and then the child's.
//!    Before the delegation rather than after, because these are **data and not capabilities** and
//!    mixing the two orders would put a `RECV` where a `RECV_CAP` belongs.
//! 3. **Delegation.** The capabilities the request announced, in a fixed order: the supervised
//!    job's pair (untyped, frame), then the **sink** (milestone 50), then the **source**, then the
//!    **diagnostic endpoint** (DECISIONS §67), then the `--mem` untyped. Order rather than tags, because both sides read the same [`Wiring`] out of
//!    the same word and a promise nobody receives would deadlock both.
//!
//!    If `mem_pages > 0`, the shell `SEND_CAP`s exactly one capability there: an
//!    untyped it split from *its own* budget, sized to `mem_pages`. This is the grant made real,
//!    not parsed and dropped. Programs that grant no capability (worker) skip this step, and init
//!    knows to skip the matching `RECV_CAP` from `mem_pages == 0`.
//! 4. **Outcome.** init builds the child, endows it (the shared result endpoint always; the
//!    delegated untyped when present), and starts it. The child reports its own answer on the
//!    result endpoint. If init cannot build it (its own budget is spent, or the program vanished),
//!    it sends [`SPAWN_FAILED`] on the result endpoint so the shell's read completes instead of
//!    hanging.
//!
//! The result endpoint carries both init's failure sentinel and the child's success answer, and
//! the shell reads exactly once: a well-formed spawn yields the child's word, a failed one yields
//! [`SPAWN_FAILED`]. One reader, one word, no ambiguity.

/// The interruptible bit, packed into the high half of the page-count word so one `SEND` still
/// carries the whole request. `mem_pages` is a small count (budgeter's ceiling is 64), so the low
/// 32 bits hold it and this bit rides above.
const INTERRUPTIBLE_BIT: u64 = 1 << 32;

/// **A capability for the child's output slot follows** (milestone 50). Set by `>` and by every
/// stage of a `|` but the last: the shell delegates an endpoint and init puts it where the result
/// endpoint would have gone, so the child writes to a pipe or a file sink without knowing which.
const SINK_BIT: u64 = 1 << 33;

/// **A capability for the child's input slot follows** (milestone 50). Set by `<` and by every
/// stage of a `|` but the first.
const SOURCE_BIT: u64 = 1 << 34;

/// **A capability for the child's declared second output follows** (DECISIONS §67). Set for a
/// program whose manifest declares one, whether or not the line has a `2>` on it: the stream exists
/// because the program says so, and the operator only names where it goes.
///
/// Unlike [`SINK_BIT`] this does **not** say which slot: init reads that from the manifest, because
/// the slot is the program's declaration and not the shell's choice. What the wire says is only
/// "expect one more capability", which is what keeps the two sides in lockstep.
const DIAG_BIT: u64 = 1 << 35;

/// **A directory grant follows, and init is to build a caretaker for it** (milestone 31 phase 3).
///
/// The odd one out on this word, because it announces **data rather than a capability**. Every other
/// bit here says "expect one more `SEND_CAP`"; this one says "expect two more `SEND`s", and the
/// reason is that the shell has nothing to delegate. A directory grant is delivered by a
/// `fs_subtree_caretaker`, the caretaker has to hold the file service to attenuate it, and **the
/// shell's file-service endpoint carries no `GRANT`**, so the shell could not hand one over if it
/// wanted to. What it can do is say what the grant *is*; init holds the endpoint and builds the rest.
///
/// See [`GRANT_WORDS`] for what the two messages carry and why they are opaque to this module.
const DIR_BIT: u64 = 1 << 36;

/// **The two messages a [`Wiring::dir`] request is followed by**, in order, each three words:
///
/// 1. **the caretaker's `START` words**, which init passes to `fs_subtree_caretaker` verbatim: the
///    granted directory's name and the `fs_proto::dir` rights the subtree capability is to carry;
/// 2. **the confined program's `START` words**, which init passes to the program verbatim: for `rm`,
///    the operand's name and the options that were typed.
///
/// **This module does not decode either, deliberately.** They are `fs_proto::grant`'s packing, and
/// `grant_plan` has no non-dev dependency on `fs_proto` on purpose (its own manifest says why: the
/// shell must be able to check a command line without linking the filesystem contract). Passing them
/// through as opaque triples keeps that true, and it means a change to how a grant is packed is a
/// change in one crate rather than in the wire this one owns. The shell packs them; init forwards
/// them; nothing in between reads them.
///
/// Two messages rather than one because the two processes are started with different names: the
/// caretaker with the *directory*, the program with the *operand inside it*. Six words do not fit in
/// three.
pub const GRANT_WORDS: usize = 2;

/// **Where the shell's operators end up on the wire**, alongside the parts of the endowment that
/// were always here. `sink` and `source` are booleans rather than capabilities because the
/// capability travels separately, over `SEND_CAP`: this word only says whether to expect it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Wiring {
    /// A foreground job the shell will supervise (DECISIONS §24). Two capabilities lead the
    /// delegation: a job untyped the child is built from so the shell can tear it down, then a
    /// shared job frame.
    pub interruptible: bool,
    /// The child's output slot is substituted (`>` or the left of a `|`).
    pub sink: bool,
    /// The child's input slot is filled (`<` or the right of a `|`).
    pub source: bool,
    /// The child declares a second output stream, so one more endpoint follows (DECISIONS §67).
    pub diagnostics: bool,
    /// **A directory grant follows as two data messages** ([`GRANT_WORDS`]), and init is to build a
    /// `fs_subtree_caretaker` for it before it builds the child. The only entry here that announces
    /// data instead of a capability; see `DIR_BIT`.
    pub dir: bool,
}

/// Build the three request words from a resolved endowment's parts.
pub fn request(prog_id: u64, arg: u64, mem_pages: u64, w: Wiring) -> (u64, u64, u64) {
    let mut w2 = mem_pages & 0xffff_ffff;
    if w.interruptible {
        w2 |= INTERRUPTIBLE_BIT;
    }
    if w.sink {
        w2 |= SINK_BIT;
    }
    if w.source {
        w2 |= SOURCE_BIT;
    }
    if w.diagnostics {
        w2 |= DIAG_BIT;
    }
    if w.dir {
        w2 |= DIR_BIT;
    }
    (prog_id, arg, w2)
}

/// The whole wiring of a received request (word 2), so init reads it once rather than asking three
/// separate questions of the same word.
pub fn wiring(w2: u64) -> Wiring {
    Wiring {
        interruptible: interruptible(w2),
        sink: w2 & SINK_BIT != 0,
        source: w2 & SOURCE_BIT != 0,
        diagnostics: w2 & DIAG_BIT != 0,
        dir: w2 & DIR_BIT != 0,
    }
}

/// The program id from a received request (word 0).
pub fn prog_id(w0: u64) -> u64 {
    w0
}

/// The integer argument from a received request (word 1).
pub fn arg(w1: u64) -> u64 {
    w1
}

/// The memory-grant page count from a received request (word 2). Non-zero means one delegated
/// untyped capability follows the interrupt caps (if any) over `SEND_CAP` / `RECV_CAP`.
pub fn mem_pages(w2: u64) -> u64 {
    w2 & 0xffff_ffff
}

/// Whether this is a supervised foreground job (word 2's high bit). When set, the delegation leads
/// with two caps: a job untyped (init builds the child from it; the shell keeps it to `DESTROY`) and
/// a shared job frame (the cooperative interrupt flag and the child's status).
pub fn interruptible(w2: u64) -> bool {
    w2 & INTERRUPTIBLE_BIT != 0
}

/// The data word carried alongside the delegated untyped in the `SEND_CAP`. It is not load-bearing
/// (init identifies the cap by the protocol position, not the tag), but a fixed marker makes a
/// misrouted message obvious in a trace. Its low bits echo the page count as a cheap cross-check.
pub const CAP_TAG: u64 = 0x6361_705f; // "cap_" little-endian-ish marker

/// The sentinel init sends on the result endpoint when it could not build the child, so the
/// shell's single read completes with a legible failure rather than blocking forever. Distinct
/// from any answer a real program would report (no phase-1 program returns `u64::MAX`).
pub const SPAWN_FAILED: u64 = u64::MAX;

/// The ack init sends on the result endpoint when a **supervised** (interruptible) child started
/// cleanly. An interruptible child reports its own progress and exit through the shared job frame,
/// not the result endpoint, so init sends this once as the go-ahead: the shell reads it, then begins
/// watching the job frame. `0` is distinct from [`SPAWN_FAILED`].
pub const SPAWN_OK: u64 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        // 6, not 1: word zero must come back verbatim, and an id of 1 cannot tell verbatim from a
        // hardcoded answer.
        let (w0, w1, w2) = request(6, 9, 16, Wiring::default());
        assert_eq!(prog_id(w0), 6);
        assert_eq!(arg(w1), 9);
        assert_eq!(mem_pages(w2), 16);
        assert_eq!(wiring(w2), Wiring::default());
    }

    #[test]
    fn interruptible_bit_survives_a_zero_page_count() {
        // The interrupt demonstrators take no --mem, so the flag must ride independent of the count.
        let (_, _, w2) = request(
            2,
            0,
            0,
            Wiring {
                interruptible: true,
                ..Wiring::default()
            },
        );
        assert_eq!(mem_pages(w2), 0);
        assert!(interruptible(w2));
    }

    #[test]
    fn no_grant_is_zero_pages() {
        let (_, _, w2) = request(0, 5, 0, Wiring::default());
        assert_eq!(mem_pages(w2), 0);
    }

    /// **The five flags are independent of each other and of the page count** (milestone 50, §67's
    /// fourth, and milestone 31 phase 3's fifth). They share one word, and what init reads next off
    /// the endpoint depends on all of them, so a bit that bled into another would make init take a
    /// capability for a data word (or the reverse) and hang rather than fail.
    #[test]
    fn the_wiring_flags_do_not_collide() {
        for &interruptible in &[false, true] {
            for &sink in &[false, true] {
                for &source in &[false, true] {
                    for &diagnostics in &[false, true] {
                        for &dir in &[false, true] {
                            let w = Wiring {
                                interruptible,
                                sink,
                                source,
                                diagnostics,
                                dir,
                            };
                            let (_, _, w2) = request(3, 0, 64, w);
                            assert_eq!(wiring(w2), w, "{w:?}");
                            assert_eq!(mem_pages(w2), 64, "{w:?}");
                        }
                    }
                }
            }
        }
    }
}
