//! The recovery witness (milestone 57): a whole tree through the real on-disk format, written by
//! one process and read back by another.
//!
//! This is the "the board is dead, can I get my data?" claim, tested the only way that means
//! anything. **Every step is a separate invocation of the built binary**, so nothing is shared
//! between the write and the read but the bytes in the image file: no cached `FileSystem`, no
//! warm allocator, no in-process state that could make a reader agree with a writer for the wrong
//! reason. A test that writes and reads inside one process proves the two halves of one program
//! agree; this proves the format is on the disk.
//!
//! The tree is deliberately not flat and not small. A 300 KiB file spans several 128 KiB RedoxFS
//! records (and dozens of 4 KiB blocks), a three-deep directory chain exercises the path walk,
//! an empty file and an empty directory are the edges that a "did you write anything?" bug slips
//! through, a symlink is a node kind that is neither, and an executable proves the mode survives.
//!
//! The write side is upstream's own archiver (`import`, wrapping `redoxfs::archive`), on purpose:
//! if our reader only ever read images our writer made, a shared misunderstanding of the format
//! would pass. See notes/host-recovery.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The built binary, not the library: `cargo test` compiles it and hands us the path.
const TOOL: &str = env!("CARGO_BIN_EXE_redoxfs_host");

/// One invocation of the tool as its own process. Returns stdout; panics with stderr on failure.
fn tool(args: &[&str]) -> Vec<u8> {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        out.status.success(),
        "redoxfs_host {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

/// An invocation that is expected to fail. Returns stderr, so the test can say why.
fn tool_fails(args: &[&str]) -> String {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        !out.status.success(),
        "redoxfs_host {args:?} was expected to fail and did not",
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Deterministic pseudo-random bytes. Not a constant pattern: RedoxFS compresses records with lz4,
/// and a run of one byte would collapse to nothing and never exercise the multi-record path this
/// payload exists to exercise.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

fn write_file(path: &Path, data: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).unwrap();
}

/// Relative path -> contents for every regular file under `root`, plus a marker for directories.
/// Symlinks are recorded by their target, not by what they point at, so the comparison notices a
/// link that was silently flattened into a copy.
fn survey(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    survey_into(root, root, &mut out);
    out
}

fn survey_into(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let ty = std::fs::symlink_metadata(&path).unwrap().file_type();
        if ty.is_symlink() {
            let target = std::fs::read_link(&path).unwrap();
            out.insert(rel, format!("symlink -> {}", target.display()));
        } else if ty.is_dir() {
            out.insert(rel, "directory".to_string());
            survey_into(root, &path, out);
        } else {
            use std::os::unix::fs::PermissionsExt;
            let data = std::fs::read(&path).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            // Hash rather than the bytes: a 300 KiB mismatch should not print 300 KiB.
            out.insert(
                rel,
                format!("file mode {mode:o} len {} {}", data.len(), sum(&data)),
            );
        }
    }
}

