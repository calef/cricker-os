//! `std::fs` for cricker-os (milestone 27 phase two): `File` bound to the FS-service contract
//! (DECISIONS §27, notes/fs-server.md, `crates/fs_proto`).
//!
//! # The interesting part: there is no global namespace
//!
//! `File::open` takes a path, and this system has no filesystem root to resolve one against. Per
//! §27, open-by-path exists **only inside the FS server**, resolved relative to the one directory
//! node the client's endpoint is bound to. So the honest mapping is:
//!
//! > a std program holds a **directory capability** (the FS-service endpoint, slot 4 of the std
//! > slot convention in `pal/cricker/rt.rs`), and `File::open("foo")` means *"foo, under the
//! > directory I was granted"*, not *"foo somewhere in a global filesystem"*.
//!
//! Three consequences, and they are the design, not a limitation to apologise for:
//!
//! 1. **A path that tries to leave the granted directory is refused here, before the wire.** An
//!    absolute path, or any `..`, names something this process holds no capability for, and there
//!    is no namespace in which to express it. That is [`io::ErrorKind::InvalidFilename`] with a
//!    message saying so, deliberately **not** `PermissionDenied`: nothing checked a permission,
//!    and pretending otherwise would be a Unix EPERM fiction over a capability refusal.
//! 2. **A program granted no directory capability gets `Unsupported` from everything.** Not an
//!    empty filesystem, not `NotFound`: the platform cannot do it, because no capability reaches a
//!    filesystem. The same shape `std::net` uses when the `Stack` endpoint is absent.
//! 3. **A nested path is refused too, and points at the milestone that fixes it.** The contract
//!    carries one name resolved in the bound directory; a subdirectory needs its own directory
//!    capability, which is milestone 31's per-file/per-directory grant.
//!
//! # What binds, and what stays honestly Unsupported
//!
//! Bound: [`File::open`] (`OPEN`), `read` (`READ`), `write` (`WRITE`), `seek`/`tell` (an offset
//! this side keeps, since the contract's read and write are both positional), `file_attr`/`size`
//! (`FSTAT`), close on `Drop` (`CLOSE`), plus [`stat`]/[`exists`] built from open + fstat + close.
//!
//! **Also bound since milestone 31 phase 2:** `create` and `create_new` (`CREATE`) and `truncate`
//! (`TRUNCATE`), which means **`File::create` and `std::fs::write` work now** rather than being
//! Unsupported by construction. Two things this corrects. Creating a *file* was never what §27 kept
//! host-side; that was creating a *filesystem*, which needs uuid and getrandom, and
//! `Transaction::create_node` is not std-gated, so a file can be made on-device without entropy
//! becoming a userspace dependency. And the previous refusal was the right call at the time for a
//! reason worth remembering: without `TRUNCATE`, `std::fs::write` would have left a tail of the old
//! contents behind, so a write that half-worked would have read as a write that failed, which is
//! precisely the confusion DECISIONS §27 records being corrected four times in one day.
//!
//! Still Unsupported, each because no verb in the contract backs it: directory iteration,
//! `mkdir`/`unlink`/`rename`/`rmdir`, symlinks and hard links, `canonicalize`, permissions, file
//! times, locks, and `duplicate` (a handle is a token the server minted; there is no dup verb).
//!
//! See notes/std.md for the full list with reasons.

use crate::ffi::OsString;
use crate::fmt;
use crate::fs::TryLockError;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut, SeekFrom};
use crate::path::{Component, Path, PathBuf};
use crate::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use crate::sys::pal::cricker::fsproto::{self, fs as proto};
use crate::sys::pal::cricker::rt;
use crate::sys::time::SystemTime;
use crate::sys::{unsupported, unsupported_err};

// The pieces of the phase-one backend that stay exactly as honest as they were: nothing in the
// contract creates a directory, renames, links, or canonicalizes, so those keep the `unsupported`
// implementations rather than gaining a cricker-shaped copy of the same refusal. `FileTimes` comes
// from there too (the server keeps an mtime but the contract does not carry one).
#[expect(dead_code)]
#[path = "unsupported.rs"]
mod unsupported_fs;
pub use unsupported_fs::{
    Dir, DirBuilder, FileTimes, canonicalize, copy, link, readlink, remove_dir_all, rename, rmdir,
    symlink, unlink,
};

