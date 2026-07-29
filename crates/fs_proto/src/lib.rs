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
/// server to one directory node, and every name in [`OPEN`] is resolved relative to it. There is no
/// global namespace and no absolute path; a client with no such endpoint can open nothing, and the
/// refusal is "no such capability" (it holds no endpoint), never a permission check. A [`OPEN`]
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

    /// The largest length or offset that fits the packing below (40 bits). Far above [`super::PAGE`],
    /// so a single request never carries more than one page of payload regardless; the bound only
    /// guards the bit-packing.
    pub const MAX_LEN: u64 = (1 << 40) - 1;
    /// The largest handle the packing can carry (16 bits). The server's table is far smaller.
    pub const MAX_HANDLE: u64 = 0xffff;

    /// Pack a file request's first word: opcode (bits 63:56), handle (bits 55:40), length (bits
    /// 39:0). [`OPEN`] passes handle 0 and length = the name length; [`READ`]/[`WRITE`] pass the
    /// handle and the byte count; [`CLOSE`]/[`FSTAT`] pass the handle and length 0.
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
    fn errors_are_negated_errnos_and_invert() {
        // The boundary convention: -errno on the wire, invertible, and successes read as no error.
        assert_eq!(reply_err(2), -2); // ENOENT
        assert_eq!(reply_err(5), -5); // EIO
        assert_eq!(reply_errno(reply_err(9)), Some(9)); // EBADF round trips
        assert_eq!(reply_errno(0), None);
        assert_eq!(reply_errno(4096), None); // a byte count is a success, not an error
    }

    #[test]
    fn a_filesystem_block_is_eight_sectors() {
        assert_eq!(blk::BLOCK_SIZE, PAGE);
        assert_eq!(blk::SECTORS_PER_BLOCK, 8);
    }
}
