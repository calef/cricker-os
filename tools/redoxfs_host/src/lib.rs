//! Host-side RedoxFS image operations (milestone 32, extended for recovery in milestone 57).
//!
//! The library half of the `redoxfs_host` tool: create an image, walk it, read a file back, and
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

use fs_proto::xattr;
use redoxfs::{Disk, DiskFile, FileSystem, Node, Transaction, TreeData, TreePtr};

/// Public so the recovery test can read an attribute back off the host file the extract wrote. The
/// round trip is the claim this feature makes, and closing it needs both halves in one place.
pub mod host_xattr;

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
    /// How many extended attributes this entry carries. Rendered as `@`, which is the marker macOS
    /// `ls -l` uses for the same fact, so a listing says that there is metadata here **before** the
    /// reader has to know that `.cricker-attrs` exists.
    pub attrs: usize,
}

/// One extended attribute, as recovery sees it: the name and value as bytes, plus the type code
/// [`fs_proto::xattr`] carries and no host filesystem can hold.
pub struct Attr {
    pub name: Vec<u8>,
    pub kind: u32,
    pub value: Vec<u8>,
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
    /// Extended attributes put back onto the extracted files.
    pub attrs: u64,
    /// Attributes the host would not take. **Counted and named on stderr, never dropped quietly**
    /// (DECISIONS §42): the usual causes are a destination filesystem with no attribute support, a
    /// Linux host refusing a name without a `user.` prefix, and a Linux host refusing any attribute
    /// on a symlink. Each is something a person can act on, and each has a different answer.
    pub attrs_skipped: u64,
    /// Attributes whose bytes were reattached but whose **type code** could not be, because no host
    /// filesystem has a per-attribute type word. The codes stay readable in the extracted
    /// `.cricker-attrs` blobs. Only counted for a code that is not [`xattr::RAW`], since dropping a
    /// zero loses nothing.
    pub kinds_dropped: u64,
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
            "{}: on-disk format version {v}, but this build of redoxfs_host reads version {}. \
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

// --- The attribute store, from the recovery side (milestone 57) --------------------------------
//
// A cricker-os image carries `.cricker-attrs` in its root: one file per node that has extended
// attributes, named for that node's `TreePtr` id in hex, holding the records `fs_proto::xattr::store`
// defines. The FS server writes it; nothing else does. This tool reads it, and the reading is why
// `fs_proto` is a dependency rather than a comment describing the layout (CLAUDE.md rule 7).
//
// **Nothing here is allowed to fail an extraction.** A recovery that abandoned a hundred thousand
// files because one attribute blob was damaged would be worse than useless, so every function below
// reports a problem to the caller as a *skip* and the caller keeps walking. The files come out even
// when the metadata does not, and the summary says how much did not.

/// The attribute store's directory, or `None` if this image has never held an attribute.
///
/// **`ENOENT` is the only error read as absence.** Any other one is propagated, because a dropped
/// read of the root's tree block answers `EIO`, and treating that as "there are no attributes" would
/// quietly recover a backup without its metadata and report success. That is the same false negative
/// the FS server's own `find_store` refuses to make (notes/fs-server.md).
fn find_store<D: Disk>(tx: &mut Transaction<D>) -> Result<Option<TreePtr<Node>>, String> {
    match tx.find_node(TreePtr::root(), xattr::STORE_DIR) {
        Ok(node) => Ok(Some(node.ptr())),
        Err(e) if e.errno == syscall::error::ENOENT => Ok(None),
        Err(e) => Err(format!(
            "cannot read the attribute store {}: {e}",
            xattr::STORE_DIR
        )),
    }
}

/// One node's raw attribute blob, empty if it has none.
///
/// A blob longer than [`xattr::store::MAX_BYTES`] is refused rather than clipped: the layer cannot
/// have written it, so reading its first 53 KB would be reading a file this format does not describe.
fn attr_blob<D: Disk>(
    tx: &mut Transaction<D>,
    store: Option<TreePtr<Node>>,
    node_id: u32,
) -> Result<Vec<u8>, String> {
    let Some(dir_ptr) = store else {
        return Ok(Vec::new());
    };
    let file = xattr::store::file_name(node_id);
    // ASCII by construction (`file_name` emits hex digits), so this cannot fail; the arm exists so a
    // future change there cannot silently alias two nodes onto one blob.
    let name = std::str::from_utf8(&file)
        .map_err(|_| format!("node {node_id:#010x}: its store file name is not UTF-8"))?;
    let node = match tx.find_node(dir_ptr, name) {
        Ok(node) => node,
        Err(e) if e.errno == syscall::error::ENOENT => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}/{name}: {e}", xattr::STORE_DIR)),
    };
    let size = node.data().size() as usize;
    if size > xattr::store::MAX_BYTES {
        return Err(format!(
            "{}/{name}: {size} bytes, more than the {} an attribute blob can be; not read",
            xattr::STORE_DIR,
            xattr::store::MAX_BYTES,
        ));
    }
    read_all(tx, &node)
}