/// The FS-service endpoint: this process's entire authority over files. Naming a file over it is a
/// request the server resolves under the one directory the endpoint is bound to.
const FS: u64 = rt::FS_DIR_SLOT;

/// The page shared with the FS server, mapped by the loader alongside the grant.
const PAGE_VA: u64 = rt::FS_PAGE;

/// The contract's transfer unit: one page, so one request never moves more than this.
const PAGE: usize = fsproto::PAGE;

// --- Is a filesystem reachable at all? -------------------------------------------------------
//
// This has to be answerable WITHOUT touching the shared page, because a program that was not
// granted a directory capability has no page mapped and a probe that wrote a name into it would
// fault instead of returning an error. So the probe is a request that carries no payload: `FSTAT`
// on a handle number the server's table can never contain.
//
//   - no capability in the slot: the kernel refuses the invoke itself and the first reply word is
//     one of its own small negatives (`NoSuchSlot` = -1, `WrongObject` = -2, `NotPermitted` = -3).
//   - a real server: it answers `-EBADF` (-9) for the impossible handle, which is a *reply*, so a
//     filesystem is reachable.
//
// Cached, because the answer cannot change: a cspace slot's contents are fixed at spawn on this
// ABI (0 = not yet asked, 1 = granted, 2 = not granted).

static REACHABLE: AtomicU8 = AtomicU8::new(0);

/// True if this process holds a directory capability. See the note above for why the probe is an
/// `FSTAT` on an impossible handle rather than anything that touches the shared page.
fn reachable() -> bool {
    match REACHABLE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let (r0, _) = rt::call(FS, proto::req(proto::FSTAT, proto::MAX_HANDLE, 0), 0);
            // A payload-free probe cannot draw `ENOENT` (whose -2 collides with the kernel's
            // `WrongObject`), so all three of the kernel's refusals are unambiguous here.
            let ok = !matches!(r0 as i64, -1 | -2 | -3);
            REACHABLE.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
            ok
        }
    }
}

/// Whether a reply word is the *kernel* refusing the invoke rather than the server answering.
///
/// The kernel's own errors are -1..-8 (`abi::Error`); the server's are negated errnos. The two
/// spaces overlap, which is a wart of the contract recorded in notes/std.md. Only the two that
/// mean "you hold no such capability" are read that way, and neither is an errno the FS server
/// speaks: `EPERM` (1) and `ESRCH` (3) are not in its vocabulary, while `ENOENT` (2), which does
/// collide, is deliberately left to the errno mapping so a missing file reads as `NotFound`.
fn no_capability(r0: u64) -> bool {
    matches!(r0 as i64, -1 | -3)
}

// --- The shared page -------------------------------------------------------------------------
//
// One page for the whole process, so two concurrent operations would trample each other. A
// spinlock guards it: uncontended on this single-threaded target, correct if threads arrive, the
// same discipline the heap and the net PAL's registry use. Every request that touches the page
// holds the guard across its `CALL`, which is also what makes the reply's bytes ours to read.

static LOCKED: AtomicBool = AtomicBool::new(false);

struct Page;

impl Drop for Page {
    fn drop(&mut self) {
        LOCKED.store(false, Ordering::Release);
    }
}

/// Take the shared page. Held across the `CALL` that uses it.
fn page() -> Page {
    while LOCKED
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        crate::hint::spin_loop();
    }
    Page
}

impl Page {
    /// Put `bytes` in the page: a name to open, or data to write. Volatile because the other side
    /// of this page is another address space, not memory the compiler may reason about.
    fn put(&mut self, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            // SAFETY: PAGE_VA is a mapped, writable page of `PAGE` bytes; callers clamp to it.
            unsafe { core::ptr::write_volatile((PAGE_VA + i as u64) as *mut u8, b) };
        }
    }

    /// Take `out.len()` bytes out of the page (a completed read landed there).
    fn get(&mut self, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: as above.
            *b = unsafe { core::ptr::read_volatile((PAGE_VA + i as u64) as *const u8) };
        }
    }
}

