//! **The spawn protocol: the wire half of the shell-to-init grant expression.**
//!
//! When the shell resolves a `run` into an [`Endowment`](crate::Endowment), it does not build the
//! child itself: init holds the initrd and is the ELF loader (the parser stays in one place, out of
//! the shell). So the shell tells init what to spawn and, crucially, *delegates the capabilities it
//! grants* over the same endpoint. This module is that contract's word layout, the capability-shell
//! analogue of `lineedit::proto`.
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
//! 2. **Delegation.** If `mem_pages > 0`, the shell `SEND_CAP`s exactly one capability next: an
//!    untyped it split from *its own* budget, sized to `mem_pages`. This is the grant made real,
//!    not parsed and dropped. Programs that grant no capability (worker) skip this step, and init
//!    knows to skip the matching `RECV_CAP` from `mem_pages == 0`.
//! 3. **Outcome.** init builds the child, endows it (the shared result endpoint always; the
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

/// Build the three request words from a resolved endowment's parts. `interruptible` tells init this
/// is a foreground job the shell will supervise: two capabilities lead the delegation (a job untyped
/// the child is built from so the shell can tear it down, then a shared job frame), before any
/// `--mem` untyped.
pub fn request(prog_id: u64, arg: u64, mem_pages: u64, interruptible: bool) -> (u64, u64, u64) {
    let w2 = (mem_pages & 0xffff_ffff) | if interruptible { INTERRUPTIBLE_BIT } else { 0 };
    (prog_id, arg, w2)
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
        let (w0, w1, w2) = request(1, 9, 16, false);
        assert_eq!(prog_id(w0), 1);
        assert_eq!(arg(w1), 9);
        assert_eq!(mem_pages(w2), 16);
        assert!(!interruptible(w2));
    }

    #[test]
    fn interruptible_bit_survives_a_zero_page_count() {
        // The interrupt demonstrators take no --mem, so the flag must ride independent of the count.
        let (_, _, w2) = request(2, 0, 0, true);
        assert_eq!(mem_pages(w2), 0);
        assert!(interruptible(w2));
    }

    #[test]
    fn no_grant_is_zero_pages() {
        let (_, _, w2) = request(0, 5, 0, false);
        assert_eq!(mem_pages(w2), 0);
    }
}
