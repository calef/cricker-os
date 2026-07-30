//! **The milestone-32 filesystem wire protocols** (notes/fs-server.md).
//!
//! Two userspace protocols, one crate, so the four programs that speak them, the block server, the
//! FS server, the client, and the kernel-side tests, share one definition and cannot drift. The
//! kernel routes these words the way it routes any IPC (§10, §12): it never reads an opcode. Adding
//! one is a change to this crate and the note, not to the syscall surface. This is the same split
//! `linedisc::proto` makes for the terminal contract.
//!
//! # The two protocols
//!
//! ```text
//!   disk ──virtio──►┌──────────────┐──blk IPC──►┌───────────┐──file IPC──► client
//!                   │ block server │            │ FS server │
//!                   └──────────────┘◄───────────└───────────┘◄──────────── (a granted
//!                        (owns the DMA            (owns RedoxFS +            directory cap)
//!                         confinement)             its own heap)
//! ```
//!
//! Nobody names anyone else (endpoint-only naming, notes/ipc-naming.md). The FS server holds "an
//! endpoint I read/write blocks on"; the client holds "an endpoint that opens and reads files under
//! the one directory this endpoint is bound to." Rewire the endpoints and neither side can tell.
//!
//! ## Control by message, bulk by shared page (DECISIONS §10)
//!
//! Both protocols are an endpoint `CALL`: the client sends two words and blocks until the server
//! replies through the one-shot Reply capability the kernel mints. Bulk bytes never ride in the
//! words. They travel in a page the two parties share, exactly [`PAGE`] bytes, one per channel. For
//! blk IPC the page holds one filesystem block; for file IPC it holds a name (on open) or file data
//! (on read/write).
//!
//! ## The error boundary
//!
//! Every reply's first word is an [`i64`]: **non-negative is a result, negative is an error**, and
//! the error value is exactly the negated errno ([`reply_err`]). RedoxFS's error type is
//! `syscall::error::Error { errno: i32 }` (the Linux numbers), so the FS server maps it to the wire
//! with `-(err.errno as i64)` at the serve loop and nowhere else, which is the "map the error type
//! once, at the server boundary" rule the roadmap sets. The client reads it back with [`reply_errno`].

#![no_std]

/// The shared-page size, in bytes. One RedoxFS block (`redoxfs::BLOCK_SIZE`) and one host page, so a
/// block move is a page move and the frame the kernel maps into both address spaces holds exactly
/// one unit of transfer. A read or write larger than this is chunked by the caller.
pub const PAGE: usize = 4096;

/// Where a request packs its opcode: bits 63:56 of the first `CALL` word, the same position
/// `linedisc::proto` uses, so the two contracts read alike.
pub const OP_SHIFT: u32 = 56;

/// Build a request's first word from just an opcode (the block protocol's shape: the operand, a
/// block index, rides in the second word).
pub const fn req(op: u64) -> u64 {
    op << OP_SHIFT
}

/// The opcode of any request word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// Turn a redox/POSIX errno into a reply's first word: the negated number, so the client can invert
/// it. This is the ONE place the error convention is defined; the FS server calls it at its boundary.
pub const fn reply_err(errno: i32) -> i64 {
    -(errno as i64)
}

/// The errno behind an error reply, or `None` if the reply is a (non-negative) success. The inverse
/// of [`reply_err`], for a client turning a wire error back into an `ErrorKind`.
pub const fn reply_errno(r0: i64) -> Option<i32> {
    if r0 < 0 { Some((-r0) as i32) } else { None }
}

/// **The block-IPC protocol** (FS server → block server). Three synchronous methods shaped exactly
/// like the RedoxFS `Disk` trait the FS server implements over it: read a block, write a block, ask
/// the size. The unit of transfer is one filesystem block ([`PAGE`] bytes); the block server splits
/// each into the eight 512-byte virtio sectors the device actually moves.
pub mod blk {
    use super::PAGE;