// --- Errors, mapped by meaning ---------------------------------------------------------------

/// One request on the FS service. Returns the reply's first word as a count/handle, or an
/// `io::Error`. The wire convention is `fs_proto`'s: non-negative is a result, negative is a
/// negated errno ([`fsproto::reply_errno`]).
fn request(w0: u64, w1: u64) -> io::Result<u64> {
    let (r0, _) = rt::call(FS, w0, w1);
    match fsproto::reply_errno(r0 as i64) {
        None => Ok(r0),
        // The kernel refusing the invoke (a revoked or missing endpoint) is the same answer a
        // program with no directory capability gets: the platform cannot do this.
        Some(_) if no_capability(r0) => Err(unsupported_err()),
        Some(errno) => Err(from_errno(errno)),
    }
}

/// The server's errno into an `io::ErrorKind`, by meaning. There is no errno anywhere else in this
/// PAL: the FS service is the one place cricker-os speaks one, because the component behind it
/// (RedoxFS) does, and §27 maps it at the server boundary and nowhere deeper.
fn from_errno(errno: i32) -> io::Error {
    match errno {
        2 => io::const_error!(io::ErrorKind::NotFound, "no such name in the granted directory"),
        9 => io::const_error!(
            io::ErrorKind::InvalidInput,
            "the FS server does not honor that handle"
        ),
        21 => io::const_error!(io::ErrorKind::IsADirectory, "that name is a directory"),
        22 => io::const_error!(io::ErrorKind::InvalidInput, "the FS server refused the request"),
        28 => io::const_error!(io::ErrorKind::StorageFull, "the filesystem is full"),
        _ => io::const_error!(io::ErrorKind::Other, "the FS server reported a failure"),
    }
}

// --- Paths: one name, under the granted directory ---------------------------------------------

/// Reduce a std path to the ONE name the contract can carry, or refuse it.
///
/// This is where "no global namespace" is enforced, on the client side, before a byte reaches the
/// server. The server enforces it again (it resolves a single component in its bound directory and
/// nothing else); doing it here as well is not redundant, it is what turns a would-be escape into
/// a legible `io::Error` instead of an `ENOENT` that reads like a missing file.
///
/// Every refusal is [`io::ErrorKind::InvalidFilename`], never `PermissionDenied`: no permission was
/// consulted, and there is no name for what was asked, because this process holds no capability
/// that designates it. The message says which of the four cases it was.
fn one_name(path: &Path) -> io::Result<&str> {
    let mut name: Option<&str> = None;
    for c in path.components() {
        match c {
            // "./motd" is "motd": the current directory IS the granted one.
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(io::const_error!(
                    io::ErrorKind::InvalidFilename,
                    "an absolute path names nothing here: this process holds a directory \
                     capability, not a filesystem root"
                ));
            }
            Component::ParentDir => {
                return Err(io::const_error!(
                    io::ErrorKind::InvalidFilename,
                    "`..` would leave the granted directory, which no capability designates"
                ));
            }
            Component::Normal(part) => {
                if name.is_some() {
                    return Err(io::const_error!(
                        io::ErrorKind::InvalidFilename,
                        "a nested path needs a directory capability for the subdirectory, which \
                         this contract does not yet grant"
                    ));
                }
                name = Some(part.to_str().ok_or_else(|| {
                    io::const_error!(
                        io::ErrorKind::InvalidFilename,
                        "a file name must be UTF-8 to cross this contract"
                    )
                })?);
            }
        }
    }
    match name {
        Some(n) if n.len() <= PAGE => Ok(n),
        Some(_) => Err(io::const_error!(
            io::ErrorKind::InvalidFilename,
            "the name is longer than the page shared with the FS server"
        )),
        None => Err(io::const_error!(
            io::ErrorKind::InvalidFilename,
            "the granted directory itself is not a file this contract can open"
        )),
    }
}

/// `OPEN` a name under the granted directory, returning the server's handle.
fn open_handle(path: &Path) -> io::Result<u64> {
    if !reachable() {
        return Err(unsupported_err());
    }
    let name = one_name(path)?;
    let mut p = page();
    p.put(name.as_bytes());
    request(proto::req(proto::OPEN, 0, name.len() as u64), 0)
}

