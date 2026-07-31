//! `redoxfs-host`: make, inspect and **recover** RedoxFS images on the host (milestones 32, 57).
//!
//!     cargo run -p redoxfs-host -- mkfs    IMAGE SIZE_MIB
//!     cargo run -p redoxfs-host -- ls      IMAGE [PATH]
//!     cargo run -p redoxfs-host -- cat     IMAGE PATH
//!     cargo run -p redoxfs-host -- extract IMAGE PATH DEST
//!     cargo run -p redoxfs-host -- put     IMAGE PATH HOST_FILE
//!     cargo run -p redoxfs-host -- import  IMAGE HOST_DIR
//!
//! `ls`, `cat` and `extract` are the disaster-recovery half: they open the image read-only, need
//! no FUSE, no kernel extension, no root and no key, and behave identically on macOS and Linux.
//! `PATH` is always relative to the image root and `..` is refused.
//!
//! The logic lives in the library (src/lib.rs) so the round-trip tests exercise exactly what this
//! binary runs. See vendor/README.md for the 0.9.1 pin this is built against, and
//! notes/host-recovery.md for why that pin has to be kept with the backup.

use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: redoxfs-host mkfs    IMAGE SIZE_MIB");
    eprintln!("       redoxfs-host ls      IMAGE [PATH]");
    eprintln!("       redoxfs-host cat     IMAGE PATH");
    eprintln!("       redoxfs-host extract IMAGE PATH DEST");
    eprintln!("       redoxfs-host put     IMAGE PATH HOST_FILE");
    eprintln!("       redoxfs-host import  IMAGE HOST_DIR");
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
        ["extract", image, path, dest] => {
            redoxfs_host::extract(Path::new(image), path, Path::new(dest)).map(|s| {
                eprintln!(
                    "extracted {} to {dest}: {} files, {} directories, {} symlinks, {} bytes{}",
                    path,
                    s.files,
                    s.dirs,
                    s.symlinks,
                    s.bytes,
                    if s.skipped > 0 {
                        format!(", {} skipped", s.skipped)
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
            eprintln!("redoxfs-host: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn list(image: &str, path: &str) -> Result<(), String> {
    redoxfs_host::ls(Path::new(image), path).map(|entries| {
        for e in entries {
            println!("{} {:>10}  {}", e.kind.label(), e.size, e.name);
        }
    })
}
