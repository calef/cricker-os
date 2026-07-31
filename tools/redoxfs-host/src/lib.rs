//! Host-side RedoxFS image operations (milestone 32, extended for recovery in milestone 57).
//!
//! The library half of the `redoxfs-host` tool: create an image, walk it, read a file back, and
//! extract a whole subtree to a host directory, all against the **vendored 0.9.1 pin**
//! (vendor/redoxfs), which is the same engine the cricker-os FS server serves it with. That
//! identity is the point twice over. It is why an image built here is proven against exactly the
//! code that will mount it on cricker-os, and it is why the answer to "the board is dead, can I
//! get my data?" is a tool rather than a kernel driver: the engine already links here with `std`,
//! so `ls`/`cat`/`extract` cost a few hundred lines and run identically on macOS and Linux with no
//! FUSE, no kernel extension, and no root. See notes/host-recovery.md.
//!
//! **The read paths open the image read-only and write nothing to it**, which matters when the
//! image is the last copy of the data. That rules out `FileSystem::open`'s `cleanup` pass and
//! rules out `Transaction::read_node`, whose atime update dirties a node (and so the header ring)
//! on any file last read more than an hour ago. `read_node_inner` is the same read without the
//! timestamp, so recovery uses it.
//!
//! **No key handling, deliberately** (roadmap milestone 57, decided 2026-07-30). RedoxFS supports
//! encryption and this volume does not use it: encryption belongs at the Time Machine layer, where
//! the Mac encrypts before anything is sent. So every `FileSystem::open` here passes `None` for the
//! password and a reader needs no secret at all.
//!
//! Errors are rendered to `String` at this boundary. The engine's error type is
//! `syscall::error::Error` (redox_syscall's errno); mapping it once here keeps the redox_syscall
//! type out of our callers, the same "map the error type once, at the boundary" rule the roadmap
//! sets for the FS server's ABI edge. We never *construct* one, which is why the `_tx` helpers
//! return `Result<T, String>` and the `fs.tx` closures hand that back through `Ok`: a
//! `Transaction` can only be made by `FileSystem::tx`, whose closure must fail in the engine's own
//! vocabulary. Read paths write nothing, so committing a walk that failed commits nothing.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redoxfs::{Disk, DiskFile, FileSystem, Node, Transaction, TreeData, TreePtr};

/// What an entry is. `Other` covers the modes RedoxFS can hold and a host directory cannot
/// meaningfully receive (sockets, and anything a later format version adds).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

impl Kind {
    /// Fixed-width label for `ls` output.
    pub fn label(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir ",
            Kind::Symlink => "link",
            Kind::Other => "?   ",
        }
    }
}

fn kind_of(node: &Node) -> Kind {
    if node.is_dir() {
        Kind::Dir
    } else if node.is_file() {
        Kind::File
    } else if node.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

/// One directory entry, as `ls` reports it.
pub struct LsEntry {
    pub name: String,
    pub size: u64,
    pub kind: Kind,
}

/// What an `extract` moved, for the one-line summary the tool prints.
#[derive(Default)]
pub struct ExtractStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    /// Entries the host directory cannot receive; named on stderr as they are met.
    pub skipped: u64,
}

/// Seconds and nanoseconds since the epoch, the timestamp shape the engine wants.
fn now() -> (u64, u32) {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (t.as_secs(), t.subsec_nanos())
}

/// Open an existing image **read-only**, for the recovery paths. No `cleanup`, so nothing is
/// replayed or tidied, and the file is opened without write permission in the first place: a disk
/// that is failing, or an image on read-only media, still reads.
///
/// Skipping `cleanup` does not weaken what is read. `FileSystem::open` picks the newest *valid*
/// header out of the ring regardless, which is the crash-consistency property itself; `cleanup`
/// only releases nodes an unclean unmount left open. Upstream's own `redoxfs-clone` reads its
/// source disk exactly this way.
fn open_ro(image: &Path) -> Result<FileSystem<DiskFile>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .open(image)
        .map_err(|e| format!("cannot open {}: {e}", image.display()))?;
    FileSystem::open(DiskFile::from(file), None, None, false).map_err(|e| open_failed(image, e))
}