/// Create `path` under the granted directory and return the handle. [`open_handle`]'s twin: the wire
/// shape is identical, only the verb differs, which is why `CREATE` was specified to match `OPEN`.
///
/// The server answers `EEXIST` if the name is already there, and this is only ever called after an
/// open reported `NotFound`, so that reply means somebody else created it in between. It surfaces as
/// `AlreadyExists` rather than being retried, because a silent retry would turn a lost race into a
/// caller writing over a file it believes it just made.
fn create_handle(path: &Path) -> io::Result<u64> {
    if !reachable() {
        return Err(unsupported_err());
    }
    let name = one_name(path)?;
    let mut p = page();
    p.put(name.as_bytes());
    request(proto::req(proto::CREATE, 0, name.len() as u64), 0)
}

// --- File ------------------------------------------------------------------------------------

/// An open file: a handle the server minted, plus the offset std's positional API keeps on this
/// side (the contract's `READ`/`WRITE` are both explicitly positional, so there is no cursor in
/// the server to get out of step with).
///
/// The offset is an atomic rather than a `Cell` because `std::fs::File` is `Sync` on every
/// platform and this target should not be the exception; on a single-threaded target the atomic
/// costs nothing.
pub struct File {
    handle: u64,
    pos: AtomicU64,
    /// `append` mode: every write goes at the current end of file, so the position is refreshed
    /// from `FSTAT` before each one. Not atomic against another writer (no lock verb in the
    /// contract), which is the same caveat every non-POSIX backend carries.
    append: bool,
}

/// A file's metadata, as much of it as the contract carries: the size. Everything else std asks
/// for either does not exist on this service or is not on the wire.
#[derive(Clone)]
pub struct FileAttr {
    size: u64,
}

/// Directory iteration needs a verb the contract does not have, so this is uninhabited: the type
/// exists for `sys::fs`'s exports, and `readdir` refuses.
pub struct ReadDir(!);

pub struct DirEntry(!);

#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

/// Permissions do not exist on this service. `readonly` is honestly false (a granted directory
/// endpoint carries WRITE or it does not, and that is a capability, not a mode bit).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilePermissions {}

/// Every name the service resolves is a regular file: the contract cannot open a directory and
/// RedoxFS symlinks are not exposed over it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileType {}

impl FileAttr {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn perm(&self) -> FilePermissions {
        FilePermissions {}
    }

    pub fn file_type(&self) -> FileType {
        FileType {}
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        // The server keeps an mtime (it advances one on write), but no contract verb reports it.
        // There IS a wall clock to interpret one against since milestone 51 (DECISIONS §43), so the
        // missing piece is now the verb and nothing else. See notes/std.md.
        unsupported()
    }

    pub fn accessed(&self) -> io::Result<SystemTime> {
        unsupported()
    }

    pub fn created(&self) -> io::Result<SystemTime> {
        unsupported()
    }
}

impl FilePermissions {
    pub fn readonly(&self) -> bool {
        false
    }

    pub fn set_readonly(&mut self, _readonly: bool) {
        // std's API cannot report a failure here, and this service has no permission bits, so the
        // only honest thing is to do nothing; `set_permissions` (which CAN fail) refuses.
    }
}

impl FileType {
    pub fn is_dir(&self) -> bool {
        false
    }

    pub fn is_file(&self) -> bool {
        true
    }

    pub fn is_symlink(&self) -> bool {
        false
    }
}

impl fmt::Debug for ReadDir {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
    }
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<io::Result<DirEntry>> {
        self.0
    }
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.0
    }

    pub fn file_name(&self) -> OsString {
        self.0
    }

    pub fn metadata(&self) -> io::Result<FileAttr> {
        self.0
    }

    pub fn file_type(&self) -> io::Result<FileType> {
        self.0
    }
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
        }
    }

    pub fn read(&mut self, read: bool) {
        self.read = read;
    }
    pub fn write(&mut self, write: bool) {
        self.write = write;
    }
    pub fn append(&mut self, append: bool) {
        self.append = append;
    }
    pub fn truncate(&mut self, truncate: bool) {
        self.truncate = truncate;
    }
    pub fn create(&mut self, create: bool) {
        self.create = create;
    }
    pub fn create_new(&mut self, create_new: bool) {
        self.create_new = create_new;
    }
}