    /// Read filesystem block `w1` from the disk into the shared page. Reply `r0` = 0 on success, or
    /// [`super::reply_err`] on failure; the [`PAGE`] bytes land in the shared page.
    pub const READ: u64 = 1;
    /// Write the [`PAGE`] bytes now in the shared page to filesystem block `w1`. Reply `r0` = 0 on
    /// success, or [`super::reply_err`] on failure.
    pub const WRITE: u64 = 2;
    /// Ask the disk's size in bytes. `w1` ignored. Reply `r0` = the size (always non-negative here).
    pub const SIZE: u64 = 3;

    /// One filesystem block, in bytes: the transfer unit and the shared-page size. Equal to
    /// `redoxfs::BLOCK_SIZE`; asserted against it in the block server so a future RedoxFS bump that
    /// changed the block size would fail to build rather than silently corrupt.
    pub const BLOCK_SIZE: usize = PAGE;

    /// The number of 512-byte virtio sectors in one filesystem block (`BLOCK_SIZE / 512 = 8`). The
    /// block server issues this many sector transfers per blk request.
    pub const SECTORS_PER_BLOCK: u64 = (BLOCK_SIZE as u64) / 512;
}

/// **The file-service protocol** (client → FS server). The contract is capability-shaped from birth
/// (notes/fs-server.md): the endpoint a client holds IS the directory capability, bound in the
/// server to one directory node, and every name in [`fs::OPEN`] is resolved relative to it. There is no
/// global namespace and no absolute path; a client with no such endpoint can open nothing, and the
/// refusal is "no such capability" (it holds no endpoint), never a permission check. An [`fs::OPEN`]
/// hands back a **handle**, a small integer the server issues and validates against this session's
/// table; a handle is likewise a capability, meaningless to forge because the server only honors the
/// ones it minted.
pub mod fs {
    /// Resolve the name in the shared page (its length is [`req_len`] of the request word) under the
    /// endpoint's bound directory. Second word 0. Reply `r0` = a handle (≥ 0) or an error.
    pub const OPEN: u64 = 1;
    /// Read up to [`req_len`] bytes from the handle ([`req_handle`]) at offset `w1` into the shared
    /// page. Reply `r0` = bytes read (≥ 0, 0 at EOF) into the shared page, or an error.
    pub const READ: u64 = 2;
    /// Write [`req_len`] bytes from the shared page to the handle at offset `w1`. Reply `r0` = bytes
    /// written (≥ 0), or an error.
    pub const WRITE: u64 = 3;
    /// Close the handle ([`req_handle`]). Reply `r0` = 0, or an error if the handle was not open.
    pub const CLOSE: u64 = 4;
    /// The current size in bytes of the handle's file. Reply `r0` = size (≥ 0), or an error.
    pub const FSTAT: u64 = 5;
    /// Create the name in the shared page (length is [`req_len`]) under the endpoint's bound
    /// directory and open it. Second word 0. Reply `r0` = a handle (≥ 0), or an error.
    ///
    /// **`EEXIST` if the name already exists, and nothing is modified.** Create is create, not
    /// create-or-open: a caller that wants either must ask for both and say which it got. The
    /// alternative makes a partly-working write read as a working one, which is the failure §27
    /// records. Shares [`OPEN`]'s shape exactly, so a client that can open can create.
    pub const CREATE: u64 = 6;
    /// Set the size of the handle's ([`req_handle`]) file to exactly `w1` bytes. [`req_len`] is 0;
    /// the size rides in the second word because it is an offset-shaped quantity, not a payload
    /// length. Reply `r0` = 0, or an error.
    ///
    /// POSIX `ftruncate` in **both** directions: shrinking discards the bytes past the new size,
    /// growing extends with zeroes. The shrink is the point. Without it a write shorter than the file
    /// leaves the old tail in place, so a caller replacing a file's contents gets a longer file than
    /// it wrote, and a write that half-works reads as a write that failed (DECISIONS §27, four times
    /// corrected). Truncating to the current size is a no-op, which matters because `std::fs::write`
    /// truncates unconditionally.
    pub const TRUNCATE: u64 = 7;

