//! Host-side RedoxFS image operations (milestone 32).
//!
//! The library half of the `redoxfs-host` tool: create an image, list its root, put a file in,
//! read a file back, all against the **vendored 0.9.1 pin** (vendor/redoxfs), which is the same
//! engine the cricker-os FS server will serve it with. That identity is the point: an image
//! built and inspected here is proven against exactly the code that will mount it on
//! cricker-os, which is the "images can be created and inspected on the host with the same
//! code" property the roadmap names as RedoxFS's gift.
//!
//! Scope, deliberate: **flat root only.** Phase 2's test fixtures are a handful of files in the
//! root directory, so `put`/`cat` resolve one name against the root and there is no path walk.
//! Creation is host-only by design (roadmap §32 port plan item 4): `FileSystem::create` needs
//! entropy (uuid v4) and std, and the server must never grow either dependency, so the server
//! opens images and this tool makes them.
//!
//! Errors are rendered to `String` at this boundary. The engine's error type is
//! `syscall::error::Error` (redox_syscall's errno); mapping it once here keeps the redox_syscall
//! type out of our callers, the same "map the error type once, at the boundary" rule the roadmap
//! sets for the FS server's ABI edge.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redoxfs::{DiskFile, FileSystem, Node, TreePtr};

/// One root-directory entry, as `ls` reports it.
pub struct LsEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Seconds and nanoseconds since the epoch, the timestamp shape the engine wants.
fn now() -> (u64, u32) {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (t.as_secs(), t.subsec_nanos())
}

/// Open an existing image read-write. `cleanup: true` matches upstream's mount path (it replays
/// the header ring to the newest consistent generation and tidies allocations).
fn open(image: &Path) -> Result<FileSystem<DiskFile>, String> {
    let disk =
        DiskFile::open(image).map_err(|e| format!("cannot open {}: {e}", image.display()))?;
    FileSystem::open(disk, None, None, true)
        .map_err(|e| format!("{} is not a RedoxFS image: {e}", image.display()))
}

/// Create a fresh, empty RedoxFS image of `size` bytes at `image`. Unencrypted, no reserved
/// bootloader area. Fails if the size cannot hold the header ring plus a minimal tree.
pub fn mkfs(image: &Path, size: u64) -> Result<(), String> {
    let disk = DiskFile::create(image, size)
        .map_err(|e| format!("cannot create {}: {e}", image.display()))?;
    let (secs, nsec) = now();
    FileSystem::create(disk, None, secs, nsec)
        .map_err(|e| format!("mkfs on {} failed: {e}", image.display()))?;
    Ok(())
}

/// List the root directory: name, size, and kind of every entry.
pub fn ls(image: &Path) -> Result<Vec<LsEntry>, String> {
    let mut fs = open(image)?;
    fs.tx(|tx| {
        let mut children = Vec::new();
        tx.child_nodes(TreePtr::root(), &mut children)?;
        let mut out = Vec::new();
        for entry in children {
            let Some(name) = entry.name() else { continue };
            let node = tx.read_tree(entry.node_ptr())?;
            out.push(LsEntry {
                name: name.to_string(),
                size: node.data().size(),
                is_dir: node.data().is_dir(),
            });
        }
        Ok(out)
    })
    .map_err(|e| format!("listing {} failed: {e}", image.display()))
}

/// Write `data` as the root-directory file `name`, creating it or truncating an existing one.
pub fn put(image: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let mut fs = open(image)?;
    let (secs, nsec) = now();
    fs.tx(|tx| {
        let node_ptr = match tx.find_node(TreePtr::root(), name) {
            Ok(node) => {
                tx.truncate_node(node.ptr(), 0, secs, nsec)?;
                node.ptr()
            }
            Err(_) => tx
                .create_node(TreePtr::root(), name, Node::MODE_FILE | 0o644, secs, nsec)?
                .ptr(),
        };
        tx.write_node(node_ptr, 0, data, secs, nsec)?;
        Ok(())
    })
    .map_err(|e| format!("putting {name} into {} failed: {e}", image.display()))
}

/// Read the whole root-directory file `name` back.
pub fn cat(image: &Path, name: &str) -> Result<Vec<u8>, String> {
    let mut fs = open(image)?;
    let (secs, nsec) = now();
    fs.tx(|tx| {
        let node = tx.find_node(TreePtr::root(), name)?;
        let mut buf = vec![0u8; node.data().size() as usize];
        let n = tx.read_node(node.ptr(), 0, &mut buf, secs, nsec)?;
        buf.truncate(n);
        Ok(buf)
    })
    .map_err(|e| format!("reading {name} from {} failed: {e}", image.display()))
}