impl File {
    pub fn open(path: &Path, opts: &OpenOptions) -> io::Result<File> {
        if !opts.read && !opts.write && !opts.append {
            return Err(io::const_error!(
                io::ErrorKind::InvalidInput,
                "an open must ask for read, write, or append"
            ));
        }
        // `CREATE` and `TRUNCATE` exist now (milestone 31 phase 2), so this whole block used to be a
        // refusal and is now a mapping. Creating a *file* was never the thing §27 kept host-side:
        // that was creating a *filesystem*, which needs uuid and getrandom. `Transaction::create_node`
        // is not std-gated, so the server can make a file without entropy ever becoming a userspace
        // dependency, and the read of §27 that conflated the two is corrected there.
        //
        // The order below is POSIX's, and it matters: create-then-truncate, with truncate applied
        // after a successful open of an existing file. `std::fs::write` is
        // `create(true).truncate(true)`, so getting the order wrong would leave the old tail behind
        // on exactly the path that exists to replace a file's contents, which is the day-costing bug
        // this milestone is here to remove.
        let handle = match open_handle(path) {
            Ok(h) if opts.create_new => {
                // `create_new` means "must not already exist", and it does. Close the handle the open
                // just minted rather than leaking it for the life of the process: the error path is
                // the one nobody exercises, so it is the one that leaks.
                let _ = request(proto::req(proto::CLOSE, h, 0), 0);
                return Err(io::const_error!(
                    io::ErrorKind::AlreadyExists,
                    "the file already exists"
                ));
            }
            Ok(h) => h,
            // Not there, and the caller asked for it to be made. This is the case that used to be
            // Unsupported.
            Err(e) if (opts.create || opts.create_new) && e.kind() == io::ErrorKind::NotFound => {
                create_handle(path)?
            }
            Err(e) => return Err(e),
        };

        // Truncate after the open, so a fresh file (already empty) pays nothing and an existing one
        // is emptied before the first write rather than after it.
        if opts.truncate {
            if let Err(e) = request(proto::req(proto::TRUNCATE, handle, 0), 0) {
                let _ = request(proto::req(proto::CLOSE, handle, 0), 0);
                return Err(e);
            }
        }

        let file = File { handle, pos: AtomicU64::new(0), append: opts.append };
        if opts.append {
            let end = file.file_attr()?.size;
            file.pos.store(end, Ordering::Relaxed);
        }
        Ok(file)
    }

    pub fn file_attr(&self) -> io::Result<FileAttr> {
        let size = request(proto::req(proto::FSTAT, self.handle, 0), 0)?;
        Ok(FileAttr { size })
    }

    pub fn fsync(&self) -> io::Result<()> {
        // Nothing is buffered on this side, and the server commits a RedoxFS transaction per write
        // (that is what makes a mid-write kill recoverable), so a successful write is already
        // durable and there is nothing left for a sync verb to do.
        Ok(())
    }

    pub fn datasync(&self) -> io::Result<()> {
        self.fsync()
    }

    pub fn lock(&self) -> io::Result<()> {
        unsupported()
    }

    pub fn lock_shared(&self) -> io::Result<()> {
        unsupported()
    }

    pub fn try_lock(&self) -> Result<(), TryLockError> {
        Err(TryLockError::Error(unsupported_err()))
    }

    pub fn try_lock_shared(&self) -> Result<(), TryLockError> {
        Err(TryLockError::Error(unsupported_err()))
    }

    pub fn unlock(&self) -> io::Result<()> {
        unsupported()
    }

    pub fn truncate(&self, _size: u64) -> io::Result<()> {
        unsupported()
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let pos = self.pos.load(Ordering::Relaxed);
        let want = buf.len().min(PAGE);
        let mut p = page();
        let n = (request(proto::req(proto::READ, self.handle, want as u64), pos)? as usize).min(want);
        p.get(&mut buf[..n]);
        drop(p);
        self.pos.store(pos + n as u64, Ordering::Relaxed);
        Ok(n)
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        crate::io::default_read_vectored(|b| self.read(b), bufs)
    }

