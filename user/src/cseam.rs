//! **The C seam's shared half** (milestone 36, DECISIONS §30).
//!
//! Two Rust programs sit either side of the foreign component: `cshim` is the shell that links the C
//! and calls it, and `cwarden` is the process that builds `cshim`, supervises it, and holds the
//! witness pages that prove what the C could not reach. This is what they agree on: the address-space
//! layout, the report protocol, and the constants that are also written down in `user/c/cseam.c`.
//!
//! Compiled into both binaries with `#[path = "cseam.rs"] mod cseam;`, the same way the supervision
//! tree shares `suptree.rs`.
//!
//! # The layout, and why these numbers
//!
//! ```text
//!   cshim (the C component's process)          cwarden (the checker)
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

/// The page size, which is the grant's size too. One page is enough: the seam is what is under test,
/// not throughput.
pub const PAGE: u64 = 4096;

/// Where the shared grant is mapped, in **both** address spaces at the same virtual address. Same
/// number on both sides on purpose, so `cseam_wild`'s target address is one the checker can name
/// without translating anything.
pub const GRANT_VA: u64 = 0x5000_0000;

/// The read-only witness: the page immediately after the grant. Mapped read-only into the C
/// component (which is what makes an off-by-one a permission fault) and read/write into the checker.
pub const WITNESS_RO_VA: u64 = GRANT_VA + PAGE;

/// The unmapped witness: two pages after the grant. **Not mapped into the C component at all.** The
/// checker maps a different frame here, at this same virtual address.
pub const WITNESS_FAR_VA: u64 = GRANT_VA + 2 * PAGE;

// ===========================================================================================
// The grant's contents. Mirrors the `CSEAM_*` defines in user/c/cseam.c; a C ABI has no way to
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
pub const INPUT: &[u8] = b"cricker-os foreign component\0";

/// The position-derived witness patterns. Different generators for the two pages so a checker bug
/// that read the wrong page would not accidentally agree.
pub fn pattern_ro(i: usize) -> u8 {
    (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8
}
pub fn pattern_far(i: usize) -> u8 {
    (i.wrapping_mul(17).wrapping_add(3) & 0xff) as u8
}

/// FNV-1a over `bytes`, uppercased the way `cseam_transform` uppercases. The Rust recomputation of
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
// What each attempt asks the C to do. Passed to `cshim` in its second argument register, which is
// also its attempt number: attempt 0 overruns, attempt 1 goes wild, attempt 2 does the real work.
// The order matters, because "the supervisor restarts it and the restarted component works" is only
// proven if the honest run comes *after* the crashes.
// ===========================================================================================

pub const ATTEMPT_OVERRUN: u64 = 0;
pub const ATTEMPT_WILD: u64 = 1;
pub const ATTEMPT_HONEST: u64 = 2;
/// How many attempts a full run makes.
pub const ATTEMPTS: u64 = 3;

// ===========================================================================================
// The report protocol. `cwarden` and `cshim` hold a WRITE view of one report endpoint; the kernel
// test is the receiver. Same shape as suptree.rs's, and mirrored in kernel/src/user.rs.
// ===========================================================================================

/// The C component's shell reached the C call. `w1` = attempt. Sent **before** the call, because two
/// of the three attempts never return from it.
pub const RPT_RAN: u64 = 1;
/// The warden's supervision endpoint delivered a death. `w1` = the kernel-stamped tid, `w2` = the
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