/// Decode a blob into attributes, plus the number of **trailing bytes that are not a record**.
///
/// `store::iter` stops at the first record that runs off the end, which is what a torn write leaves
/// and is the behaviour that keeps a truncated blob reading short rather than wrong. That is right
/// for the FS server and not enough for a recovery tool, which should say that something was there
/// and could not be read. Trailing zeroes do not count: an empty blob is removed rather than
/// zero-filled, so a zero tail means a shorter record list and nothing lost.
fn decode_attrs(blob: &[u8]) -> (Vec<Attr>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for (name, kind, value) in xattr::store::iter(blob) {
        used += xattr::store::record_len(name.len(), value.len());
        out.push(Attr {
            name: name.to_vec(),
            kind,
            value: value.to_vec(),
        });
    }
    let tail = &blob[used.min(blob.len())..];
    let unread = if tail.iter().all(|b| *b == 0) {
        0
    } else {
        tail.len()
    };
    (out, unread)
}

/// Every extended attribute on `path` inside the image, in the order the store holds them (which is
/// the order they were set, since a replacement keeps its place).
pub fn attrs(image: &Path, path: &str) -> Result<Vec<Attr>, String> {
    let comps = components(path)?;
    let mut fs = open_ro(image)?;
    fs.tx(|tx| Ok(attrs_tx(tx, &comps)))
        .map_err(|e| format!("reading the attributes of {path} failed: {e}"))?
}