    #[inline]
    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn read_buf(&self, cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        crate::io::default_read_buf(|b| self.read(b), cursor)
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.append {
            let end = self.file_attr()?.size;
            self.pos.store(end, Ordering::Relaxed);
        }
        let pos = self.pos.load(Ordering::Relaxed);
        let chunk = buf.len().min(PAGE);
        let mut p = page();
        p.put(&buf[..chunk]);
        let n =
            (request(proto::req(proto::WRITE, self.handle, chunk as u64), pos)? as usize).min(chunk);
        drop(p);
        self.pos.store(pos + n as u64, Ordering::Relaxed);
        Ok(n)
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        crate::io::default_write_vectored(|b| self.write(b), bufs)
    }

    #[inline]
    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    /// The position is this side's, since the contract's `READ` and `WRITE` both carry an explicit
    /// offset: there is no cursor in the server to get out of step with, and a seek costs no
    /// message at all except `SeekFrom::End`, which needs the size.
    pub fn seek(&self, pos: SeekFrom) -> io::Result<u64> {
        let (base, delta) = match pos {
            SeekFrom::Start(off) => {
                self.pos.store(off, Ordering::Relaxed);
                return Ok(off);
            }
            SeekFrom::Current(off) => (self.pos.load(Ordering::Relaxed), off),
            SeekFrom::End(off) => (self.file_attr()?.size, off),
        };
        // A resulting offset before the start is `InvalidInput`, std's contract, not a wrap-around.
        let target = base.checked_add_signed(delta).ok_or_else(|| {
            io::const_error!(io::ErrorKind::InvalidInput, "cannot seek before the start of a file")
        })?;
        self.pos.store(target, Ordering::Relaxed);
        Ok(target)
    }

    pub fn size(&self) -> Option<io::Result<u64>> {
        // `FSTAT` is one message, so answering the size hint is cheaper than the read-until-EOF
        // loop `read_to_string`/`read_to_end` fall back to.
        Some(self.file_attr().map(|a| a.size))
    }

    pub fn tell(&self) -> io::Result<u64> {
        Ok(self.pos.load(Ordering::Relaxed))
    }

    pub fn duplicate(&self) -> io::Result<File> {
        // A handle is a token the server minted for one session; there is no dup verb, and copying
        // the number would forge a second owner of the same handle, including its close.
        unsupported()
    }

    pub fn set_permissions(&self, _perm: FilePermissions) -> io::Result<()> {
        unsupported()
    }

    pub fn set_times(&self, _times: FileTimes) -> io::Result<()> {
        unsupported()
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File").field("handle", &self.handle).finish()
    }
}

impl Drop for File {
    fn drop(&mut self) {
        // `CLOSE` frees the server's handle-table slot. A failure here has nowhere to go and
        // nothing to fix: the process is done with the file either way.
        let _ = request(proto::req(proto::CLOSE, self.handle, 0), 0);
    }
}

// --- Path-level operations --------------------------------------------------------------------

/// A file's metadata by name: open, `FSTAT`, close. Three messages instead of one, because the
/// contract has no stat-by-name verb; the effect is the same and the authority is identical (the
/// name still resolves only under the granted directory).
pub fn stat(path: &Path) -> io::Result<FileAttr> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    File::open(path, &opts)?.file_attr()
}

/// No symlinks cross this contract, so following one and not following one are the same thing.
pub fn lstat(path: &Path) -> io::Result<FileAttr> {
    stat(path)
}

pub fn exists(path: &Path) -> io::Result<bool> {
    match stat(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn readdir(_p: &Path) -> io::Result<ReadDir> {
    // Listing a directory needs a verb the contract does not have. Adding one is a change to
    // `fs_proto` and DECISIONS §27, not something to fake here by guessing names.
    unsupported()
}

pub fn set_perm(_p: &Path, _perm: FilePermissions) -> io::Result<()> {
    unsupported()
}

pub fn set_times(_p: &Path, _times: FileTimes) -> io::Result<()> {
    unsupported()
}

pub fn set_times_nofollow(_p: &Path, _times: FileTimes) -> io::Result<()> {
    unsupported()
}