    /// The largest length or offset that fits the packing below (40 bits). Far above [`super::PAGE`],
    /// so a single request never carries more than one page of payload regardless; the bound only
    /// guards the bit-packing.
    pub const MAX_LEN: u64 = (1 << 40) - 1;
    /// The largest handle the packing can carry (16 bits). The server's table is far smaller.
    pub const MAX_HANDLE: u64 = 0xffff;

    /// Pack a file request's first word: opcode (bits 63:56), handle (bits 55:40), length (bits
    /// 39:0). [`OPEN`] and [`CREATE`] pass handle 0 and length = the name length;
    /// [`READ`]/[`WRITE`] pass the handle and the byte count; [`CLOSE`]/[`FSTAT`]/[`TRUNCATE`] pass
    /// the handle and length 0 ([`TRUNCATE`]'s new size rides in the second word).
    pub const fn req(op: u64, handle: u64, len: u64) -> u64 {
        (op << super::OP_SHIFT) | ((handle & MAX_HANDLE) << 40) | (len & MAX_LEN)
    }
    /// The handle of a file request word.
    pub const fn req_handle(w0: u64) -> u64 {
        (w0 >> 40) & MAX_HANDLE
    }
    /// The length/count of a file request word.
    pub const fn req_len(w0: u64) -> usize {
        (w0 & MAX_LEN) as usize
    }
}

/// **The per-file grant** (milestone 31 phase 2): what it means to hold one file rather than a
/// directory, and how the narrowing travels.
///
/// A directory capability lets its holder name anything in the bound directory. `run wc report.txt`
/// must hand over less than that: **one file, in one direction, and nothing else**. The narrowing is
/// done by an attenuator, `user/src/fwarden.rs`, which holds the directory capability, opens exactly
/// the granted name once at startup, and then serves the *same* [`fs`] protocol on its own endpoint
/// with three rules:
///
/// 1. **[`fs::OPEN`] answers only the granted name.** Any other name is `ENOENT`, because in this
///    scope there is no such name; nothing consulted a permission. The holder cannot enumerate, and
///    cannot discover what else exists.
/// 2. **[`fs::CREATE`] is `ENOTDIR`.** A file capability is not a directory, so "create a name in it"
///    is not a request that means anything, which is a better answer than a permission refusal.
/// 3. **[`fs::WRITE`] and [`fs::TRUNCATE`] are [`grant::EROFS`] without [`grant::WRITE`].** The
///    capability is read-only; there is no policy to consult and no way to widen it from inside.
///
/// The attenuator pattern is Mark Miller's caretaker, and putting it in a separate process is what
/// makes the claim checkable: the confined program holds an endpoint to the warden and nothing that
/// names the FS server, so it cannot route around the narrowing even in principle. Open-by-path
/// still exists only inside a server (DECISIONS §27); the warden just serves a server whose entire
/// namespace is one name.
pub mod grant {
    /// The granted directions, packed into the warden's spec word. Read alone is the common case
    /// (`run wc report.txt`); write implies read, since a writer that cannot read back is a shape
    /// nothing has asked for.
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;

    /// The longest granted name, in bytes. The name rides in **two `START` argument words** rather
    /// than a frame, so a per-file grant costs no extra page and no extra mapping, and the warden
    /// needs nothing mapped before it runs. Sixteen bytes is short, and deliberately so: it is a
    /// demonstrator's limit, not a filesystem's, and lifting it means giving the warden a frame,
    /// which is a change to the wiring and not to this contract.
    pub const MAX_NAME: usize = 16;

    /// `EROFS`, the reply to a write through a read-only grant. Chosen over `EACCES` on purpose:
    /// `EACCES` is the Unix answer, "you were denied", which implies a policy that could have said
    /// yes. There is no policy here. The capability carries one direction, so the honest statement
    /// is about the thing itself, and `std::io::ErrorKind::ReadOnlyFilesystem` is what a caller sees.
    pub const EROFS: i32 = 30;

