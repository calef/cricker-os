//! **The attribute half of the recovery witness** (milestone 57): metadata written by the FS server
//! on one side, and back on a real host file on the other.
//!
//! The claim this closes is Chris's, and it is the one that decides whether the backup is credible:
//! *if I am struggling to get the data off, the backup has failed at its job.* Before this, the
//! attributes came out of an image as `.cricker-attrs/0000002a`, a blob whose owner you had to work
//! out by hand. A Time Machine sparsebundle carries Apple metadata in exactly those attributes, so
//! "by hand" was the recovery story for the part of the backup a Mac needs to make sense of the rest.
//!
//! **Who writes the fixture matters.** It is `fs_server::Server`, the sans-IO core that runs on the
//! board, driven here over a `DiskFile`. Not a second writer in this crate: a reader and a writer in
//! one crate can share a misunderstanding of the format and agree perfectly, which is exactly the
//! trap `recovery.rs` avoids by importing through upstream's archiver. The store is written by the
//! thing that will write it in the field, then a separate process reads it.
//!
//! **A host that cannot hold attributes is a case, not a skip.** If the destination filesystem
//! refuses them, this test asserts that the tool *said so*, loudly and with a count, rather than
//! reporting a clean extraction. DECISIONS §42 is the rule and this is where it is measured: the
//! failure mode that matters is a recovery that looks complete and is not.

use std::path::{Path, PathBuf};
use std::process::Command;

use fs_proto::{dir, fs, xattr};
use redoxfs::DiskFile;

const TOOL: &str = env!("CARGO_BIN_EXE_redoxfs-host");

/// One invocation of the built binary. Returns (stdout, stderr); panics on a non-zero exit.
fn tool(args: &[&str]) -> (String, String) {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "redoxfs-host {args:?} failed: {stderr}"
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), stderr)
}