/// Explain a failed open, and in particular explain the one failure the operational rule exists to
/// prevent: a reader that does not match the format it is reading.
///
/// `Header::valid` checks the version before anything else, so an image written by a *different*
/// RedoxFS presents as no valid header anywhere in the ring, and the engine's own error for that is
/// ENOENT. "No such file or directory" for a file that is plainly there is the wrong thing to be
/// told at 2am, so when the signature is on the disk we read the version field beside it and say so.
/// See notes/host-recovery.md: this is the message that tells you to go and find the pin.
fn open_failed(image: &Path, err: impl std::fmt::Display) -> String {
    match on_disk_version(image) {
        Some(v) if v != redoxfs::VERSION => format!(
            "{}: on-disk format version {v}, but this build of redoxfs-host reads version {}. \
             A reader must match the format version it reads; recover with the RedoxFS pin the \
             backup was written by (see notes/host-recovery.md).",
            image.display(),
            redoxfs::VERSION,
        ),
        _ => format!("{} is not a RedoxFS image: {err}", image.display()),
    }
}

/// The format version recorded on the disk, if a RedoxFS signature is anywhere in the header ring's
/// range. Best effort and deliberately independent of the engine: it reads 16 bytes at a block
/// boundary, so it still works when nothing in the image parses.
fn on_disk_version(image: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(image).ok()?;
    let mut buf = [0u8; 16];
    for block in 0..redoxfs::HEADER_RING {
        file.seek(SeekFrom::Start(block * redoxfs::BLOCK_SIZE))
            .ok()?;
        if file.read_exact(&mut buf).is_err() {
            return None;
        }
        if &buf[..8] == redoxfs::SIGNATURE {
            return Some(u64::from_le_bytes(buf[8..16].try_into().ok()?));
        }
    }
    None
}

/// Open an existing image read-write, for the paths that change it. `cleanup: true` matches
/// upstream's mount path (it replays the header ring to the newest consistent generation and tidies
/// allocations).
fn open_rw(image: &Path) -> Result<FileSystem<DiskFile>, String> {
    let disk =
        DiskFile::open(image).map_err(|e| format!("cannot open {}: {e}", image.display()))?;
    FileSystem::open(disk, None, None, true).map_err(|e| open_failed(image, e))
}

/// Split an image path into components. Leading, trailing and doubled slashes are ignored, `.` is
/// ignored, and **`..` is refused**: paths resolve from the image root downward and nothing else.
/// That is the same rule the FS server enforces on the wire (notes/fs-server.md), kept here so a
/// path means the same thing whether cricker-os or this tool resolves it.
fn components(path: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "{path}: `..` is not accepted, paths resolve from the image root downward"
                ));
            }
            name => out.push(name),
        }
    }
    Ok(out)
}

/// Walk `comps` from the image root and return the node it names.
fn resolve<D: Disk>(tx: &mut Transaction<D>, comps: &[&str]) -> Result<TreeData<Node>, String> {
    // Annotated because `read_tree` is generic over the block type it decodes.
    let mut node: TreeData<Node> = tx
        .read_tree(TreePtr::root())
        .map_err(|e| format!("cannot read the image root: {e}"))?;
    for (i, name) in comps.iter().enumerate() {
        if !node.data().is_dir() {
            return Err(format!("/{} is not a directory", comps[..i].join("/")));
        }
        node = tx
            .find_node(node.ptr(), name)
            .map_err(|e| format!("/{}: {e}", comps[..=i].join("/")))?;
    }
    Ok(node)
}