    /// `ENOTDIR`, the reply to a [`super::fs::CREATE`] through a file grant. "This is a file, not a
    /// directory" is a fact about what the holder has, not a refusal of what it asked.
    pub const ENOTDIR: i32 = 20;

    /// The handle the warden mints for the one file it serves. Fixed, because there is exactly one:
    /// a holder that guesses a different number gets `EBADF` from the same check every other handle
    /// goes through.
    pub const HANDLE: u64 = 0;

    /// Pack a granted name into the two argument words the warden is started with. Names shorter than
    /// [`MAX_NAME`] are zero-padded; longer ones are refused by the caller (see [`fits`]).
    pub const fn pack_name(name: &[u8]) -> (u64, u64) {
        let mut lo = [0u8; 8];
        let mut hi = [0u8; 8];
        let mut i = 0;
        while i < name.len() && i < 8 {
            lo[i] = name[i];
            i += 1;
        }
        while i < name.len() && i < MAX_NAME {
            hi[i - 8] = name[i];
            i += 1;
        }
        (u64::from_le_bytes(lo), u64::from_le_bytes(hi))
    }

    /// Unpack a granted name into `buf` (at least [`MAX_NAME`] bytes) and return its length.
    pub fn unpack_name(lo: u64, hi: u64, len: usize, buf: &mut [u8; MAX_NAME]) -> usize {
        buf[..8].copy_from_slice(&lo.to_le_bytes());
        buf[8..].copy_from_slice(&hi.to_le_bytes());
        len.min(MAX_NAME)
    }

    /// Whether a name can travel as a per-file grant at all.
    pub const fn fits(name: &[u8]) -> bool {
        !name.is_empty() && name.len() <= MAX_NAME
    }

    /// Pack the name length and the granted rights into the warden's third argument word.
    pub const fn spec(len: usize, rights: u64) -> u64 {
        ((len as u64) & 0xff) | (rights << 8)
    }

    /// The granted name's length from a spec word.
    pub const fn spec_len(w: u64) -> usize {
        (w & 0xff) as usize
    }

    /// The granted rights from a spec word.
    pub const fn spec_rights(w: u64) -> u64 {
        w >> 8
    }

    /// Whether a spec word grants writing.
    pub const fn writable(w: u64) -> bool {
        spec_rights(w) & WRITE != 0
    }
}

/// The phase-2 end-to-end test fixture, in one place so the three programs that touch it agree: the
/// host build (`cargo xtask`, which writes these into the RedoxFS image with the host tool), the
/// client (which reads and writes them through the FS server), and the kernel test (which asserts
/// the client's report). Not part of either wire protocol; a shared constant, like a test vector.
pub mod fixture {
    /// A file the image ships with; the client reads it back through a granted directory capability.
    pub const MOTD_NAME: &str = "motd";
    /// Its exact contents. Longer than eight bytes so the report's head word is a real prefix.
    pub const MOTD: &[u8] =
        b"redoxfs served this file to an EL0 client through a capability handle\n";

    /// A file the image ships with (with placeholder contents) so the client can open it and write.
    pub const SCRATCH_NAME: &str = "scratch";
    /// Placeholder contents the host tool writes; overwritten by the client's write test.
    pub const SCRATCH_INIT: &[u8] = b"(placeholder overwritten by the fs-server write test)";
    /// What the client writes to `scratch` and reads back; the host tool re-reads it after the run
    /// to prove the write reached the on-disk image and the filesystem is still consistent.
    pub const WRITE_PATTERN: &[u8] =
        b"CRKFS_WRITE_OK: this round-tripped through the RedoxFS image\n";

    /// The client's success sentinel, sent as the report's second word; the head of [`MOTD`] is the
    /// first. Any other value (or silence) fails the test.
    pub const SUCCESS: u64 = 0xF11E_600D;

    /// The FS server's readiness sentinel: sent once, after it has opened the RedoxFS image over blk
    /// IPC and before it serves clients. The test waits for it, so a hang in `open` (the blk path)
    /// is distinguishable from a hang in the serve/client path, and a booted-but-empty run is caught.
    pub const READY: u64 = 0xF5_0BEEF5;