/// A cheap content fingerprint (FNV-1a). Any byte that moves changes it.
fn sum(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("redoxfs_host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn a_whole_tree_survives_the_trip_through_the_image() {
    let src = scratch("recovery-src");
    let img = scratch("recovery.img");
    let out = scratch("recovery-out");

    // ---- build the source tree on the host -------------------------------------------------
    let large = noise(300_000, 0x5eed);
    write_file(&src.join("top.txt"), b"the file in the root\n", 0o644);
    write_file(&src.join("nested/note.txt"), b"one level down\n", 0o644);
    write_file(&src.join("nested/deep/large.bin"), &large, 0o644);
    write_file(&src.join("nested/deep/empty.txt"), b"", 0o644);
    write_file(
        &src.join("nested/deep/run.sh"),
        b"#!/bin/sh\nexit 0\n",
        0o755,
    );
    std::fs::create_dir_all(src.join("nested/empty-dir")).unwrap();
    std::os::unix::fs::symlink("note.txt", src.join("nested/link.txt")).unwrap();

    let motd_text: &[u8] = b"put by the older, flat command\n";
    let motd = scratch("recovery-motd.txt");
    std::fs::write(&motd, motd_text).unwrap();

    // ---- invocation 1: make the image ------------------------------------------------------
    tool(&["mkfs", img.to_str().unwrap(), "32"]);

    // ---- invocation 2: fill it, using UPSTREAM's archiver ----------------------------------
    tool(&["import", img.to_str().unwrap(), src.to_str().unwrap()]);

    // ---- invocation 3: the flat `put` the xtask fixture flow uses, still working ------------
    tool(&["put", img.to_str().unwrap(), "motd", motd.to_str().unwrap()]);

    // ---- invocation 4: list a directory that is not the root -------------------------------
    let listing = String::from_utf8(tool(&["ls", img.to_str().unwrap(), "nested"])).unwrap();
    let names: Vec<&str> = listing
        .lines()
        .map(|l| l.rsplit("  ").next().unwrap().trim())
        .collect();
    assert_eq!(
        names,
        vec!["deep", "empty-dir", "link.txt", "note.txt"],
        "ls of a nested directory listed the wrong thing:\n{listing}",
    );
    assert!(
        listing.contains("dir ") && listing.contains("file") && listing.contains("link"),
        "ls did not distinguish the three kinds:\n{listing}",
    );

    // ---- invocation 5: cat a multi-record file by path --------------------------------------
    let back = tool(&["cat", img.to_str().unwrap(), "nested/deep/large.bin"]);
    assert_eq!(
        back.len(),
        large.len(),
        "cat returned the wrong length for a 300 KiB file",
    );
    assert!(
        back == large,
        "cat of a file spanning several records did not match byte for byte",
    );

    // ---- invocation 6: extract the whole thing ---------------------------------------------
    tool(&["extract", img.to_str().unwrap(), "/", out.to_str().unwrap()]);

    // The comparison. Everything the host tree had, plus the file `put` added separately.
    let mut want = survey(&src);
    want.insert(
        PathBuf::from("motd"),
        format!("file mode 644 len {} {}", motd_text.len(), sum(motd_text)),
    );
    let got = survey(&out);
    assert_eq!(want, got, "the extracted tree is not the tree that went in",);

    // The symlink survived as a symlink, which the survey above asserts by value but which is
    // worth naming: a reader that followed links would have produced a second copy of note.txt.
    assert!(
        std::fs::symlink_metadata(out.join("nested/link.txt"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink came back as a regular file",
    );

    // ---- invocation 7: extract a subtree, not the whole image -------------------------------
    let sub = scratch("recovery-sub");
    tool(&[
        "extract",
        img.to_str().unwrap(),
        "nested/deep",
        sub.to_str().unwrap(),
    ]);
    assert_eq!(
        survey(&sub),
        survey(&src.join("nested/deep")),
        "extracting a subtree did not reproduce it",
    );

    // ---- the refusals ----------------------------------------------------------------------
    let err = tool_fails(&["cat", img.to_str().unwrap(), "nested"]);
    assert!(err.contains("is a directory"), "cat of a directory: {err}");

    let err = tool_fails(&["cat", img.to_str().unwrap(), "nested/../nested/note.txt"]);
    assert!(err.contains(".."), "`..` should be refused, got: {err}");

    let err = tool_fails(&["cat", img.to_str().unwrap(), "nested/nonesuch"]);
    assert!(!err.is_empty(), "a missing file must say something");

    let err = tool_fails(&["ls", img.to_str().unwrap(), "top.txt"]);
    assert!(err.contains("not a directory"), "ls of a file: {err}",);

    // ---- the image was not written by any of the reads --------------------------------------
    // Recovery reads open read-only, so an image on read-only media (or a disk you would rather
    // not write to) still reads. Proven by comparing the whole file across another read.
    let before = std::fs::metadata(&img).unwrap();
    let digest_before = sum(&std::fs::read(&img).unwrap());
    tool(&["ls", img.to_str().unwrap(), "/"]);
    tool(&["cat", img.to_str().unwrap(), "top.txt"]);
    let after = std::fs::metadata(&img).unwrap();
    assert_eq!(before.len(), after.len(), "a read changed the image size");
    assert_eq!(
        digest_before,
        sum(&std::fs::read(&img).unwrap()),
        "a read modified the image; the recovery paths must not write",
    );

    for p in [&src, &out, &sub] {
        let _ = std::fs::remove_dir_all(p);
    }
    let _ = std::fs::remove_file(&img);
    let _ = std::fs::remove_file(&motd);
}

/// The operational rule, made to speak for itself.
///
/// A reader must match the on-disk format version, and RedoxFS enforces that by making *every*
/// header invalid when the version differs, which the engine reports as ENOENT. "No such file or
/// directory" about a file that is plainly there is the wrong thing to be told while recovering a
/// backup, so the tool reads the version off the disk itself and names the mismatch. This test
/// forges that condition by bumping the version field in every header copy: the signature is still
/// there, the version no longer matches, and the message must say which is which.
#[test]
fn an_image_written_by_another_format_version_says_which() {
    let img = scratch("version.img");
    tool(&["mkfs", img.to_str().unwrap(), "8"]);

    let mut bytes = std::fs::read(&img).unwrap();
    let mut headers = 0;
    for block in 0..256usize {
        let off = block * 4096;
        if &bytes[off..off + 8] == b"RedoxFS\0" {
            let version = u64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap());
            bytes[off + 8..off + 16].copy_from_slice(&(version + 1).to_le_bytes());
            headers += 1;
        }
    }
    assert!(headers > 0, "a fresh image had no header to alter");
    std::fs::write(&img, &bytes).unwrap();

    let err = tool_fails(&["ls", img.to_str().unwrap(), "/"]);
    assert!(
        err.contains("on-disk format version"),
        "a version mismatch must be named, not reported as a missing file: {err}",
    );

    let _ = std::fs::remove_file(&img);
}
