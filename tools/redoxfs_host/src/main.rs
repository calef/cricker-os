//! `redoxfs_host`: make, inspect and **recover** RedoxFS images on the host (milestones 32, 57).
//!
//!     cargo run -p redoxfs_host -- mkfs    IMAGE SIZE_MIB
//!     cargo run -p redoxfs_host -- ls      IMAGE [PATH]
//!     cargo run -p redoxfs_host -- cat     IMAGE PATH
//!     cargo run -p redoxfs_host -- xattr   IMAGE PATH [NAME]
//!     cargo run -p redoxfs_host -- extract IMAGE PATH DEST
//!     cargo run -p redoxfs_host -- put     IMAGE PATH HOST_FILE
//!     cargo run -p redoxfs_host -- import  IMAGE HOST_DIR
//!
//! `ls`, `cat`, `xattr` and `extract` are the disaster-recovery half: they open the image
//! read-only, need no FUSE, no kernel extension, no root and no key, and behave identically on
//! macOS and Linux. `PATH` is always relative to the image root and `..` is refused.
//!
//! `xattr` is deliberately shaped like macOS's own `xattr(1)`: with no `NAME` it lists what is
//! attached, with a `NAME` it writes that attribute's bytes to stdout the way `cat` writes a file's.
//!
//! The logic lives in the library (src/lib.rs) so the round-trip tests exercise exactly what this
//! binary runs. See vendor/README.md for the 0.9.1 pin this is built against, and
//! notes/host-recovery.md for why that pin has to be kept with the backup.

use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: redoxfs_host mkfs    IMAGE SIZE_MIB");
    eprintln!("       redoxfs_host ls      IMAGE [PATH]");
    eprintln!("       redoxfs_host cat     IMAGE PATH");
    eprintln!("       redoxfs_host xattr   IMAGE PATH [NAME]");
    eprintln!("       redoxfs_host extract IMAGE PATH DEST");
    eprintln!("       redoxfs_host put     IMAGE PATH HOST_FILE");
    eprintln!("       redoxfs_host import  IMAGE HOST_DIR");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    let result = match strs.as_slice() {
        ["mkfs", image, size_mib] => {
            let Ok(mib) = size_mib.parse::<u64>() else {
                return usage();
            };
            redoxfs_host::mkfs(Path::new(image), mib * 1024 * 1024)
                .map(|()| eprintln!("created {image}: {mib} MiB, empty RedoxFS"))
        }
        ["ls", image] => list(image, "/"),
        ["ls", image, path] => list(image, path),
        ["cat", image, path] => redoxfs_host::cat(Path::new(image), path).map(|data| {
            use std::io::Write;
            // Bytes to stdout, verbatim: cat means cat, even for a binary payload.
            let _ = std::io::stdout().write_all(&data);
        }),
        ["xattr", image, path] => attrs(image, path),
        ["xattr", image, path, name] => attr_value(image, path, name),
        ["extract", image, path, dest] => {
            redoxfs_host::extract(Path::new(image), path, Path::new(dest)).map(|s| {
                // The attribute counts are always printed, including the zeroes, and that is the
                // point rather than noise: "0 attributes reattached" on a backup you know carried
                // some is the line that tells you the destination filesystem cannot hold them,
                // while a summary that mentioned them only when non-zero would look identical to a
                // backup that never had any.
                eprintln!(
                    "extracted {} to {dest}: {} files, {} directories, {} symlinks, {} bytes{}, \
                     {} attributes reattached{}{}",
                    path,
                    s.files,
                    s.dirs,
                    s.symlinks,
                    s.bytes,
                    if s.skipped > 0 {
                        format!(", {} entries skipped", s.skipped)
                    } else {
                        String::new()
                    },
                    s.attrs,
                    if s.attrs_skipped > 0 {
                        format!(", {} refused by the host", s.attrs_skipped)
                    } else {
                        String::new()
                    },
                    if s.kinds_dropped > 0 {
                        format!(", {} type codes dropped", s.kinds_dropped)
                    } else {
                        String::new()
                    },
                )
            })
        }
        ["put", image, path, host_file] => match std::fs::read(host_file) {
            Ok(data) => redoxfs_host::put(Path::new(image), path, &data)
                .map(|()| eprintln!("put {path}: {} bytes", data.len())),
            Err(e) => Err(format!("cannot read {host_file}: {e}")),
        },
        ["import", image, host_dir] => redoxfs_host::import(Path::new(image), Path::new(host_dir))
            .map(|()| eprintln!("imported {host_dir} into the root of {image}")),
        _ => return usage(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("redoxfs_host: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn list(image: &str, path: &str) -> Result<(), String> {
    redoxfs_host::ls(Path::new(image), path).map(|entries| {
        for e in entries {
            // `@` for "this one carries extended attributes", which is what macOS `ls -l` puts in
            // the same position. Borrowed on purpose: a reader who has seen it once elsewhere does
            // not have to learn a second convention here, and it says the metadata is there without
            // making anybody find out that `.cricker-attrs` exists.
            let marker = if e.attrs > 0 { '@' } else { ' ' };
            println!("{}{marker} {:>10}  {}", e.kind.label(), e.size, e.name);
        }
    })
}

/// What is attached, one line each. The type code is printed as hex and, when its four bytes are
/// printable, also as the four-character code BFS-style clients write (notes/xattr.md), because
/// `0x4353_5452` and `'CSTR'` are the same fact and only one of them is readable.
fn attrs(image: &str, path: &str) -> Result<(), String> {
    redoxfs_host::attrs(Path::new(image), path).map(|attrs| {
        for a in attrs {
            println!(
                "{:>10}  kind {:#010x}{}  {}",
                a.value.len(),
                a.kind,
                four_cc(a.kind),
                String::from_utf8_lossy(&a.name),
            );
        }
    })
}

/// One attribute's bytes to stdout, verbatim, exactly as `cat` writes a file's. An attribute value
/// is usually a binary blob (Apple's `FinderInfo` is 32 bytes of structure), so rendering it would
/// be lying about it.
fn attr_value(image: &str, path: &str, name: &str) -> Result<(), String> {
    let attrs = redoxfs_host::attrs(Path::new(image), path)?;
    let Some(attr) = attrs.into_iter().find(|a| a.name == name.as_bytes()) else {
        return Err(format!("{path}: no attribute named {name}"));
    };
    use std::io::Write;
    let _ = std::io::stdout().write_all(&attr.value);
    Ok(())
}

/// ` 'CSTR'` if all four bytes of the code are printable ASCII, otherwise nothing.
fn four_cc(kind: u32) -> String {
    let bytes = kind.to_be_bytes();
    if bytes.iter().all(|b| (0x20..0x7f).contains(b)) {
        format!(" '{}'", String::from_utf8_lossy(&bytes))
    } else {
        String::new()
    }
}