    /// **The attacker's report: a bitmap of what got through**, not a pass/fail. Each bit says one
    /// specific thing happened, so the test asserts an *expected set* rather than "zero", and a
    /// failure names itself. That shape is what lets one attacker serve as its own negative control:
    /// run against a read-only grant every bit must be clear, and run against a read/write grant of
    /// the same shape the two write bits must be **set**, which is what proves the read-only
    /// refusals were a narrowed capability rather than a warden that refuses everything.
    pub mod escape {
        /// It opened a file the grant does not designate. Never allowed.
        pub const SECOND_FILE: u64 = 1 << 0;
        /// Its write to the granted file was accepted. Expected only with `grant::WRITE`.
        pub const WROTE: u64 = 1 << 1;
        /// Its truncate of the granted file was accepted. Expected only with `grant::WRITE`; a
        /// separate bit because a truncate carries no bytes, so a guard that only covered `WRITE`
        /// would leave a way to destroy a file just as thoroughly.
        pub const TRUNCATED: u64 = 1 << 2;
        /// It created a file through a file capability. Never allowed: a file is not a directory.
        pub const CREATED: u64 = 1 << 3;
        /// It reached a file with a handle it was never given. Never allowed.
        pub const FORGED_HANDLE: u64 = 1 << 4;
        /// The thing it *should* be able to do failed, so nothing above was actually proven. A
        /// capability that reaches nothing is trivially unescapable, and a test that only checked
        /// the refusals would pass against one.
        pub const GRANTED_READ_FAILED: u64 = 1 << 5;
    }

    /// The attacker's report leads with this so a silent client (a trapped one) cannot be mistaken
    /// for a clean verdict of zero.
    pub const VERDICT: u64 = 0xE5_CA9E00;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_request_roundtrips() {
        let w0 = req(blk::READ);
        assert_eq!(op(w0), blk::READ);
        // The block index rides in the second word untouched, so any u64 survives.
        for block in [0u64, 1, 42, u64::MAX] {
            let (rw0, rw1) = (req(blk::WRITE), block);
            assert_eq!(op(rw0), blk::WRITE);
            assert_eq!(rw1, block);
        }
    }

    #[test]
    fn file_request_packs_and_unpacks() {
        // Every field survives the pack, at its extremes, without bleeding into another.
        for &(o, h, l) in &[
            (fs::OPEN, 0u64, 7u64),
            (fs::READ, fs::MAX_HANDLE, fs::MAX_LEN),
            (fs::WRITE, 1, 4096),
            (fs::CLOSE, 65535, 0),
            (fs::FSTAT, 3, 0),
            (fs::CREATE, 0, 11),
            (fs::TRUNCATE, 42, 0),
        ] {
            let w = fs::req(o, h, l);
            assert_eq!(op(w), o, "opcode");
            assert_eq!(fs::req_handle(w), h, "handle");
            assert_eq!(fs::req_len(w) as u64, l, "len");
        }
    }

    #[test]
    fn a_handle_never_collides_with_the_opcode_or_length() {
        // A big handle must not spill into the opcode field, and a max length must not spill into
        // the handle: the READ path packs all three and reads them back independently.
        let w = fs::req(fs::READ, fs::MAX_HANDLE, fs::MAX_LEN);
        assert_eq!(op(w), fs::READ);
        assert_eq!(fs::req_handle(w), fs::MAX_HANDLE);
        assert_eq!(fs::req_len(w) as u64, fs::MAX_LEN);
    }