fn tool_bytes(args: &[&str]) -> Vec<u8> {
    let out = Command::new(TOOL)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run the tool: {e}"));
    assert!(
        out.status.success(),
        "redoxfs-host {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("redoxfs-attrs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    let _ = std::fs::remove_file(&p);
    p
}

/// Whether this host, on this temporary directory, will hold an extended attribute at all.
///
/// Asked rather than assumed, because the answer is a property of the *filesystem* and not of the
/// operating system: APFS and ext4 hold them, and a Linux `tmpfs` older than 6.6 does not hold
/// `user.*` ones, which is exactly what `/tmp` is on a lot of CI runners. Getting this wrong would
/// present as a mystery failure in a test about metadata.
fn host_holds_attributes(dir: &Path) -> bool {
    let probe = dir.join("attribute-probe");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&probe, b"probe").unwrap();
    let ok = redoxfs_host::host_xattr::set(&probe, b"user.cricker.probe", b"1").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// A 'CSTR' type code, BFS's spelling of a string, which is the kind of thing the `u32` exists to
/// carry (notes/xattr.md). Nothing in the layer interprets it; the point here is that it round-trips
/// through the image and that the host cannot hold it, both of which the tool has to be honest about.
const CSTR: u32 = 0x4353_5452;

/// Deterministic bytes, so a 1 KiB value is a real 1 KiB value and not a run lz4 collapses.
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

#[test]
fn attributes_written_by_the_fs_server_come_back_onto_host_files() {
    let src = scratch("src");
    let img = scratch("attrs.img");
    let out = scratch("out");

    // ---- a small tree, laid down by upstream's archiver ------------------------------------
    std::fs::create_dir_all(src.join("nested")).unwrap();
    std::fs::write(src.join("top.txt"), b"the file with the metadata\n").unwrap();
    std::fs::write(src.join("nested/note.txt"), b"one level down\n").unwrap();
    tool(&["mkfs", img.to_str().unwrap(), "16"]);
    tool(&["import", img.to_str().unwrap(), src.to_str().unwrap()]);

    // ---- the attributes, written by the FS SERVER's own core --------------------------------
    let finder = noise(1024, 0x1234_5678);
    {
        let disk = DiskFile::open(&img).expect("open the image read-write");
        let mut srv = fs_server::Server::open(disk).expect("mount the image as the FS server does");

        let top = srv.open_file("top.txt").unwrap();
        srv.set_xattr(top, b"user.com.apple.FinderInfo", xattr::RAW, &finder)
            .unwrap();
        srv.set_xattr(
            top,
            b"user.com.apple.metadata:tags",
            CSTR,
            b"green,backed-up",
        )
        .unwrap();

        let nested = srv
            .open_dir(fs::ROOT as u32, "nested", dir::ALL)
            .expect("descend into nested");
        srv.set_xattr(nested, b"user.cricker.kind", xattr::RAW, b"directory")
            .unwrap();

        let note = srv.open_file_at(nested, "note.txt").unwrap();
        srv.set_xattr(note, b"user.cricker.note", xattr::RAW, b"deep")
            .unwrap();
    }

    // ---- the listing says which entries carry metadata --------------------------------------
    let (listing, _) = tool(&["ls", img.to_str().unwrap(), "/"]);
    let top_line = listing
        .lines()
        .find(|l| l.ends_with("top.txt"))
        .unwrap_or_else(|| panic!("top.txt is missing from the listing:\n{listing}"));
    assert!(
        top_line.contains('@'),
        "a file with attributes must be marked in the listing, got: {top_line}",
    );
    let store_line = listing
        .lines()
        .find(|l| l.ends_with(xattr::STORE_DIR))
        .unwrap_or_else(|| panic!("the store is missing from the listing:\n{listing}"));
    assert!(
        !store_line.contains('@'),
        "the store directory itself carries no attributes: {store_line}",
    );

    // ---- `xattr` renders them, so a recovery never has to open a blob by hand ----------------
    let (rendered, _) = tool(&["xattr", img.to_str().unwrap(), "top.txt"]);
    assert!(
        rendered.contains("user.com.apple.FinderInfo") && rendered.contains("1024"),
        "the listing must name the attribute and its length:\n{rendered}",
    );
    assert!(
        rendered.contains("'CSTR'"),
        "a printable type code is rendered as its four-character code:\n{rendered}",
    );
    let value = tool_bytes(&[
        "xattr",
        img.to_str().unwrap(),
        "top.txt",
        "user.com.apple.FinderInfo",
    ]);
    assert_eq!(
        value, finder,
        "`xattr IMAGE PATH NAME` must write the value verbatim, as `cat` does",
    );

    // ---- the extraction ---------------------------------------------------------------------
    let supported = host_holds_attributes(&scratch("probe-dir"));
    let (_, summary) = tool(&["extract", img.to_str().unwrap(), "/", out.to_str().unwrap()]);

    if !supported {
        // The §42 case. The files came out; the metadata did not, and the tool has to say that
        // rather than report a clean run. A recovery that looks complete and is not is the failure
        // this whole feature exists to avoid.
        assert!(
            summary.contains("0 attributes reattached") && summary.contains("refused by the host"),
            "on a host that holds no attributes the summary must say so, got:\n{summary}",
        );
        eprintln!(
            "note: {} does not hold extended attributes, so this run proved the refusal path \
             rather than the round trip",
            out.display(),
        );
        cleanup(&[&src, &out], &img);
        return;
    }

    assert!(
        summary.contains("4 attributes reattached"),
        "four attributes went in and four must come back, got:\n{summary}",
    );
    assert!(
        summary.contains("1 type codes dropped"),
        "the one non-RAW type code must be reported as dropped, got:\n{summary}",
    );
    assert!(
        !summary.contains("refused by the host"),
        "nothing should have been refused on this host:\n{summary}",
    );

    // ---- the round trip, closed on the host file --------------------------------------------
    let get = |p: &Path, n: &str| {
        redoxfs_host::host_xattr::get(p, n.as_bytes())
            .unwrap_or_else(|e| panic!("{}: cannot read back {n}: {e}", p.display()))
    };
    assert_eq!(
        get(&out.join("top.txt"), "user.com.apple.FinderInfo"),
        finder,
        "a 1 KiB attribute did not survive the trip onto the host file",
    );
    assert_eq!(
        get(&out.join("top.txt"), "user.com.apple.metadata:tags"),
        b"green,backed-up",
        "the typed attribute's bytes must survive even though its type code cannot",
    );
    assert_eq!(
        get(&out.join("nested"), "user.cricker.kind"),
        b"directory",
        "a DIRECTORY's attributes must be reattached too, not only a file's",
    );
    assert_eq!(
        get(&out.join("nested/note.txt"), "user.cricker.note"),
        b"deep",
        "an attribute below the top level did not come back",
    );

    // The type code is not lost, it is relocated: the raw store comes out with the tree, and the
    // format is published in `fs_proto::xattr::store`. That is what makes "dropped" honest rather
    // than lossy, so the test asserts the fallback is really there.
    let blobs = std::fs::read_dir(out.join(xattr::STORE_DIR))
        .expect("the raw store must be extracted alongside, as the type codes' only home")
        .count();
    assert_eq!(
        blobs, 3,
        "one blob per node that carries attributes: top.txt, nested, nested/note.txt",
    );

    // ---- confirmed by a program we did not write --------------------------------------------
    // Our own getter agreeing with our own setter proves less than it looks. macOS ships `xattr(1)`;
    // where it exists, ask it. Where it does not, the assertion above stands alone and says so.
    if Path::new("/usr/bin/xattr").exists() {
        let out_bytes = Command::new("/usr/bin/xattr")
            .args(["-l", out.join("nested/note.txt").to_str().unwrap()])
            .output()
            .expect("run the system xattr");
        let listed = String::from_utf8_lossy(&out_bytes.stdout);
        assert!(
            listed.contains("user.cricker.note"),
            "the system's own xattr(1) does not see the attribute we reattached:\n{listed}",
        );
    }

    cleanup(&[&src, &out], &img);
}

fn cleanup(dirs: &[&PathBuf], img: &Path) {
    for d in dirs {
        let _ = std::fs::remove_dir_all(d);
    }
    let _ = std::fs::remove_dir_all(scratch("probe-dir"));
    let _ = std::fs::remove_file(img);
}
