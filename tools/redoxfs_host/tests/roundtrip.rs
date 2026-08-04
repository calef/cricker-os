//! The pin's witness: a RedoxFS image round trip on the host, against the vendored 0.9.1.
//!
//! mkfs an image, put a file, list it, read it back byte-identical, and prove the read went
//! through the filesystem rather than a cache by reopening the image for every operation (each
//! library call opens fresh; nothing is shared but the bytes on disk). This is the same engine
//! the FS server will serve images with, so a regression here is a phase-2 regression caught on
//! the host in milliseconds.

use std::path::PathBuf;

use redoxfs_host::Volume;

fn scratch_image(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("redoxfs_host-{}-{name}", std::process::id()))
}

#[test]
fn an_image_round_trips_a_file_through_the_real_engine() {
    let img = scratch_image("roundtrip.img");
    let _ = std::fs::remove_file(&img);

    // 8 MiB: small, but comfortably past the header ring + minimal tree floor.
    redoxfs_host::mkfs(&img, 8 * 1024 * 1024).expect("mkfs failed");

    // A payload larger than one 4 KiB block, so the write exercises real record logic, with a
    // recognizable head and a position-dependent tail so shifts and truncation cannot pass.
    let mut payload = b"made on the host, served on cricker-os\n".to_vec();
    payload.extend((0..10_000u32).map(|i| (i % 251) as u8));

    redoxfs_host::put(&img, "motd", &payload).expect("put failed");

    let listing = redoxfs_host::ls(Volume::image(&img), "/").expect("ls failed");
    let entry = listing
        .iter()
        .find(|e| e.name == "motd")
        .expect("put file missing from the root listing");
    assert_eq!(entry.size, payload.len() as u64, "listed size is wrong");
    assert_eq!(
        entry.kind,
        redoxfs_host::Kind::File,
        "a file came back as something else"
    );

    let back = redoxfs_host::cat(Volume::image(&img), "motd").expect("cat failed");
    assert_eq!(
        back, payload,
        "the payload did not round trip byte-identical"
    );

    // Overwrite must truncate, not append or splice.
    let short = b"rewritten".to_vec();
    redoxfs_host::put(&img, "motd", &short).expect("overwrite failed");
    let back = redoxfs_host::cat(Volume::image(&img), "motd").expect("cat after overwrite failed");
    assert_eq!(back, short, "overwrite did not truncate the old contents");

    // A name nobody put there is a clean error, not a panic or empty file.
    assert!(
        redoxfs_host::cat(Volume::image(&img), "nonesuch").is_err(),
        "reading a missing file did not error",
    );

    let _ = std::fs::remove_file(&img);
}
