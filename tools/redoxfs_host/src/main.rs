//! `redoxfs_host`: make, inspect and **recover** RedoxFS images and disks on the host
//! (milestones 32, 57, 110).
//!
//!     cargo run -p redoxfs_host -- mkfs       IMAGE SIZE_MIB
//!     cargo run -p redoxfs_host -- partitions DEVICE
//!     cargo run -p redoxfs_host -- ls         VOLUME [PATH]
//!     cargo run -p redoxfs_host -- cat        VOLUME PATH
//!     cargo run -p redoxfs_host -- xattr      VOLUME PATH [NAME]
//!     cargo run -p redoxfs_host -- extract    VOLUME PATH DEST
//!     cargo run -p redoxfs_host -- put        IMAGE PATH HOST_FILE
//!     cargo run -p redoxfs_host -- import     IMAGE HOST_DIR
//!
//! `ls`, `cat`, `xattr` and `extract` are the disaster-recovery half: they open the volume
//! read-only, need no FUSE, no kernel extension, no root and no key, and behave identically on
//! macOS and Linux. `PATH` is always relative to the filesystem root and `..` is refused.
//!
//! **A `VOLUME` is an image file, or a device plus a partition** (milestone 110). With no selector
//! the bytes at offset zero are the filesystem, which is what an image is. With `--partition N` or
//! `--partition-type GUID` the table at offset zero is read first and the filesystem is the one
//! inside that partition, which is what a disk pulled out of the board is. `partitions DEVICE`
//! prints the table so there is something to choose from. **Both flag names are PROVISIONAL.**
//!
//! The selector is **refused on `mkfs`, `put` and `import`**, which are the verbs that open
//! read-write. Recovery never writes, and opening a *device* read-write by accident is a
//! considerably worse mistake than opening an image read-write.
//!
//! `xattr` is deliberately shaped like macOS's own `xattr(1)`: with no `NAME` it lists what is
//! attached, with a `NAME` it writes that attribute's bytes to stdout the way `cat` writes a file's.
//!
//! The logic lives in the library (src/lib.rs) so the round-trip tests exercise exactly what this
//! binary runs. See vendor/README.md for the 0.9.1 pin this is built against, and
//! notes/host-recovery.md for why that pin has to be kept with the backup.

use std::path::Path;
use std::process::ExitCode;

use redoxfs_host::{PartitionSelector, Volume};

fn usage() -> ExitCode {
    eprintln!("usage: redoxfs_host mkfs       IMAGE SIZE_MIB");
    eprintln!("       redoxfs_host partitions DEVICE");
    eprintln!("       redoxfs_host ls         VOLUME [PATH]");
    eprintln!("       redoxfs_host cat        VOLUME PATH");
    eprintln!("       redoxfs_host xattr      VOLUME PATH [NAME]");
    eprintln!("       redoxfs_host extract    VOLUME PATH DEST");
    eprintln!("       redoxfs_host put        IMAGE PATH HOST_FILE");
    eprintln!("       redoxfs_host import     IMAGE HOST_DIR");
    eprintln!();
    eprintln!("VOLUME is IMAGE, or DEVICE with one of:");
    eprintln!("       --partition N          the partition number `partitions` prints (1-based)");
    eprintln!("       --partition-type GUID  its type GUID, e.g. for a cricker-os data partition");
    ExitCode::FAILURE
}