/// Read a whole node's data. Loops because a short read is legal (`read_node_inner` stops at a
/// record boundary in principle), so a file bigger than one 128 KiB record must not depend on a
/// single call filling the buffer.
fn read_all<D: Disk>(tx: &mut Transaction<D>, node: &TreeData<Node>) -> Result<Vec<u8>, String> {
    let size = node.data().size() as usize;
    let mut buf = vec![0u8; size];
    let mut got = 0;
    while got < size {
        let n = tx
            .read_node_inner(node, got as u64, &mut buf[got..])
            .map_err(|e| format!("read failed at offset {got}: {e}"))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

/// Create a fresh, empty RedoxFS image of `size` bytes at `image`. Unencrypted, no reserved
/// bootloader area. Fails if the size cannot hold the header ring plus a minimal tree.
///
/// **Start from an empty file.** `DiskFile::create` opens with `create(true)` but does NOT
/// truncate, so running `mkfs` over an existing image leaves that image's stale blocks past the
/// new (smaller) write, and the result fails to open ("not a RedoxFS image: I/O error"). Removing
/// the file first makes `mkfs` idempotent, which the phase-2 test flow relies on (it regenerates
/// the image every run).
pub fn mkfs(image: &Path, size: u64) -> Result<(), String> {
    let _ = std::fs::remove_file(image); // ignore "not found"; any other error surfaces below
    let disk = DiskFile::create(image, size)
        .map_err(|e| format!("cannot create {}: {e}", image.display()))?;
    let (secs, nsec) = now();
    FileSystem::create(disk, None, secs, nsec)
        .map_err(|e| format!("mkfs on {} failed: {e}", image.display()))?;
    Ok(())
}

/// List a directory: name, size, and kind of every entry, sorted by name. `path` may be `/` or
/// empty for the image root.
pub fn ls(image: &Path, path: &str) -> Result<Vec<LsEntry>, String> {
    let comps = components(path)?;
    let mut fs = open_ro(image)?;
    fs.tx(|tx| Ok(ls_tx(tx, &comps)))
        .map_err(|e| format!("listing {} failed: {e}", image.display()))?
}

fn ls_tx<D: Disk>(tx: &mut Transaction<D>, comps: &[&str]) -> Result<Vec<LsEntry>, String> {
    let node = resolve(tx, comps)?;
    if !node.data().is_dir() {
        return Err(format!("/{}: not a directory", comps.join("/")));
    }
    let mut children = Vec::new();
    tx.child_nodes(node.ptr(), &mut children)
        .map_err(|e| format!("/{}: listing failed: {e}", comps.join("/")))?;
    let mut out = Vec::new();
    for entry in children {
        let Some(name) = entry.name() else { continue };
        let child = tx
            .read_tree(entry.node_ptr())
            .map_err(|e| format!("{name}: cannot read its node: {e}"))?;
        out.push(LsEntry {
            name: name.to_string(),
            size: child.data().size(),
            kind: kind_of(child.data()),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read a whole file out of the image. A directory is an error, not an empty result.
pub fn cat(image: &Path, path: &str) -> Result<Vec<u8>, String> {
    let comps = components(path)?;
    let mut fs = open_ro(image)?;
    fs.tx(|tx| Ok(cat_tx(tx, &comps)))
        .map_err(|e| format!("reading {path} from {} failed: {e}", image.display()))?
}

fn cat_tx<D: Disk>(tx: &mut Transaction<D>, comps: &[&str]) -> Result<Vec<u8>, String> {
    let node = resolve(tx, comps)?;
    if node.data().is_dir() {
        return Err(format!("/{}: is a directory", comps.join("/")));
    }
    read_all(tx, &node)
}

/// Extract the subtree at `path` to the host path `dest`, which **becomes** that subtree: a
/// directory in the image lands as a directory at `dest`, a file lands as the file `dest`. That is
/// `cp -R SRC DEST` with a `DEST` that does not exist yet, and it is unambiguous in a way "copy
/// into" is not.
pub fn extract(image: &Path, path: &str, dest: &Path) -> Result<ExtractStats, String> {
    let comps = components(path)?;
    let mut fs = open_ro(image)?;
    fs.tx(|tx| Ok(extract_tx(tx, &comps, dest)))
        .map_err(|e| format!("extracting {path} from {} failed: {e}", image.display()))?
}

fn extract_tx<D: Disk>(
    tx: &mut Transaction<D>,
    comps: &[&str],
    dest: &Path,
) -> Result<ExtractStats, String> {
    let node = resolve(tx, comps)?;
    let mut stats = ExtractStats::default();
    extract_node(tx, &node, dest, &mut stats)?;
    Ok(stats)
}

/// Reject a name that would not stay inside the destination directory.
///
/// The image is the untrusted input here: it may be damaged, or it may have been written by
/// something other than this engine. RedoxFS does not itself permit `/` in a name, so this is
/// belt and braces rather than a known hole, and it is exactly the check a tool that writes
/// image-supplied names into a host filesystem must not omit.
fn safe_component(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(format!(
            "refusing to extract an entry named {name:?}: it would not stay inside the destination"
        ));
    }
    Ok(())
}

/// Permission bits to give an extracted file or directory.
///
/// Masked to `0o777`, dropping setuid, setgid and the sticky bit. Those three carry authority on
/// the host, and an image is data we are recovering, not something that gets to hand out authority
/// on the machine doing the recovery.
fn host_mode(node: &Node) -> u32 {
    u32::from(node.mode()) & 0o777
}

fn extract_node<D: Disk>(
    tx: &mut Transaction<D>,
    node: &TreeData<Node>,
    dest: &Path,
    stats: &mut ExtractStats,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    match kind_of(node.data()) {
        Kind::Dir => {
            std::fs::create_dir_all(dest)
                .map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
            let mut children = Vec::new();
            tx.child_nodes(node.ptr(), &mut children)
                .map_err(|e| format!("cannot list {}: {e}", dest.display()))?;
            // Names and pointers first: the recursion needs `tx` mutably, and a borrowed
            // `DirEntry` would keep it borrowed for the whole loop.
            let entries: Vec<(String, TreePtr<Node>)> = children
                .iter()
                .filter_map(|e| e.name().map(|n| (n.to_string(), e.node_ptr())))
                .collect();
            for (name, ptr) in entries {
                safe_component(&name)?;
                let child = tx
                    .read_tree(ptr)
                    .map_err(|e| format!("{name}: cannot read its node: {e}"))?;
                extract_node(tx, &child, &dest.join(&name), stats)?;
            }
            // Permissions last: a directory extracted read-only would otherwise refuse its own
            // children.
            std::fs::set_permissions(dest, PermissionsExt::from_mode(host_mode(node.data())))
                .map_err(|e| format!("cannot set the mode of {}: {e}", dest.display()))?;
            stats.dirs += 1;
        }
        Kind::File => {
            let data = read_all(tx, node)?;
            std::fs::write(dest, &data)
                .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
            std::fs::set_permissions(dest, PermissionsExt::from_mode(host_mode(node.data())))
                .map_err(|e| format!("cannot set the mode of {}: {e}", dest.display()))?;
            stats.files += 1;
            stats.bytes += data.len() as u64;
        }
        Kind::Symlink => {
            // A RedoxFS symlink stores its target as the node's data, which is how `archive_at`
            // writes one. Recreate it as a symlink rather than following it: a dangling link
            // recovered faithfully beats a copy of whatever the *host* has at that path today.
            let target = read_all(tx, node)?;
            let target = String::from_utf8(target)
                .map_err(|_| format!("{}: symlink target is not UTF-8", dest.display()))?;
            let _ = std::fs::remove_file(dest);
            std::os::unix::fs::symlink(&target, dest)
                .map_err(|e| format!("cannot link {} -> {target}: {e}", dest.display()))?;
            stats.symlinks += 1;
        }
        Kind::Other => {
            eprintln!(
                "redoxfs-host: skipping {} (mode {:#06o} is not a file, directory or symlink)",
                dest.display(),
                node.data().mode()
            );
            stats.skipped += 1;
        }
    }
    Ok(())
}

/// Write `data` as the file `path`, creating it or truncating an existing one. The parent
/// directory must already exist: this makes files, and `import` makes trees.
pub fn put(image: &Path, path: &str, data: &[u8]) -> Result<(), String> {
    let comps = components(path)?;
    let mut fs = open_rw(image)?;
    fs.tx(|tx| Ok(put_tx(tx, &comps, data)))
        .map_err(|e| format!("putting {path} into {} failed: {e}", image.display()))?
}

fn put_tx<D: Disk>(tx: &mut Transaction<D>, comps: &[&str], data: &[u8]) -> Result<(), String> {
    let Some((name, parents)) = comps.split_last() else {
        return Err("no file name given".to_string());
    };
    let (secs, nsec) = now();
    let parent = resolve(tx, parents)?;
    if !parent.data().is_dir() {
        return Err(format!("/{}: not a directory", parents.join("/")));
    }
    let node_ptr = match tx.find_node(parent.ptr(), name) {
        Ok(node) => {
            tx.truncate_node(node.ptr(), 0, secs, nsec)
                .map_err(|e| format!("{name}: truncate failed: {e}"))?;
            node.ptr()
        }
        Err(_) => tx
            .create_node(parent.ptr(), name, Node::MODE_FILE | 0o644, secs, nsec)
            .map_err(|e| format!("{name}: create failed: {e}"))?
            .ptr(),
    };
    tx.write_node(node_ptr, 0, data, secs, nsec)
        .map_err(|e| format!("{name}: write failed: {e}"))?;
    Ok(())
}

/// Copy the *contents* of the host directory `dir` into the image root, recursively.
///
/// This is upstream's own archiver (`redoxfs::archive`, the engine half of `redoxfs-ar`) rather
/// than a directory walk of ours, and that is the point: it is the write side of the format,
/// written by the people who defined the format, so an `extract` proven against it is proven
/// against something other than our own writer. `redoxfs-ar` itself cannot be used here because it
/// *creates* the filesystem as it archives; this imports into an image `mkfs` already made.
///
/// A failure part-way leaves what it had already written in place, committed and consistent. The
/// filesystem is not damaged, the import is simply incomplete, and the tool says so and exits
/// non-zero.
pub fn import(image: &Path, dir: &Path) -> Result<(), String> {
    let mut fs = open_rw(image)?;
    redoxfs::archive(&mut fs, dir).map_err(|e| {
        format!(
            "importing {} into {} failed: {e}",
            dir.display(),
            image.display()
        )
    })?;
    Ok(())
}
