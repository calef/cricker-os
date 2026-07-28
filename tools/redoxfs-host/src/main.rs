//! `redoxfs-host`: make and inspect RedoxFS images on the host (milestone 32).
//!
//!     cargo run -p redoxfs-host -- mkfs  IMAGE SIZE_MIB
//!     cargo run -p redoxfs-host -- ls    IMAGE
//!     cargo run -p redoxfs-host -- put   IMAGE NAME HOST_FILE
//!     cargo run -p redoxfs-host -- cat   IMAGE NAME
//!
//! The logic lives in the library (src/lib.rs) so the round-trip test exercises exactly what
//! this binary runs. See vendor/README.md for the 0.9.1 pin this is built against.

use std::path::Path;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: redoxfs-host mkfs IMAGE SIZE_MIB");
    eprintln!("       redoxfs-host ls   IMAGE");
    eprintln!("       redoxfs-host put  IMAGE NAME HOST_FILE");
    eprintln!("       redoxfs-host cat  IMAGE NAME");
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
        ["ls", image] => redoxfs_host::ls(Path::new(image)).map(|entries| {
            for e in entries {
                let kind = if e.is_dir { "dir " } else { "file" };
                println!("{kind} {:>10}  {}", e.size, e.name);
            }
        }),
        ["put", image, name, host_file] => match std::fs::read(host_file) {
            Ok(data) => redoxfs_host::put(Path::new(image), name, &data)
                .map(|()| eprintln!("put {name}: {} bytes", data.len())),
            Err(e) => Err(format!("cannot read {host_file}: {e}")),
        },
        ["cat", image, name] => redoxfs_host::cat(Path::new(image), name).map(|data| {
            use std::io::Write;
            // Bytes to stdout, verbatim: cat means cat, even for a binary payload.
            let _ = std::io::stdout().write_all(&data);
        }),
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
