#![cfg_attr(not(test), no_std)]
//! **The C seam's shared half** (milestone 36, DECISIONS §31).
//!
//! Two Rust programs sit either side of the foreign component: `c_shim` is the shell that links the
//! C and calls it, and `c_confiner` is the process that builds `c_shim`, supervises it, and holds
//! the witness pages that prove what the C could not reach. This is what they agree on: the
//! address-space layout, the report protocol, and the constants that are also written down in
//! `user/c/c_seam.c`.
//!
//! A crate rather than a `#[path]` module since 2026-08-01 (CLAUDE.md rule 7): what two binaries
//! must agree on is a crate, so that the agreement is reachable by host tests and by Kani. The test
//! at the bottom of this file is what that bought.
//!
//! # The layout, and why these numbers
//!
//! ```text
//!   c_shim (the C component's process)          c_confiner (the checker)
//!   ------------------------------------       -----------------------------------------
//!   0x0040_0000  text / rodata / data          0x0040_0000  text / rodata / data
//!   0x0050_0000  stack                         0x0050_0000  stack
//!   0x4000_0000  heap (malloc/free)            0x2000_0000  the initrd, read-only
//!   0x5000_0000  GRANT       1 page, RW  <---> 0x5000_0000  the same frame, RW
//!   0x5000_1000  WITNESS_RO  1 page, RO  <---> 0x5000_1000  the same frame, RW
//!   0x5000_2000  NOTHING (unmapped)            0x5000_2000  a different frame, RW
//! ```
//!
//! The two witness pages answer two different questions, which is why there are two:
//!
//! - **`WITNESS_RO` is the same physical page, present in the C component's own page tables.** So
//!   when the off-by-one store does not change it, that is not "the write went somewhere else"; the
//!   page was right there, reachable, and the write did not happen. This is the stronger witness.
//! - **`WITNESS_FAR` is a different physical page at the same virtual address.** The C component has
//!   no mapping at `0x5000_2000` at all, and the checker does. So when a wild store to that address
//!   does not change the checker's page, that is the statement "a virtual address means nothing
//!   outside the address space that owns it," which is the MMU claim, made concrete.
//!
//! Both patterns are position-derived, the discipline milestone 29 used for the framebuffer: a
//! partial overwrite is detected, and a `memset` of any single value could not pass.
//!
//! # Examples
//!
//! Everything in here that is arithmetic rather than a layout number is host-testable, which is the
//! whole reason rule 7 made this a crate instead of a `#[path]` module. The checksum is the piece
//! that matters: it is a Rust recomputation of what the C returns, so it is **two implementations of
//! one definition** rather than an echo of whatever the C happened to produce.
//!
//! ```
//! use c_seam::{INPUT, expected_checksum};
//!
//! let sum = expected_checksum(INPUT);
//!
//! // The uppercasing is folded into the hash, matching `c_seam_transform`, so the input's case
//! // cannot change the answer. A C side that skipped the transform would not land on this number.
//! assert_eq!(expected_checksum(b"nife"), expected_checksum(b"NIFE"));
//! assert_ne!(expected_checksum(b"nife"), sum);
//!
//! // FNV-1a over the input the checker writes before every attempt, including its NUL. This is the
//! // number `c_seam_transform` has to put in the grant's output area for the honest attempt to pass.
//! assert_eq!(sum, 0x61a0_7dae);
//! ```
//!
//! The witness patterns are position-derived, and the two properties that buys are worth asserting
//! because both are about what *cannot* pass:
//!
//! ```
//! use c_seam::{MARK, pattern_far, pattern_ro};
//!
//! // No two adjacent bytes agree, so a `memset` of any single value fails, and so does a partial
//! // overwrite. This is milestone 29's framebuffer discipline applied to one page.
//! for i in 0..64 {
//!     assert_ne!(pattern_ro(i), pattern_ro(i + 1));
//!     assert_ne!(pattern_far(i), pattern_far(i + 1));
//! }
//!
//! // Two different generators, so a checker bug that read the wrong page would not accidentally
//! // agree with the page it should have read.
//! assert!((0..64).any(|i| pattern_ro(i) != pattern_far(i)));
//!
//! // And the byte the misbehaving C stores differs from both at offset 0, which is the only offset
//! // either of them targets. So "the witness is unchanged" cannot hold by accident.
//! assert_ne!(MARK, pattern_ro(0));
//! assert_ne!(MARK, pattern_far(0));
//! ```
//!
//! Name: ratified 2026-08-01 (calef, milestone 61), when the C symbols became `c_seam_*` and the
//! file left `user/src/` under rule 7. Refused `cseam` (run together, and it sat among 48 programs
//! while being none of them, which is one of the cases that produced the rule). The `c_` prefix
//! means "written in C" (DECISIONS §31).

// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

/// The page size, which is the grant's size too. One page is enough: the seam is what is under test,
/// not throughput.
pub const PAGE: u64 = 4096;

/// Where the shared grant is mapped, in **both** address spaces at the same virtual address. Same
/// number on both sides on purpose, so `c_seam_wild`'s target address is one the checker can name
/// without translating anything.
pub const GRANT_VA: u64 = 0x5000_0000;

/// The read-only witness: the page immediately after the grant. Mapped read-only into the C
/// component (which is what makes an off-by-one a permission fault) and read/write into the checker.
pub const WITNESS_RO_VA: u64 = GRANT_VA + PAGE;

/// The unmapped witness: two pages after the grant. **Not mapped into the C component at all.** The
/// checker maps a different frame here, at this same virtual address.
pub const WITNESS_FAR_VA: u64 = GRANT_VA + 2 * PAGE;

// ===========================================================================================
// The grant's contents. Mirrors the `C_SEAM_*` defines in user/c/c_seam.c; a C ABI has no way to
// share a struct definition without one language generating the other's bindings, and for one page
// of bytes the comment in both files is the honest cheaper answer.
// ===========================================================================================

/// Where the input string starts in the grant.
pub const IN_OFF: usize = 0;
/// Where the output starts: four little-endian checksum bytes, then the transformed string.
pub const OUT_OFF: usize = 2048;

/// The byte the misbehaving C functions store. Differs from both witness patterns at offset 0, which
/// is the only offset either of them targets, so "the witness is unchanged" cannot hold by accident.
pub const MARK: u8 = 0xC0;

/// The input the checker writes before every attempt, and the answer it expects back. ASCII and
/// lowercase, so the uppercasing transform has visible work to do.
pub const INPUT: &[u8] = b"nife foreign component\0";