/// Why a device cannot be opened for writing here. Named once, because all three write verbs give
/// the same answer.
const NO_WRITE_TO_A_PARTITION: &str = "a partition selector is refused on mkfs, put and import: \
     those verbs open read-write, recovery never does, and opening a device read-write by accident \
     is worse than the inconvenience (notes/host-recovery.md)";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let selector = match take_selector(&mut args) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("redoxfs_host: {msg}");
            return usage();
        }
    };
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    let result = match strs.as_slice() {
        // The write verbs first, so that a selector on one of them is refused rather than silently
        // ignored by an arm that never looks at it.
        ["mkfs", ..] | ["put", ..] | ["import", ..] if selector.is_some() => {
            Err(NO_WRITE_TO_A_PARTITION.to_string())
        }
        ["partitions", device] => table(device),
        ["mkfs", image, size_mib] => {
            let Ok(mib) = size_mib.parse::<u64>() else {
                return usage();
            };
            redoxfs_host::mkfs(Path::new(image), mib * 1024 * 1024)
                .map(|()| eprintln!("created {image}: {mib} MiB, empty RedoxFS"))
        }
        ["ls", image] => list(vol(image, selector), "/"),
        ["ls", image, path] => list(vol(image, selector), path),
        ["cat", image, path] => redoxfs_host::cat(vol(image, selector), path).map(|data| {
            use std::io::Write;
            // Bytes to stdout, verbatim: cat means cat, even for a binary payload.
            let _ = std::io::stdout().write_all(&data);
        }),
        ["xattr", image, path] => attrs(vol(image, selector), path),
        ["xattr", image, path, name] => attr_value(vol(image, selector), path, name),
        ["extract", image, path, dest] => {
            redoxfs_host::extract(vol(image, selector), path, Path::new(dest)).map(|s| {
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

/// A `VOLUME` argument and the selector that came with it.
fn vol(path: &str, selector: Option<PartitionSelector>) -> Volume<'_> {
    Volume {
        path: Path::new(path),
        partition: selector,
    }
}

/// Pull the partition selector out of the arguments, leaving the positional ones behind.
///
/// Scanned rather than positional so a flag can go before or after the verb, which is what a person
/// who has already typed the command and wants to add a partition to it will do. `--partition=3`
/// and `--partition 3` both work, because half the tools in the world take one and half the other.
fn take_selector(args: &mut Vec<String>) -> Result<Option<PartitionSelector>, String> {
    let mut found: Option<PartitionSelector> = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        let selector = match flag.as_str() {
            "--partition" => {
                let value = value_for(&flag, inline, &mut it)?;
                let n: u32 = value.parse().map_err(|_| {
                    format!(
                        "--partition wants a partition number, not {value:?}. It is the number \
                         `redoxfs_host partitions` prints, the same one as in /dev/sda2."
                    )
                })?;
                PartitionSelector::Index(n)
            }
            "--partition-type" => {
                let value = value_for(&flag, inline, &mut it)?;
                let guid = gpt::Guid::try_from_ascii(value.as_bytes()).ok_or_else(|| {
                    format!("--partition-type wants a 36-character type GUID, not {value:?}")
                })?;
                PartitionSelector::Type(guid)
            }
            _ => {
                rest.push(arg);
                continue;
            }
        };
        if found.is_some() {
            // Two selectors could name two different partitions, and picking one of them silently
            // is how a recovery reads the wrong half of a disk.
            return Err("give one partition selector, not two".to_string());
        }
        found = Some(selector);
    }
    *args = rest;
    Ok(found)
}

/// The value of a flag: from `--flag=value` if it was written that way, otherwise the next argument.
fn value_for(
    flag: &str,
    inline: Option<String>,
    it: &mut impl Iterator<Item = String>,
) -> Result<String, String> {
    match inline {
        Some(v) => Ok(v),
        None => it.next().ok_or_else(|| format!("{flag} wants a value")),
    }
}

/// The partition table, so a person can see what is on a disk before choosing what to read from it.
///
/// The type is printed by name where this project recognises it and as a raw GUID where it does not,
/// because an unrecognised GUID is a string somebody can look up and "unknown" is not. The name
/// column is empty on most real disks: macOS writes no GPT partition names at all (notes/gpt.md).
fn table(device: &str) -> Result<(), String> {
    let table = redoxfs_host::partitions(Path::new(device))?;
    println!(
        "{}-byte logical blocks, disk GUID {}",
        table.block_size,
        String::from_utf8_lossy(&table.disk_guid.to_ascii()),
    );
    println!("  #   first LBA    last LBA          size  type");
    for p in &table.partitions {
        let kind = p
            .type_name
            .map(str::to_string)
            .unwrap_or_else(|| String::from_utf8_lossy(&p.type_guid.to_ascii()).into_owned());
        println!(
            "{:>3}  {:>10}  {:>10}  {:>12}  {kind}{}",
            p.number,
            p.first_lba,
            p.last_lba,
            human(p.bytes),
            if p.name.is_empty() {
                String::new()
            } else {
                format!("  {}", p.name)
            },
        );
    }
    if table.partitions.is_empty() {
        println!("(the table is valid and has no used entries)");
    }
    Ok(())
}

/// A size a person can compare at a glance. Binary units, which is what every partition tool prints
/// and what the LBA arithmetic is in.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn list(volume: Volume, path: &str) -> Result<(), String> {
    redoxfs_host::ls(volume, path).map(|entries| {
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
fn attrs(volume: Volume, path: &str) -> Result<(), String> {
    redoxfs_host::attrs(volume, path).map(|attrs| {
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
fn attr_value(volume: Volume, path: &str, name: &str) -> Result<(), String> {
    let attrs = redoxfs_host::attrs(volume, path)?;
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