    #[test]
    fn every_opcode_is_distinct_and_fits_its_field() {
        // Two verbs sharing a number is a wire bug the packing tests cannot see: each would pack and
        // unpack perfectly and mean the wrong thing. Cheap to assert, so assert it.
        let ops = [
            ("OPEN", fs::OPEN),
            ("READ", fs::READ),
            ("WRITE", fs::WRITE),
            ("CLOSE", fs::CLOSE),
            ("FSTAT", fs::FSTAT),
            ("CREATE", fs::CREATE),
            ("TRUNCATE", fs::TRUNCATE),
        ];
        for (i, (na, a)) in ops.iter().enumerate() {
            assert!(*a <= 0xff, "{na} does not fit the 8-bit opcode field");
            assert_ne!(
                *a, 0,
                "0 is not a verb: a zeroed word must not decode as one"
            );
            for (nb, b) in &ops[i + 1..] {
                assert_ne!(a, b, "{na} and {nb} share an opcode");
            }
        }
        // The blk protocol is a separate namespace on a separate endpoint, so its numbers may and do
        // overlap. Asserted so nobody "fixes" the overlap and renumbers a live wire.
        assert_eq!(
            blk::READ,
            fs::OPEN,
            "the two protocols are deliberately independent"
        );
    }

    #[test]
    fn errors_are_negated_errnos_and_invert() {
        // The boundary convention: -errno on the wire, invertible, and successes read as no error.
        assert_eq!(reply_err(2), -2); // ENOENT
        assert_eq!(reply_err(5), -5); // EIO
        assert_eq!(reply_errno(reply_err(9)), Some(9)); // EBADF round trips
        assert_eq!(reply_errno(0), None);
        assert_eq!(reply_errno(4096), None); // a byte count is a success, not an error
    }

    #[test]
    fn a_granted_name_survives_the_two_argument_words() {
        // The name rides in START arguments rather than a frame, so a per-file grant costs no page.
        // Every length up to the limit has to come back exactly, including the 8-byte boundary where
        // it splits across the two words.
        for name in [
            &b"a"[..],
            b"motd",
            b"scratch",
            b"12345678",
            b"123456789",
            b"sixteen-bytes!!!",
        ] {
            assert!(grant::fits(name), "{name:?} should fit");
            let (lo, hi) = grant::pack_name(name);
            let mut buf = [0u8; grant::MAX_NAME];
            let n = grant::unpack_name(lo, hi, name.len(), &mut buf);
            assert_eq!(&buf[..n], name, "{name:?} did not survive the round trip");
        }
        assert!(!grant::fits(b""), "an empty name designates nothing");
        assert!(
            !grant::fits(b"seventeen-bytes!!"),
            "a name past the limit must be refused where it is packed, not truncated silently",
        );
    }

    #[test]
    fn a_grant_spec_carries_its_length_and_its_direction_apart() {
        // A read grant and a write grant of the same name differ only here, so the two fields must
        // not bleed: a 16-byte name must not look like a write bit, and write must not lengthen it.
        let ro = grant::spec(16, grant::READ);
        let rw = grant::spec(16, grant::READ | grant::WRITE);
        assert_eq!(grant::spec_len(ro), 16);
        assert_eq!(grant::spec_len(rw), 16);
        assert!(!grant::writable(ro));
        assert!(grant::writable(rw));
        // And a zero-rights spec grants nothing, rather than defaulting to something.
        assert!(!grant::writable(grant::spec(4, 0)));
    }

    #[test]
    fn the_escape_bits_are_distinct() {
        // The attacker reports a bitmap and the test asserts an expected set, so two outcomes
        // sharing a bit would hide one of them and make a wrong verdict read as a right one.
        use fixture::escape::*;
        let bits = [
            SECOND_FILE,
            WROTE,
            TRUNCATED,
            CREATED,
            FORGED_HANDLE,
            GRANTED_READ_FAILED,
        ];
        let mut seen = 0u64;
        for b in bits {
            assert_ne!(b, 0, "zero is the pass; it cannot also be a breach");
            assert_eq!(seen & b, 0, "two escapes share a bit");
            seen |= b;
        }
    }

    #[test]
    fn a_filesystem_block_is_eight_sectors() {
        assert_eq!(blk::BLOCK_SIZE, PAGE);
        assert_eq!(blk::SECTORS_PER_BLOCK, 8);
    }
}