/// The position-derived witness patterns. Different generators for the two pages so a checker bug
/// that read the wrong page would not accidentally agree.
pub fn pattern_ro(i: usize) -> u8 {
    (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8
}
/// The far witness page's generator; see [`pattern_ro`] for why it differs.
pub fn pattern_far(i: usize) -> u8 {
    (i.wrapping_mul(17).wrapping_add(3) & 0xff) as u8
}

/// FNV-1a over `bytes`, uppercased the way `c_seam_transform` uppercases. The Rust recomputation of
/// what the C returns: two implementations of one definition, which is what makes the checksum a
/// real check rather than an echo.
pub fn expected_checksum(bytes: &[u8]) -> u32 {
    let mut h: u32 = 2166136261;
    for &b in bytes {
        let up = if b.is_ascii_lowercase() { b - 32 } else { b };
        h ^= up as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

// ===========================================================================================
// What each attempt asks the C to do. Passed to `c_shim` in its second argument register, which is
// also its attempt number: attempt 0 overruns, attempt 1 goes wild, attempt 2 does the real work.
// The order matters, because "the supervisor restarts it and the restarted component works" is only
// proven if the honest run comes *after* the crashes.
// ===========================================================================================

/// Attempt 0: `c_seam_overrun`, an off-by-one write that lands one byte into the read-only
/// witness page and faults on the permission check.
pub const ATTEMPT_OVERRUN: u64 = 0;
/// Attempt 1: `c_seam_wild`, a write a whole page further out, landing on a virtual address
/// this process has no mapping for at all and faulting on translation rather than permission.
pub const ATTEMPT_WILD: u64 = 1;
/// Attempt 2: `c_seam_transform`, the real work, run after both crashes to prove the
/// supervisor's restart left the component actually usable rather than merely alive.
pub const ATTEMPT_HONEST: u64 = 2;
/// How many attempts a full run makes.
pub const ATTEMPTS: u64 = 3;

/// **The attempt selectors are dense and start at zero, because the C indexes an array with them.**
/// Asserted at compile time rather than in a test: every operand is a `const`, so a runtime check
/// would only re-verify what the compiler already knows, and a build failure names the problem at
/// the edit instead of at the next test run. Same idiom as `fs_proto`'s verdict/byte-count
/// assertion (notes/rm.md).
const _: () = assert!(ATTEMPT_OVERRUN == 0);
const _: () = assert!(ATTEMPT_WILD < ATTEMPTS && ATTEMPT_HONEST < ATTEMPTS);
const _: () = assert!(ATTEMPTS == 3);

// =========================================================================================== The
// report protocol. `c_confiner` and `c_shim` hold a WRITE view of one report endpoint; the kernel
// test is the receiver. Same shape as suptree.rs's, and mirrored in kernel/src/user.rs.
// ===========================================================================================

/// The C component's shell reached the C call. `w1` = attempt. Sent **before** the call, because two
/// of the three attempts never return from it.
pub const RPT_RAN: u64 = 1;
/// The confiner's supervision endpoint delivered a death. `w1` = the kernel-stamped tid, `w2` = the
/// event (`EVENT_FAULT` or `EVENT_EXIT`).
pub const RPT_DEATH: u64 = 2;
/// Where the death happened, as the kernel reported it. `w1` = the faulting pc, `w2` = the faulting
/// address. Carried separately from [`RPT_DEATH`] because `SEND` moves three words and this is the
/// fourth and fifth.
pub const RPT_SITE: u64 = 3;
/// The verdict on one attempt: `w1` = attempt, `w2` = a bitmap of the [`checks`] that **passed**.
pub const RPT_VERDICT: u64 = 4;
/// Something could not be built. `w1` = a stage code, so a broken run is legible instead of silent.
pub const RPT_FAILED: u64 = 9;

/// The bits in [`RPT_VERDICT`]'s bitmap. Each one is a separate claim, checked separately, so a
/// failure names which part of the confinement story broke.
pub mod checks {
    /// The C component's *legal* store, inside the grant, landed. Without this the other bits are
    /// worthless: a process whose stores never work would pass a witness check trivially.
    pub const IN_GRANT_WRITE_LANDED: u64 = 1 << 0;
    /// Every byte of the read-only witness page still holds its pattern, read through the checker's
    /// own read/write view of the same physical frame.
    pub const WITNESS_RO_INTACT: u64 = 1 << 1;
    /// Every byte of the unmapped-in-the-component witness page still holds its pattern.
    pub const WITNESS_FAR_INTACT: u64 = 1 << 2;
    /// The address the kernel reported as the fault site is the address the C code computed. Proves
    /// the fault is *this* bug and not some unrelated crash on the way to it.
    pub const FAULT_ADDR_AS_EXPECTED: u64 = 1 << 3;
    /// The honest attempt's output is in the grant and correct: the checksum the C returned matches
    /// an independent Rust computation, and the transformed string is byte-for-byte right.
    pub const OUTPUT_CORRECT: u64 = 1 << 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C source, read at compile time from the other side of the seam.
    ///
    /// **This is the test the seam never had.** `user/c/c_seam.c` and this crate state the same
    /// constants twice, by hand, because a C compiler cannot see Rust and the shared page has to
    /// mean the same thing to both. Nothing checked that until now: as a `#[path]` module inside a
    /// `no_std` binary this file was unreachable by host tests, so a drift here surfaced as the C
    /// component writing to the wrong offset, arbitrarily far from the edit that caused it.
    ///
    /// `include_str!` is what makes it cheap: the C is a build input to the test, so the two cannot
    /// be edited apart without this failing.
    const C_SOURCE: &str = include_str!("../../../user/c/c_seam.c");

    /// Pull `#define NAME <digits>` out of the C, in decimal or hex.
    fn c_define(name: &str) -> u64 {
        let needle = alloc_line(name);
        let line = C_SOURCE
            .lines()
            .find(|l| l.trim_start().starts_with(&needle))
            .unwrap_or_else(|| panic!("{name} is not defined in user/c/c_seam.c"));
        let rest = line[line.find(&needle).unwrap() + needle.len()..].trim();
        let tok: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == 'x' || *c == 'X')
            .collect();
        let tok = tok.trim_end_matches(['u', 'U']);
        if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16).expect("hex literal")
        } else {
            tok.parse().expect("decimal literal")
        }
    }

    fn alloc_line(name: &str) -> String {
        format!("#define {name} ")
    }

    /// **Both sides of the seam agree, constant for constant.**
    #[test]
    fn the_c_side_states_the_same_offsets() {
        assert_eq!(c_define("C_SEAM_IN_OFF"), IN_OFF as u64, "C_SEAM_IN_OFF");
        assert_eq!(c_define("C_SEAM_OUT_OFF"), OUT_OFF as u64, "C_SEAM_OUT_OFF");
        assert_eq!(c_define("C_SEAM_MARK"), MARK as u64, "C_SEAM_MARK");
    }

    /// The wild-store skip has to land outside the two mapped pages, which is the whole of what
    /// `WITNESS_FAR_VA` proves. A C-side edit that shrank it to 4095 would put the store back
    /// inside `WITNESS_RO`, and the test that "a wild store changed nothing" would then be
    /// measuring the wrong page while still passing.
    #[test]
    fn the_wild_store_skips_a_whole_page() {
        assert_eq!(c_define("C_SEAM_WILD_SKIP"), PAGE, "C_SEAM_WILD_SKIP");
        assert_eq!(WITNESS_FAR_VA - WITNESS_RO_VA, PAGE);
    }

    /// The two halves of the shared page do not overlap: `OUT_OFF` starts after `IN_OFF` plus
    /// everything the input can occupy, and both fit in one frame.
    #[test]
    fn the_in_and_out_halves_do_not_overlap() {
        assert!(
            IN_OFF + INPUT.len() <= OUT_OFF,
            "input runs into the output half"
        );
        assert!(OUT_OFF < PAGE as usize, "output half starts past the page");
    }

    /// **The two witness patterns are different sequences, but they are not different everywhere,
    /// and the exact number matters.**
    ///
    /// `pattern_ro(i) = 31i + 7` and `pattern_far(i) = 17i + 3`, both mod 256. Setting them equal
    /// gives `14i = -4 (mod 256)`, which reduces to `i = 18 (mod 128)`: they agree at exactly 32 of
    /// a page's 4096 offsets, and nowhere else.
    ///
    /// This test exists because an earlier version of it asserted they never collide, which is
    /// false, and the arithmetic corrected the assertion. Pinning the real number is worth more
    /// than the wrong absolute: it says a page filled with the *other* pattern differs in 4064
    /// places, so a checker comparing whole pages cannot miss a swap, while recording that a
    /// single-byte comparison at a chosen offset could.
    #[test]
    fn the_witness_patterns_collide_exactly_32_times_in_a_page() {
        let collisions = (0..PAGE as usize)
            .filter(|&i| pattern_ro(i) == pattern_far(i))
            .count();
        assert_eq!(collisions, 32, "the pattern arithmetic changed");
        assert!(
            (0..PAGE as usize)
                .filter(|&i| pattern_ro(i) == pattern_far(i))
                .all(|i| i % 128 == 18),
            "collisions are not where the arithmetic says they are",
        );
    }

    /// Both patterns must actually vary. A constant one would be satisfied by any uniform scribble.
    #[test]
    fn the_witness_patterns_are_not_constant() {
        let ro_first = pattern_ro(0);
        let far_first = pattern_far(0);
        assert!((1..256).any(|i| pattern_ro(i) != ro_first));
        assert!((1..256).any(|i| pattern_far(i) != far_first));
    }

    /// The checksum folds case, which is what the C component is asked to demonstrate: it upcases
    /// its input in place and the Rust side predicts the answer without running it.
    #[test]
    fn the_checksum_is_case_insensitive() {
        assert_eq!(expected_checksum(b"nife"), expected_checksum(b"NIFE"));
        assert_eq!(expected_checksum(b"NiFe"), expected_checksum(b"nife"));
    }

    /// ...and still distinguishes different bytes, or case-folding would have been achieved by
    /// ignoring the input.
    #[test]
    fn the_checksum_separates_different_inputs() {
        assert_ne!(expected_checksum(b"nife"), expected_checksum(b"nifd"));
        assert_ne!(expected_checksum(b""), expected_checksum(b"a"));
    }

    /// The real input the C is handed is NUL-terminated, because C reads it with string functions.
    #[test]
    fn the_input_is_nul_terminated() {
        assert_eq!(
            INPUT.last(),
            Some(&0),
            "the C side reads INPUT as a C string"
        );
        assert!(
            INPUT.len() < OUT_OFF,
            "input does not reach the output half"
        );
    }

    /// Report codes are distinct. Two sharing a value would make a verdict unreadable.
    #[test]
    fn report_codes_are_distinct() {
        let codes = [RPT_RAN, RPT_DEATH, RPT_SITE, RPT_VERDICT, RPT_FAILED];
        for (i, a) in codes.iter().enumerate() {
            for b in &codes[i + 1..] {
                assert_ne!(a, b, "two report codes share a value");
            }
        }
    }

    /// The verdict bits are a wire format: the checker ORs them into `RPT_VERDICT`'s payload and
    /// the report reader ANDs them back out, at runtime, across a process boundary. Each must be a
    /// distinct single bit and the exact values are load-bearing. Milestone 85's mutation run
    /// showed nothing pinned them: `1 << 1` could become `1 >> 1`, which is zero, and that claim
    /// would silently vanish from every verdict.
    #[test]
    fn the_verdict_bits_are_distinct_single_bits() {
        use checks::{
            FAULT_ADDR_AS_EXPECTED, IN_GRANT_WRITE_LANDED, OUTPUT_CORRECT, WITNESS_FAR_INTACT,
            WITNESS_RO_INTACT,
        };
        assert_eq!(
            [
                IN_GRANT_WRITE_LANDED,
                WITNESS_RO_INTACT,
                WITNESS_FAR_INTACT,
                FAULT_ADDR_AS_EXPECTED,
                OUTPUT_CORRECT
            ],
            [1, 2, 4, 8, 16]
        );
    }
}