fn attrs_tx<D: Disk>(tx: &mut Transaction<D>, comps: &[&str]) -> Result<Vec<Attr>, String> {
    let node = resolve(tx, comps)?;
    let store = find_store(tx)?;
    let blob = attr_blob(tx, store, node.ptr().id())?;
    let (attrs, unread) = decode_attrs(&blob);
    if unread > 0 {
        eprintln!(
            "redoxfs_host: /{}: {unread} trailing bytes in its attribute blob are not a record \
             and were not read",
            comps.join("/"),
        );
    }
    Ok(attrs)
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
    let store = find_store(tx)?;
    // Names and pointers first, so the store lookup below can borrow `tx` mutably (a `DirEntry`
    // would keep it borrowed for the whole loop). Same shape as `extract_node`'s.
    let entries: Vec<(String, TreePtr<Node>)> = children
        .iter()
        .filter_map(|e| e.name().map(|n| (n.to_string(), e.node_ptr())))
        .collect();
    let mut out = Vec::new();
    for (name, ptr) in entries {
        let child = tx
            .read_tree(ptr)
            .map_err(|e| format!("{name}: cannot read its node: {e}"))?;
        // A store that cannot be read is a listing that says nothing about attributes, not a listing
        // that fails: `ls` is the first thing a person runs on a damaged image.
        let attrs = match attr_blob(tx, store, ptr.id()) {
            Ok(blob) => xattr::store::count(&blob),
            Err(msg) => {
                eprintln!("redoxfs_host: {name}: {msg}");
                0
            }
        };
        out.push(LsEntry {
            name,
            size: child.data().size(),
            kind: kind_of(child.data()),
            attrs,
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
///
/// **Extended attributes come back with the files** where the host will take them (milestone 57).
/// Anything it refuses is counted and named on stderr rather than dropped, and the raw store is
/// copied out beside the tree either way, so a refusal costs presentation and not data. See
/// notes/host-recovery.md.
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
    // Found once, from the image root, and carried down the walk. The store's location does not
    // depend on which subtree is being extracted, which is why extracting `nested/deep` still
    // recovers the attributes of what is in it.
    let store = find_store(tx)?;
    let mut stats = ExtractStats::default();
    extract_node(tx, &node, dest, store, &mut stats)?;
    Ok(stats)
}

/// Put one node's extended attributes back onto the host file just written for it.
///
/// Every failure here is reported and counted, and none of them stops the extraction. The three that
/// actually happen are worth naming where a reader meets them: a destination filesystem that holds
/// no attributes (`ENOTSUP`), a Linux host refusing a name with no `user.` prefix (`EPERM`), and a
/// Linux host refusing any attribute on a symlink (`EPERM` again, which is why the message carries
/// the errno rather than an interpretation of it).
fn reattach<D: Disk>(
    tx: &mut Transaction<D>,
    node: &TreeData<Node>,
    dest: &Path,
    store: Option<TreePtr<Node>>,
    stats: &mut ExtractStats,
) {
    let blob = match attr_blob(tx, store, node.ptr().id()) {
        Ok(blob) => blob,
        Err(msg) => {
            eprintln!("redoxfs_host: {}: {msg}", dest.display());
            stats.attrs_skipped += 1;
            return;
        }
    };
    if blob.is_empty() {
        return;
    }
    let (attrs, unread) = decode_attrs(&blob);
    if unread > 0 {
        eprintln!(
            "redoxfs_host: {}: {unread} trailing bytes in its attribute blob are not a record \
             and were not reattached",
            dest.display(),
        );
        stats.attrs_skipped += 1;
    }
    for attr in attrs {
        let shown = String::from_utf8_lossy(&attr.name).into_owned();
        match host_xattr::set(dest, &attr.name, &attr.value) {
            Ok(()) => {
                stats.attrs += 1;
                if attr.kind != xattr::RAW {
                    // Named per attribute rather than summarised, because the code is the one part
                    // of this record the host cannot carry and a reader who wants it needs to know
                    // which file to go back to `.cricker-attrs` for.
                    eprintln!(
                        "redoxfs_host: {}: attribute {shown} kept its {} bytes but not its type \
                         code {:#010x}; host filesystems have no field for it (see \
                         .cricker-attrs in the extracted tree)",
                        dest.display(),
                        attr.value.len(),
                        attr.kind,
                    );
                    stats.kinds_dropped += 1;
                }
            }
            Err(e) => {
                eprintln!(
                    "redoxfs_host: {}: cannot reattach attribute {shown} ({} bytes): {e}",
                    dest.display(),
                    attr.value.len(),
                );
                stats.attrs_skipped += 1;
            }
        }
    }
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
    store: Option<TreePtr<Node>>,
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
                extract_node(tx, &child, &dest.join(&name), store, stats)?;
            }
            // Attributes before the permissions, for the same reason the permissions come last: a
            // directory already made read-only can still refuse a `setxattr` on some filesystems,
            // and a directory whose own attributes failed for that reason would be a self-inflicted
            // skip.
            reattach(tx, node, dest, store, stats);
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
            reattach(tx, node, dest, store, stats);
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
            // The attributes of the **link**, not of what it points at. macOS takes them; Linux
            // refuses `user.*` on a symlink and the refusal is counted rather than hidden.
            reattach(tx, node, dest, store, stats);
            stats.symlinks += 1;
        }
        Kind::Other => {
            eprintln!(
                "redoxfs_host: skipping {} (mode {:#06o} is not a file, directory or symlink)",
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
