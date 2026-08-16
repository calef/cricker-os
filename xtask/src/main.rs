//! Build orchestration for nife.
//!
//! A normal Rust binary that runs on the *host*. Building a kernel means a custom
//! target, a linker script, and driving QEMU with the right flags, none of which fits
//! neatly into `cargo build`. This beats a Makefile because it's Rust and it composes.
//! See DECISIONS.md §7.
//!
//!     cargo xtask run      boot the kernel (the milestone tour), print to this terminal
//!     cargo xtask shell    boot straight to the interactive shell (add --hvf for the real core)
//!     cargo xtask shell-check  boot that same shell, type at it, and check what it answered
//!     cargo xtask test     host tests (milliseconds), then the kernel under QEMU
//!                          (--hvf runs the aarch64 kernel leg on the physical core)
//!     cargo xtask gdb      boot paused, waiting for a debugger on :1234
//!     cargo xtask objdump  disassemble the kernel
//!     cargo xtask image    build the flat arm64 Image and dump its header
//!
//! Note that `run` and `test` do NOT invoke QEMU themselves. They just call cargo,
//! which invokes `scripts/qemu-runner-aarch64.sh` via the runner setting in
//! `.cargo/config.toml`. That script is the single source of truth for how the kernel
//! gets booted, so there is exactly one place to get the QEMU flags wrong.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const RUNNER: &str = "scripts/qemu-runner-aarch64.sh";

/// The RISC-V target, for the second-architecture initrd (milestone 20). The kernel itself is built
/// and run through cargo + `scripts/qemu-runner-riscv64.sh` directly, not this xtask; this const exists
/// only so `initrd-riscv` builds the userspace archive for the matching target.
const RISCV_TARGET: &str = "riscv64imac-unknown-none-elf";

/// Whether this run builds optimized binaries. Only `bench --release` sets it (a fair cross-OS
/// comparison wants an optimized kernel and userspace, not the debug default). Everything else stays
/// debug: faster builds, and the tests and the tour want debuginfo and cheap rebuilds.
static RELEASE: AtomicBool = AtomicBool::new(false);

/// `"release"` or `"debug"`: the cargo profile directory the built artifacts land in.
fn profile_dir() -> &'static str {
    if RELEASE.load(Ordering::Relaxed) {
        "release"
    } else {
        "debug"
    }
}

/// Run `cargo <args>`, adding `--release` when this is a release run. For the build commands whose
/// output profile must match `profile_dir()` (the kernel and user builds behind `bench --release`).
fn cargo_profiled(args: &[&str]) -> bool {
    let mut v = args.to_vec();
    if RELEASE.load(Ordering::Relaxed) {
        v.push("--release");
    }
    cargo(&v)
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    let ok = match cmd.as_str() {
        "build" => build(),
        "run" => {
            maybe_hvf();
            // Build the disk and the initrd first: the kernel boots with them, and `cargo run`
            // would not rebuild them on its own (the kernel does not depend on them in cargo).
            mkdisk() && user() && cargo(&["run", "-p", "kernel", "--target", TARGET])
        }
        "shell" => {
            // Boot straight to the interactive shell (the milestone tour compiled out).
            maybe_hvf();
            eprintln!("--- booting nife to an interactive shell (type `help`, Ctrl-C to quit) ---");
            // **The filesystem the prompt's `>` and `<` need** (milestone 50). The FS server first,
            // because `user()` packs the initrd and the boot loads it out of there by name, and the
            // RedoxFS image because the runner attaches it only when the file exists. Both are
            // rebuilt per boot, so the prompt always meets a fresh fixture rather than whatever the
            // last session wrote.
            fs_server_build(TARGET)
                && mkredoxfs()
                && mkdisk()
                && user()
                && cargo(&[
                    "run",
                    "-p",
                    "kernel",
                    "--features",
                    "shell",
                    "--target",
                    TARGET,
                ])
        }
        "initboot" => {
            // Milestone 19d.2c: boot with userspace init as the boot path (it brings up the
            // console). Add --hvf for the real core.
            maybe_hvf();
            eprintln!("--- booting nife via userspace init (Ctrl-C to quit) ---");
            mkdisk()
                && user()
                && cargo(&[
                    "run",
                    "-p",
                    "kernel",
                    "--features",
                    "initboot",
                    "--target",
                    TARGET,
                ])
        }
        "smb-serve" => {
            // The SMB serve boot (milestone 54): the guest serves its share on port 445 and QEMU
            // forwards a fixed host port into it, so a Mac (this one included) can attempt a real
            // mount. Fixed rather than free: a mount instruction with a port that changes per run
            // is an instruction nobody can follow. Ctrl-C quits; see notes/smb.md.
            maybe_hvf();
            eprintln!("--- booting nife as an SMB file server (milestone 54, Ctrl-C to quit) ---");
            eprintln!("    mount: smb://127.0.0.1:10445/share  (connect as Guest; notes/smb.md)");
            // SAFETY: `set_var` became unsafe in edition 2024 because it races other threads.
            // xtask is single-threaded here: main thread, before the child that reads these.
            unsafe {
                std::env::set_var("NIFE_SMB_HOSTFWD_PORT", "10445");
                std::env::set_var("NIFE_SMB_GUEST_PORT", "445");
            }
            // The FS server and the RedoxFS image, exactly as `shell` builds them: with the
            // disk attached the share the Mac mounts is the real filesystem (milestone 54's
            // point); without it the boot announces its fixture fallback and serves that.
            fs_server_build(TARGET)
                && mkredoxfs()
                && mkdisk()
                && user()
                && cargo(&[
                    "run",
                    "-p",
                    "kernel",
                    "--features",
                    "smb_serve",
                    "--target",
                    TARGET,
                ])
        }
        "initrd-riscv" => initrd_riscv(),
        // The documentation store (milestone 40): build it, print what it costs, and optionally
        // answer a query against it with the same reader the guest uses.
        "manual" => manual_store(std::env::args().nth(2)),
        "std-src" => std_src(),
        // Print the farm's input stamp and exit. Exists so that "the stamp does not depend on where
        // the checkout lives" is a claim anyone can CHECK rather than one they have to believe:
        //   cargo xtask std-stamp                        # in the main checkout
        //   git worktree add /tmp/w HEAD && (cd /tmp/w && cargo xtask std-stamp)
        // The two must print the same value. If they ever diverge, something location-dependent has
        // crept back into `std_inputs_stamp`, and `nife-dev` will start being stolen again.
        "std-stamp" => {
            println!("{:016x}", std_inputs_stamp());
            true
        }
        "std-exerciser" => std_exerciser(),
        "shell-check" => shell_check(),
        "test" => test(),
        "undefined-behavior-check" => undefined_behavior_check(),
        "bench" => bench(),
        "gdb" => gdb(),
        "objdump" => objdump(),
        "image" => image(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!(
                "usage: cargo xtask <build|run|shell|shell-check|initboot|smb-serve|initrd-riscv|std-src|std-stamp|std-exerciser|test|undefined-behavior-check|bench|gdb|objdump|image> [--hvf]"
            );
            eprintln!("       cargo xtask shell-check [--arch aarch64|riscv64]");
            eprintln!(
                "       cargo xtask undefined-behavior-check [extra cargo-miri-test args, e.g. -p <crate>]"
            );
            eprintln!(
                "       cargo xtask bench [--riscv] [--real] [--release] [--smp] [--check] [--save]"
            );
            eprintln!(
                "       cargo xtask test [--arch aarch64|riscv64] [--cpu <qemu-cpu-model>] [--hvf]"
            );
            return ExitCode::FAILURE;
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn build() -> bool {
    // The user program and the disk image first: the kernel boots with the program as an initrd
    // and reads the disk over virtio, so both have to exist before it runs.
    mkdisk() && user() && cargo(&["build", "-p", "kernel", "--target", TARGET])
}

/// Build the userspace ELF that the kernel will load at milestone 7.
///
/// It is a **separate crate with its own linker script** (linked at `0x40_0000`, in the low half,
/// where `TTBR0` lives), so it cannot accidentally share anything with the kernel. And it stays
/// an **ELF**: the kernel's loader wants program headers, unlike the kernel itself, which QEMU
/// wants as a flat image. See notes/elf.md.
fn user() -> bool {
    cargo_profiled(&["build", "-p", "user", "--target", TARGET]) && mkinitrd()
}

// ===========================================================================================
// Rust `std` on the native ABI (milestone 27).
//
// std's Platform Abstraction Layer for nife lives in patches/std-nife (the Hermit shape:
// a `sys` backend on the capability ABI, not a libc shim). `std-src` materializes a patched
// rust-src into a linked `nife-dev` toolchain; `std-exerciser` builds the `std_exerciser` program for the
// custom targets with -Zbuild-std against it. See notes/std.md.
// ===========================================================================================

/// The custom-target triples the std demo builds for, one per supported ISA. The name is the
/// JSON spec's file stem, which is also cargo's target-dir subdirectory.
const STD_TARGETS: [&str; 2] = ["aarch64-unknown-nife", "riscv64-unknown-nife"];

/// The linked toolchain name (`rustup toolchain link`) whose rust-src carries the nife PAL.
const NIFE_TOOLCHAIN: &str = "nife-dev";

/// Bump to force every farm to rebuild after a change to the patch logic itself (not the inputs).
const STD_SRC_PATCH_VERSION: u32 = 5;

fn farm_dir() -> PathBuf {
    workspace_root().join("target/nife-farm")
}

/// The real nightly sysroot the farm is hardlink-cloned from.
fn real_sysroot() -> Option<PathBuf> {
    capture("rustc", &["--print", "sysroot"]).map(|s| PathBuf::from(s.trim()))
}

/// The farm's patched std source root (`.../library/std/src`).
fn farm_std_src() -> PathBuf {
    farm_dir().join("lib/rustlib/src/rust/library/std/src")
}

/// The `std_exerciser` ELF for a given custom-target triple. `std_exerciser` is its own workspace, so its
/// artifacts land under `std_exerciser/target/<triple>/release/`.
fn std_exerciser_elf(triple: &str) -> PathBuf {
    workspace_root().join(format!(
        "std_exerciser/target/{triple}/release/std_exerciser"
    ))
}

/// A cheap FNV-1a over a byte slice, folded into the running hash. No crypto, no dep: this only
/// needs to notice when a PAL input changed so the farm (and thus the build-std cache) is rebuilt.
fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Hash everything that determines the farm's contents: the toolchain version, the patch-logic
/// version, the ABI/heap crates copied in verbatim, the target specs, and every overlay file.
/// A mismatch means the linked toolchain is stale and std must be rebuilt from patched source.
fn std_inputs_stamp() -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    h = fnv(h, &STD_SRC_PATCH_VERSION.to_le_bytes());
    if let Some(v) = capture("rustc", &["-vV"]) {
        h = fnv(h, v.as_bytes());
    }
    let root = workspace_root();
    let mut files: Vec<PathBuf> = vec![
        root.join("crates/abi/src/lib.rs"),
        root.join("crates/user_heap/src/lib.rs"),
        // The net PAL generates its wire constants verbatim from the net_stack contract; a change to it
        // must rebuild the farm just like a change to the ABI crate.
        root.join("crates/socket_proto/src/lib.rs"),
        // Likewise the FS-service contract: `std::fs` is a client of it (milestone 27 phase two),
        // and its wire constants are generated verbatim into the PAL.
        root.join("crates/fs_proto/src/lib.rs"),
        // The wall-clock and entropy contracts, for the same reason: `sys/time` reads the clock
        // page's layout out of one and `sys/random` packs its requests with the other, so a change
        // to either must rebuild the farm or the PAL silently drifts from the service.
        root.join("crates/clock_proto/src/lib.rs"),
        root.join("crates/entropy_proto/src/lib.rs"),
        root.join("targets/aarch64-unknown-nife.json"),
        root.join("targets/riscv64-unknown-nife.json"),
    ];
    collect_files(&root.join("patches/std-nife/overlay"), &mut files);
    files.sort();
    for f in files {
        // **Hash the path RELATIVE to the workspace root, never the absolute path.** An absolute path
        // makes the stamp a function of *where the checkout lives*, so two trees with byte-identical
        // inputs never match, `std_src` rebuilds the farm unconditionally, and `rustup toolchain link`
        // repoints `nife-dev`, which is global to the machine, not to the worktree. That is the
        // race behind three broken toolchains on 2026-07-31: an agent worktree ran `script/test`, took
        // the link, and deleting that worktree left `nife-dev` dangling for everything else, failing
        // far from the cause as "override toolchain 'nife-dev' is not installed".
        //
        // The stamp is meant to answer "are the farm's *inputs* unchanged", and a checkout's location
        // is not one of its inputs. `strip_prefix` cannot fail here (every path is built from `root` or
        // collected beneath it), but fall back to the full path rather than panicking in a build tool.
        let rel = f.strip_prefix(&root).unwrap_or(&f);
        h = fnv(h, rel.to_string_lossy().as_bytes());
        if let Ok(bytes) = std::fs::read(&f) {
            h = fnv(h, &bytes);
        }
    }
    h
}

/// Walk `dir` and push every regular file into `out` (used to fingerprint the overlay tree).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// **Materialize the patched `nife-dev` toolchain** (milestone 27).
///
/// build-std reads std's source from the sysroot of the rustc it invokes, so a patched std means
/// a toolchain whose sysroot IS patched. We hardlink-clone the real nightly (`cp -al`, near-zero
/// disk since blocks are shared) so rustc resolves *this* directory as its sysroot, then replace
/// the `src` subtree with a real (independent-inode) copy and patch that copy: the overlay PAL
/// files, the ABI/heap crates generated verbatim, and a `target_os = "nife"` arm inserted into
/// std's `cfg_select!` dispatchers. The real toolchain is never touched.
///
/// Idempotent: a stamp of all inputs guards the rebuild, so a warm farm (and its build-std cache)
/// survives across runs and only a PAL change forces std to recompile.
fn std_src() -> bool {
    let stamp = std_inputs_stamp();
    let stamp_file = farm_dir().join(".nife-stamp");
    if farm_std_src().is_dir()
        && std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(&stamp.to_string())
    {
        return true;
    }

    let Some(real) = real_sysroot() else {
        eprintln!("std-src: cannot find the nightly sysroot (rustc --print sysroot)");
        return false;
    };
    let farm = farm_dir();
    eprintln!("--- std-src: building the patched nife-dev toolchain (this recompiles std) ---");

    // Fresh farm. `cp -al` clones bin+lib as hardlinks; the src subtree is then a real copy so
    // patching it never mutates the shared rustup toolchain.
    let _ = std::fs::remove_dir_all(&farm);
    if let Err(e) = std::fs::create_dir_all(&farm) {
        eprintln!("std-src: cannot create {}: {e}", farm.display());
        return false;
    }
    let cp = |args: &[&str]| run("cp", args);
    if !cp(&["-al", &s(real.join("bin")), &s(farm.join("bin"))])
        || !cp(&["-al", &s(real.join("lib")), &s(farm.join("lib"))])
    {
        eprintln!("std-src: hardlink-clone of the toolchain failed");
        return false;
    }
    let src = farm.join("lib/rustlib/src");
    let _ = std::fs::remove_dir_all(&src);
    if !cp(&["-R", &s(real.join("lib/rustlib/src")), &s(src)]) {
        eprintln!("std-src: real copy of rust-src failed");
        return false;
    }

    if !std_apply_overlay() || !std_generate_modules() || !std_patch_dispatch() {
        return false;
    }

    // Link (or relink) the farm as `nife-dev`. Idempotent: rustup replaces an existing link to
    // the same path.
    if !run(
        "rustup",
        &["toolchain", "link", NIFE_TOOLCHAIN, &s(farm.clone())],
    ) {
        eprintln!("std-src: `rustup toolchain link {NIFE_TOOLCHAIN}` failed");
        return false;
    }

    if let Err(e) = std::fs::write(&stamp_file, stamp.to_string()) {
        eprintln!("std-src: cannot write stamp {}: {e}", stamp_file.display());
        return false;
    }
    true
}

/// Path-to-string helper for the `cp`/`rustup` argument lists.
fn s(p: PathBuf) -> String {
    p.display().to_string()
}

/// Copy the PAL overlay (`patches/std-nife/overlay/std/src/...`) over the farm's std source.
fn std_apply_overlay() -> bool {
    let overlay = workspace_root().join("patches/std-nife/overlay/std/src");
    let dst_root = farm_std_src();
    let mut files = Vec::new();
    collect_files(&overlay, &mut files);
    for f in files {
        let rel = f.strip_prefix(&overlay).unwrap();
        let dst = dst_root.join(rel);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::copy(&f, &dst) {
            eprintln!("std-src: overlay copy {} failed: {e}", rel.display());
            return false;
        }
    }
    true
}

/// Generate `abi.rs` and `user_heap.rs` verbatim from the host-tested crates, so the ABI numbers and
/// the heap algorithm have exactly one definition. The transform strips crate-level inner
/// attributes (`#![no_std]`, illegal in a non-root module) and any trailing `#[cfg(test)]` module.
fn std_generate_modules() -> bool {
    let root = workspace_root();
    let jobs = [
        (
            root.join("crates/abi/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/abi.rs"),
        ),
        (
            root.join("crates/user_heap/src/lib.rs"),
            farm_std_src().join("sys/alloc/nife/user_heap.rs"),
        ),
        // The net_stack socket-contract wire format, verbatim, so the net PAL cannot drift from the
        // server it talks to (same discipline as the ABI and heap crates above).
        (
            root.join("crates/socket_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/netproto.rs"),
        ),
        // The FS-service wire protocol (DECISIONS §27), so `std::fs`'s PAL cannot drift from the
        // server it opens files through. Same discipline as the three above.
        (
            root.join("crates/fs_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/fsproto.rs"),
        ),
        // The wall-clock contract (DECISIONS §43), so the time PAL reads the clock page with the
        // same seqlock and the same layout the clock service publishes it with. Same discipline as
        // the four above; this one matters more than most, because a drift here would be a torn
        // read of a timestamp rather than a compile error.
        (
            root.join("crates/clock_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/clockproto.rs"),
        ),
        // The entropy contract (DECISIONS §44), so the random PAL packs its requests and reads its
        // replies exactly the way the entropy service serves them. Same discipline as the five
        // above; a drift here would be a program reading the wrong bytes as a key.
        (
            root.join("crates/entropy_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/entropyproto.rs"),
        ),
        // The byte-sink contract (milestone 50), so `println!`'s framing and the classification of
        // a failed SEND are one definition shared with every sink and with the kernel-side tests.
        // Same discipline as the six above, and the one that would hurt most to get wrong: a drift
        // in `GONE` would be a program that keeps printing into a pipe whose reader has exited.
        (
            root.join("crates/sink_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/nife/sinkproto.rs"),
        ),
    ];
    for (src, dst) in jobs {
        let Ok(text) = std::fs::read_to_string(&src) else {
            eprintln!("std-src: cannot read {}", src.display());
            return false;
        };
        let mut body: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("#!["))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(idx) = body.find("\n#[cfg(test)]\nmod tests") {
            body.truncate(idx);
        }
        let body = format!("{}\n", body.trim_end());
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&dst, body) {
            eprintln!("std-src: cannot write {}: {e}", dst.display());
            return false;
        }
    }
    true
}

/// Insert `text` immediately after the first occurrence of `anchor` in `path`.
fn patch_after(path: &Path, anchor: &str, insert: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("std-src: cannot read {}", path.display());
        return false;
    };
    let Some(pos) = text.find(anchor) else {
        eprintln!(
            "std-src: anchor not found in {} (std internals changed?): {anchor:?}",
            path.display()
        );
        return false;
    };
    let at = pos + anchor.len();
    let new = format!(
        "{}\n{}\n{}",
        &text[..at],
        insert.trim_end_matches('\n'),
        &text[at..]
    );
    if let Err(e) = std::fs::write(path, new) {
        eprintln!("std-src: cannot write {}: {e}", path.display());
        return false;
    }
    true
}

/// Add a `target_os = "nife"` arm to std's `cfg_select!` dispatchers so they pick the nife
/// backend, and add nife to std's `build.rs` known-platform chain (so std is not
/// `restricted_std` and ordinary programs need no `#![feature]`). These string anchors couple us
/// to the pinned nightly's std internals; a rustc bump that reshapes them fails loudly here, which
/// is the intended tripwire (see notes/std.md).
fn std_patch_dispatch() -> bool {
    let sys = farm_std_src().join("sys");
    patch_after(
        &sys.join("pal/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        pub(crate) mod nife;\n        pub use self::nife::*;\n    }",
    ) && patch_after(
        &sys.join("alloc/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        use nife as imp;\n    }",
    ) && patch_after(
        &sys.join("stdio/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        pub use nife::*;\n    }",
    ) && patch_after(
        // random: `fill_bytes` AND `hashmap_random_keys`, because milestone 56 splits them. The
        // first promises cryptographic strength and panics without the entropy capability; the
        // second is a hash seed and degrades to the old counter-seeded stream. Exporting both means
        // std's blanket `hashmap_random_keys` (the `#[cfg(not(any(...)))]` fallback at the bottom of
        // the same file) must exclude nife, or the two definitions collide; that is the next
        // patch, and it is anchored on the wasi line because "xous" appears twice in the file.
        &sys.join("random/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        pub use nife::{fill_bytes, hashmap_random_keys};\n    }",
    ) && patch_after(
        &sys.join("random/mod.rs"),
        "    all(target_os = \"wasi\", not(target_env = \"p1\")),",
        "    target_os = \"nife\",",
    ) && patch_after(
        &sys.join("thread/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        pub use nife::{Thread, available_parallelism, current_os_id, set_name, sleep, yield_now, DEFAULT_MIN_STACK_SIZE};\n    }",
    ) && patch_after(
        &sys.join("time/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        use nife as imp;\n    }",
    ) && patch_after(
        // net: TcpStream + outbound UdpSocket over the net_stack socket contract (milestone 27 phase
        // two). The first cfg_select in connection/mod.rs is the backend dispatcher; the nife
        // arm precedes the `_ =>` unsupported fallback that phase one used. hostname has its own
        // `_ =>` fallback to unsupported, so it needs no arm.
        &sys.join("net/connection/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        pub use nife::*;\n    }",
    ) && patch_after(
        // fs: File open/read/metadata over the FS-service contract (milestone 27 phase two). The
        // arm precedes the `_ =>` unsupported fallback phase one used, and mirrors the shape of
        // the other single-backend arms (`use nife as imp`).
        &sys.join("fs/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        use nife as imp;\n    }",
    ) && patch_after(
        // io/error has no fallback arm; route nife to the generic backend.
        &sys.join("io/error/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod generic;\n        pub use generic::*;\n    }",
    ) && patch_after(
        // Single-threaded, no native TLS: storage is a plain static (no_threads).
        &sys.join("thread_local/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod no_threads;\n        pub use no_threads::{EagerStorage, LazyStorage, thread_local_inner};\n        pub(crate) use no_threads::{LocalPointer, local_pointer};\n    }",
    ) && patch_after(
        // ... and the TLS-destructor guard is a no-op.
        &sys.join("thread_local/mod.rs"),
        "pub(crate) mod guard {\n    cfg_select! {",
        "        target_os = \"nife\" => {\n            pub(crate) fn enable() {}\n        }",
    ) && patch_after(
        // std::env::consts::OS. `cfg_unordered!` turns each arm's cfg into the fallback's
        // exclusion set, so adding a nife arm both defines OS and keeps the fallback off it.
        &sys.join("env_consts.rs"),
        "cfg_unordered! {",
        "#[cfg(target_os = \"nife\")]\npub mod os {\n    pub const FAMILY: &str = \"\";\n    pub const OS: &str = \"nife\";\n    pub const DLL_PREFIX: &str = \"\";\n    pub const DLL_SUFFIX: &str = \"\";\n    pub const DLL_EXTENSION: &str = \"\";\n    pub const EXE_SUFFIX: &str = \"\";\n    pub const EXE_EXTENSION: &str = \"\";\n}",
    ) && patch_after(
        // nife has a real PAL: not restricted_std.
        &farm_std_src().parent().unwrap().join("build.rs"),
        "        || target_os == \"vexos\"\n",
        "        || target_os == \"nife\"",
    )
}

/// **Build the `std_exerciser` program for both custom targets** (milestone 27), via -Zbuild-std against
/// the patched `nife-dev` toolchain. panic=abort and singlethread come from the target specs;
/// `compiler-builtins-mem` supplies memcpy/memset for the bare target.
///
/// `RUSTUP_TOOLCHAIN` is set explicitly rather than via `+nife-dev`, because the cargo proxy
/// that launched this xtask already exports `RUSTUP_TOOLCHAIN=nightly`, which would override a
/// `+` selector and silently build std from the *unpatched* sysroot.
fn std_exerciser() -> bool {
    if !std_src() {
        return false;
    }
    let manifest = s(workspace_root().join("std_exerciser/Cargo.toml"));
    for triple in STD_TARGETS {
        let spec = s(workspace_root().join(format!("targets/{triple}.json")));
        let ok = Command::new("cargo")
            .env("RUSTUP_TOOLCHAIN", NIFE_TOOLCHAIN)
            .args([
                "build",
                "--release",
                "--manifest-path",
                &manifest,
                "-Zjson-target-spec",
                "-Zbuild-std=core,alloc,std,panic_abort",
                "-Zbuild-std-features=compiler-builtins-mem",
                "--target",
                &spec,
            ])
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("std-exerciser: building std_exerciser for {triple} failed");
            return false;
        }
    }
    true
}

// ===========================================================================================
// Measured boot: the digest of the boot program, handed to the kernel build (milestone 22 phase
// B.1, DECISIONS §22).
//
// The kernel loads exactly one program itself, the boot program, and until now it loaded whatever
// bytes it was handed. Now the build measures that entry and the kernel image carries the digest, so
// the check means "this kernel runs exactly this init." The ordering is one-way and the build
// already had it: userspace -> archive -> manifest -> kernel. See kernel/build.rs (which consumes
// the manifest) and notes/trusted-init.md.
// ===========================================================================================

/// The archive entries the kernel itself may enter as the boot program, per architecture. Everything
/// else in the archive is loaded by init, in userspace, so it is not part of the kernel's trust root.
/// aarch64 boots `init` (the `hello` binary's init role); riscv64's tour boots `init` (the portable
/// `builder`) and its shell boot boots `system_initializer`.
///
/// riscv64 also lists **`hello`**, which is the same program aarch64 measures as `init`: `spawn_init`
/// enters it directly for the userspace-init tests, and `trust::require` refuses any entry the trust
/// root does not name. A kernel that could enter a program it never measured would be the hole
/// measured boot exists to close, so the entry is here rather than the check being relaxed there.
fn boot_programs(arch: &str) -> &'static [&'static str] {
    match arch {
        "riscv64" => &["init", "system_initializer", "hello"],
        _ => &["init"],
    }
}

/// Where the measurement manifest for an architecture is written. `kernel/build.rs` derives exactly
/// this path from `CARGO_CFG_TARGET_ARCH`, so the two stay in lockstep without an env var to forget.
fn measure_manifest_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/init-measure-{arch}.txt"))
}

/// **The table init measures its own loads against** (milestone 104), packed as an ordinary archive
/// entry under [`measured_boot::PROGRAM_MEASUREMENTS`].
///
/// One line per program, in the same `name <sha256>` format the kernel's manifest uses, because
/// there is one format and one parser (`measured_boot::manifest_entries`). Every entry in the
/// archive is measured except the table itself, which cannot contain its own digest; that includes
/// the boot programs the kernel already measures, which costs nothing and means a reader does not
/// have to know which side of the boundary a name falls on to find it here.
///
/// **Sorted**, so the table is a function of the archive's contents and not of the order the packer
/// happened to list them in. An unsorted table would rewrite itself, and therefore relink the
/// kernel, whenever somebody reordered the file list for readability.
///
/// **Why this is an archive entry rather than something compiled into init.** The kernel's own trust
/// root is compiled into the kernel image, which works because the kernel is not in the archive it
/// measures. init is. Generating a table of its siblings into init's own binary would mean building
/// userspace, measuring it, and building userspace again, with a "and nothing else changed in the
/// second build" invariant holding up the whole chain. Packing the table beside the programs and
/// letting the kernel's trust root name it buys the same guarantee with a one-pass build: the
/// kernel vouches for the table exactly as it vouches for init.
fn measurement_table(files: &[(&str, &[u8])]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .filter(|(name, _)| *name != measured_boot::PROGRAM_MEASUREMENTS)
        .map(|(name, bytes)| {
            let hex = measured_boot::hex(&measured_boot::sha256(bytes));
            let hex = std::str::from_utf8(&hex).expect("hex is ascii");
            format!("{name} {hex}\n")
        })
        .collect();
    lines.sort();
    let mut text = String::from(
        "# generated by cargo xtask: every program in this archive, for init to measure what it \
         loads\n",
    );
    for line in lines {
        text.push_str(&line);
    }
    text
}

/// Hash the boot-program entries out of the archive we just packed and write the manifest.
///
/// It parses the packed image back with `nifefs` rather than hashing the input file, deliberately:
/// what must be measured is the bytes **the kernel will read out of the archive**, not the bytes we
/// meant to put in. If packing ever mangled an entry, this measures the mangling and the boot fails,
/// which is the correct direction to be wrong in.
fn write_measure_manifest(arch: &str, image: &[u8]) -> bool {
    let fs = match nifefs::Fs::parse(image) {
        Ok(fs) => fs,
        Err(e) => {
            eprintln!("measure: the archive we just packed does not parse: {e:?}");
            return false;
        }
    };
    let mut text = format!(
        "# generated by cargo xtask; the boot programs this {arch} kernel image is built against\n"
    );
    // The boot programs the kernel may enter, plus the table it vouches for on init's behalf
    // (milestone 104). The kernel never reads the table's contents; it hashes the entry and refuses
    // to hand the archive to init if it is not the one this kernel image was built against, which is
    // what makes init's refusals worth as much as init's own measurement.
    for name in boot_programs(arch)
        .iter()
        .copied()
        .chain([measured_boot::PROGRAM_MEASUREMENTS])
    {
        let Some(bytes) = fs.read(name) else {
            // Not every archive carries every boot program (the aarch64 one has no `system_initializer`). A
            // name that is absent simply gets no measurement, and the kernel refuses to enter a
            // program it has no measurement for, so nothing is quietly waved through.
            continue;
        };
        let digest = measured_boot::sha256(bytes);
        let hex = measured_boot::hex(&digest);
        let hex = std::str::from_utf8(&hex).expect("hex is ascii");
        text.push_str(&format!("{name} {hex}\n"));
    }
    let path = measure_manifest_path(arch);
    // Write only on change, so an unchanged userspace does not make build.rs relink the kernel.
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return true;
    }
    if let Err(e) = std::fs::write(&path, &text) {
        eprintln!("measure: cannot write {}: {e}", path.display());
        return false;
    }
    true
}

// ===========================================================================================
// The scanout check (milestone 29): prove the pixels reached the DEVICE, not only our buffer.
//
// The in-guest test proves the framebuffer byte for byte, and cannot do better: the suite runs
// `-display none` and nothing inside the guest can read QEMU's host-side surface back, so a wrong
// pixel format or scanout rectangle would pass it and show garbage on a real screen.
//
// QEMU's monitor closes that gap, and it works headlessly: `screendump FILE` writes a PPM of the
// scanout even with no display backend. So the runners take a monitor socket (NIFE_GPU_MON), and
// this drives it **while the ordinary test run is happening**, rather than paying for a second boot:
// the suite is minutes long per ISA and the pattern stays on the scanout from the display test until
// QEMU exits, so there is no need to synchronize with the guest at all. Poll, dump, compare; the
// first match ends the polling.
//
// Fail-safe by construction. If the pattern never reaches the scanout, or the display test stops
// running, or the confinement test's device reset moves after it and wipes the surface, no dump
// matches and this reports it. Nothing here can make a broken scanout look fine.
// ===========================================================================================

/// The unix socket the QEMU monitor listens on for `arch`. **In /tmp on purpose**: a unix socket path
/// must fit in 104 bytes, and a worktree checkout plus `target/` gets close enough to that limit to
/// break on someone else's machine. The PPM it dumps goes under `target/`, where path length is free.
fn gpu_mon_socket(arch: &str) -> String {
    format!("/tmp/nife-gpu-{arch}-{}.sock", std::process::id())
}

fn gpu_shot_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/gpu-scanout-{arch}.ppm"))
}

/// Where the composed screen's matching dump is kept (milestone 33). A separate file because the
/// composed screen is transient: the poll loop overwrites [`gpu_shot_path`] on every dump, so the one
/// that matched has to be copied aside or there is nothing left to look at after a run.
fn gpu_compose_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/gpu-compose-{arch}.ppm"))
}

/// Where the display terminal's matching dump is kept (milestone 29's text increment). Transient for
/// the same reason the composed screen is: rung one's pattern replaces it on the same scanout.
fn gpu_text_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/gpu-text-{arch}.ppm"))
}

/// Does this PPM hold the pattern rung one's client painted (milestone 29)?
///
/// Compares against `gfx_proto::pixel`, the same definition the client painted from and the kernel
/// test digested against, so the host cannot disagree with the guest about what the pattern is.
fn scanout_holds_the_pattern(ppm: &[u8]) -> Result<(), String> {
    scanout_matches(ppm, gfx_proto::pixel)
}

/// Does this PPM hold the screen rung two's compositor composed (milestone 33)?
///
/// The same check against a different definition: `compositor::expected_screen_pixel` with every window
/// of the scene committed, which is the picture the kernel test predicted and the capture client
/// digested. **This is the check a guest-side digest cannot replace.** Three witnesses inside the
/// guest agree about the framebuffer; only the host can see what the device is actually scanning out,
/// so a wrong pixel format, a wrong scanout rectangle, or a compositor that wrote its picture
/// somewhere other than the scanout would pass all three and fail here.
fn scanout_holds_the_composed_screen(ppm: &[u8]) -> Result<(), String> {
    scanout_matches(ppm, |x, y| {
        compositor::expected_screen_pixel(compositor::SCENE.len(), x, y)
    })
}

/// Does this PPM hold the **text** the display terminal drew (milestone 29's remaining increment)?
///
/// The definition is the VT engine itself, run here on the host over `video_terminal::script`, the same script
/// the kernel sent the terminal and the same engine the terminal drew from. So this is not "is there
/// ink on the screen": it is every pixel of every glyph, in the right cell, in the right colour,
/// with the cursor where the engine says it is.
///
/// **This is the check a guest-side digest cannot replace**, and for text it matters more than for a
/// pattern: a wrong pixel format turns a test pattern into an odd-looking test pattern, and it turns
/// text into text nobody can read. Its negative control is
/// `tests::the_scanout_check_rejects_text_that_is_one_letter_wrong`.
fn scanout_holds_the_terminals_text(ppm: &[u8]) -> Result<(), String> {
    let expect = video_terminal::script::full_screen();
    scanout_matches(ppm, |x, y| expect.pixel(x, y))
}

/// Compare a `screendump` PPM against a per-pixel definition of what should be on the screen.
///
/// The geometry must match too: a scanout of the wrong size is a `SET_SCANOUT` bug, not a near miss.
///
/// Returns `Err(reason)` rather than a bool so a mismatch says which pixel and what it should have
/// been, since "the screen is wrong" is otherwise the least actionable failure in graphics.
fn scanout_matches(ppm: &[u8], want_pixel: impl Fn(u32, u32) -> u32) -> Result<(), String> {
    // P6 header: "P6\n<w> <h>\n<maxval>\n", then w*h*3 bytes, RGB per pixel.
    let text = String::from_utf8_lossy(&ppm[..ppm.len().min(64)]).to_string();
    let mut fields = text.split_ascii_whitespace();
    if fields.next() != Some("P6") {
        return Err("not a P6 PPM".into());
    }
    let w: u32 = fields
        .next()
        .and_then(|f| f.parse().ok())
        .ok_or("no width")?;
    let h: u32 = fields
        .next()
        .and_then(|f| f.parse().ok())
        .ok_or("no height")?;
    let maxval = fields.next().ok_or("no maxval")?;
    if maxval != "255" {
        return Err(format!("maxval {maxval}, expected 255"));
    }
    if (w, h) != (gfx_proto::WIDTH, gfx_proto::HEIGHT) {
        return Err(format!(
            "scanout is {w}x{h}, the surface is {}x{}",
            gfx_proto::WIDTH,
            gfx_proto::HEIGHT
        ));
    }
    // The pixel data starts after the fourth whitespace-terminated field. Find it by walking the
    // header rather than assuming a byte offset, because QEMU is free to format the header its way.
    let mut seen = 0;
    let mut i = 0;
    while i < ppm.len() && seen < 4 {
        if ppm[i].is_ascii_whitespace() {
            seen += 1;
            while seen < 4 && i + 1 < ppm.len() && ppm[i + 1].is_ascii_whitespace() {
                i += 1;
            }
        }
        i += 1;
    }
    let pixels = &ppm[i..];
    let want_len = (w * h * 3) as usize;
    if pixels.len() < want_len {
        // A dump caught mid-write. Not a failure, just not usable yet.
        return Err(format!(
            "short by {} bytes (QEMU may still be writing)",
            want_len - pixels.len()
        ));
    }
    for y in 0..h {
        for x in 0..w {
            let o = ((y * w + x) * 3) as usize;
            let want = want_pixel(x, y);
            let (r, g, b) = (
                ((want >> 16) & 0xff) as u8,
                ((want >> 8) & 0xff) as u8,
                (want & 0xff) as u8,
            );
            if (pixels[o], pixels[o + 1], pixels[o + 2]) != (r, g, b) {
                return Err(format!(
                    "pixel ({x},{y}) is rgb({},{},{}), it should be rgb({r},{g},{b})",
                    pixels[o],
                    pixels[o + 1],
                    pixels[o + 2],
                ));
            }
        }
    }
    Ok(())
}

/// **Press a key on the guest's keyboard**, over the same monitor the scanout check uses.
///
/// Nothing inside the guest can press a key, which is the point of testing a real input device, so
/// this is the one place the host is an *actor* rather than an observer. `sendkey` sends a press and
/// a release, which is also what makes the driver's handling of the event's value field checkable:
/// counting the release would double every character.
///
/// Sent on every poll, from the start of the run. That needs no synchronization with the guest
/// because QEMU **drops key events until a driver sets `DRIVER_OK`**, so keys pressed before the
/// keyboard driver exists go nowhere, and once it exists the next one lands. `video_terminal::script::HOST_KEY`
/// is the one definition of which key, shared with the kernel test that asserts the byte.
fn sendkey(sock: &str, key: &str) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let Ok(mut s) = UnixStream::connect(sock) else {
        return;
    };
    let _ = s.write_all(format!("sendkey {key}\n").as_bytes());
    let _ = s.flush();
}

/// Ask the QEMU monitor on `sock` for a screendump into `out`. Returns false while the socket is not
/// there yet (QEMU still starting, or already gone), which the caller treats as "try again".
fn screendump(sock: &str, out: &Path) -> bool {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let Ok(mut s) = UnixStream::connect(sock) else {
        return false;
    };
    // The monitor greets us, then takes one command per line. We never read the reply: the evidence is
    // the file, and a reply we misparsed would only be a second way to be wrong.
    let _ = s.write_all(format!("screendump {}\n", out.display()).as_bytes());
    let _ = s.flush();
    // Give QEMU a moment to write the file before the caller reads it. The size check in
    // `scanout_holds_the_pattern` catches a partial write anyway, so this only reduces retries.
    std::thread::sleep(std::time::Duration::from_millis(150));
    true
}

/// **Run the kernel test suite for `arch` and prove BOTH scanouts while it runs.** `test_args` is the
/// cargo invocation the caller would otherwise have handed to [`run`].
///
/// **Three** pictures reach the device's scanout over one boot, in this order, because that is the
/// order the suite runs them in (tests sort by name, so `compositor_tests` comes before
/// `display_tests`, and within the latter `a_backing...` < `a_bitmap...` < `a_confined...`):
///
/// 1. rung two's **composed screen** (milestone 33): three clients' surfaces, composited by `compositor`.
///    The compositor test holds it up for a few seconds precisely so this poll cannot miss it;
/// 2. the display terminal's **text** (milestone 29's remaining increment): real glyphs from the
///    `bitfont` table, laid out by the `video_terminal` engine. Held up the same way, for the same reason;
/// 3. rung one's **test pattern** (milestone 29), which then stays on the scanout until QEMU exits.
///
/// All three must be seen or the run fails, and the order is part of the check: this looks for each
/// picture until it finds it and only then starts looking for the next. So a reordering of the suite,
/// or a component that never got its picture to the device, fails loudly instead of being waved
/// through. The child inherits stdio, so the suite's output streams exactly as before.
fn cargo_test_with_scanout_check(arch: &str, test_args: &[&str]) -> bool {
    let mut referee = ScanoutReferee::new(arch);
    // The other host-side actor (milestone 107): a process that connects INTO the guest, which is
    // the one thing no in-guest test can stage. Constructed before the child for the same reason
    // the referee is: it is what sets `NIFE_HOSTFWD_PORT`, and the runner reads it from the
    // environment the child inherits.
    let prober = InboundProber::new(arch);
    // The multicast prober (milestone 55's stack half), on the same terms: it sets
    // `NIFE_MCAST_PORT` before the child exists, so it must be constructed first, and it runs
    // passively for the whole boot because nothing here knows when the mDNS test starts.
    let mcast = MulticastProber::new(arch);
    // And the SMB prober (milestone 54), same terms: it sets `NIFE_SMB_HOSTFWD_PORT` before the
    // child exists, and it drives the mount-shaped exchange the SMB gate asserts.
    let smb_prober = SmbProber::new(arch);
    let mut child = match Command::new("cargo").args(test_args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to run cargo: {e}");
            return false;
        }
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                break;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("waiting for the test child failed: {e}");
                return false;
            }
        }
        referee.poll();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // All four, and not short-circuited: a run that lost the scanout AND a network answer should
    // say so once rather than making the reader run it again to find the next failure.
    let scanout = referee.report();
    let inbound = prober.report();
    let multicast = mcast.report();
    let smb = smb_prober.report();
    scanout && inbound && multicast && smb
}

/// **The host-side referee for one booted suite**: presses a key through QEMU's monitor and watches
/// the device's own scanout for the three pictures the suite puts there.
///
/// It exists as a struct rather than a loop body because **two legs need the same referee driven
/// two different ways** (milestone 81). Under TCG the harness exits by itself, so the loop can be
/// "poll until the child is gone". Under HVF nothing exits (QEMU does not answer the semihosting
/// trap), so the verdict comes from reading the transcript, which blocks, and the referee has to be
/// driven from a second thread beside it. Same state machine, same messages, two drivers.
struct ScanoutReferee {
    arch: String,
    sock: String,
    shot: PathBuf,
    composed_shot: PathBuf,
    text_shot: PathBuf,
    composed: Option<String>,
    text: Option<String>,
    matched: Option<String>,
    last_composed: String,
    last_text: String,
    last_reason: String,
}

impl ScanoutReferee {
    /// Clear last run's evidence and tell the runner where to put the monitor socket.
    fn new(arch: &str) -> Self {
        let sock = gpu_mon_socket(arch);
        let shot = gpu_shot_path(arch);
        let composed_shot = gpu_compose_path(arch);
        let text_shot = gpu_text_path(arch);
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_file(&shot);
        let _ = std::fs::remove_file(&composed_shot);
        let _ = std::fs::remove_file(&text_shot);

        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the threads xtask ever starts (the transcript reader in
        // shell_check_leg, and this referee's driver in hvf_kernel_leg) copy pipe bytes and poll a
        // socket, and neither touches the environment.
        unsafe { std::env::set_var("NIFE_GPU_MON", &sock) };

        let missing = String::from("no screendump was ever taken (did QEMU get a monitor?)");
        Self {
            arch: arch.to_string(),
            sock,
            shot,
            composed_shot,
            text_shot,
            composed: None,
            text: None,
            matched: None,
            last_composed: missing.clone(),
            last_text: missing.clone(),
            last_reason: missing,
        }
    }

    /// One pass: press a key, take a screendump, and see whether it is the picture we are waiting
    /// for. Call it on a cadence for as long as the suite is running.
    fn poll(&mut self) {
        // Press a key every poll. Harmless before the keyboard driver exists (QEMU drops the event)
        // and harmless after its test has passed (the driver ends up parked in a `CALL` nobody
        // answers), so there is nothing to time.
        sendkey(&self.sock, video_terminal::script::HOST_KEY);
        if self.matched.is_none()
            && screendump(&self.sock, &self.shot)
            && let Ok(bytes) = std::fs::read(&self.shot)
        {
            // Each picture is transient except the last (the next test on the same device replaces
            // it), so the dump that matched is copied aside: `shot` is overwritten on every poll.
            if self.composed.is_none() {
                match scanout_holds_the_composed_screen(&bytes) {
                    Ok(()) => {
                        let _ = std::fs::write(&self.composed_shot, &bytes);
                        self.composed = Some(format!("{}", self.composed_shot.display()));
                    }
                    Err(reason) => self.last_composed = reason,
                }
            } else if self.text.is_none() {
                match scanout_holds_the_terminals_text(&bytes) {
                    Ok(()) => {
                        let _ = std::fs::write(&self.text_shot, &bytes);
                        self.text = Some(format!("{}", self.text_shot.display()));
                    }
                    Err(reason) => self.last_text = reason,
                }
            } else {
                match scanout_holds_the_pattern(&bytes) {
                    Ok(()) => self.matched = Some(format!("{}", self.shot.display())),
                    Err(reason) => self.last_reason = reason,
                }
            }
        }
    }

    /// Say what reached the device's scanout and what did not, and return whether all three did.
    fn report(self) -> bool {
        let _ = std::fs::remove_file(&self.sock);
        let arch = &self.arch;
        let (composed, text, matched) = (&self.composed, &self.text, &self.matched);
        let (last_composed, last_text, last_reason) =
            (&self.last_composed, &self.last_text, &self.last_reason);

        let mut ok = true;
        match composed {
            Some(path) => eprintln!(
                "scanout check ({arch}): the compositor's {} windows reached the DEVICE's scanout, \
             verified pixel for pixel against compositor::expected_screen_pixel ({path})",
                compositor::SCENE.len(),
            ),
            None => {
                eprintln!();
                eprintln!(
                    "scanout check ({arch}) FAILED: the compositor test passed, so the guest's witnesses \
                 agree about the framebuffer, but QEMU's scanout never held the composed screen. Last \
                 mismatch: {last_composed}"
                );
                eprintln!(
                    "  A compositor's output is exactly what a guest-side digest cannot confirm; this is \
                 the check that can. See notes/compositor.md."
                );
                ok = false;
            }
        }
        match text {
            Some(path) => eprintln!(
                "scanout check ({arch}): the display terminal's text reached the DEVICE's scanout, \
             verified pixel for pixel against the vt engine run over video_terminal::script ({path})",
            ),
            None => {
                eprintln!();
                eprintln!(
                    "scanout check ({arch}) FAILED: the display-terminal test passed, so the guest \
                 agrees about the framebuffer, but QEMU's scanout never held the terminal's text. \
                 Last mismatch: {last_text}"
                );
                eprintln!(
                    "  A wrong pixel format makes a test pattern look odd and makes text unreadable, \
                 which is why this check exists for glyphs too. See notes/glyphs.md."
                );
                ok = false;
            }
        }
        match matched {
            Some(path) => eprintln!(
                "scanout check ({arch}): the {}x{} pattern reached the DEVICE's scanout, verified pixel \
             for pixel against gfx_proto::pixel ({path})",
                gfx_proto::WIDTH,
                gfx_proto::HEIGHT,
            ),
            None => {
                eprintln!();
                eprintln!(
                    "scanout check ({arch}) FAILED: the display test passed, so the framebuffer holds \
                 the pattern, but QEMU's scanout never did. Last mismatch: {last_reason}"
                );
                eprintln!(
                    "  This is the check that catches a wrong pixel format or scanout rectangle, which \
                 the in-guest test cannot see. See notes/framebuffer-contract.md."
                );
                ok = false;
            }
        }
        ok
    }
}

/// The bytes the inbound prober sends into the guest and the answer it requires back. They must
/// match `user/src/socket_test_client.rs` (`IN_MSG`/`OUT_MSG`), and they are deliberately different
/// strings: an echo would pass even if the guest were only reflecting our own bytes, and the point
/// of this gate is that the guest **composed** an answer to a connection it did not make.
const INBOUND_IN: &[u8] = b"nife-in!";
const INBOUND_OUT: &[u8] = b"nife-out!";
/// How many connections the prober must complete. **Two, and the second is the load-bearing one:**
/// a listener that accepts one connection and then goes deaf is a listener a file server cannot
/// use, and nothing but a second accept proves the re-arm (milestone 107).
const INBOUND_ROUNDS: usize = 2;

/// Ask the OS for a free TCP port on the loopback and let it go again.
///
/// There is a race between letting go and QEMU binding it, and it is the right trade: the
/// alternative is a *fixed* port, which two lanes running the suite on one machine collide on every
/// time rather than rarely. A lost race fails loudly (QEMU refuses to start), which is the failure
/// mode this project prefers to a quiet one.
fn free_loopback_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// **The host side of the inbound gate** (milestone 107): a host process that connects TO the guest.
///
/// Everything else the suite proves about the network is the guest as a client. This is the mirror,
/// and it needs a host actor for the same reason the scanout check does: nothing inside the guest
/// can open a connection to the guest from outside it. QEMU's `hostfwd` forwards a loopback port
/// into the guest's listening port, and this thread connects to it, sends a payload, and requires
/// the guest's own answer back.
///
/// It **retries for the whole run** rather than being timed to the accept test, because nothing here
/// knows when that test starts. A connection that arrives while some other net test holds the NIC
/// finds no listener and is reset by smoltcp, which costs nothing and is indistinguishable from any
/// other closed port. It stops the moment both rounds have completed.
struct InboundProber {
    arch: String,
    port: Option<u16>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl InboundProber {
    /// Pick the port, tell the runner about it, and start poking. Call this **before** the child is
    /// spawned: the runner reads `NIFE_HOSTFWD_PORT` from the environment it inherits.
    fn new(arch: &str) -> Self {
        let Some(port) = free_loopback_port() else {
            eprintln!("inbound prober ({arch}): could not get a free loopback port");
            return Self {
                arch: arch.to_string(),
                port: None,
                stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                thread: None,
            };
        };
        // SAFETY: `set_var` became unsafe in edition 2024 because it races other threads. This runs
        // on the main thread before both the child that reads it and the prober thread below, and
        // that thread only touches sockets; no thread xtask starts ever writes the environment.
        unsafe { std::env::set_var("NIFE_HOSTFWD_PORT", port.to_string()) };

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::spawn(move || probe_inbound(port, stop_thread));
        Self {
            arch: arch.to_string(),
            port: Some(port),
            stop,
            thread: Some(thread),
        }
    }

    /// Stop poking, and say whether the guest answered. Fails the leg when it did not: the guest's
    /// own assertion covers "somebody connected", and this covers the other half, that what came
    /// back was the answer the guest meant to send.
    fn report(mut self) -> bool {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let arch = &self.arch;
        let Some(thread) = self.thread.take() else {
            eprintln!("inbound check ({arch}) FAILED: the prober never started");
            return false;
        };
        let port = self.port.unwrap_or(0);
        match thread.join() {
            Ok(Ok(())) => {
                eprintln!(
                    "inbound check ({arch}): {INBOUND_ROUNDS} host connections to 127.0.0.1:{port} \
                     were forwarded into the guest, accepted, and answered with the guest's own \
                     bytes. The second one is the proof the listener re-arms."
                );
                true
            }
            Ok(Err(reason)) => {
                eprintln!();
                eprintln!("inbound check ({arch}) FAILED: {reason}");
                eprintln!(
                    "  A host process connecting to the guest is the one thing no in-guest test can \
                     stage. See notes/net.md."
                );
                false
            }
            Err(_) => {
                eprintln!("inbound check ({arch}) FAILED: the prober thread panicked");
                false
            }
        }
    }
}

/// One prober thread: connect, speak, and require the guest's answer, `INBOUND_ROUNDS` times.
///
/// **Never abandon a connection because it is slow, and this is the whole subtlety** (found by the
/// first green run, where the guest passed and the prober reported nothing). A `connect` here
/// succeeds the moment QEMU accepts the host side; slirp only then starts the guest side, and if the
/// guest is not *polling* nothing answers the SYN. Dropping such a connection does not take back the
/// payload already written: slirp keeps the guest-side connection, completes the handshake whenever
/// the guest next polls, and delivers those bytes to a socket whose host end has gone. The guest then
/// serves a round nobody is listening for, and its answer is discarded.
///
/// One retry every 100 ms for a whole boot makes that a queue of them, which is exactly what
/// happened: the guest served both its rounds from abandoned connections and passed, while the
/// prober timed out on its own live one and reported zero.
///
/// So a timeout is **not** a reason to give up: keep reading the same connection until it answers,
/// dies, or the run ends. A hard error (the RST a guest with no listener sends, which is the common
/// case for most of a boot) *is* a reason, and a cheap one, because nothing was consumed.
fn probe_inbound(
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use std::io::{ErrorKind, Read, Write};
    use std::sync::atomic::Ordering;

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let mut done = 0usize;
    let mut last = String::from("nothing ever answered on the forwarded port");

    while done < INBOUND_ROUNDS && !stop.load(Ordering::Relaxed) {
        let mut s = match std::net::TcpStream::connect_timeout(
            &addr,
            std::time::Duration::from_millis(500),
        ) {
            Ok(s) => s,
            Err(e) => {
                last = format!("could not connect to 127.0.0.1:{port}: {e}");
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        // Short, so the loop below can notice `stop`; not a deadline for the exchange.
        let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(250)));
        let _ = s.set_nodelay(true);

        if let Err(e) = s.write_all(INBOUND_IN) {
            last = format!(
                "the guest closed before reading our {} bytes: {e}",
                INBOUND_IN.len()
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        while got.len() < INBOUND_OUT.len() {
            match s.read(&mut buf) {
                Ok(0) => break, // the guest had no listener and closed; nothing was consumed
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    // Still waiting on a guest that has not polled yet. Hold the connection: see
                    // this function's note on why dropping it would feed the guest a round we
                    // cannot collect. The run ending is the only thing that ends this wait.
                    if stop.load(Ordering::Relaxed) {
                        last = String::from(
                            "the run ended while a connection was still waiting for the guest to \
                             accept it",
                        );
                        break;
                    }
                }
                Err(e) => {
                    last = format!("reading the guest's answer failed: {e}");
                    break;
                }
            }
        }
        drop(s);

        if got == INBOUND_OUT {
            done += 1;
            continue;
        }
        // Not an answer: almost always "no listener yet", which is the normal state for most of the
        // run. Keep the last one only so a genuine failure has something to say.
        if !got.is_empty() {
            last = format!(
                "the guest answered {:?}, wanted {:?}",
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(INBOUND_OUT),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if done == INBOUND_ROUNDS {
        Ok(())
    } else {
        Err(format!(
            "the guest served {done} of {INBOUND_ROUNDS} inbound connections on the port forwarded \
             to 127.0.0.1:{port}; last attempt: {last}"
        ))
    }
}

/// The multicast exchange's three payloads, which must match `user/src/socket_test_client.rs`
/// (`MDNS_TRIGGER`/`MDNS_QUERY`/`MDNS_ANSWER` there). The trigger is what the guest multicasts
/// first; its arrival HERE, off the raw wire, is the proof that a multicast SENDTO leaves the
/// guest at all. The query is what this prober injects to the group, and the answer is what the
/// guest composes back, a different string for the inbound gate's reason: an echo would pass even
/// if the guest were reflecting bytes.
const MDNS_TRIGGER: &[u8] = b"nife-mdns?";
const MDNS_QUERY: &[u8] = b"nife-mdns-in";
const MDNS_ANSWER: &[u8] = b"nife-mdns-out";
/// RFC 6762's group and port, and the ethernet address IPv4 multicast maps onto (01:00:5e plus
/// the group's low 23 bits).
const MDNS_GROUP: [u8; 4] = [224, 0, 0, 251];
const MDNS_PORT: u16 = 5353;
const MDNS_GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
/// The injected frame's spoofed source. The address is on slirp's subnet but held by nothing, so
/// when the guest asserts it back from its RECV header the value can only have come from the
/// datagram; the source port is 5353 because that is the RFC 6762 §6.7 case whose semantics the
/// eventual responder will branch on. The MAC is locally administered and never claimed by ARP.
const MDNS_PROBER_IP: [u8; 4] = [10, 0, 2, 99];
const MDNS_PROBER_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x99, 0x99, 0x99];

/// **The host side of the mDNS gate** (milestone 55's stack half): the peer on the frame-level
/// hub the runner wires beside slirp when `NIFE_MCAST_PORT` is set.
///
/// It exists because slirp cannot carry multicast in either direction, so no exchange through it
/// can prove the one thing the `multicast` feature was enabled for: that a datagram addressed to
/// a *group*, not to the guest, is accepted once the guest has joined. This prober speaks QEMU's
/// socket-netdev protocol (each ethernet frame prefixed with a 4-byte big-endian length, over one
/// TCP connection) and therefore sees and injects raw frames, below every slirp limitation.
///
/// Same shape and lifecycle as [`InboundProber`]: constructed before the child so the runner
/// inherits the port, running for the whole boot because nothing here knows when the mDNS test
/// starts, stopped and reported after the suite. It is passive until the guest's trigger arrives,
/// so the other net tests never see it.
struct MulticastProber {
    arch: String,
    port: Option<u16>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl MulticastProber {
    /// Pick the port, tell the runner about it, and start listening. Call this **before** the
    /// child is spawned: the runner reads `NIFE_MCAST_PORT` from the environment it inherits.
    fn new(arch: &str) -> Self {
        let Some(port) = free_loopback_port() else {
            eprintln!("multicast prober ({arch}): could not get a free loopback port");
            return Self {
                arch: arch.to_string(),
                port: None,
                stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                thread: None,
            };
        };
        // SAFETY: `set_var` became unsafe in edition 2024 because it races other threads. This
        // runs on the main thread before both the child that reads it and the prober thread
        // below, and that thread only touches sockets; no thread xtask starts ever writes the
        // environment.
        unsafe { std::env::set_var("NIFE_MCAST_PORT", port.to_string()) };

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::spawn(move || probe_multicast(port, stop_thread));
        Self {
            arch: arch.to_string(),
            port: Some(port),
            stop,
            thread: Some(thread),
        }
    }

    /// Stop listening, and say whether the whole exchange happened: the guest's own multicast
    /// send seen raw on the wire, the injected group-addressed query, and the guest's composed
    /// answer back to the group. The guest's assertion covers what it received; this covers what
    /// actually crossed the wire.
    fn report(mut self) -> bool {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let arch = &self.arch;
        let Some(thread) = self.thread.take() else {
            eprintln!("multicast check ({arch}) FAILED: the prober never started");
            return false;
        };
        match thread.join() {
            Ok(Ok(())) => {
                eprintln!(
                    "multicast check ({arch}): the guest's send to {}.{}.{}.{} reached the wire, \
                     the injected group-addressed query was accepted, and the guest answered the \
                     group with its own bytes.",
                    MDNS_GROUP[0], MDNS_GROUP[1], MDNS_GROUP[2], MDNS_GROUP[3],
                );
                true
            }
            Ok(Err(reason)) => {
                let port = self.port.unwrap_or(0);
                eprintln!();
                eprintln!("multicast check ({arch}) FAILED: {reason}");
                eprintln!(
                    "  (frame socket on 127.0.0.1:{port}.) Slirp cannot carry multicast, so this \
                     frame-level exchange is the only QEMU proof the joined group receives. See \
                     notes/mdns.md."
                );
                false
            }
            Err(_) => {
                eprintln!("multicast check ({arch}) FAILED: the prober thread panicked");
                false
            }
        }
    }
}

/// One prober thread: connect to the hub's socket backend, wait for the guest's trigger, inject
/// the group-addressed query, and require the guest's composed answer, all as raw frames.
///
/// Passive until spoken to, deliberately: the hub floods every frame of the whole boot here
/// (DHCP for every net test, TFTP, the TCP exchanges), and this thread must never answer any of
/// it, so it filters for exactly the two group-addressed payloads it knows. Re-injects on every
/// trigger rather than once, because the guest retries when the first round was lost to this
/// prober connecting late, and a duplicate injection is at worst a duplicate datagram the guest's
/// single RECV never sees.
fn probe_multicast(
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use std::io::{ErrorKind, Read, Write};
    use std::sync::atomic::Ordering;

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();

    // QEMU owns the listening side and comes up whenever the runner gets there; retry until then.
    let mut s = loop {
        if stop.load(Ordering::Relaxed) {
            return Err(format!(
                "the run ended before QEMU ever listened on 127.0.0.1:{port}; is the runner's \
                 NIFE_MCAST_PORT block attaching the injection hub?"
            ));
        }
        match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)) {
            Ok(s) => break s,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    // Short, so the loop can notice `stop`; not a deadline for anything.
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(250)));
    let _ = s.set_nodelay(true);

    let mut triggered = false;
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(if triggered {
                "the guest's trigger arrived and the query was injected, but the guest never \
                 answered the group: the injected datagram was most likely dropped by the IPv4 \
                 accept filter, which is exactly what an unjoined group looks like"
                    .to_string()
            } else {
                "the guest never sent its trigger to the group: either the mDNS test did not run, \
                 or multicast SENDTO never reached the wire"
                    .to_string()
            });
        }
        match s.read(&mut buf) {
            Ok(0) => return Err("QEMU closed the frame socket".to_string()),
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => return Err(format!("reading frames failed: {e}")),
        }
        // The stream is 4-byte big-endian length, then that many bytes of ethernet frame.
        while acc.len() >= 4 {
            let flen = u32::from_be_bytes([acc[0], acc[1], acc[2], acc[3]]) as usize;
            if flen > 65536 {
                return Err(format!(
                    "desynchronized from the frame stream (claimed frame length {flen})"
                ));
            }
            if acc.len() < 4 + flen {
                break;
            }
            let payload = group_udp_payload(&acc[4..4 + flen]).map(<[u8]>::to_vec);
            acc.drain(..4 + flen);
            if payload.as_deref() == Some(MDNS_TRIGGER) {
                triggered = true;
                let frame = mdns_query_frame();
                let mut msg = (frame.len() as u32).to_be_bytes().to_vec();
                msg.extend_from_slice(&frame);
                if let Err(e) = s.write_all(&msg) {
                    return Err(format!("injecting the query frame failed: {e}"));
                }
            } else if payload.as_deref() == Some(MDNS_ANSWER) {
                return Ok(());
            }
        }
    }
}

/// The UDP payload of `frame`, if it is an IPv4 datagram addressed to the mDNS group and port;
/// `None` for everything else on the hub (ARP, DHCP, TCP, unicast UDP), which this prober must
/// ignore.
fn group_udp_payload(frame: &[u8]) -> Option<&[u8]> {
    let ip = frame.get(14..)?;
    if frame[12..14] != [0x08, 0x00] || ip.first()? >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0xf) as usize) * 4;
    if *ip.get(9)? != 17 || ip.get(16..20)? != MDNS_GROUP {
        return None;
    }
    let udp = ip.get(ihl..)?;
    if u16::from_be_bytes([*udp.get(2)?, *udp.get(3)?]) != MDNS_PORT {
        return None;
    }
    let udp_len = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    udp.get(8..udp_len)
}

/// The frame this prober injects: ethernet to the group's multicast MAC, IPv4 from the spoofed
/// source to the group with TTL 255 (RFC 6762 §11), UDP 5353 to 5353 carrying [`MDNS_QUERY`],
/// checksums real so nothing in the guest's stack has a reason to drop it.
fn mdns_query_frame() -> Vec<u8> {
    let udp_len = 8 + MDNS_QUERY.len();
    let ip_len = 20 + udp_len;

    let mut ip = vec![
        0x45,
        0x00,
        (ip_len >> 8) as u8,
        ip_len as u8,
        0,
        0,
        0,
        0,
        255,
        17,
        0,
        0,
    ];
    ip.extend_from_slice(&MDNS_PROBER_IP);
    ip.extend_from_slice(&MDNS_GROUP);
    let c = internet_checksum(&ip, 0);
    ip[10] = (c >> 8) as u8;
    ip[11] = c as u8;

    let mut udp = vec![
        (MDNS_PORT >> 8) as u8,
        MDNS_PORT as u8,
        (MDNS_PORT >> 8) as u8,
        MDNS_PORT as u8,
        (udp_len >> 8) as u8,
        udp_len as u8,
        0,
        0,
    ];
    udp.extend_from_slice(MDNS_QUERY);
    // The UDP checksum runs over a pseudo-header of the addresses, the protocol, and the length.
    let mut pseudo = 0u32;
    for chunk in MDNS_PROBER_IP.chunks(2).chain(MDNS_GROUP.chunks(2)) {
        pseudo += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    pseudo += 17 + udp_len as u32;
    let uc = internet_checksum(&udp, pseudo);
    // A computed zero means "no checksum" on the wire; the ones'-complement convention transmits
    // it as all-ones instead.
    let uc = if uc == 0 { 0xffff } else { uc };
    udp[6] = (uc >> 8) as u8;
    udp[7] = uc as u8;

    let mut frame = Vec::with_capacity(14 + ip_len);
    frame.extend_from_slice(&MDNS_GROUP_MAC);
    frame.extend_from_slice(&MDNS_PROBER_MAC);
    frame.extend_from_slice(&[0x08, 0x00]);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&udp);
    frame
}

/// RFC 1071's ones'-complement sum over `data` (odd trailing byte padded with zero), folded and
/// inverted, starting from `init` (zero for an IPv4 header, the pseudo-header sum for UDP).
fn internet_checksum(data: &[u8], init: u32) -> u16 {
    let mut sum = init;
    for chunk in data.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], 0])
        };
        sum += word as u32;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// How many SMB connections the prober must complete (milestone 54). Two, mirroring the echo
/// prober: the second proves the SMB adapter re-arms after a client disconnects. Must match the
/// rounds the kernel test hands `smb_server`.
const SMB_ROUNDS: usize = 2;

/// **The host side of the SMB gate** (milestone 54): a host process that performs a mount-shaped
/// SMB2 exchange against the guest, through the second `hostfwd` (`NIFE_SMB_HOSTFWD_PORT`).
///
/// The mirror of [`InboundProber`], one protocol up: instead of echo bytes, each round is a full
/// negotiate, guest session setup, tree connect, open of the file the guest's boot seeded
/// through its FS server, a read whose bytes are asserted against `fs_proto::fixture::SMB_SEED`
/// (the same constant the seeding client wrote, so there is no second copy of the expected
/// contents), close, and logoff. Asserting the *seeded* file is what makes this the
/// RedoxFS-to-TCP gate rather than a wire check against the adapter's baked-in fixture. Same
/// retry discipline, including the never-abandon-a-slow-connection rule the echo prober
/// documents: a connected stream is held until it answers, dies, or the run ends.
struct SmbProber {
    arch: String,
    port: Option<u16>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl SmbProber {
    /// Pick the port, tell the runner about it, and start probing. Call **before** the child is
    /// spawned: the runner reads `NIFE_SMB_HOSTFWD_PORT` from the environment it inherits.
    fn new(arch: &str) -> Self {
        let Some(port) = free_loopback_port() else {
            eprintln!("smb prober ({arch}): could not get a free loopback port");
            return Self {
                arch: arch.to_string(),
                port: None,
                stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                thread: None,
            };
        };
        // SAFETY: `set_var` became unsafe in edition 2024 because it races other threads. This
        // runs on the main thread before both the child that reads it and the prober thread
        // below, and no thread xtask starts writes the environment afterwards.
        unsafe { std::env::set_var("NIFE_SMB_HOSTFWD_PORT", port.to_string()) };

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::spawn(move || probe_smb(port, stop_thread));
        Self {
            arch: arch.to_string(),
            port: Some(port),
            stop,
            thread: Some(thread),
        }
    }

    /// Stop probing, and say whether the guest served the exchange. The guest's own assertion
    /// covers "connections were served"; this covers the other half, that what was served was SMB
    /// and the file's bytes came back right.
    fn report(mut self) -> bool {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let arch = &self.arch;
        let Some(thread) = self.thread.take() else {
            eprintln!("smb check ({arch}) FAILED: the prober never started");
            return false;
        };
        let port = self.port.unwrap_or(0);
        match thread.join() {
            Ok(Ok(())) => {
                eprintln!(
                    "smb check ({arch}): {SMB_ROUNDS} SMB2 sessions to 127.0.0.1:{port} were \
                     forwarded into the guest and served end to end: negotiate, guest session, \
                     tree connect, and a read of the FS-server-seeded file whose bytes matched \
                     (RedoxFS to TCP). The second session is the proof the adapter re-arms."
                );
                true
            }
            Ok(Err(reason)) => {
                eprintln!();
                eprintln!("smb check ({arch}) FAILED: {reason}");
                eprintln!(
                    "  A host process mounting the guest's share is the one thing no in-guest \
                     test can stage. See notes/smb.md."
                );
                false
            }
            Err(_) => {
                eprintln!("smb check ({arch}) FAILED: the prober thread panicked");
                false
            }
        }
    }
}

/// The prober thread: complete [`SMB_ROUNDS`] full SMB sessions, retrying for the whole run (the
/// guest's SMB listener exists only while its test runs, exactly like the echo listener).
fn probe_smb(port: u16, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let mut done = 0usize;
    let mut last = String::from("no SMB listener ever answered on the forwarded port");

    while done < SMB_ROUNDS && !stop.load(Ordering::Relaxed) {
        let mut s = match std::net::TcpStream::connect_timeout(
            &addr,
            std::time::Duration::from_millis(500),
        ) {
            Ok(s) => s,
            Err(e) => {
                last = format!("could not connect to 127.0.0.1:{port}: {e}");
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        // Short, so the read loop can notice `stop`; not a deadline for the exchange (see
        // `probe_inbound` on why a slow connection must be held, not abandoned).
        let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(250)));
        let _ = s.set_nodelay(true);

        match smb_session(&mut s, &stop) {
            Ok(()) => done += 1,
            Err(e) => {
                last = e;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    if done == SMB_ROUNDS {
        Ok(())
    } else {
        Err(format!(
            "completed {done} of {SMB_ROUNDS} SMB sessions against the port forwarded to \
             127.0.0.1:{port}; last attempt: {last}"
        ))
    }
}

/// Send one SMB2 message with its direct-TCP transport prefix.
fn smb_send(s: &mut std::net::TcpStream, msg: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut hdr = [0u8; smb_proto::XPORT_LEN];
    smb_proto::xport_write(&mut hdr, msg.len());
    s.write_all(&hdr)
        .and_then(|()| s.write_all(msg))
        .map_err(|e| format!("sending a request failed: {e}"))
}

/// Read one transport-framed SMB2 message, holding the connection through read timeouts until
/// `stop` ends the run (the guest may not have polled yet; see `probe_inbound`).
fn smb_recv(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>, String> {
    use std::io::{ErrorKind, Read};
    use std::sync::atomic::Ordering;

    let mut buf = Vec::new();
    let mut need = smb_proto::XPORT_LEN;
    let mut chunk = [0u8; 4096];
    loop {
        while buf.len() < need {
            match s.read(&mut chunk) {
                Ok(0) => return Err(String::from("the guest closed mid-message")),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    if stop.load(Ordering::Relaxed) {
                        return Err(String::from(
                            "the run ended while a response was still awaited",
                        ));
                    }
                }
                Err(e) => return Err(format!("reading a response failed: {e}")),
            }
        }
        if need == smb_proto::XPORT_LEN {
            let hdr: [u8; smb_proto::XPORT_LEN] = buf[..smb_proto::XPORT_LEN].try_into().unwrap();
            let Some(mlen) = smb_proto::xport_parse(&hdr) else {
                return Err(format!("not SMB2 direct-TCP framing: {hdr:02x?}"));
            };
            need += mlen;
            continue;
        }
        return Ok(buf[smb_proto::XPORT_LEN..need].to_vec());
    }
}

/// One full mount-shaped exchange. Any protocol surprise is an `Err` naming the step, so a
/// failing leg says which part of the mount broke rather than "it did not work".
fn smb_session(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    use smb_proto::{H_SESSION_ID, H_STATUS, H_TREE_ID, client, r32, r64};

    let status = |resp: &[u8]| r32(resp, H_STATUS);

    smb_send(s, &client::negotiate(1))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("negotiate: status {:#x}", status(&resp)));
    }
    if client::negotiate_dialect(&resp) != smb_proto::DIALECT_0210 {
        return Err(format!(
            "negotiate settled on dialect {:#x}, wanted SMB 2.1",
            client::negotiate_dialect(&resp)
        ));
    }

    smb_send(s, &client::session_setup_negotiate(2))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_MORE_PROCESSING_REQUIRED {
        return Err(format!(
            "session setup (negotiate leg): status {:#x}, wanted MORE_PROCESSING_REQUIRED",
            status(&resp)
        ));
    }
    let sid = r64(&resp, H_SESSION_ID);
    if client::session_setup_token(&resp).is_none() {
        return Err(String::from("session setup carried no NTLMSSP challenge"));
    }

    smb_send(s, &client::session_setup_authenticate(3, sid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "session setup (auth leg): status {:#x}",
            status(&resp)
        ));
    }

    smb_send(s, &client::tree_connect(4, sid, b"\\\\10.0.2.15\\share"))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("tree connect: status {:#x}", status(&resp)));
    }
    let tid = r32(&resp, H_TREE_ID);

    // The file the guest's boot seeded through its FS server (fs_test_client's seed role):
    // asserting these bytes is what proves the served share is the real filesystem, not the
    // adapter's baked-in fixture. One constant on both sides (fs_proto::fixture), no second copy.
    let (name, expected) = (
        fs_proto::fixture::SMB_SEED_NAME.as_bytes(),
        fs_proto::fixture::SMB_SEED,
    );
    smb_send(s, &client::create(5, sid, tid, name))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "create {}: status {:#x} (a NOT_FOUND here usually means the guest fell back to its \
             fixture share: was the RedoxFS disk attached, and did the seed role run?)",
            String::from_utf8_lossy(name),
            status(&resp)
        ));
    }
    let fid = client::create_file_id(&resp);
    let size = client::create_end_of_file(&resp);
    if size != expected.len() as u64 {
        return Err(format!(
            "create reported {size} bytes, the seeded file is {}",
            expected.len()
        ));
    }

    smb_send(s, &client::read(6, sid, tid, &fid, 0, 4096))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("read: status {:#x}", status(&resp)));
    }
    let data = client::read_data(&resp);
    if data != expected {
        return Err(format!(
            "the file came back wrong: {:?}, wanted {:?}",
            String::from_utf8_lossy(data),
            String::from_utf8_lossy(expected)
        ));
    }

    smb_send(s, &client::close(7, sid, tid, &fid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("close: status {:#x}", status(&resp)));
    }

    smb_send(s, &client::logoff(8, sid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("logoff: status {:#x}", status(&resp)));
    }
    Ok(())
}

/// Where the packed initrd archive is written.
fn initrd_path() -> String {
    workspace_root()
        .join("target/initrd.img")
        .display()
        .to_string()
}

/// Where the RISC-V initrd archive is written (milestone 20). Separate from the aarch64 one because
/// it holds riscv64 ELFs, not aarch64 ones.
fn riscv_initrd_path() -> String {
    workspace_root()
        .join("target/initrd-riscv.img")
        .display()
        .to_string()
}

/// **Build the RISC-V userspace archive** (milestone 20, the richer-initrd step). Compiles the two
/// portable programs the second architecture runs (`builder`, the minimal init, and `worker`, the
/// child it loads) for the riscv target, and packs them into a nifefs archive: `builder` under
/// the name `init` (the entry the kernel loads first), `worker` under `worker` (the one init loads by
/// name). Point `NIFE_INITRD` at the result and boot the riscv kernel, e.g.:
///
/// ```text
/// cargo xtask initrd-riscv
/// NIFE_INITRD=target/initrd-riscv.img cargo run -p kernel --target riscv64imac-unknown-none-elf
/// ```
fn initrd_riscv() -> bool {
    if !run(
        "cargo",
        &[
            "build",
            "-p",
            "user",
            "--bin",
            "builder",
            "--bin",
            "worker",
            "--bin",
            "driver",
            "--bin",
            "os_primitives_benchmarker",
            "--bin",
            "coremark",
            "--bin",
            "system_initializer",
            "--bin",
            "console",
            "--bin",
            "input",
            "--bin",
            "swish",
            "--bin",
            "line_editor",
            "--bin",
            "terminal_sink_caretaker",
            "--bin",
            "blk",
            "--bin",
            "allocator_exerciser",
            "--bin",
            "net_stack",
            "--bin",
            "smb_server",
            "--bin",
            "budgeter",
            "--bin",
            "fs_test_client",
            "--bin",
            "fs_file_caretaker",
            "--bin",
            "fs_subtree_caretaker",
            "--bin",
            "fs_nameset_caretaker",
            "--bin",
            "heeder",
            "--bin",
            "spinner",
            "--bin",
            "root_supervisor",
            "--bin",
            "spawner",
            "--bin",
            "sub_server_supervisor",
            "--bin",
            "flaky",
            "--bin",
            "job_undertaker",
            "--bin",
            "display",
            "--bin",
            "painter",
            "--bin",
            "c_confiner",
            "--bin",
            "c_shim",
            "--bin",
            "compositor",
            "--bin",
            "window",
            "--bin",
            "display_terminal",
            "--bin",
            "kbd",
            "--bin",
            "swapper",
            "--bin",
            "rust_swappable",
            "--bin",
            "c_swappable",
            "--bin",
            "chatty",
            "--bin",
            "broker",
            "--bin",
            "clock",
            "--bin",
            "date",
            "--bin",
            "rm",
            "--bin",
            "entropy",
            "--bin",
            "ntp",
            "--bin",
            "outlaw",
            "--bin",
            "hello",
            "--bin",
            "sink",
            "--bin",
            "wc",
            "--bin",
            "doc",
            // The credential pair (milestone 56). These were listed in the riscv initrd tables below
            // but never added HERE, so a clean tree could not build them and `mkinitrd` failed on a
            // file the build was never asked to produce. The lane's own riscv leg went green on a
            // stale binary left in its target directory by an earlier build, which is exactly the
            // failure a dirty target dir hides: parity was asserted against an artifact no
            // invocation creates.
            "--bin",
            "credentialer",
            "--bin",
            "credentialer_test_client",
            // The disk surveyor (milestone 57), portable like the rest, and its write-half twin.
            "--bin",
            "disk_surveyor",
            "--bin",
            "disk_partitioner",
            "--target",
            RISCV_TARGET,
        ],
    ) {
        return false;
    }

    let bin = |name: &str| {
        workspace_root()
            .join(format!("target/{RISCV_TARGET}/debug/{name}"))
            .display()
            .to_string()
    };
    // Read each bin's ELF into an owned buffer, then pack. The archive name comes first, the bin
    // name second: `builder` is packed as `init` (the entry the kernel loads); the rest keep their
    // names. `system_initializer`/`console`/`input`/`shell` are the interactive-shell system (parity D).
    let entries: &[(&str, &str)] = &[
        ("init", "builder"),
        ("worker", "worker"),
        ("driver", "driver"),
        ("os_primitives_benchmarker", "os_primitives_benchmarker"),
        ("coremark", "coremark"),
        ("system_initializer", "system_initializer"),
        ("console", "console"),
        ("input", "input"),
        ("swish", "swish"),
        ("line_editor", "line_editor"),
        ("terminal_sink_caretaker", "terminal_sink_caretaker"),
        ("blk", "blk"),
        ("allocator_exerciser", "allocator_exerciser"),
        ("net_stack", "net_stack"),
        ("smb_server", "smb_server"),
        ("budgeter", "budgeter"),
        ("fs_test_client", "fs_test_client"),
        ("fs_file_caretaker", "fs_file_caretaker"),
        ("fs_subtree_caretaker", "fs_subtree_caretaker"),
        ("fs_nameset_caretaker", "fs_nameset_caretaker"),
        ("heeder", "heeder"),
        ("spinner", "spinner"),
        // The authority-shrinking supervision tree (milestone 22 phase B.2): an init that hands its
        // construction authority to a spawner and its restart policy to a supervisor, then drops the
        // budget. Portable, so both archives carry all four.
        ("root_supervisor", "root_supervisor"),
        ("spawner", "spawner"),
        ("sub_server_supervisor", "sub_server_supervisor"),
        ("flaky", "flaky"),
        // The interactive boot's undertaker (milestone 22, the interactive increment): init
        // endows every job it builds with one supervision endpoint and this collects the corpses, so
        // a job's region comes back to init's budget. Portable, so both archives carry it.
        ("job_undertaker", "job_undertaker"),
        // The display pair (milestone 29): the confined virtio-gpu driver and the client that draws
        // into the surface it serves. Portable, so both archives carry both.
        ("display", "display"),
        ("painter", "painter"),
        // The C seam (milestone 36): the confiner and the Rust shell that links user/c/c_seam.c.
        // The C is compiled for this ISA by user/build.rs, so the riscv shell carries riscv C.
        ("c_confiner", "c_confiner"),
        ("c_shim", "c_shim"),
        // The compositor and a window client (milestone 33, rung two). Portable, so both archives
        // carry both: the isolation this rung proves is a property of the kernel's mappings, and it
        // has to hold on either ISA or it is not a property.
        ("compositor", "compositor"),
        ("window", "window"),
        // The display terminal (milestone 29's text increment): one binary, two wirings. Portable,
        // so both archives carry it and both ISAs run literally the same test.
        ("display_terminal", "display_terminal"),
        // The keyboard driver (milestone 29's input). Portable, so both archives carry it.
        ("kbd", "kbd"),
        // Live component replacement (milestone 23): the operator, the two instances of the
        // swappable component (the second computes its answers in C), the client that talks across
        // the swap, and the queue broker for the opt-in rung. Portable, so both archives carry all
        // five and both ISAs run literally the same swap.
        ("swapper", "swapper"),
        ("rust_swappable", "rust_swappable"),
        ("c_swappable", "c_swappable"),
        ("chatty", "chatty"),
        ("broker", "broker"),
        // The clock service (milestone 51). Portable, so both archives carry it: it holds both RTC
        // drivers and the kernel tells it which one the machine has.
        ("clock", "clock"),
        // `date` (milestone 51). Portable for the same reason the service is: it reads a page and
        // formats it, and neither half knows which instruction set it is on.
        ("date", "date"),
        ("rm", "rm"),
        // The disk surveyor (milestone 57): reads the block-device roster it was granted and the
        // partition table of the one disk it holds. Portable, so both archives carry it and both
        // ISAs read literally the same table off literally the same image.
        ("disk_surveyor", "disk_surveyor"),
        // The disk partitioner (milestone 57's write half): writes the table the surveyor reads,
        // and refuses to without an entropy endpoint. Portable, so both archives carry it.
        ("disk_partitioner", "disk_partitioner"),
        // The entropy service (milestone 56). Portable, so both archives carry it: it holds the
        // virtio-rng driver, and the wiring tells it which bus the device came off.
        ("entropy", "entropy"),
        // The credential service and its clients (milestone 56, the credential half). Portable, so
        // both archives carry both: the claim is that holding the verify endpoint does not let you
        // read or write the store, and that has to hold on either instruction set or it is not a
        // claim.
        ("credentialer", "credentialer"),
        ("credentialer_test_client", "credentialer_test_client"),
        // The NTP client (milestone 51), with its test server and its clock-page probe as roles of
        // the same binary. Portable, so both archives carry it and both ISAs run the same tests.
        ("ntp", "ntp"),
        // The outlaw (milestone 19's user-test port): the privilege-boundary programs
        // kernel::user::tests used to hand-assemble as aarch64 machine code.
        ("outlaw", "outlaw"),
        // **`hello` under its own name.** On aarch64 the archive's "init" IS hello, and the whole
        // milestone 7-19 role catalogue (the printing client, the untyped demo, the granter and
        // receiver, the call server, the init roles) lives in it. riscv's "init" is the `builder`
        // demo instead, so the roles need their own entry here for the test suite to reach them.
        // The stale claim this replaces read "hello/console/input/shell are aarch64-wired and do not
        // build here"; three of the four are in the list above, and hello only ever needed its
        // hand-rolled syscalls routed through user_rt.
        ("hello", "hello"),
        // The sink contract's ends (milestone 50). Portable, so both archives carry it: the claim
        // is that a program cannot tell what its output slot holds, and that has to hold on either
        // instruction set or it is not a claim.
        ("sink", "sink"),
        // The consumer (milestone 50). Both archives, for the sink's reason: `date | wc` has to
        // compose on either instruction set or it is not a claim about the system.
        ("wc", "wc"),
        // The viewer (milestone 40). Both archives for the sink's reason: `doc page.md | wc` is a
        // claim about how the streams compose, and a claim that holds on one instruction set is not
        // one.
        ("doc", "doc"),
    ];
    let mut blobs: Vec<(&str, Vec<u8>)> = Vec::new();
    for &(archive_name, bin_name) in entries {
        match read_stripped(&bin(bin_name)) {
            Ok(b) => blobs.push((archive_name, b)),
            Err(e) => {
                eprintln!("initrd-riscv: cannot read {}: {e}", bin(bin_name));
                return false;
            }
        }
    }
    // The std demo (milestone 27), built through the nife-dev toolchain for the riscv custom
    // target, rides along when present, exactly as on aarch64. `test` builds it first.
    if let Ok(bytes) = read_stripped(
        &std_exerciser_elf("riscv64-unknown-nife")
            .display()
            .to_string(),
    ) {
        blobs.push(("std_exerciser", bytes));
    }
    // The FS server (milestone 32 phase 2), built for the riscv bare target, rides along when
    // present, exactly as std_exerciser does; `test` builds it first.
    if let Ok(bytes) = read_stripped(&fs_server_elf(RISCV_TARGET)) {
        blobs.push(("fs_server", bytes));
    }
    // And `mkfs` (milestone 57's write half), on the same terms.
    if let Ok(bytes) = read_stripped(&mkfs_elf(RISCV_TARGET)) {
        blobs.push(("mkfs", bytes));
    }
    let mut files: Vec<(&str, &[u8])> = blobs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    // The measurement table (milestone 104), on the same terms as aarch64's: last, so it measures
    // everything above it, and vouched for by the kernel's trust root so init's refusals mean
    // something. Parity is the point (§19): the same table, the same parser, the same policy.
    let table = measurement_table(&files);
    files.push((measured_boot::PROGRAM_MEASUREMENTS, table.as_bytes()));
    let size = nifefs::image_size(&files);
    let mut img = std::vec![0u8; size];
    // Carry the reason. "could not build the archive" with the error thrown away sent me hunting
    // through MAX_FILES, image_size and write_image's bounds check by hand; the error names which.
    if let Err(e) = nifefs::write_image(&files, &mut img) {
        eprintln!(
            "initrd-riscv: could not build the archive: {e:?} ({} files, {} bytes)",
            files.len(),
            size
        );
        return false;
    }
    if let Err(e) = std::fs::write(riscv_initrd_path(), &img) {
        eprintln!("initrd-riscv: could not write {}: {e}", riscv_initrd_path());
        return false;
    }
    // Measure the boot programs before the riscv kernel is built (milestone 22 phase B.1).
    if !write_measure_manifest("riscv64", &img) {
        return false;
    }
    eprintln!(
        "wrote {} ({size} bytes): init=builder, worker=worker",
        riscv_initrd_path()
    );
    true
}

/// Pack the built user ELF into the initrd archive the kernel hands init (milestone 19f).
///
/// The initrd is a **nifefs image**, the same format the virtio disk uses, so one parser serves
/// both the RAM archive and the disk. It holds `init` (the `hello` binary, which the kernel loads
/// and init re-enters at its remaining roles) plus the distinct binaries lifted out of hello:
/// `worker` (19f.2) and `console` (19f.3). The kernel reads the `init` entry to boot; init loads the
/// rest by name. Generated, not checked in, exactly like the disk and the flat kernel image: a blob
/// in git is a blob nobody can review.
fn mkinitrd() -> bool {
    let hello = match read_stripped(&user_elf()) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", user_elf());
            return false;
        }
    };
    let worker = match read_stripped(&bin_elf("worker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("worker"));
            return false;
        }
    };
    let console = match read_stripped(&bin_elf("console")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("console"));
            return false;
        }
    };
    let input = match read_stripped(&bin_elf("input")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("input"));
            return false;
        }
    };
    let swish = match read_stripped(&bin_elf("swish")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("swish"));
            return false;
        }
    };
    let coremark = match read_stripped(&bin_elf("coremark")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("coremark"));
            return false;
        }
    };
    let os_primitives_benchmarker = match read_stripped(&bin_elf("os_primitives_benchmarker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "mkinitrd: cannot read {}: {e}",
                bin_elf("os_primitives_benchmarker")
            );
            return false;
        }
    };
    let line_editor = match read_stripped(&bin_elf("line_editor")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("line_editor"));
            return false;
        }
    };
    let terminal_sink_caretaker = match read_stripped(&bin_elf("terminal_sink_caretaker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "mkinitrd: cannot read {}: {e}",
                bin_elf("terminal_sink_caretaker")
            );
            return false;
        }
    };
    let allocator_exerciser = match read_stripped(&bin_elf("allocator_exerciser")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "mkinitrd: cannot read {}: {e}",
                bin_elf("allocator_exerciser")
            );
            return false;
        }
    };
    let net_stack = match read_stripped(&bin_elf("net_stack")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("net_stack"));
            return false;
        }
    };
    let smb_server = match read_stripped(&bin_elf("smb_server")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("smb_server"));
            return false;
        }
    };
    let budgeter = match read_stripped(&bin_elf("budgeter")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("budgeter"));
            return false;
        }
    };
    let fs_file_caretaker = match read_stripped(&bin_elf("fs_file_caretaker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "mkinitrd: cannot read {}: {e}",
                bin_elf("fs_file_caretaker")
            );
            return false;
        }
    };
    let fs_subtree_caretaker = match read_stripped(&bin_elf("fs_subtree_caretaker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "mkinitrd: cannot read {}: {e}",
                bin_elf("fs_subtree_caretaker")
            );
            return false;
        }
    };
    let fs_test_client = match read_stripped(&bin_elf("fs_test_client")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("fs_test_client"));
            return false;
        }
    };
    let heeder = match read_stripped(&bin_elf("heeder")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("heeder"));
            return false;
        }
    };
    let spinner = match read_stripped(&bin_elf("spinner")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("spinner"));
            return false;
        }
    };
    // "init" is the hello binary (the kernel loads it, init re-enters it at its remaining roles);
    // "worker", "console", "input", "swish" are the split system binaries (19f.2-5), "line_editor" is
    // the line discipline between them (milestone 28), "coremark" is the compute workload (19e),
    // "os_primitives_benchmarker" is the EL0 microbenchmark program (primitive suite), and "allocator_exerciser" proves the
    // user_rt heap (milestone 27). init (and the bench boot) load each by name. All are entries in
    // the one archive. The authority-shrinking supervision tree (milestone 22 phase B.2), read as a
    // group: four small portable programs that share one module, packed under their own names for
    // both ISAs. The display pair (milestone 29) reads as a group for the same reason: the confined
    // virtio-gpu driver and the client that draws into the surface it serves, both portable. The C
    // seam (milestone 36) reads as a group too: the confiner that builds, supervises, and checks
    // the foreign component, and the Rust shell that links it. Both portable. The compositor and
    // its window clients (milestone 33, rung two) read as a group for the same reason: one server,
    // one client binary with several roles, both portable.
    let mut tree: Vec<(&str, Vec<u8>)> = Vec::new();
    for name in [
        "root_supervisor",
        "spawner",
        "sub_server_supervisor",
        "flaky",
        // The interactive boot's undertaker (milestone 22, the interactive increment): one
        // endpoint capability and nothing else, so a job's region comes back to init's budget.
        "job_undertaker",
        "display",
        "painter",
        "c_confiner",
        "c_shim",
        "compositor",
        "window",
        "display_terminal",
        "kbd",
        "swapper",
        "rust_swappable",
        "c_swappable",
        "chatty",
        "broker",
        "clock",
        "date",
        // `rm` (milestone 47's rmdir lane): the first program endowed a directory capability.
        // Portable, so both archives carry it.
        "rm",
        // The disk surveyor (milestone 57): the block-device roster and the partition table of the
        // one disk it holds. Portable, so both archives carry it and both ISAs read literally the
        // same table off literally the same image.
        "disk_surveyor",
        // The disk partitioner (milestone 57's write half): the same disk authority pointed the
        // other way, plus the entropy endpoint a unique GUID needs. Portable, so both archives
        // carry it, and both ISAs write a table the other could read.
        "disk_partitioner",
        // The nameset caretaker (milestone 47's globbing lane): a directory capability attenuated
        // to the names a pattern matched. Portable, so both archives carry it.
        "fs_nameset_caretaker",
        "entropy",
        // The credential service and its clients (milestone 56, the credential half). Portable, so
        // both archives carry both.
        "credentialer",
        "credentialer_test_client",
        "ntp",
        // The outlaw (milestone 19's user-test port): the privilege-boundary programs
        // kernel::user::tests used to hand-assemble. Portable, so both archives carry it.
        "outlaw",
        // The sink contract's ends (milestone 50): the indifferent writer, the file sink, and the
        // read-back. Portable, so both archives carry it and both ISAs run the same indifference
        // test.
        "sink",
        // `wc` (milestone 50): the right-hand side of a pipe, and the first program that reads a
        // stream. Portable, so both archives carry it.
        "wc",
        // `doc` (milestone 40): the documentation viewer, a filter from markdown to styled text.
        // Portable for the same reason as `wc`.
        "doc",
    ] {
        match read_stripped(&bin_elf(name)) {
            Ok(bytes) => tree.push((name, bytes)),
            Err(e) => {
                eprintln!("mkinitrd: cannot read {}: {e}", bin_elf(name));
                return false;
            }
        }
    }

    let mut files: Vec<(&str, &[u8])> = vec![
        ("init", &hello),
        ("worker", &worker),
        ("console", &console),
        ("input", &input),
        ("swish", &swish),
        ("line_editor", &line_editor),
        ("terminal_sink_caretaker", &terminal_sink_caretaker),
        ("coremark", &coremark),
        ("os_primitives_benchmarker", &os_primitives_benchmarker),
        ("allocator_exerciser", &allocator_exerciser),
        ("net_stack", &net_stack),
        ("smb_server", &smb_server),
        ("budgeter", &budgeter),
        ("fs_test_client", &fs_test_client),
        ("fs_file_caretaker", &fs_file_caretaker),
        ("fs_subtree_caretaker", &fs_subtree_caretaker),
        ("heeder", &heeder),
        ("spinner", &spinner),
    ];
    for (name, bytes) in &tree {
        files.push((name, bytes.as_slice()));
    }
    // The std demo (milestone 27) rides along IFF it has been built (`cargo xtask std-exerciser`, which
    // `test` runs). It builds through a separate toolchain and target, so an interactive `run` that
    // never built it simply ships an initrd without it; nothing loads it there.
    let std_exerciser = read_stripped(
        &std_exerciser_elf("aarch64-unknown-nife")
            .display()
            .to_string(),
    )
    .ok();
    if let Some(bytes) = &std_exerciser {
        files.push(("std_exerciser", bytes.as_slice()));
    }
    // The FS server (milestone 32 phase 2) rides along IFF built (its own workspace/target; `test`
    // builds it). Absent for a plain interactive boot, which simply skips the FS-server test.
    let fs_server = read_stripped(&fs_server_elf(TARGET)).ok();
    if let Some(bytes) = &fs_server {
        files.push(("fs_server", bytes.as_slice()));
    }
    // `mkfs` (milestone 57's write half) rides along on the same terms: the same package, the
    // same build, and absent from an interactive boot that never built it.
    let mkfs = read_stripped(&mkfs_elf(TARGET)).ok();
    if let Some(bytes) = &mkfs {
        files.push(("mkfs", bytes.as_slice()));
    }
    // **The measurement table, last, so it measures everything above it** (milestone 104). init
    // reads this entry out of the archive it already holds and refuses to load a program whose
    // bytes it does not match. See [`measurement_table`] for why it lives here rather than inside
    // init's own image.
    let table = measurement_table(&files);
    files.push((measured_boot::PROGRAM_MEASUREMENTS, table.as_bytes()));

    let size = nifefs::image_size(&files);
    let mut img = std::vec![0u8; size];
    if nifefs::write_image(&files, &mut img).is_err() {
        eprintln!("mkinitrd: could not build the initrd archive");
        return false;
    }
    if let Err(e) = std::fs::write(initrd_path(), &img) {
        eprintln!("mkinitrd: could not write {}: {e}", initrd_path());
        return false;
    }
    // Measure the boot program before the kernel is built (milestone 22 phase B.1). Every caller
    // reaches the kernel build through `user()`, which calls this, so the manifest is always current
    // by the time `kernel/build.rs` reads it.
    write_measure_manifest("aarch64", &img)
}

/// The packed initrd archive ([`initrd_path`]) is what `scripts/qemu-runner-aarch64.sh` passes to QEMU as
/// `-initrd` (milestone 19f); the raw user ELF ([`user_elf`]) is only the input `mkinitrd` packs.
///
/// **Deliberately the same road Linux's initramfs travels**, now literally an archive like theirs.
/// QEMU loads the file into RAM and writes its address into `/chosen/linux,initrd-start` in the
/// device tree; the kernel finds it there (`memory::initrd_region`, built at milestone 3 for
/// exactly this). Nothing about the contents is known to the kernel at build time, which is the
/// entire point of milestone 7c.
///
/// If `--hvf` was passed, boot under Apple's Hypervisor.framework instead of TCG.
fn maybe_hvf() {
    if std::env::args().any(|a| a == "--hvf") {
        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
        // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
        unsafe { std::env::set_var("NIFE_ACCEL", "hvf") };
        eprintln!("--- on the real Apple Silicon core via Hypervisor.framework ---");
    }
}

/// Where the nifefs disk image is written.
fn disk_path() -> String {
    workspace_root()
        .join("target/nifefs.img")
        .display()
        .to_string()
}

/// The PCIe transport's copy of the disk image, a sibling of [`disk_path`]. Two files because
/// both transports are now attached **writable** (milestone 32's write path) and QEMU's image
/// locking refuses to attach one file to two devices once either attachment can write. The
/// runner derives this name from `NIFE_DISK`, so the two stay in lockstep.
fn disk_pci_path() -> String {
    workspace_root()
        .join("target/nifefs-pci.img")
        .display()
        .to_string()
}

/// Build the nifefs disk images the virtio-blk driver will read and write.
///
/// **The disk is generated, not checked in**, the same way the flat kernel image is: a binary
/// blob in git is a blob nobody can review. The contents are a couple of tiny files, written
/// through the same `nifefs::write_image` the userspace filesystem server reads back, so the
/// format has exactly one definition.
///
/// `scratch` is the write-path tests' one-block playground: the driver writes a pattern into its
/// block and reads it back, so nothing else on the disk is ever a write target. Regenerating the
/// images here is also what makes test runs independent: whatever a previous run wrote to
/// scratch is rebuilt to zeros.
fn mkdisk() -> bool {
    let files: [(&str, &[u8]); 3] = [
        (
            "motd",
            b"nife: read from a virtio disk, by a driver at EL0.\n",
        ),
        (
            "readme",
            b"this file came off a real block device through a userspace driver.\n",
        ),
        ("scratch", &[0u8; 512]),
    ];
    let size = nifefs::image_size(&files).max(64 * 1024); // pad to a friendly size
    let mut img = std::vec![0u8; size];
    if nifefs::write_image(&files, &mut img).is_err() {
        eprintln!("mkdisk: could not build the image");
        return false;
    }
    // One identical image per transport; see disk_pci_path for why they cannot share a file.
    for path in [disk_path(), disk_pci_path()] {
        if let Err(e) = std::fs::write(&path, &img) {
            eprintln!("mkdisk: could not write {path}: {e}");
            return false;
        }
    }
    true
}

// ===========================================================================================
// The RedoxFS FS server and its test image (milestone 32 phase 2).
//
// The FS-server binary is out-of-workspace (it links the vendored engine), built for the bare
// targets with the pure no_std core (`--no-default-features`) plus the EL0 runtime (`el0`),
// release so the initrd stays small. The test image is made HOST-side by the redoxfs_host tool,
// the same engine the server opens it with; the server never creates. See notes/fs-server.md.
// ===========================================================================================

/// Build the FS-server ELF for `triple`. Its own workspace, so it takes `--manifest-path` and its
/// artifacts land under `fs_server/target/`.
fn fs_server_build(triple: &str) -> bool {
    run(
        "cargo",
        &[
            "build",
            "--manifest-path",
            "fs_server/Cargo.toml",
            // Both binaries out of the one package: the server that opens an image and never
            // creates, and `mkfs` (milestone 57), which creates one and never serves.
            "--bin",
            "fs_server",
            "--bin",
            "mkfs",
            "--no-default-features",
            "--features",
            "el0",
            "--release",
            "--target",
            triple,
        ],
    )
}

/// The FS-server ELF path for a target triple (always the release profile; see `fs_server_build`).
fn fs_server_elf(triple: &str) -> String {
    workspace_root()
        .join(format!("fs_server/target/{triple}/release/fs_server"))
        .display()
        .to_string()
}

/// The `mkfs` ELF path for a target triple. Same package, same profile, same build.
fn mkfs_elf(triple: &str) -> String {
    workspace_root()
        .join(format!("fs_server/target/{triple}/release/mkfs"))
        .display()
        .to_string()
}

// ---- the documentation store (milestone 40) -------------------------------------------------

/// **What each package's documentation is.**
///
/// A bundle is a package's pages plus the index shard over them, and it is installed as a unit:
/// `doc/<bundle>/` in the filesystem image, with `doc/bundles` listing the names. That is the shape
/// milestone 40 asked for ("installed by the package that owns it") minus a package manager, and it
/// is the reason the table is here rather than a `doc/` directory per crate: **copying a note into a
/// crate directory would make a second copy that can drift**, and the whole point of in-tree
/// documentation is that there is one.
///
/// So a bundle names paths that already exist, and the store is a build artifact. A page that has
/// moved fails the build rather than shipping stale.
const DOC_BUNDLES: &[(&str, &[&str])] = &[
    ("manual", &["notes/manual.md"]),
    ("swish", &["notes/pipes.md", "notes/line-discipline.md"]),
    ("kernel", &["notes/ipc-naming.md", "notes/stack.md"]),
    ("glob", &["notes/glob.md"]),
];

/// Where the store is staged on the host before it is imported into the filesystem image.
fn doc_store_path() -> std::path::PathBuf {
    workspace_root().join("target/redoxfs-tree/doc")
}

/// What one bundle cost, so the numbers in notes/manual.md are measured rather than estimated.
struct Shard {
    bundle: &'static str,
    pages: usize,
    terms: usize,
    postings: usize,
    /// Bytes of markdown.
    source: usize,
    /// Bytes of index.
    index: usize,
}

/// Build the store into `target/redoxfs-tree/doc`, ready for `mkredoxfs`'s `import`.
///
/// Returns one [`Shard`] per bundle. `None` means a listed page is missing, which is a build
/// failure rather than a warning: a store that quietly ships without a page is a manual with a
/// missing chapter and nothing to say so.
fn doc_store() -> Option<Vec<Shard>> {
    let root = doc_store_path();
    let _ = std::fs::remove_dir_all(&root);
    if std::fs::create_dir_all(&root).is_err() {
        eprintln!("doc-store: cannot create {}", root.display());
        return None;
    }

    let mut shards = Vec::new();
    let mut names = String::new();
    for (bundle, pages) in DOC_BUNDLES {
        let dir = root.join(bundle);
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("doc-store: cannot create {}", dir.display());
            return None;
        }
        // The page's name in the store is its basename, because the store is where a reader types
        // it: `cd doc/glob` then `doc glob.md`. Its *path* in the index keeps the whole repository
        // path, so a search result says where the page came from.
        let mut loaded: Vec<(String, String, Vec<u8>)> = Vec::new();
        for page in *pages {
            let src = workspace_root().join(page);
            let Ok(bytes) = std::fs::read(&src) else {
                eprintln!("doc-store: {bundle} lists {page}, which does not exist");
                return None;
            };
            let base = std::path::Path::new(page)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(page);
            if std::fs::write(dir.join(base), &bytes).is_err() {
                eprintln!("doc-store: cannot write {base}");
                return None;
            }
            let title = manual::index::title_of(&bytes).unwrap_or(base).to_string();
            loaded.push(((*page).to_string(), title, bytes));
        }

        let sources: Vec<manual::index::Source<'_>> = loaded
            .iter()
            .map(|(path, title, bytes)| manual::index::Source {
                path,
                title,
                text: bytes,
            })
            .collect();
        let index = manual::index::build(&sources);
        let header = manual::index::Header::parse(&index[..manual::index::PAGE]).ok()?;
        if std::fs::write(dir.join("index"), &index).is_err() {
            eprintln!("doc-store: cannot write {bundle}/index");
            return None;
        }
        shards.push(Shard {
            bundle,
            pages: header.pages as usize,
            terms: header.terms as usize,
            postings: header.postings as usize,
            source: loaded.iter().map(|(_, _, b)| b.len()).sum(),
            index: index.len(),
        });
        names.push_str(bundle);
        names.push('\n');
    }

    // The manifest a reader (and, when it exists, a guest-side `apropos`) uses to find the shards.
    // A file rather than a directory listing, because **there is no directory iteration in this
    // system** and adding one would be adding authority: a program that can list a directory can
    // discover what it was not given. See notes/manual.md.
    if std::fs::write(root.join("bundles"), names).is_err() {
        eprintln!("doc-store: cannot write the bundle manifest");
        return None;
    }
    Some(shards)
}

/// `cargo xtask manual`: build the store and print what it costs, then answer a query against it.
///
/// The query at the end is not a demo. It is the only thing that proves the reader and the writer
/// agree, and it runs the **same** `no_std` lookup the guest runs, over the same bytes, through the
/// same one-page-at-a-time [`manual::index::Pages`] interface. Only the IO differs.
fn manual_store(term: Option<String>) -> bool {
    let Some(shards) = doc_store() else {
        return false;
    };
    println!("documentation store: {}", doc_store_path().display());
    println!();
    println!(
        "  {:<10} {:>5} {:>7} {:>8} {:>9} {:>8} {:>6}",
        "bundle", "pages", "terms", "postings", "markdown", "index", "probes"
    );
    let (mut src, mut idx) = (0usize, 0usize);
    for s in &shards {
        // A lookup is a binary search over index PAGES, so its cost is the log of how many pages the
        // term table occupies, plus the one read that finishes inside a page. This is the number the
        // layout exists to keep small, so it is the number the build prints.
        let per = manual::index::PAGE / manual::index::TERM_REC;
        let term_pages = s.terms.div_ceil(per).max(1) as u64;
        let probes = 64 - (term_pages - 1).leading_zeros().min(63) + 1;
        println!(
            "  {:<10} {:>5} {:>7} {:>8} {:>9} {:>8} {:>6}",
            s.bundle, s.pages, s.terms, s.postings, s.source, s.index, probes
        );
        src += s.source;
        idx += s.index;
    }
    println!();
    println!("  {src} bytes of markdown, {idx} bytes of index");

    let Some(term) = term else {
        return true;
    };
    println!();
    println!("search: {term}");
    let mut found = 0;
    for (bundle, _) in DOC_BUNDLES {
        let Ok(bytes) = std::fs::read(doc_store_path().join(bundle).join("index")) else {
            continue;
        };
        let mut pages = manual::index::Slice(&bytes);
        let Ok(header) = manual::index::Header::parse(&bytes[..manual::index::PAGE]) else {
            continue;
        };
        let Some(hit) = manual::index::lookup(&header, term.as_bytes(), &mut pages) else {
            continue;
        };
        let mut posts = [manual::index::Posting { page: 0, count: 0 }; 16];
        let n = manual::index::postings(&header, &hit, &mut pages, &mut posts);
        for p in &posts[..n] {
            let Some(rec) = manual::index::page_record(&header, p.page, &mut pages) else {
                continue;
            };
            println!(
                "  {:>4}  {:<24}  {}",
                p.count,
                String::from_utf8_lossy(rec.title()),
                String::from_utf8_lossy(rec.path())
            );
            found += 1;
        }
    }
    if found == 0 {
        println!("  nothing in the store says that");
    }
    true
}

/// Where the RedoxFS test image is written. The runners derive exactly this name from
/// `NIFE_DISK` (`${NIFE_DISK%.img}-redoxfs.img`), so the two stay in lockstep.
fn redoxfs_disk_path() -> String {
    workspace_root()
        .join("target/nifefs-redoxfs.img")
        .display()
        .to_string()
}

/// Drive the `redoxfs_host` tool (its own workspace) by `--manifest-path`, quietly. Returns success.
fn redoxfs_host(args: &[&str]) -> bool {
    let mut v = vec![
        "run",
        "--quiet",
        "--manifest-path",
        "tools/redoxfs_host/Cargo.toml",
        "--",
    ];
    v.extend_from_slice(args);
    run("cargo", &v)
}

/// Build the RedoxFS test image the FS server serves: an empty filesystem with the two fixture
/// files (`motd`, `scratch`) the client reads and writes, plus milestone 47's **subtree**
/// (`sub/` with a file and a grandchild, and the sibling `other/` a directory capability must not
/// reach). Made host-side with the pinned engine, so an image the server opens is proven against
/// exactly the code that opens it. Arch-neutral (the on-disk format does not depend on the CPU), so
/// one image serves both ISA test legs.
fn mkredoxfs() -> bool {
    let img = redoxfs_disk_path();
    // **`NIFE_KEEP_REDOXFS=1` keeps an existing image instead of rebuilding it.** This is the
    // deliberate way to run the second-boot case: run the suite once normally, then again with this
    // set, and every mount in the second run is a mount of an image a previous *boot* wrote. That is
    // the condition the cross-boot write failure needs, and doing it this way keeps it independent of
    // which ISA leg happens to run first (the order-coupling that hid the bug for three rounds).
    // Absent the variable, each leg gets a fresh fixture, which is what makes the legs reproducible.
    if std::env::var_os("NIFE_KEEP_REDOXFS").is_some() && std::path::Path::new(&img).exists() {
        eprintln!("mkredoxfs: keeping the existing image (NIFE_KEEP_REDOXFS)");
        return true;
    }
    // Stage the fixture contents in temp files (the host tool's `put` takes a host file), then load
    // them. The contents live in fs_proto::fixture, shared with the client and the fixture's readers.
    let motd = workspace_root().join("target/redoxfs-motd.tmp");
    let scratch = workspace_root().join("target/redoxfs-scratch.tmp");
    if std::fs::write(&motd, fs_proto::fixture::MOTD).is_err()
        || std::fs::write(&scratch, fs_proto::fixture::SCRATCH_INIT).is_err()
    {
        eprintln!("mkredoxfs: cannot stage the fixture files");
        return false;
    }
    let motd = motd.display().to_string();
    let scratch = scratch.display().to_string();
    let Some(tree) = stage_subtree() else {
        return false;
    };
    redoxfs_host(&["mkfs", &img, "16"])
        && redoxfs_host(&["put", &img, fs_proto::fixture::MOTD_NAME, &motd])
        && redoxfs_host(&["put", &img, fs_proto::fixture::SCRATCH_NAME, &scratch])
        && doc_store().is_some()
        && redoxfs_host(&["import", &img, &tree])
}

/// Stage milestone 47's subtree as a host directory and return its path, for `import` to copy into
/// the image root.
///
/// **`import` rather than a new `mkdir` verb on the host tool**, deliberately: `import` is upstream
/// RedoxFS's own archiver (`redoxfs::archive`), so the directories the confinement tests attack are
/// written by the people who defined the format rather than by us. notes/host-recovery.md already
/// makes that argument for `extract`; this is the same one on the write side.
///
/// The staging directory is rebuilt from scratch each time, because a name left behind by an older
/// fixture would end up in the image and fail the post-run `ls /` check for a reason that has
/// nothing to do with the run.
fn stage_subtree() -> Option<String> {
    use fs_proto::fixture::tree;
    let root = workspace_root().join("target/redoxfs-tree");
    let _ = std::fs::remove_dir_all(&root);
    let sub = root.join(tree::SUB);
    let deeper = sub.join(tree::DEEPER);
    let other = root.join(tree::OTHER);
    // Milestone 47's `rm -r` tree, a sibling of `sub` so a capability to one is provably not a
    // capability to the other. `rm-keep` sits beside the doomed tree, inside the same grant: the
    // program could have removed it and does not, because nothing named it.
    let rmtree = root.join(tree::RMTREE);
    let doomed = rmtree.join(tree::RM_DOOMED);
    let nested = doomed.join(tree::RM_NESTED);
    // Milestone 47's globbing tree, a sibling of both. Two names the pattern matches and two it does
    // not, in one directory, so "the grant is what matched" is a claim about *which* names.
    let globset = root.join(tree::GLOBSET);
    let globdir = globset.join(tree::GLOB_DIR);
    // Milestone 109's batching tree, a sibling of the globbing one: eleven names one pattern
    // matches, which is more than a single grant can carry, so `xargs` has a real directory to
    // sweep at a real prompt. Its own directory rather than more files in `globset`, because the
    // globbing lane asserts that `gl-*.txt` matches exactly two names.
    let globmany = root.join(tree::GLOBMANY);
    // Milestone 50's redirection tree, a sibling of all of them: the witness shell writes files into
    // its root, and it needs somewhere those writes cannot be confused with another test's.
    let redir = root.join(tree::REDIR);
    let mut ok = std::fs::create_dir_all(&globmany).is_ok()
        && std::fs::write(globmany.join(tree::MANY_MISS), tree::MANY_BODY).is_ok();
    for name in tree::MANY_NAMES {
        ok = ok && std::fs::write(globmany.join(name), tree::MANY_BODY).is_ok();
    }
    let ok = ok
        && std::fs::create_dir_all(&deeper).is_ok()
        && std::fs::create_dir_all(&other).is_ok()
        && std::fs::create_dir_all(&nested).is_ok()
        && std::fs::create_dir_all(&globdir).is_ok()
        && std::fs::create_dir_all(&redir).is_ok()
        && std::fs::write(redir.join(tree::REDIR_ONE), tree::REDIR_BODY).is_ok()
        && std::fs::write(redir.join(tree::REDIR_TWO), tree::REDIR_BODY).is_ok()
        && std::fs::write(globset.join(tree::GLOB_ONE), tree::GLOB_BODY).is_ok()
        && std::fs::write(globset.join(tree::GLOB_TWO), tree::GLOB_BODY).is_ok()
        && std::fs::write(globset.join(tree::GLOB_MISS), tree::GLOB_BODY).is_ok()
        && std::fs::write(globdir.join(tree::GLOB_INNER), tree::GLOB_BODY).is_ok()
        && std::fs::write(sub.join(tree::INNER), tree::INNER_BODY).is_ok()
        && std::fs::write(deeper.join(tree::LEAF), tree::LEAF_BODY).is_ok()
        && std::fs::write(other.join(tree::SECRET), tree::SECRET_BODY).is_ok()
        && std::fs::write(rmtree.join(tree::RM_KEEP), tree::RM_KEEP_BODY).is_ok()
        && std::fs::write(rmtree.join(tree::RM_SOLO), tree::RM_BODY).is_ok()
        && std::fs::write(doomed.join(tree::RM_ONE), tree::RM_BODY).is_ok()
        && std::fs::write(doomed.join(tree::RM_TWO), tree::RM_BODY).is_ok()
        && std::fs::write(nested.join(tree::RM_LEAF), tree::RM_BODY).is_ok();
    if !ok {
        eprintln!("mkredoxfs: cannot stage the milestone-47 subtree");
        return None;
    }
    Some(root.display().to_string())
}

/// Where the **crash test's** RedoxFS image is written (milestone 37). The runners derive exactly
/// this name from `NIFE_DISK`, the way they derive the shared one.
fn crash_disk_path() -> String {
    workspace_root()
        .join("target/nifefs-redoxfs-crash.img")
        .display()
        .to_string()
}

/// Build the crash test's image: an empty filesystem with one file, `cut`, holding a known value.
///
/// **Its own disk, and regenerated every run, both deliberately** (milestone 37, DECISIONS §34
/// condition 1). The crash test kills an FS server mid-transaction and leaves the filesystem
/// half-written on purpose. Pointing it at the shared fixture would make every other FS test's
/// result depend on whether this one had run first, and keeping the image across runs would make
/// each run's starting state a function of the last one's damage. Both are the order-coupled fixture
/// DECISIONS §27 spent a day on, so `NIFE_KEEP_REDOXFS` deliberately does **not** apply here: the
/// cross-boot case is interesting for the shared disk and is nothing but noise for this one.
fn mkredoxfs_crash() -> bool {
    let img = crash_disk_path();
    let initial = workspace_root().join("target/redoxfs-crash-initial.tmp");
    if std::fs::write(&initial, fs_proto::fixture::crash::INITIAL).is_err() {
        eprintln!("mkredoxfs_crash: cannot stage the fixture file");
        return false;
    }
    let initial = initial.display().to_string();
    redoxfs_host(&["mkfs", &img, "16"])
        && redoxfs_host(&["put", &img, fs_proto::fixture::crash::NAME, &initial])
}

/// Where the GPT-partitioned test image is written. The runners derive exactly this name from
/// `NIFE_DISK` (`${NIFE_DISK%.img}-gpt.img`), so the two stay in lockstep.
fn gpt_disk_path() -> String {
    workspace_root()
        .join("target/nifefs-gpt.img")
        .display()
        .to_string()
}

/// Build the milestone-57 test disk: a 64 MiB image whose partition table **`sgdisk` wrote**.
///
/// The point of this image is its provenance. `crates/gpt` can lay out a table, and a disk it laid
/// out would test the reader against the writer, which is the weakest test available: every mistake
/// made on the way out is made symmetrically on the way in. So the bytes come from the committed
/// fixture in `crates/gpt/tests/fixtures/`, produced by `sgdisk` 1.0.10 (gptfdisk, C++), and the
/// guest reads a table this project did not write. The regeneration commands are at the top of
/// `crates/gpt/tests/real_disks.rs`.
///
/// The fixture is the first 34 and last 33 blocks of a 64 MiB disk, which is exactly the primary
/// table and the backup table with the 64 MiB of nothing between them left out. Reconstituting it is
/// therefore the head, a run of zeros, and the tail. Nothing is put in the partitions: what is under
/// test is finding them.
fn mkgptdisk() -> bool {
    const BLOCK: usize = 512;
    const BLOCKS: usize = 131_072; // 64 MiB
    let dir = workspace_root().join("crates/gpt/tests/fixtures");
    let (Ok(head), Ok(tail)) = (
        std::fs::read(dir.join("sgdisk-64m.head")),
        std::fs::read(dir.join("sgdisk-64m.tail")),
    ) else {
        eprintln!("mkgptdisk: cannot read the sgdisk fixtures");
        return false;
    };
    if head.len() != 34 * BLOCK || tail.len() != 33 * BLOCK {
        eprintln!(
            "mkgptdisk: the fixtures are {} and {} bytes; expected {} and {}",
            head.len(),
            tail.len(),
            34 * BLOCK,
            33 * BLOCK,
        );
        return false;
    }
    let mut img = std::vec![0u8; BLOCKS * BLOCK];
    img[..head.len()].copy_from_slice(&head);
    let at = BLOCKS * BLOCK - tail.len();
    img[at..].copy_from_slice(&tail);
    let path = gpt_disk_path();
    if let Err(e) = std::fs::write(&path, &img) {
        eprintln!("mkgptdisk: could not write {path}: {e}");
        return false;
    }
    true
}

/// Where the blank test disk is written. The runners derive exactly this name from `NIFE_DISK`
/// (`${NIFE_DISK%.img}-blank.img`), so the two stay in lockstep.
fn blank_disk_path() -> String {
    workspace_root()
        .join("target/nifefs-blank.img")
        .display()
        .to_string()
}

/// Where the NVMe test image is written; the runners take the full path in `NIFE_NVME` rather
/// than deriving it, because unlike the mmio disks it does not ride beside `NIFE_DISK`.
fn nvme_disk_path() -> String {
    workspace_root()
        .join("target/nife-nvme.img")
        .display()
        .to_string()
}

/// The NVMe test image (milestone 53's storage half): 8 MiB of zeros behind QEMU's `-device nvme`.
/// Zeros because the boot test's negative check is that an untouched block still reads as the
/// image's zeros after a neighboring block was written; 8 MiB because the test asserts IDENTIFY's
/// size answer against exactly this number, so the file and the assertion must move together
/// (kernel/src/nvme.rs). Regenerated per leg like the blank disk, and for the same reason: the
/// test writes it, and a leg starting from the previous leg's damage is not reproducible alone.
fn mknvmedisk() -> bool {
    let path = nvme_disk_path();
    if let Err(e) = std::fs::write(&path, std::vec![0u8; 8 * 1024 * 1024]) {
        eprintln!("mknvmedisk: could not write {path}: {e}");
        return false;
    }
    true
}

/// Build milestone 57's write-half disk: 64 MiB of **zeros**, and that is the whole point.
///
/// It carries no table, no filesystem and no fixture, because what the guest is going to do to it is
/// write both. Regenerated every run and never shared, for milestone 37's reason (DECISIONS §27): a
/// test that partitions a disk cannot be pointed at an image another test reads, and a test whose
/// starting state is last run's damage is not reproducible on its own. `NIFE_KEEP_REDOXFS` does
/// not apply here for the same reason it does not apply to the crash image.
fn mkblankdisk() -> bool {
    let path = blank_disk_path();
    let bytes = std::vec![0u8; (fs_proto::fixture::blank::DISK_BLOCKS * fs_proto::fixture::blank::LBA) as usize];
    if let Err(e) = std::fs::write(&path, &bytes) {
        eprintln!("mkblankdisk: could not write {path}: {e}");
        return false;
    }
    true
}

/// After a test run, read the blank disk back **from the host** and check what the guest put on it:
/// the partition table with `crates/gpt`, and the filesystem inside the data partition with the
/// pinned engine through `tools/redoxfs_host`.
///
/// This is the half a guest-side assertion cannot make. The in-guest check reads the filesystem
/// through the same block server that wrote it, on the same machine, minutes later; this is a
/// different program, on a different operating system, with a different engine build, opening the
/// file the run left behind. If the two ever disagreed, the guest would be the one to doubt.
///
/// **The filesystem is read out of the partition in place** (milestone 110). This used to slice the
/// partition into its own file first, because the tool took an image rather than a device plus a
/// selector; that was twenty lines of the join written in a build script, and it is now in the tool
/// where a person can use it. The partition is named by **type GUID**, not by slot number, for the
/// same reason the guest's `mkfs` finds it that way: the type is what the partition is, and the slot
/// is a fact about this table's current order.
fn blank_check_after_run() -> bool {
    use fs_proto::fixture::blank;

    let path = blank_disk_path();
    let Ok(img) = std::fs::read(&path) else {
        eprintln!("BLANK IMAGE CHECK FAILED: cannot read {path}");
        return false;
    };
    let lba = blank::LBA as usize;
    if img.len() < 34 * lba {
        eprintln!("BLANK IMAGE CHECK FAILED: {path} is {} bytes", img.len());
        return false;
    }

    // The table the guest wrote, judged by the parser the guest did not run: this process's own
    // copy, on the host, against the bytes on disk.
    if let Err(e) = gpt::mbr::validate(&img[..lba], blank::DISK_BLOCKS) {
        eprintln!("BLANK IMAGE CHECK FAILED: the protective MBR the guest wrote is bad: {e:?}");
        return false;
    }
    let table = match gpt::Gpt::parse(&img[lba..2 * lba], &img[2 * lba..34 * lba]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "BLANK IMAGE CHECK FAILED: the guest's partition table does not parse: {e:?}"
            );
            return false;
        }
    };
    let parts: Vec<_> = table.partitions().collect();
    if parts.len() != blank::PARTITIONS {
        eprintln!(
            "BLANK IMAGE CHECK FAILED: the guest wrote {} partitions, expected {}",
            parts.len(),
            blank::PARTITIONS,
        );
        return false;
    }
    // Every unique GUID distinct and version 4. Two partitions with one id is the failure the
    // entropy capability exists to prevent, and it is invisible to everything else here.
    for (i, (_, part)) in parts.iter().enumerate() {
        let text = part.unique_guid.to_ascii();
        if text[14] != b'4' {
            eprintln!(
                "BLANK IMAGE CHECK FAILED: partition {i}'s unique GUID is not version 4: {}",
                String::from_utf8_lossy(&text),
            );
            return false;
        }
        for (j, (_, other)) in parts.iter().enumerate() {
            if i != j && part.unique_guid == other.unique_guid {
                eprintln!("BLANK IMAGE CHECK FAILED: partitions {i} and {j} share a unique GUID");
                return false;
            }
        }
    }
    if !parts
        .iter()
        .any(|(_, p)| p.type_guid == gpt::guid::types::NIFE_DATA)
    {
        eprintln!("BLANK IMAGE CHECK FAILED: no nife data partition on the guest's disk");
        return false;
    }

    // The filesystem inside it, opened by the pinned engine on the host, at the offset the tool
    // works out from the same table this function just checked.
    let data_type = String::from_utf8_lossy(&gpt::guid::types::NIFE_DATA.to_ascii()).into_owned();
    let out = capture(
        "cargo",
        &[
            "run",
            "--quiet",
            "--manifest-path",
            "tools/redoxfs_host/Cargo.toml",
            "--",
            "cat",
            &path,
            blank::MADE_NAME,
            "--partition-type",
            &data_type,
        ],
    );
    match out.as_deref() {
        Some(s) if s.as_bytes() == blank::MADE_BODY => {
            eprintln!(
                "blank image: the guest partitioned it ({} partitions, distinct v4 GUIDs) and the \
                 host engine reads `{}` out of the filesystem the guest created",
                parts.len(),
                blank::MADE_NAME,
            );
            true
        }
        other => {
            eprintln!(
                "BLANK IMAGE CHECK FAILED: the host tool did not read the guest's file back (got \
                 {:?}). The table is fine, so this is the filesystem `mkfs` made.",
                other.unwrap_or("<host tool error: the partition did not even open>"),
            );
            false
        }
    }
}

/// After a test run, reopen the **crash** image with the host tool and confirm the property holds
/// from outside the guest: `cut` reads back as exactly one of the two payloads, whole.
///
/// This is the half a cache cannot fake. The guest's verifier read the file back through an FS
/// server that had just mounted the damaged disk; this is a different process, on the host, with the
/// pinned engine, opening the image the run left behind. It also proves the image is still a
/// consistent RedoxFS at all, because `cat` cannot succeed on one that is not.
fn redoxfs_crash_check_after_run() -> bool {
    let out = capture(
        "cargo",
        &[
            "run",
            "--quiet",
            "--manifest-path",
            "tools/redoxfs_host/Cargo.toml",
            "--",
            "cat",
            &crash_disk_path(),
            fs_proto::fixture::crash::NAME,
        ],
    );
    let want_a = fs_proto::fixture::crash::A;
    let want_b = fs_proto::fixture::crash::B;
    match out.as_deref() {
        Some(s) if s.as_bytes() == want_a => {
            eprintln!(
                "crash image: `cut` holds payload A, whole (the interrupted write is absent)"
            );
            true
        }
        Some(s) if s.as_bytes() == want_b => {
            eprintln!(
                "crash image: `cut` holds payload B, whole (the interrupted write completed)"
            );
            true
        }
        other => {
            eprintln!(
                "CRASH CONSISTENCY FAILED: after a kill mid-transaction the image's `cut` is \
                 neither payload whole (got {:?}). A write must be wholly present or wholly absent.",
                other.unwrap_or("<host tool error: the image did not even open>"),
            );
            false
        }
    }
}

/// After a test run, reopen the image with the host tool and confirm it still parses, that the file
/// the FS server served reads back byte for byte, and that the write the `std::fs` test performed
/// **reached the disk**. `cat` succeeding at all proves the image is still a consistent RedoxFS
/// after the run (the FS server opened it read-write with cleanup, which advances the header ring);
/// the bytes prove nothing was corrupted.
///
/// The `scratch` half is the on-disk half of the write proof, and it is the part a cache cannot
/// fake: the guest read its own write back through the same FS server, but this reopens the image
/// with a different process and the pinned engine. It is also what closes the write blocker
/// notes/fs-server.md used to record, so it belongs in the gate, not in a comment.
fn redoxfs_check_after_run() -> bool {
    redoxfs_reads_back(fs_proto::fixture::MOTD_NAME, fs_proto::fixture::MOTD)
        && redoxfs_reads_back(
            fs_proto::fixture::SCRATCH_NAME,
            fs_proto::fixture::WRITE_PATTERN,
        )
        && redoxfs_subtree_was_confined()
        && redoxfs_glob_grant_took_exactly_the_match()
}

/// **The set grant, witnessed from outside the guest** (milestone 47's globbing lane).
///
/// The guest reports that `echo gl-*.txt` and the grant `rm gl-*.txt` would transfer are the same
/// names, and that a `rm` behind a nameset caretaker removed what it held. Both are statements by
/// the thing under test. This is the other kind: the host, with the pinned engine, reading the
/// image the run left behind.
///
/// Three claims, and they only mean anything together:
///
/// 1. **The two matched names are gone.** The expansion the guest printed is what actually
///    disappeared, so "the expansion is the grant" is a fact about the disk.
/// 2. **The two names the pattern did not match are still there.** They sit in the same directory,
///    one entry away, and the caretaker a hop up holds a capability that could remove either. So
///    their survival is a fact about the *set*, not about what was reachable.
/// 3. **The unmatched directory still holds its file.** A `rm` that had walked into it would have
///    emptied it, and a set capability carrying no `-r` cannot even look inside one it *did* match.
fn redoxfs_glob_grant_took_exactly_the_match() -> bool {
    use fs_proto::fixture::tree;
    let img = redoxfs_disk_path();
    let Some(globset) = redoxfs_ls(&img, tree::GLOBSET) else {
        eprintln!("milestone-47 glob check: `{}` did not list", tree::GLOBSET);
        return false;
    };
    // No RedoxFS disk, or a boot that never ran the test: the fixture is untouched, and this check
    // has nothing to say. `redoxfs_subtree_was_confined` makes the same allowance for the same
    // reason (a `run` that skipped the FS tests must not fail the gate).
    let matched_gone = ![tree::GLOB_ONE, tree::GLOB_TWO]
        .iter()
        .any(|n| globset.iter().any(|got| got == n));
    if !matched_gone && globset.len() == 4 {
        eprintln!(
            "milestone-47 glob check: `{}` is untouched; the guest never ran the set grant (skipping)",
            tree::GLOBSET
        );
        return true;
    }
    for name in [tree::GLOB_ONE, tree::GLOB_TWO] {
        if globset.iter().any(|got| got == name) {
            eprintln!(
                "MILESTONE-47 GLOB FAILED: `{name}` matched the pattern and is still in `{}` \
                 ({globset:?}). What the expansion showed is not what the grant took away.",
                tree::GLOBSET,
            );
            return false;
        }
    }
    for name in [tree::GLOB_MISS, tree::GLOB_DIR] {
        if !globset.iter().any(|got| got == name) {
            eprintln!(
                "MILESTONE-47 GLOB FAILED: `{name}` did NOT match the pattern and is gone from \
                 `{}` ({globset:?}). A set capability reached a name one directory entry away that \
                 the command line never designated.",
                tree::GLOBSET,
            );
            return false;
        }
    }
    let inner = format!("{}/{}", tree::GLOBSET, tree::GLOB_DIR);
    match redoxfs_ls(&img, &inner) {
        Some(names) if names.iter().any(|n| n == tree::GLOB_INNER) => {
            eprintln!(
                "glob grant: `{}` matched two names in `{}` and exactly those two are gone; \
                 {globset:?} is what the pattern did not designate",
                core::str::from_utf8(tree::GLOB_PATTERN).unwrap_or("?"),
                tree::GLOBSET,
            );
            true
        }
        other => {
            eprintln!(
                "MILESTONE-47 GLOB FAILED: `{inner}` holds {other:?} and should still hold `{}`. \
                 An unmatched directory was walked into.",
                tree::GLOB_INNER,
            );
            false
        }
    }
}

/// **The directory capability's confinement, asserted from outside the confined program**
/// (milestone 47).
///
/// The in-guest attacker reports a bitmap of what got through, which is a statement by the thing
/// being tested. This is the other kind of evidence: a different process, on the host, with the
/// pinned engine, reading the image the run left behind. Four claims, and each one is an escape
/// that no in-guest verdict could have reported, because a program that broke out and then lied
/// would still have left the file on the disk.
///
/// 1. **The fixture's own names are all still in the image root.** A capability granted on `sub`
///    can remove nothing above itself, so a missing name here is an escape too.
/// 2. **Nothing the attacker made is in the root.** It was granted `sub` and creates inside it, so
///    a name of its making at this level got out.
/// 3. **Its creations ARE in `sub`**, which is what stops claim 2 from being vacuous: an attacker
///    that created nothing would satisfy it perfectly, and so would a caretaker that refused
///    everything. And `sub` holds both a **renamed** name and an **un**renamed one, which is the
///    `REMOVE` rung witnessed from outside the guest: one capability moved a name and another,
///    running the same code against the same directory, could not.
/// 4. **The two files nothing was granted the authority to change read back byte for byte.**
///    `other/secret` is one directory entry away from the grant and the FS server can reach it on
///    any request it likes; `sub/inner` is *inside* the grant, and the attacker writes only to what
///    it made, so a change there means it wrote through something it should not have.
///
/// **BUGS.** Claim 1 checks containment, not equality, and that is deliberate rather than lazy: the
/// root is shared with every other test in the boot, and the `std::fs` test creates `made-by-std`
/// in it. An exact comparison would make this check fail whenever an unrelated test started or
/// stopped writing a file, which is a coupling that manufactures facts (DECISIONS §27). The cost is
/// that a leaked name whose spelling matches neither fixture prefix would slip past claim 2, so the
/// attacker's names are the thing this check is precise about.
fn redoxfs_subtree_was_confined() -> bool {
    use fs_proto::fixture::tree;
    let img = redoxfs_disk_path();

    let (Some(root), Some(sub)) = (redoxfs_ls(&img, "/"), redoxfs_ls(&img, tree::SUB)) else {
        eprintln!("milestone-47 confinement check: the image did not list after the run");
        return false;
    };
    for want in tree::ROOT_ENTRIES {
        if !root.iter().any(|n| n == want) {
            eprintln!(
                "MILESTONE-47 CONFINEMENT FAILED: `{want}` is gone from the image root (it holds \
                 {root:?}). Nothing in this run held a capability that could remove it.",
            );
            return false;
        }
    }
    // A run index is appended to each name so three attacker runs sharing one image do not collide,
    // so the prefix is what identifies a creation rather than the whole name.
    let attackers_own = |n: &String| {
        n.starts_with(tree::MADE) || n.starts_with(tree::MADE_DIR) || n.starts_with(tree::MOVED)
    };
    if let Some(leaked) = root.iter().find(|n| attackers_own(n)) {
        eprintln!(
            "MILESTONE-47 CONFINEMENT FAILED: `{leaked}` is in the image ROOT. A program granted a \
             capability to `{}` created a name in its parent.",
            tree::SUB,
        );
        return false;
    }
    let count = |prefix: &str| sub.iter().filter(|n| n.starts_with(prefix)).count();
    let (made_files, made_dirs, moved) =
        (count(tree::MADE), count(tree::MADE_DIR), count(tree::MOVED));
    if made_files == 0 || made_dirs == 0 {
        eprintln!(
            "milestone-47 confinement check: `{}` holds {sub:?}, with {made_files} created files \
             and {made_dirs} created directories. The attacker created nothing, so \"nothing it \
             made escaped to the root\" is true of a capability that reaches nothing at all.",
            tree::SUB,
        );
        return false;
    }
    // `made_files` counts the names that were created and NOT renamed, so requiring both counts to
    // be non-zero is the `REMOVE` rung asserted from out here: a capability carrying it moved a
    // name, and one without it left its own name exactly where it made it.
    if moved == 0 {
        eprintln!(
            "MILESTONE-47 CONFINEMENT FAILED: `{}` holds {sub:?} and nothing was renamed. A \
             capability carrying REMOVE and CREATE must be able to move a name it made, or the \
             refusals the other runs report are refusals of a verb that never works.",
            tree::SUB,
        );
        return false;
    }
    let sibling = format!("{}/{}", tree::OTHER, tree::SECRET);
    let granted = format!("{}/{}", tree::SUB, tree::INNER);
    redoxfs_reads_back(&sibling, tree::SECRET_BODY)
        && redoxfs_reads_back(&granted, tree::INNER_BODY)
        && shell_navigation_landed(&root, &sub)
        && match redoxfs_ls(&img, tree::OTHER) {
            // The **second** shell's leavings, in the sibling it was rooted at. Checking only `sub`
            // would leave the headline property half-witnessed from out here: two shells with two
            // roots each wrote into their own and neither into the other's.
            Some(other) => shell_navigation_landed(&root, &other),
            None => false,
        }
}

/// **`rm` witnessed from outside the guest** (milestone 47's commands).
///
/// The navigating shell reports that its `rm` worked and that the handle it still held kept reading
/// the bytes. Both are statements by the thing under test. This is the other kind of evidence: the
/// host, with the pinned engine, reading the image the run left behind, where the name it removed
/// must not be, the name it kept must be, and neither may have appeared in the root.
///
/// The pair is what makes it non-vacuous. A shell that created nothing satisfies "the removed name
/// is absent" perfectly, so the kept name has to be there beside it.
fn shell_navigation_landed(root: &[String], home: &[String]) -> bool {
    use fs_proto::fixture::tree;
    let count = |dir: &[String], prefix: &str| dir.iter().filter(|n| n.starts_with(prefix)).count();

    if count(home, tree::NAV_KEPT) == 0 || count(home, tree::NAV_DIR) == 0 {
        eprintln!(
            "milestone-47 navigation check: a shell's root holds {home:?}, with nothing a \
             navigating shell made in it. \"what it removed is gone\" is true of a shell that \
             created nothing, so this proves nothing without it.",
        );
        return false;
    }
    if count(home, tree::NAV_GONE) != 0 {
        eprintln!(
            "MILESTONE-47 NAVIGATION FAILED: a shell's root still holds a `{}` name, so its `rm` \
             reported success and never reached the platter: {home:?}",
            tree::NAV_GONE,
        );
        return false;
    }
    let leaked = |n: &&String| {
        n.starts_with(tree::NAV_KEPT)
            || n.starts_with(tree::NAV_GONE)
            || n.starts_with(tree::NAV_DIR)
    };
    if let Some(name) = root.iter().find(leaked) {
        eprintln!(
            "MILESTONE-47 NAVIGATION FAILED: `{name}` is in the image ROOT. A shell rooted at a \
             subtree made a name in its parent.",
        );
        return false;
    }
    true
}

/// The names in one directory of the post-run image, sorted, via the host tool's `ls`. Its output is
/// `kind size name` per line and the fixture's names carry no spaces, so the name is the last field.
fn redoxfs_ls(image: &str, path: &str) -> Option<Vec<String>> {
    let out = capture(
        "cargo",
        &[
            "run",
            "--quiet",
            "--manifest-path",
            "tools/redoxfs_host/Cargo.toml",
            "--",
            "ls",
            image,
            path,
        ],
    )?;
    let mut names: Vec<String> = out
        .lines()
        .filter_map(|l| l.split_whitespace().last().map(str::to_string))
        .collect();
    names.sort();
    Some(names)
}

/// `cat` one file out of the post-run image with the host tool and compare it byte for byte.
fn redoxfs_reads_back(name: &str, want: &[u8]) -> bool {
    let out = capture(
        "cargo",
        &[
            "run",
            "--quiet",
            "--manifest-path",
            "tools/redoxfs_host/Cargo.toml",
            "--",
            "cat",
            &redoxfs_disk_path(),
            name,
        ],
    );
    match out {
        Some(s) if s.as_bytes() == want => true,
        other => {
            eprintln!(
                "redoxfs consistency check failed: {name} did not read back after the run (got {:?})",
                other.as_deref().unwrap_or("<host tool error>")
            );
            false
        }
    }
}

fn user_elf() -> String {
    // ABSOLUTE, and that is not fussiness.
    //
    // Cargo runs the runner script with the working directory set to the **package** dir for
    // `cargo test` and the workspace root for `cargo run`. A relative path therefore resolved
    // under `cargo run` and silently did not under `cargo test`, so the tests booted with no
    // initrd at all and the one that noticed was the one that panicked.
    workspace_root()
        .join(format!("target/{TARGET}/{}/hello", profile_dir()))
        .display()
        .to_string()
}

/// The ELF path of a named binary the `user` package builds beside `hello` (milestone 19f.2+):
/// `worker`, `console`, and so on. `mkinitrd` packs each into the archive under that same name.
fn bin_elf(name: &str) -> String {
    workspace_root()
        .join(format!("target/{TARGET}/{}/{name}", profile_dir()))
        .display()
        .to_string()
}

/// The repo root, from the *compile-time* location of this crate, so it does not depend on
/// whatever directory cargo happens to hand us.
fn workspace_root() -> std::path::PathBuf {
    // Runtime, not env!: the compile-time form bakes the absolute path into the binary, and a
    // cached xtask built before the checkout moved (the 2026-08-15 cricker-os -> nife rename)
    // then aims every path it computes, the farm, the initrds, the images, at a directory that
    // no longer exists. Cargo sets the variable at run time for every cargo-invoked binary, and
    // that one is always the live path. The render test in crates/manual had the same bug the
    // same day; if a third place grows this pattern, it is worth a lint.
    let manifest =
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for xtask");
    std::path::Path::new(&manifest)
        .parent()
        .expect("xtask has no parent directory")
        .to_path_buf()
}

/// The value of a `--name value` or `--name=value` flag on our own command line, if it is there.
///
/// Both spellings, because a caller who has typed `--cpu=sifive-u54` once should not have to learn
/// that this particular tool only accepts one of them. Returns `None` when the flag is absent and
/// when it is present with nothing after it, which the callers treat as "not given"; a flag whose
/// value went missing is a typo, and defaulting is friendlier than a panic in a build tool.
fn flag_value(name: &str) -> Option<String> {
    let mut args = std::env::args();
    let eq = format!("{name}=");
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
}

/// The architecture legs `test` should run: both by default, one when `--arch` names it.
///
/// **`--arch` did not exist before milestone 59**, and this is the correction worth stating: the
/// milestone brief said to follow how it "already threads through", and nothing in the tree parsed
/// it. `test` ran both ISA legs unconditionally, which is right for the parity gate (§19) and wrong
/// for a CPU-model matrix that wants the riscv64 leg four times over with a different `-cpu` each
/// time. So the flag is new here, and the default is unchanged: no `--arch` means both legs, and
/// the parity gate cannot be weakened by forgetting to pass something.
#[derive(Clone, Copy, PartialEq)]
enum ArchLegs {
    Both,
    Aarch64,
    Riscv64,
}

impl ArchLegs {
    fn aarch64(self) -> bool {
        self != ArchLegs::Riscv64
    }
    fn riscv64(self) -> bool {
        self != ArchLegs::Aarch64
    }
}

/// Host tests first, then the kernel under QEMU.
///
/// The host crates (`dtb`, `frames`) hold the pure logic and run in *milliseconds* with no
/// emulator, so they fail fast and cheap. Only once they pass is it worth spending twenty
/// seconds booting QEMU. See DECISIONS.md §7.
///
/// Three flags narrow what runs, and all three default to today's behaviour:
///
/// - `--arch aarch64|riscv64` runs one ISA leg instead of both (milestone 59).
/// - `--cpu <model>` picks the emulated CPU model (`NIFE_CPU`, read by both QEMU runners).
///   Unset means `cortex-a72` on aarch64 and `rv64` on riscv64, exactly as before (milestone 59).
/// - `--hvf` runs the aarch64 kernel leg on the physical Apple Silicon core (milestone 81). It is
///   aarch64-only by construction, so it narrows the run to that leg and refuses `--cpu`; see
///   [`hvf_kernel_leg`] for the mechanism and notes/hvf-leg.md for what differs.
///
/// `script/cpu_matrix` is the caller that needs the first two (notes/cpu-models.md); `script/gates`
/// is the caller that needs the third.
fn test() -> bool {
    // Milestone 81. Read before `--arch`, because it constrains it: Hypervisor.framework runs the
    // host's own ISA and this host is aarch64, so there is no riscv64 leg to accelerate and asking
    // for one is a mistake worth naming rather than ignoring.
    let hvf = std::env::args().any(|a| a == "--hvf");
    let legs = match flag_value("--arch").as_deref() {
        None if hvf => ArchLegs::Aarch64,
        None => ArchLegs::Both,
        Some("aarch64") => ArchLegs::Aarch64,
        Some("riscv64") if hvf => {
            eprintln!(
                "test: --hvf is aarch64 only (Hypervisor.framework runs this host's own ISA; riscv64 \
                 has no equivalent until the board lands)"
            );
            return false;
        }
        Some("riscv64") => ArchLegs::Riscv64,
        Some(other) => {
            eprintln!("test: --arch {other} is not an architecture (aarch64 or riscv64)");
            return false;
        }
    };
    if hvf && flag_value("--cpu").is_some() {
        eprintln!(
            "test: --cpu cannot apply under --hvf (the guest runs the physical core; -cpu host is \
             mandatory)"
        );
        return false;
    }
    // The CPU model rides to the runners in the environment rather than on the QEMU command line,
    // because cargo owns that command line: the runner is invoked by cargo, and the only channel we
    // have to it is env. Unset it when no flag was given so a stale value from the caller's shell
    // cannot silently change what a plain `script/test` means.
    match flag_value("--cpu") {
        Some(model) => {
            eprintln!("--- CPU model: {model} (NIFE_CPU) ---");
            // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
            // threads. xtask is single-threaded here: this runs on the main thread before the child
            // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
            // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
            unsafe { std::env::set_var("NIFE_CPU", model) };
        }
        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
        // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
        None => unsafe { std::env::remove_var("NIFE_CPU") },
    }

    // Nothing cargo starts inherits an accelerator choice. The default leg is TCG, which is the
    // right place for reproducible tests (deterministic, identical on any host), and the HVF leg
    // does not go through cargo at all: it sets `NIFE_ACCEL` on the one child that needs it
    // (see `hvf_kernel_leg`), so a stale value from the caller's shell cannot reach anything else.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::remove_var("NIFE_ACCEL") };
    if hvf {
        eprintln!(
            "--- host tests, the vendored redoxfs round trip and the fs_server core: SKIPPED under \
             --hvf ---"
        );
        eprintln!(
            "    They are host code on the host; no accelerator exists on that path, so running \
             them again would cost ~30 s and prove nothing the TCG leg has not. What --hvf re-runs \
             is the part an accelerator can change: the kernel, under QEMU."
        );
    }
    if !hvf {
        eprintln!("--- host tests (pure logic, no emulator) ---");
        // Every host crate, by asking cargo which ones those are instead of listing them.
        //
        // This was a hand-maintained list of twenty `-p` flags, and it drifted exactly the way a
        // hand-maintained list does. It was written because `paging`, `heap` and `slab` were silently not
        // run for four milestones; by milestone 51 it had five crates missing again, and `fs_proto`,
        // `compositor`, `video_terminal`, `bitfont` and `grant_plan` carried **82 host tests that this gate never ran**. All
        // 82 passed when finally run, which is the point: nobody noticed because nothing failed, and a
        // gate that quietly covers less than it claims is the failure mode script/fmt's `--check` bug
        // already cost this project a day over.
        //
        // The exclusions are every crate that cannot compile for the host, which means `user_rt` (EL0
        // syscall `asm!`) and everything that depends on it.
        //
        // **`--exclude` removes a package from the test SELECTION, not from the dependency graph.**
        // Excluding `user_rt` alone stopped working on 2026-08-03, when `swap_proto`, `virtio` and
        // `supervision_proto` took unconditional `user_rt` dependencies (`system_initializer`
        // followed a day later): cargo still had to build it for them, so the host pass stopped
        // compiling on an x86_64 host and nobody noticed, because CI moved to `ubuntu-24.04-arm` the
        // same day and on an aarch64 host it builds by accident. A stranger with a clean x86_64
        // checkout found it on 2026-08-14 (milestone 117's first run).
        //
        // `script/lint`'s "host pass excludes exactly the bare-metal crates" gate now DERIVES this
        // set from `cargo metadata` and fails if this list disagrees with it, so the next crate to
        // take a `user_rt` dependency breaks the gate rather than the host build.
        if !cargo(&[
            "test",
            "--workspace",
            "--exclude",
            "kernel",
            "--exclude",
            "user",
            "--exclude",
            "user_rt",
            "--exclude",
            "swap_proto",
            "--exclude",
            "virtio",
            "--exclude",
            "supervision_proto",
            "--exclude",
            "system_initializer",
        ]) {
            return false;
        }

        // The vendored RedoxFS pin (vendor/redoxfs, milestone 32) is kept honest here, both halves of
        // vendor/README.md's promise. Both are driven by --manifest-path because the engine and the
        // host tool are their OWN workspaces, deliberately outside ours so upstream code never reaches
        // our clippy/fmt gates (see the workspace `exclude` in Cargo.toml).
        //
        // First: the host tool's round trip (mkfs, put, ls, cat) against the pinned engine, the same
        // code phase 2's FS server will open images with, so a regression is caught on the host in
        // milliseconds. Second: the engine's no_std core built for BOTH bare-metal targets, because
        // upstream does not CI the no_std path and it bit-rotted once already (the two Vec imports the
        // pin carries); this build catches the next such regression instead of phase 2 doing it.
        eprintln!();
        eprintln!("--- vendored redoxfs: host round trip + no_std core (both targets) ---");
        if !run(
            "cargo",
            &["test", "--manifest-path", "tools/redoxfs_host/Cargo.toml"],
        ) {
            return false;
        }
        // The FS server's sans-IO core (fs_server, its own workspace): open, read, write, close against
        // a real RedoxFS image in memory, in milliseconds. This proves the filesystem logic for BOTH the
        // read and write paths on the host, which the on-device test can only do for reads today.
        eprintln!();
        eprintln!("--- fs_server sans-IO core (host, its own workspace) ---");
        if !run(
            "cargo",
            &["test", "--manifest-path", "fs_server/Cargo.toml"],
        ) {
            return false;
        }
        for target in [TARGET, RISCV_TARGET] {
            if !run(
                "cargo",
                &[
                    "build",
                    "--manifest-path",
                    "vendor/redoxfs/Cargo.toml",
                    "--no-default-features",
                    "--target",
                    target,
                ],
            ) {
                return false;
            }
        }
    }

    // Build the std demo (milestone 27) for both custom targets first, so both initrds carry it:
    // mkinitrd (inside `user`) packs the aarch64 std_exerciser, initrd_riscv packs the riscv one. Outside
    // the leg guards below because BOTH legs need it, and the nifefs data disk with it: it is
    // arch-neutral, and the riscv leg reads it whether or not the aarch64 leg ran.
    if !std_exerciser() || !mkdisk() {
        return false;
    }
    // Attach a virtio-gpu for the display test (milestone 29). Set here, in `test`, rather than in
    // `cargo()`: the benchmark boot uses the same runner and adding a device to it would change what
    // the icount instrument measures, so the GPU is a test-leg device only. Both ISA legs get it,
    // because parity is the gate (§19), and the display test ASSERTS the device is present rather
    // than skipping, so a leg that lost this line fails loudly.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_GPU", "1") };
    // And a virtio keyboard (milestone 29's input), on the same terms and for the same reason: a
    // test-leg device only, on both ISA legs, and the keyboard test ASSERTS one is present rather
    // than skipping, so a leg that lost this line fails loudly instead of quietly proving nothing.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_KBD", "1") };
    // And two virtio-rng devices, one per transport (milestone 56, the entropy half), on the same
    // terms again: a test-leg device only, both ISA legs, and the entropy tests ASSERT a device on
    // each bus rather than skipping. Out of the benchmark boot for the same reason as the GPU: it
    // shares the runner, and a device the instrument did not measure last time is drift.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_RNG", "1") };
    // And an NVMe controller (milestone 53's storage half), on the same terms: a test-leg device
    // only (the benchmark boot shares the runner and must not grow devices its instrument never
    // measured), on both ISA legs because parity is the gate (§19), and the NVMe test ASSERTS the
    // controller is present rather than skipping. The variable carries the image path; each leg
    // regenerates the image below, beside the other write-target disks.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_NVME", nvme_disk_path()) };

    if legs.aarch64() {
        eprintln!();
        eprintln!("--- kernel tests, aarch64 (QEMU) ---");
        // The FS server (milestone 32 phase 2), for the aarch64 bare target, before `user()` so
        // mkinitrd packs it; then the RedoxFS test images the runner attaches as extra mmio disks.
        if !fs_server_build(TARGET)
            || !user()
            || !mkredoxfs()
            || !mkredoxfs_crash()
            || !mkgptdisk()
            || !mkblankdisk()
            || !mknvmedisk()
        {
            return false;
        }
        // `cargo()` only exports the env the runner needs; the test itself runs under the scanout
        // check, which drives QEMU's monitor beside the suite and proves the pixels reached the
        // device's scanout rather than only the driver's frames.
        if !cargo(&["build", "-p", "kernel", "--target", TARGET]) {
            return false;
        }
        let leg = if hvf {
            hvf_kernel_leg()
        } else {
            cargo_test_with_scanout_check("aarch64", &["test", "-p", "kernel", "--target", TARGET])
        };
        if !leg {
            return false;
        }
    }

    // The same booted kernel test suite on the second architecture (parity workstream B). The
    // portable tests (scheduler, capabilities, revocation, memory, sync) run on RISC-V's real Sv39
    // kernel; what stays gated to aarch64 is what genuinely needs aarch64 (the userspace-exec suite's
    // hand-written machine code, and SMP). The two interrupt-delivery tests used to be on that list
    // because they trigger with a GIC SGI; milestone 19 made the trigger per-arch instead, so they
    // run here too. RISC-V exits via the sifive_test finisher, same harness. See
    // notes/riscv-parity-scope.md and notes/interrupts.md.
    if legs.riscv64() {
        eprintln!();
        eprintln!("--- kernel tests, riscv64 (QEMU) ---");
        // The riscv userspace tests (parity C) load programs from the initrd and read the disk, so
        // build the riscv archive and point the runner at IT, not at the aarch64 archive `cargo()`
        // exports: the riscv ELF loader must never be handed aarch64 ELFs. The disk is arch-neutral
        // (a nifefs data image) and was built by mkdisk() above.
        // The riscv FS server, before the riscv archive that packs it.
        if !fs_server_build(RISCV_TARGET) || !initrd_riscv() {
            return false;
        }
        // **A fresh RedoxFS image for this leg.** The two ISA legs share one image path, and the
        // aarch64 leg above WRITES it (the std::fs test and the FS client both do). Reusing it here
        // would make the riscv leg's writes land on an image a previous boot mutated, so the legs
        // would be order-coupled and neither would be reproducible on its own. Each leg gets the
        // same known-good fixture instead. This is test determinism, not a workaround: the
        // cross-boot write failure it separates out is real, and notes/fs-server.md carries it as a
        // tracked open item with the exact recipe to reproduce it (run one leg, then the other,
        // without regenerating in between).
        if !mkredoxfs() || !mkredoxfs_crash() || !mkgptdisk() || !mkblankdisk() || !mknvmedisk() {
            return false;
        }
        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
        // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
        unsafe { std::env::set_var("NIFE_INITRD", riscv_initrd_path()) };
        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
        // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
        unsafe { std::env::set_var("NIFE_DISK", disk_path()) };
        // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
        // threads. xtask is single-threaded here: this runs on the main thread before the child
        // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
        // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
        unsafe { std::env::set_var("NIFE_NET", "1") }; // a virtio-net NIC for the net test (m30)
        if !cargo_test_with_scanout_check(
            "riscv64",
            &["test", "-p", "kernel", "--target", RISCV_TARGET],
        ) {
            return false;
        }
    }

    // FS-level consistency after the runs (milestone 32 phase 2): reopen the RedoxFS image with the
    // host tool and confirm the FS server's write persisted and the filesystem still parses. This
    // checks the image of whichever leg ran LAST (riscv64 unless `--arch aarch64` narrowed the run),
    // on its own fresh fixture.
    eprintln!();
    eprintln!("--- redoxfs image consistency after the run (host tool) ---");
    // Both images: the shared fixture (the write persisted, the filesystem still parses) and the
    // crash test's own disk (milestone 37: after a kill mid-transaction, `cut` is one payload whole).
    // Three images: the shared fixture (the write persisted and the filesystem still parses), the
    // crash test's own disk (milestone 37), and milestone 57's blank disk, where the guest wrote
    // both the partition table and the filesystem inside it.
    redoxfs_check_after_run() && redoxfs_crash_check_after_run() && blank_check_after_run()
}

/// **The aarch64 kernel suite on the physical Apple Silicon core** (milestone 81, `--hvf`).
///
/// # Why this is not just `cargo test` with an env var set
///
/// **QEMU does not intercept ARM semihosting under HVF**, and the whole test harness reports its
/// verdict through it: the kernel's `testing::runner` ends in `semihosting::exit`, and so do the panic
/// handler and both watchdogs. Measured against QEMU 11.0.2 with a nine-instruction guest that
/// writes a byte to the PL011 and then executes the semihosting trap: under TCG the process exits
/// at the trap, under HVF the byte appears and `hlt #0xf000` never returns. So under HVF the guest
/// prints its result and then wedges, and cargo (which waits for the child) would wait forever.
///
/// The answer is the one `run_bench` already uses for the same reason: **own the QEMU child and
/// read its transcript.** We ask cargo for the test ELF without running it, hand that ELF to the
/// same runner script everything else boots through, and read stdout until the harness says how it
/// went. Three markers decide the verdict, all of them printed *before* the exit that will not
/// happen:
///
/// - `test result: ok. N passed` from the runner: the suite passed;
/// - `[PANIC] ` from the panic handler: a failing assertion, which is a failing test;
/// - `WATCHDOG:` from either watchdog: a hang or a livelock, also a failure.
///
/// Reaching end of output with none of them means QEMU died on its own, which is a failure too.
/// The guest's own watchdogs are what bound this leg, exactly as they bound the TCG one: they still
/// fire (the virtual timer is passed through and QEMU injects the interrupt), and their message is
/// what we read. So there is no host-side deadline.
///
/// # The referee runs beside it, on a thread, and it has to
///
/// [`ScanoutReferee`] is not optional here even though it is a *display* check: it is also what
/// presses keys, over QEMU's monitor, and the keyboard test asserts that a keystroke arrived
/// ("the keyboard driver came up but never typed anything in ten seconds"). Nothing in the guest
/// can press a key. Reading the transcript blocks, so the referee is driven from a second thread
/// and joined when the verdict is in. It touches a unix socket and two files and never the
/// environment.
fn hvf_kernel_leg() -> bool {
    let Some(elf) = kernel_test_elf() else {
        return false;
    };

    eprintln!();
    eprintln!("--- kernel tests, aarch64, ON THE PHYSICAL CORE (QEMU + Hypervisor.framework) ---");

    // Constructed before the child, because it is what sets `NIFE_GPU_MON`: the runner reads
    // that when it builds the QEMU command line, so a referee born later would find no monitor.
    let referee = ScanoutReferee::new("aarch64");
    // And the inbound prober, for the same reason and on the same terms: it sets
    // `NIFE_HOSTFWD_PORT` before the child exists, and it runs on its own thread throughout. The
    // accept test is not accelerator-sensitive, but it is in the suite, so a leg without a prober
    // would fail it. Its "before the child" placement is load-bearing exactly as the referee's is.
    let prober = InboundProber::new("aarch64");
    // And the multicast prober (milestone 55's stack half), for the same reason: the mDNS test is
    // in the suite, and a leg without the injection hub and its peer would fail it.
    let mcast = MulticastProber::new("aarch64");
    // And the SMB prober (milestone 54), same reasoning and same placement.
    let smb_prober = SmbProber::new("aarch64");

    let mut cmd = Command::new(RUNNER);
    cmd.arg(&elf);
    // The one child that gets the accelerator. `test()` cleared it from our own environment, so
    // nothing else in this process can inherit it.
    cmd.env("NIFE_ACCEL", "hvf");
    // The same devices the TCG leg attaches, set by `test()` and `cargo()` in our environment and
    // inherited from there: the initrd, the disks, the NIC, the GPU, the keyboard, the RNGs.
    cmd.env("NIFE_INITRD", initrd_path());
    cmd.env("NIFE_DISK", disk_path());
    cmd.env("NIFE_NET", "1");
    cmd.stdout(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("test --hvf: failed to start {RUNNER}: {e}");
            return false;
        }
    };

    // The referee, on its own thread, polling on the same 100 ms cadence the TCG leg uses. It stops
    // when `running` clears and hands itself back through the join, so the reporting happens on this
    // thread exactly as it does for TCG.
    let running = std::sync::Arc::new(AtomicBool::new(true));
    let watcher = {
        let running = running.clone();
        let mut referee = referee;
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                referee.poll();
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            referee
        })
    };

    use std::io::BufRead;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::io::BufReader::new(stdout);
    let mut verdict: Option<bool> = None;
    // How much more transcript to relay once something has failed. The watchdogs print a thread
    // dump after the line that names the failure and that dump is the diagnosis, so we cannot stop
    // at the marker; but we cannot read to the end either, because **there is no end**. The
    // semihosting trap the failure path takes is not answered under HVF: it raises a real
    // synchronous exception (EC 0x00, "Unknown reason") into the guest's own vector table, whose
    // handler panics, whose panic handler takes the same trap again. Four cores doing that write
    // interleaved garbage at native speed forever. 200 lines is comfortably more than the longest
    // dump and stops well short of the storm.
    const AFTER_FAILURE: usize = 200;
    let mut budget = AFTER_FAILURE;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // Stream it, because a test suite you cannot watch is a test suite you cannot debug. The
        // TCG leg inherits stdio and prints as it goes; this leg has to relay.
        println!("{line}");
        if line.starts_with("test result: ok.") {
            verdict = Some(true);
            break;
        }
        if line.contains("[PANIC] ") || line.contains("WATCHDOG:") {
            verdict = Some(false);
        }
        if verdict == Some(false) {
            budget -= 1;
            if budget == 0 {
                break;
            }
        }
    }

    // Stop and collect the referee BEFORE killing QEMU: its last look at the scanout has to happen
    // while there is still a device to look at.
    running.store(false, Ordering::Relaxed);
    let scanout_ok = match watcher.join() {
        Ok(referee) => referee.report(),
        Err(_) => {
            eprintln!("test --hvf: the scanout referee panicked");
            false
        }
    };

    // Same ordering argument as the referee's: the probers have to stop while the guest is still
    // there, and their verdicts are collected before QEMU is killed.
    let inbound_ok = prober.report();
    let mcast_ok = mcast.report();
    let smb_ok = smb_prober.report();

    // It is parked at a semihosting trap HVF will not answer, so it will never exit by itself.
    let _ = child.kill();
    let _ = child.wait();

    match verdict {
        Some(true) => scanout_ok && inbound_ok && mcast_ok && smb_ok,
        Some(false) => {
            eprintln!();
            eprintln!(
                "test --hvf: the suite failed on the physical core (see the transcript above)"
            );
            false
        }
        None => {
            eprintln!();
            eprintln!(
                "test --hvf: QEMU's output ended without a verdict. The harness prints one before \
                 every exit, so this is QEMU dying rather than the suite finishing."
            );
            false
        }
    }
}

/// Ask cargo to build the kernel's test binary and say where it put it, without running it.
///
/// `cargo test --no-run` is the build; `--message-format=json` is how we learn the path, which
/// carries a content hash and lives under the build script's `OUT_DIR`, so it cannot be spelled
/// out by hand. The scan is a substring match rather than a parse because xtask has no JSON
/// dependency and taking one for a single field would be the wrong trade (DECISIONS §46): the
/// field is a filesystem path emitted by cargo, so it contains no escapes, and the only artifact
/// line `cargo test --no-run -p kernel` emits with a non-null `executable` is the one we want.
fn kernel_test_elf() -> Option<String> {
    let out = Command::new("cargo")
        .args([
            "test",
            "-p",
            "kernel",
            "--target",
            TARGET,
            "--no-run",
            // Diagnostics still render as text on stderr; only the machine-readable artifact
            // records go to stdout. A compile error is as readable as it always was.
            "--message-format=json-render-diagnostics",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|c| c.wait_with_output());
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            eprintln!("test --hvf: building the kernel test binary failed");
            return None;
        }
        Err(e) => {
            eprintln!("test --hvf: cannot run cargo: {e}");
            return None;
        }
    };

    const KEY: &str = "\"executable\":\"";
    let found = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            l.contains("\"reason\":\"compiler-artifact\"") && l.contains("\"name\":\"kernel\"")
        })
        .filter_map(|l| l.split_once(KEY).map(|(_, rest)| rest.to_string()))
        .filter_map(|rest| rest.split_once('"').map(|(path, _)| path.to_string()))
        .next_back();

    if found.is_none() {
        eprintln!(
            "test --hvf: cargo built the kernel but named no test executable. That is a change in \
             cargo's JSON output, not a test failure."
        );
    }
    found
}

/// **The host tests again, under Miri's interpreter** (milestone 79, notes/undefined-behavior.md).
///
/// Miri checks the rules nothing else in the tree checks: aliasing (tree borrows), pointer
/// provenance, uninitialized reads, leaks. Kani proves the properties it is asked about and the
/// fuzzers see crashes; neither sees a `&mut` that aliases. The crate selection is `test()`'s,
/// verbatim and for the same reason it is `--workspace --exclude` there rather than a list: a
/// hand-maintained list drifted twice, and this way a new crate is covered the moment it joins
/// the workspace. The exclusions are the three bare-metal crates that do not compile for the
/// host, plus one that does:
///
/// **`xtask` is excluded like it is from `script/coverage`, and for cost, not principle.** It is
/// the build tool, not a host-logic crate; its three tests are the scanout referees, safe pixel
/// arithmetic with no `unsafe` on any path, and under the interpreter they cost around seven
/// minutes for nothing the type system has not already said. They were run once under Miri during
/// milestone 79's first full sweep and were clean; the recurring run leaves them out.
///
/// **"Miri-clean" means the sampled paths.** An interpreter runs roughly a thousand times slower
/// than the silicon, so the exhaustive suites gate themselves down under `cfg(miri)`: `ntp_proto`
/// strides its 10^9-value sweep, `gpt` skips its 460k-parse corruption sweeps, `calendar` and
/// `glob` shrink their strides and scales, `cred` derives at Argon2's floor (each site says so,
/// next to the test). What Miri certifies is every path the sampled suite executes, not the
/// exhaustive claims; those remain native-only.
///
/// The two out-of-workspace test surfaces stay out deliberately: `tools/redoxfs_host` and
/// `fs_server` spend their runtime inside the vendored RedoxFS engine, and a finding in vendored
/// code lands in the vendor pin, not in a crate this tree can fix (vendor/README.md). Extra args
/// are forwarded to `cargo miri test`, so `cargo xtask undefined-behavior-check -p gpt` narrows the run.
fn undefined_behavior_check() -> bool {
    eprintln!("--- host tests under Miri (aliasing, provenance, uninitialized reads) ---");
    let mut args = vec![
        "miri",
        "test",
        "--workspace",
        "--exclude",
        "kernel",
        "--exclude",
        "user",
        "--exclude",
        "user_rt",
        "--exclude",
        "xtask",
    ];
    let extra: Vec<String> = std::env::args().skip(2).collect();
    args.extend(extra.iter().map(String::as_str));
    run("cargo", &args)
}

/// **Boot the `--features shell` system and type at it** (milestone 50, notes/pipes.md).
///
/// # Why this exists
///
/// Everything else that exercises the shell wires it from **the kernel**, which serves the spawn
/// protocol in place of `user/src/system_initializer.rs`. The shell cannot tell the difference, and
/// that is the problem: a change to init that broke the spawn path fails nothing. The interactive
/// boot is the only thing that runs the real init, and until this verb existed nothing ran the
/// interactive boot.
///
/// It bit milestone 50 three times in one session and **all three presented as a boot that printed
/// nothing at all**: a virtual-address collision between the shell's terminal page and the page six
/// FS clients map, init's sixteen-slot cspace overflowing when the kernel handed it two more grants,
/// and four stack pages being one deep call short of the redirection path. Each cost a manual bisect
/// against a live prompt. Each is caught here in one boot.
///
/// # What it types, and why those lines
///
/// The lines answer each other rather than a constant, which is the shape the milestone's guest
/// tests already have:
///
/// ```text
/// echo hello world | wc      -> 1 2 12   the bytes went through a real spawned process
/// echo hello world > gate    -> nothing  the same bytes into a file the shell backs
/// wc < gate                  -> 1 2 12   ... and they are the same bytes
/// echo hello world >> gate   -> nothing
/// wc < gate                  -> 2 4 24   ... exactly twice, so `>>` kept the first line
/// wc gate                    -> 2 4 24   milestone 31: the name IS the grant, same bytes
/// wc                         -> refused  ... and with no name there is nothing to read
/// caps wc gate               -> input     ... and the preview says which file, and how
/// date                       -> ...UTC   a real wall clock, wired through the real init
/// caps date                  -> cap 1    ... and `caps` names the capability that made it real
/// ```
///
/// One line would meet the BUGS entry that asked for this. Five is still seconds, and it walks the
/// whole endowment: a spawn through the real init, the FS service the real init narrowed into the
/// shell, and both redirection operators.
fn shell_check() -> bool {
    let legs = match flag_value("--arch").as_deref() {
        None => ArchLegs::Both,
        Some("aarch64") => ArchLegs::Aarch64,
        Some("riscv64") => ArchLegs::Riscv64,
        Some(other) => {
            eprintln!("shell-check: --arch {other} is not an architecture (aarch64 or riscv64)");
            return false;
        }
    };
    // TCG only. This boot never exits (the shell loops on its prompt), so it is killed rather than
    // waited on, and there is nothing HVF would buy a gate that spends its time in QEMU's serial.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::remove_var("NIFE_ACCEL") };
    if legs.aarch64() && !shell_check_leg(false) {
        return false;
    }
    if legs.riscv64() && !shell_check_leg(true) {
        return false;
    }
    true
}

/// The text this gate types and what each line must answer. `None` is a line whose answer is
/// checked by a later one rather than by itself, which is every line that writes a file.
///
/// `hello world` plus the newline `echo` adds is twelve bytes; the append arm is exactly twice
/// that. The numbers are spelled out here rather than derived because this is a **boot** gate: if
/// the arithmetic and the boot were both wrong, deriving one from the other would hide it.
const SHELL_CHECK_SCRIPT: [(&str, &[&str]); 40] = [
    ("echo hello world | wc", &["1 2 12"]),
    ("echo hello world > gate.txt", &[]),
    ("wc < gate.txt", &["1 2 12"]),
    ("echo hello world >> gate.txt", &[]),
    ("wc < gate.txt", &["2 4 24"]),
    // **Milestone 31's headline, at the one interface a human touches**: naming a resource in a
    // command IS granting it. The answer has to be the same as the `<` above it, because it is the
    // same designation with the operator left out, and the pair is what makes that a claim rather
    // than an assertion: one line reaches the file through an operator and one through a name, so
    // if they disagree, one of them opened something else.
    ("wc gate.txt", &["2 4 24"]),
    // **And the same name at the head of a pipeline**, which is the line that answered nothing at
    // all until milestone 50's draining lane. An input operand is resolved by the planner, and the
    // shell used to wire a pipeline's head off the `Line` (which has no `<` on it), so the planned
    // source was dropped and the stage counted an empty stream (a `recv` on an empty slot answers
    // `NoSuchSlot`, which reads as end of document). Two spawned processes, and this shell feeds the
    // first.
    //
    // `2 4 24` plus a newline is seven bytes and three words on one line, so the answer is the
    // answer above it counted. Spelled out rather than derived for this file's reason: it is a boot
    // gate, and deriving one number from another would hide the case where both are wrong.
    ("wc gate.txt | wc", &["1 3 7"]),
    // The negative control the pair would be weaker without. `wc` alone is refused **at the
    // prompt**, before anything is spawned, because its manifest declares that it reads a stream;
    // on Unix the same command is a shell that appears to hang. So the line above granted
    // something, rather than falling back on a default.
    ("wc", &["name a file"]),
    // And `caps` says which file and how, which is the honest half: the shell reads it and streams
    // it in, so what the child holds is an endpoint and not a capability naming the disk.
    ("caps wc gate.txt", &["input    gate.txt"]),
    // **Milestone 40 at the same interface, and only this much of it.** `doc` is in the image, is
    // spawnable, and declares that it reads a stream, so bare `doc` is refused at the prompt before
    // anything is spawned, exactly as `wc` is and for the same reason: a viewer that could open the
    // page it renders could open any page.
    //
    // What is deliberately NOT here is `doc gate.txt`, and the reason is a shell limitation this
    // lane found by typing it rather than by reading. `swish` sends a spawned stage its **whole**
    // input and only then drains that stage's output, and `sink_proto` is a rendezvous `SEND`, so a
    // program that writes while it is still reading blocks against a shell that is still writing.
    // `wc` never meets it because it produces nothing until end of stream. Both workarounds fail
    // for a second reason: neither `doc gate.txt | wc` nor `doc gate.txt > page.txt` delivers the
    // named file to the stage, and each answers `0 0 0`, which is the viewer rendering an empty
    // input. See notes/manual.md; showing a document at this prompt is a lane of its own.
    ("doc", &["name a file"]),
    // **The clock, from the prompt** (milestone 51's wiring). The answer cannot be a constant, so
    // the check is the one word that separates a real time from both ways of not having one:
    // `Format::Human` ends in the offset's name and the two unknown-clock sentences ("the machine
    // has no clock it believes" / "this process holds no clock capability") contain no `UTC` at
    // all. So this fails if the clock service did not run, if the kernel granted init no page, if
    // init did not endow `date`, or if `date` was handed a page nobody published to.
    ("date", &["UTC"]),
    // And the visibility surface agrees with the wiring. `caps` is the only thing in this system
    // that claims to print a process's whole authority, so a clock endowed and not printed would
    // make that claim false. Its wording is host-tested; this proves the wording is about a
    // capability the boot really moves.
    ("caps date", &["cap 1  frame     clock"]),
    // **`2>`, at the one interface a human touches** (DECISIONS §67). The four lines below are the
    // whole of the decision, and only this gate runs them through the real init: the guest tests
    // wire the shell from the kernel, whose `Spawn` fills a cspace from zero and cannot place a
    // capability at the slot a manifest names, so `date` there never receives a second stream.
    //
    // `date` is the declarer, and at *this* prompt it has a clock and nothing to complain about. So
    // the assertion is that its second stream exists, is separate, and is **empty**: `2> err.txt`
    // creates the file, `date` closes the stream with nothing on it, and `wc` counts zero of
    // everything. A shell that had merged the two streams would put a timestamp in there.
    ("date 2> err.txt", &["UTC"]),
    ("wc < err.txt", &["0 0 0"]),
    // And the visibility surface names the second destination, which is what stops `caps date >
    // when.txt` being a half-truth: two destinations on one line, and a reader can see that the
    // complaint is not going into the file.
    ("caps date 2> err.txt", &["diags    err.txt"]),
    // The refusal, which is the other half of "a declaration, not a number". `wc` writes one stream
    // and its diagnostics ride it, so `2>` names nothing and the line does not run.
    ("wc gate.txt 2> err.txt", &["declares no second output"]),
    // **`time`, at the one interface a human touches** (milestone 86). Only this gate runs the real
    // inits, and the clock the shell times with is granted by them: the guest tests wire it from the
    // kernel, so a boot where init never handed the shell a clock would pass every one of those and
    // print "this shell holds no clock capability" here.
    //
    // The answer is the same three numbers `wc gate.txt` gave four lines up, which is the claim the
    // milestone rests on: the tail runs exactly as typed and timing it changes nothing about it. A
    // `time` that re-tokenized its tail, or spawned a differently endowed child, would answer
    // something else here and the duration would still look fine.
    ("time wc gate.txt", &["2 4 24"]),
    // And the duration itself, checked for its shape rather than its value: the number is a real
    // measurement and cannot be a constant, but `real` and a unit are what a stopwatch prints.
    ("time date", &["time: real"]),
    // The visibility surface agrees with the wiring, the same pairing `caps date` makes for the
    // child's clock. This one is about the shell's own: `caps` is the only thing in this system that
    // claims to print a process's whole authority, and a clock the boot really grants would make
    // that claim false if it went unprinted. The rights half is the load-bearing word: READ without
    // GRANT is why nothing typed here can hand a clock to a child.
    //
    // **The second wanted phrase is milestone 31 phase 3's**, and it is the machine-checked form of "flip
    // `holdings()`": the shell's `holdings().dir` is true exactly when init granted it a directory,
    // and this row is the only place a person can read that at the real prompt. Every other test
    // that runs the shell has the kernel play init, so a boot that stopped granting it would fail
    // nothing; `wc gate.txt` above would keep working, because the shell opens that file itself.
    (
        "caps",
        &[
            "frame     clock      READ only, NOT delegable",
            "endpoint  directory",
        ],
    ),
    // **Milestone 31 phase 3's one remaining item, gated as the gap it is.** The line above proves
    // the shell holds a directory; these two prove what it can and cannot do with one, so the day
    // init builds a caretaker per grant, both of these change and the gate says so.
    //
    // A directory grant is delivered by a `fs_subtree_caretaker`, and init is the only process that
    // can build one (the shell's file-service endpoint carries no `GRANT`, so it holds nothing it
    // could hand a caretaker). init deletes that endpoint during the boot, so the answer is a
    // refusal at the prompt with **nothing spawned**, which is the one outcome this model must
    // never trade away: a program told to destroy something, holding nothing, saying nothing.
    // Neither line costs a job, so the pool arithmetic at the end of this script is unchanged.
    ("rm gate.txt", &["needs init to build the caretaker"]),
    // And the preview says exactly what the missing delivery would move, which is why the refusal
    // is a gap rather than a hole: `caps` can name the grant before anything exists to carry it.
    // The root is the interesting half. `caps rm globmany/m-*.txt` below designates a directory one
    // component down, which is the only depth every wiring in this tree can build a caretaker for
    // (`fs_service::narrow_dir` descends one name from the image root); a name typed at the top
    // prompt designates the root itself, which has no name to descend into. See
    // notes/grant-expression.md.
    (
        "caps rm gate.txt",
        &["dir      /  (the directory holding gate.txt)"],
    ),
    // **`xargs`, at the one interface a human touches** (milestone 109). `globmany` holds eleven
    // names one pattern matches, which is more than the eight a single grant can carry.
    //
    // The negative control first, and it is the state of the world this milestone answers: unbatched,
    // a match over the bound is a **refusal at the prompt with nothing spawned**, which is milestone
    // 47's answer at the bound and the reason `xargs` was raised. It still is the answer, because
    // batching is opt-in: a line that silently ran N times would make `caps rm *.txt`'s single
    // printed grant a lie.
    (
        "echo globmany/m-*.txt",
        &["matched more names than one grant can carry"],
    ),
    // And batched, the same pattern is swept. **Asserting the second batch is what pins the resume
    // rule**: `m-08.txt` first means batch one ended at `m-07.txt` and the watermark carried, so
    // this one line rules out an off-by-one at the boundary, a batch that restarted from the top,
    // and a batch that took the first eight the directory happened to yield.
    (
        "xargs echo globmany/m-*.txt",
        &["batch 2: m-08.txt m-09.txt m-10.txt"],
    ),
    // **And the authority per batch is exactly that batch**, which is the claim the milestone rests
    // on and the one only `caps` can make before the delegation chain exists. The preview prints
    // what `rm` would be handed, and what it would be handed in the second invocation is the three
    // remaining names: not the eleven the pattern matched, and not the directory they live in.
    (
        "xargs caps rm globmany/m-*.txt",
        &["the directory holding m-08.txt m-09.txt m-10.txt"],
    ),
    // **Quoting, at the one interface a human touches** (milestone 67). The gap it closes is an
    // authority one: a file called `my notes.txt` could not be *named* before this, so it could not
    // be granted, in a shell whose whole thesis is that naming a resource is granting it.
    //
    // Three lines, and the third is the one that makes the pair a claim. The `>` writes twelve bytes
    // plus a newline into a name only quoting can express; the `<` reads them back; and `wc "my
    // notes.txt"` is the same designation with the operator left out, so if the two disagree, one of
    // them opened something else. That is `gate.txt`'s trio above, asked of a name with a space in
    // it.
    ("echo hello world > \"my notes.txt\"", &[]),
    ("wc < \"my notes.txt\"", &["1 2 12"]),
    ("wc \"my notes.txt\"", &["1 2 12"]),
    // **And the one thing quoting does to authority**: it suppresses expansion, so the same four
    // characters are one name quoted and a set unquoted. `echo` prints what a grant would move, so
    // this line is the narrowing made visible before anything moves. Unquoted, the same pattern is
    // the refusal five lines up.
    ("echo \"*.txt\"", &["*.txt"]),
    // And the preview names it, which is the pairing `caps` exists for: what the line designates is
    // on the screen before anything moves, and a name with a space in it is now something that
    // sentence can be about.
    ("caps wc \"my notes.txt\"", &["input    my notes.txt"]),
    // **Sequencing and the status** (milestone 67). `worker 3` runs and `worker` alone is refused at
    // the prompt for the integer its manifest requires, so these three lines cover both arms of the
    // condition table with real commands rather than with a branch written for a gate.
    ("worker 3 && echo yes", &["yes"]),
    ("worker || echo no", &["no"]),
    // **The decision this milestone settled, read at a prompt.** `worker` alone is refused, and a
    // refusal is not an error: nothing was spawned, nothing was opened, and the status says so with
    // its own number. Unix cannot draw this line, because there `127` and a program's own `exit(1)`
    // are the same kind of integer.
    //
    // The bare `worker` is here because the *first* draft of this gate put `echo $?` straight after
    // `worker || echo no` and got `0`, which was the shell being right: the last thing that ran was
    // the `echo`. `$?` is the previous **command**, not the previous line, and that is bash's rule
    // and this shell's.
    ("worker", &["needs an integer argument"]),
    ("echo $?", &["2"]),
    // **Init's job budget is bounded and comes back** (milestone 22, the interactive increment).
    // Init now holds a pool with room for six live jobs instead of the kernel's whole construction
    // budget, and every job runs in a region of its own that `job_undertaker` returns when the job ends.
    // **Twelve spawns above plus these six are eighteen jobs through a six-job pool**, so a boot
    // where nothing collected would answer "could not spawn (init is out of memory)" somewhere in
    // here rather than the arithmetic. (Eleven when milestone 22 wrote this line, `2>` added two
    // more spawning lines above, milestone 86's `time` added two more, and milestone 67's quoting
    // added three; the count is a fact about the whole script, so it is taken at the merge and not
    // from any one lane.) Six distinct arguments rather than one repeated, because the
    // transcript is walked with a moving cursor and six identical answers would let a missed line
    // pass as its neighbour.
    ("worker 3", &["3*3 = 9"]),
    ("worker 4", &["4*4 = 16"]),
    ("worker 5", &["5*5 = 25"]),
    ("worker 6", &["6*6 = 36"]),
    ("worker 7", &["7*7 = 49"]),
    ("worker 8", &["8*8 = 64"]),
    ("echo shell-boot-gate-done", &["shell-boot-gate-done"]),
];

/// How long to wait for the banner, for one line's echo, and for the whole transcript. Generous:
/// under TCG on a loaded machine a cold boot to the prompt is seconds, and a gate that flakes on a
/// busy laptop is a gate people learn to ignore.
const SHELL_CHECK_BOOT_SECS: u64 = 120;
const SHELL_CHECK_LINE_SECS: u64 = 30;

/// One architecture's leg of [`shell_check`].
fn shell_check_leg(riscv: bool) -> bool {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let arch = if riscv { "riscv64" } else { "aarch64" };
    eprintln!();
    eprintln!("--- shell-check ({arch}): boot `--features shell` and type at the prompt ---");

    // The same build the interactive boot takes, because a gate that builds something else is
    // gating something else. The FS server first (`user()` packs the initrd by reading the ELF off
    // disk), then the RedoxFS image, because the runner attaches the disk only when the file is
    // there and `<` and `>` need one.
    let target = if riscv { RISCV_TARGET } else { TARGET };
    let built = if riscv {
        fs_server_build(RISCV_TARGET) && mkdisk() && mkredoxfs() && initrd_riscv()
    } else {
        fs_server_build(TARGET) && mkredoxfs() && mkdisk() && user()
    } && run(
        "cargo",
        &[
            "build",
            "-p",
            "kernel",
            "--features",
            "shell",
            "--target",
            target,
        ],
    );
    if !built {
        return false;
    }

    // The runner directly rather than through `cargo run`, so the process this owns **is** QEMU
    // (the runner script `exec`s it). A `cargo run` in between would leave the emulator alive when
    // the kill lands on cargo, which is the leak CLAUDE.md's QEMU rule exists about.
    let mut cmd = Command::new(if riscv {
        "scripts/qemu-runner-riscv64.sh"
    } else {
        RUNNER
    });
    cmd.arg(format!("target/{target}/{}/kernel", profile_dir()));
    cmd.env(
        "NIFE_INITRD",
        if riscv {
            riscv_initrd_path()
        } else {
            initrd_path()
        },
    );
    cmd.env("NIFE_DISK", disk_path());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("shell-check: failed to start the runner: {e}");
            return false;
        }
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");

    // A reader thread rather than blocking reads on this one, because every wait below needs a
    // deadline: a boot that hangs is exactly the failure this gate is for, and a gate that hangs
    // with it reports nothing.
    let seen = Arc::new(Mutex::new(String::new()));
    let collector = Arc::clone(&seen);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                return;
            }
            // The terminal's own carriage returns are the line editor's, not content. Dropped so
            // the checks below can be about what a person reads.
            let text = String::from_utf8_lossy(&buf[..n]).replace('\r', "");
            collector.lock().expect("transcript lock").push_str(&text);
        }
    });

    // Poll for a needle **after `from`** with a deadline, `from` being how long the transcript was
    // when the thing we are waiting for was asked for.
    //
    // The position matters because this gate deliberately types `wc < gate.txt` twice. A
    // whole-transcript search finds the first one's echo instantly and the wait returns before the
    // second line has been read at all, which then types the line after it into a prompt that has
    // not appeared. That is exactly what the first version of this did.
    let wait_after = |from: usize, needle: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if seen.lock().expect("transcript lock")[from..].contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    };
    let mark = || seen.lock().expect("transcript lock").len();

    // **Wait for the prompt to come back before typing the next line**, and this is not politeness.
    // The line editor echoes a character the moment it arrives, whether or not the shell has asked
    // for a line yet, so typing ahead produces a transcript in which a command's echo appears
    // *before* the `$ ` that should introduce it. The first version of this gate typed ahead and
    // then failed to find its own echo, which is a bug in the gate rather than in the shell.
    //
    // A transcript ending in the bare prompt is the unambiguous "ready": the prompt is out and
    // nothing has been echoed since.
    let wait_for_prompt = |secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if seen.lock().expect("transcript lock").ends_with("$ ") {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    };

    // Everything below must reach the kill, so failures are recorded rather than returned.
    let mut failed: Vec<String> = Vec::new();
    // The banner is the first claim: init built the console, the line editor, the input driver and
    // the shell, and gave the shell every capability it needs to say hello. A boot that dies in any
    // of that prints nothing, which is the symptom all three of this milestone's bugs shared.
    if !wait_after(0, "nife capability shell", SHELL_CHECK_BOOT_SECS) {
        failed.push(format!(
            "no prompt banner within {SHELL_CHECK_BOOT_SECS}s: the `--features shell` boot never \
             reached a shell"
        ));
    } else {
        // **Init gave the construction budget away, and says so from the inside** (milestone 22,
        // the interactive increment). Init prints this one line after deleting the root untyped and
        // before starting the shell, and it prints it only when `RETYPE` and `RETYPE_OBJ` on that
        // slot both answered `NoSuchSlot`: the capability is gone, not narrowed. The other branch
        // says "NOT dropped", so a boot that kept its budget fails here rather than passing quietly.
        // It is already in the transcript by now, because the banner comes from a shell init starts
        // afterwards; there is nothing to wait for.
        if !seen
            .lock()
            .expect("transcript lock")
            .contains("construction budget dropped")
        {
            failed.push(
                "init never reported dropping its construction budget: either it still holds the \
                 kernel's root untyped, or the delete did not take (the slot answered something \
                 other than NoSuchSlot)"
                    .to_string(),
            );
        }
        // **And init measured every program it loaded** (milestone 104), which is the line that
        // keeps the second link of the chain from evaporating. A kernel built without the
        // measurement step refuses to boot at all, but a *table* that stopped naming things would
        // leave a system that boots, prompts, and vouches for nothing, and it would look exactly
        // like a healthy one. So init says which way it went either way, and the affirmative
        // sentence is what this gate reads. The other branch names the programs it refused, so a
        // boot that quietly stopped spawning half the prompt's commands fails here.
        if !seen
            .lock()
            .expect("transcript lock")
            .contains("every program measured against the archive table")
        {
            failed.push(
                "init never reported measuring the programs it loads: either the archive's \
                 measurement table stopped naming them, or init refused some (it prints the names \
                 in that case)"
                    .to_string(),
            );
        }
        for (line, _) in SHELL_CHECK_SCRIPT {
            if !wait_for_prompt(SHELL_CHECK_LINE_SECS) {
                failed.push(format!(
                    "the prompt never came back to take `{line}`; the line before it did not finish"
                ));
                break;
            }
            let at = mark();
            if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
                failed.push(format!("could not type `{line}` at the prompt"));
                break;
            }
            if !wait_after(at, &format!("{line}\n"), SHELL_CHECK_LINE_SECS) {
                failed.push(format!("the prompt never echoed `{line}`"));
                break;
            }
        }
        // One more, for the last line: every other answer is bounded by the next line's wait, and
        // the last one has no next line. Without this the transcript is read while the final
        // command is still running.
        if failed.is_empty() && !wait_for_prompt(SHELL_CHECK_LINE_SECS) {
            failed.push("the prompt never came back after the last line".to_string());
        }
    }

    let transcript = seen.lock().expect("transcript lock").clone();
    // The transcript is printed on failure below, because that is when somebody needs it. This
    // prints it on success too, and it exists because the notes in this tree quote real prompt
    // sessions: `NIFE_SHOW_TRANSCRIPT=1 script/shell-check --arch aarch64` is where the EXAMPLES
    // in notes/swish-language.md and notes/pipes.md come from, rather than from somebody retyping
    // what they remember the shell saying.
    if std::env::var_os("NIFE_SHOW_TRANSCRIPT").is_some() {
        eprintln!("--- shell-check ({arch}) transcript ---");
        eprintln!("{transcript}");
    }
    if failed.is_empty() {
        // Walked in order with a moving cursor, not searched. The script types `wc < gate.txt`
        // twice on purpose and the two answers are the whole point of the append arm, so a search
        // that found either one would read the same answer for both lines and pass a `>>` that had
        // truncated.
        let mut cursor = 0usize;
        for (line, want) in SHELL_CHECK_SCRIPT {
            match shell_check_answer(&transcript, cursor, line) {
                Some((answer, next)) => {
                    cursor = next;
                    // **Every wanted phrase, not the first**, because one answer can carry several
                    // independent claims and checking one of them makes the rest decoration. `caps`
                    // is the case that forced it: it prints the shell's whole endowment, and a gate
                    // that read only the clock row would pass a boot that had stopped granting the
                    // shell a directory, which is the wiring milestone 31's headline rests on.
                    for want in want {
                        if !answer.contains(want) {
                            failed.push(format!(
                                "`{line}` answered {:?}, wanted {want:?}",
                                answer.trim()
                            ));
                        }
                    }
                }
                None => failed.push(format!("`{line}` produced no answer at all")),
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();

    if failed.is_empty() {
        eprintln!(
            "shell-check ({arch}): the prompt booted, piped, redirected, appended, named a \
             file to a reader, read the clock, timed a command with a clock of its own, kept \
             a declared second stream off the redirection, printed a directory among the \
             capabilities it holds and refused the one grant it cannot yet deliver, swept a \
             match too large to hand over in batches whose authority is exactly what each was \
             designated, named a file whose name has a space in it, ran a && past a command \
             that succeeded and not past one it refused, and ran eighteen jobs through init's \
             six-job pool after init gave its construction budget away"
        );
        return true;
    }
    eprintln!();
    eprintln!("--- shell-check ({arch}) transcript ---");
    eprintln!("{transcript}");
    eprintln!("--- shell-check ({arch}) FAILED ---");
    for f in &failed {
        eprintln!("  {f}");
    }
    false
}

/// **What the prompt printed in response to the first `line` at or after `from`**, plus where to
/// resume looking. `None` when that line is not in the transcript at all.
///
/// `kernel::user::pipeline_service::answer` does this inside the guest and this does it on the
/// host, for the same reason: an assertion should be able to name the command it is about instead
/// of counting lines.
fn shell_check_answer<'a>(
    transcript: &'a str,
    from: usize,
    line: &str,
) -> Option<(&'a str, usize)> {
    let echo = format!("$ {line}\n");
    let at = from + transcript[from..].find(&echo)? + echo.len();
    let rest = &transcript[at..];
    // The answer runs to the next prompt, which is always at the start of a line, and `rest` begins
    // at one because the echo consumed its own newline. So a command that printed nothing has an
    // empty answer rather than swallowing the line after it, which is the case `>` and `>>` are
    // and which the first version of this got wrong.
    let end = if rest.starts_with("$ ") {
        0
    } else {
        rest.find("\n$ ").map(|e| e + 1).unwrap_or(rest.len())
    };
    Some((&rest[..end], at + end))
}

/// The microbenchmarks (milestone 21; design/roadmap/21-benchmarks.md).
///
/// Two instruments:
/// - default: TCG with `-icount`, where virtual time is a deterministic function of instructions
///   executed. Counts are exact and reproducible; `--check` diffs them against
///   `bench/baseline-aarch64.txt` and fails on drift, `--save` rewrites the baseline (a deliberate act,
///   committed alongside whatever changed the numbers).
/// - `--real`: HVF, natively on the host core. Real caches and TLBs, statistical numbers,
///   reported in nanoseconds, never gating.
///
/// The bench kernel never exits on its own (semihosting does not work under HVF; see `test`).
/// We own the QEMU child, watch its output for `bench: done`, and kill it: one exit mechanism
/// for both accelerators.
fn bench() -> bool {
    let check = std::env::args().any(|a| a == "--check");
    let save = std::env::args().any(|a| a == "--save");
    // `--release` builds an optimized kernel and userspace, for a fair cross-OS comparison (the debug
    // default is fine for the icount gate, whose counts are path length, but not for magnitudes next
    // to release Linux). Release changes instruction counts, so it never runs under icount and never
    // gates: it implies `--real` (HVF magnitudes only).
    let release = std::env::args().any(|a| a == "--release");
    RELEASE.store(release, Ordering::Relaxed);
    let real = release || std::env::args().any(|a| a == "--real");
    if real && (check || save) {
        let why = if release { "--release" } else { "--real" };
        eprintln!("bench: {why} numbers are statistical and never gate; no --check/--save");
        return false;
    }

    // The second architecture. RISC-V has its own path (its own kernel target, runner, and initrd,
    // no disk, no HVF); everything else -- the icount instrument, the parsing, the table, the
    // baseline gate -- is shared through run_bench. See bench_riscv.
    if std::env::args().any(|a| a == "--riscv") {
        return bench_riscv(check, save);
    }

    // `--smp`: boot the full 4-hart machine under HVF so the multi-hart throughput bench
    // (`smp_throughput`, DECISIONS §28) and the FS service-path bench (`fs_read`, DECISIONS §32) have
    // cores and, for the FS one, a filesystem to work with. Both self-skip on one hart, so without
    // this flag the `--real` run is single-hart and neither builds the FS image nor prints their
    // lines. Only meaningful with `--real`.
    let smp = std::env::args().any(|a| a == "--smp");

    // For --smp, build the FS server (before user(), so mkinitrd packs the fs_server ELF) and the
    // RedoxFS test image the runner attaches as the second mmio disk. The fs_read bench opens it; on
    // any run without the image the bench finds no second disk and skips, so this stays out of the
    // icount gate's build entirely.
    if (smp && !fs_server_build(TARGET))
        || !mkdisk()
        || !user()
        || (smp && !mkredoxfs())
        || !cargo_profiled(&[
            "build",
            "-p",
            "kernel",
            "--features",
            "bench",
            "--target",
            TARGET,
        ])
    {
        return false;
    }

    // Run the kernel through the same runner script as everything else, with the accelerator
    // chosen by env and, for the deterministic instrument, icount pinning virtual time to the
    // instruction stream (sleep=off: virtual time never waits for the wall clock).
    let mut cmd = Command::new(RUNNER);
    cmd.arg(kernel_elf());
    if real {
        cmd.env("NIFE_ACCEL", "hvf");
        if smp {
            // The full machine, for the aggregate-throughput bench. The per-core primitive magnitudes
            // in this same run are then NOT per-core clean (the reap-heavy ones, spawn_el0 and
            // spawn_reap, inflate and go noisy under cross-core reap lag); read those from the default
            // single-hart run instead. See notes/benchmarks.md, the multi-hart section.
            // "4" matches the runner's default; the throughput bench reads
            // the actual online count at runtime, so this only needs to be more than one.
            cmd.env("NIFE_SMP", "4");
            eprintln!(
                "--- bench: HVF, 4 harts (for smp_throughput; primitives are not per-core here) ---"
            );
        } else {
            // One hart by default, the same choice the icount instrument makes and for a kindred
            // reason: a primitive magnitude is a PER-CORE number, and the cross-OS comparison
            // (notes/benchmarks.md) reads it as one. At `-smp 4` the reap-heavy primitives pick up
            // cross-core reap lag that has nothing to do with per-core cost (spawn_el0 ~4.8 us here
            // goes ~13.6 us and swings wildly there; spawn_reap likewise). So the default `--real`
            // run is single-hart and clean; `--real --smp` boots the whole machine for the throughput
            // bench, which needs more than one core to mean anything.
            cmd.env("NIFE_SMP", "1");
            eprintln!(
                "--- bench: HVF, single hart, per-core magnitudes (statistical; medians matter) ---"
            );
        }
    } else {
        cmd.env_remove("NIFE_ACCEL");
        cmd.args(["-icount", "shift=0,sleep=off"]);
        // One hart, the same reason the riscv path forces it (bench_riscv): a primitive benchmark
        // measures per-core path length, and the counter it reads (CNTVCT) advances with QEMU's
        // GLOBAL virtual time. Under `-icount` all vCPUs share that one clock, and an idle secondary
        // hart sitting in `wfi` jumps virtual time to the next timer tick, so with `-smp 4` the
        // measured window counts three other harts' idle jumps and load-balanced spawns, not the
        // path under test. That contamination (not any code change) is what made the counts swing
        // wildly and non-physically across today's merges: coremark, pure compute, moved 63%. See
        // notes/benchmarks.md, the 2026-07-28 attribution. The aarch64 default is 4 (SMP tests);
        // the icount bench pins 1 to match riscv and measure the primitive, not the machine.
        cmd.env("NIFE_SMP", "1");
        eprintln!(
            "--- bench: aarch64, single hart, TCG + icount (deterministic instruction counts) ---"
        );
    }
    cmd.env("NIFE_INITRD", initrd_path());
    cmd.env("NIFE_DISK", disk_path());

    run_bench(
        cmd,
        real,
        check,
        save,
        workspace_root().join("bench/baseline-aarch64.txt"),
    )
}

/// **The RISC-V benchmark path** (parity E's follow-up). Same primitive suite, same deterministic
/// icount instrument, on the second architecture, so the tick counts are directly comparable to the
/// aarch64 ones: both are the virtual timer advancing under `-icount`, which is instruction-clocked,
/// not wall-clock. No HVF (there is no RISC-V hypervisor on this host) and no disk (the bench boot
/// runs no virtio); it just needs the riscv initrd carrying `os_primitives_benchmarker` + `coremark`. Its baseline is a
/// separate file, since the counts differ by ISA. `cargo xtask bench --riscv [--check|--save]`.
fn bench_riscv(check: bool, save: bool) -> bool {
    if !initrd_riscv()
        || !run(
            "cargo",
            &[
                "build",
                "-p",
                "kernel",
                "--features",
                "bench",
                "--target",
                RISCV_TARGET,
            ],
        )
    {
        return false;
    }

    let mut cmd = Command::new("scripts/qemu-runner-riscv64.sh");
    cmd.arg(format!("target/{RISCV_TARGET}/debug/kernel"));
    // icount pins virtual time (rdtime) to the instruction stream; sleep=off so it never waits on the
    // wall clock. This is what makes the riscv counts deterministic and comparable to aarch64's.
    cmd.args(["-icount", "shift=0,sleep=off"]);
    cmd.env("NIFE_INITRD", riscv_initrd_path());
    // One hart: a primitive benchmark measures per-core cost. With more harts, a thread that waits
    // for a spawned child leaves its hart idling in `wfi`, and under `-icount` a `wfi` jumps virtual
    // time to the next timer tick, inflating the spawn primitives to timer-quantized nonsense. The
    // single-core costs are what compare to aarch64 anyway.
    cmd.env("NIFE_SMP", "1");
    eprintln!(
        "--- bench: riscv64, single hart, TCG + icount (deterministic instruction counts) ---"
    );

    run_bench(
        cmd,
        false,
        check,
        save,
        workspace_root().join("bench/baseline-riscv64.txt"),
    )
}

/// Run a bench kernel through `cmd`, read its `bench:` lines until `bench: done`, and report the
/// table (and, off the deterministic icount instrument, save or check against `baseline`). Shared by
/// the aarch64 and RISC-V bench paths so the parsing, the table, and the regression gate are one
/// implementation. `real` only chooses the "ns are fiction" footer.
fn run_bench(
    mut cmd: Command,
    real: bool,
    check: bool,
    save: bool,
    baseline_path: std::path::PathBuf,
) -> bool {
    cmd.stdout(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bench: failed to start the runner: {e}");
            return false;
        }
    };

    // Read lines until the guest says it is done, then kill it: it is parked in wfi and will
    // never exit by itself (deliberately; see kernel/src/bench.rs).
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::io::BufReader::new(stdout);
    let mut results: Vec<(String, u64, u64)> = Vec::new();
    let mut cntfrq: u64 = 0;
    let mut done = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Some(rest) = line.strip_prefix("bench: ") else {
            continue;
        };
        if rest == "done" {
            done = true;
            break;
        }
        let parts: Vec<&str> = rest.split_whitespace().collect();
        match parts.as_slice() {
            ["cntfrq", hz] => cntfrq = hz.parse().unwrap_or(0),
            [name, ticks, iters] => {
                if let (Ok(t), Ok(i)) = (ticks.parse(), iters.parse()) {
                    results.push((name.to_string(), t, i));
                }
            }
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    if !done {
        eprintln!("bench: QEMU ended before printing `bench: done`; no results");
        return false;
    }

    // Report. icount counts are the regression currency; ns is computed for both instruments
    // (fictional under icount, real under HVF) because a human wants a magnitude to look at.
    eprintln!();
    eprintln!(
        "{:<14} {:>12} {:>8} {:>12} {:>10}",
        "benchmark", "ticks", "iters", "ticks/iter", "ns/iter"
    );
    for (name, ticks, iters) in &results {
        // `checked_div`, not `/`: a benchmark that reports zero iterations (a skip, or a future
        // diagnostic line) must not panic the whole harness after the run already happened.
        let per = ticks.checked_div(*iters).unwrap_or(0);
        let ns = (ticks * 1_000_000_000)
            .checked_div(cntfrq)
            .and_then(|v| v.checked_div(*iters))
            .unwrap_or(0);
        eprintln!("{name:<14} {ticks:>12} {iters:>8} {per:>12} {ns:>10}");
    }
    if !real {
        eprintln!("(TCG+icount: ticks are deterministic; ns are fiction. --real for magnitudes.)");
    }

    if save {
        // The header names the file it is in. It used to be the literal `bench/baseline.txt` for
        // both baselines, so the riscv one claimed to be the aarch64 one; deriving it from the path
        // makes the two agree with themselves (milestone 73).
        let stem = baseline_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("baseline.txt");
        let mut out = format!(
            "# bench/{stem}: deterministic icount tick counts (cargo xtask bench --save).
# Recorded against the QEMU pinned in .qemu-version. icount counts guest instructions, so the
# emulator version is part of what these numbers mean: script/qemu-check warns when the QEMU on
# PATH is not the pinned one, precisely because that is when a baseline comparison stops being
# apples to apples.
             # Updating this file is a statement that a performance change is intended and
             # understood; do it in the commit that causes the change. Checked by --check, a coarse
             # 10% tripwire (icount counts drift across builds; see notes/benchmarks.md).
",
        );
        for (name, ticks, iters) in &results {
            out.push_str(&format!(
                "{name} {ticks} {iters}
"
            ));
        }
        if let Err(e) = std::fs::write(&baseline_path, out) {
            eprintln!("bench: cannot write {}: {e}", baseline_path.display());
            return false;
        }
        eprintln!("bench: baseline saved to {}", baseline_path.display());
        return true;
    }

    if check {
        let Ok(text) = std::fs::read_to_string(&baseline_path) else {
            eprintln!(
                "bench: no baseline at {} (run `cargo xtask bench --save` first)",
                baseline_path.display()
            );
            return false;
        };
        let mut ok = true;
        // `trim_start` matters: this file's own header has INDENTED comment lines, which a
        // column-0-only check treats as data. They survive today only because they happen to split
        // into more than three tokens and fall through the destructure below. A three-word indented
        // comment would be silently parsed as a benchmark named after its first word.
        for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [name, base, _iters] = parts.as_slice() else {
                continue;
            };
            let base: u64 = base.parse().unwrap_or(0);
            let Some((_, cur, _)) = results.iter().find(|(n, _, _)| n == name) else {
                eprintln!("bench: CHECK FAIL {name}: in the baseline but not in this run");
                ok = false;
                continue;
            };
            // A COARSE tripwire: 10% either way, with a small absolute floor so tiny counts do not
            // false-alarm. Not 2%: adding unrelated *live* code shifts even untouched benchmarks by
            // several percent, non-uniformly, because the compiler remakes whole-crate inlining and
            // monomorphization decisions (measured: a new bench function moved yield_switch -7% while
            // ipc_rtt went +1.8%). So icount --check catches a gross regression, "you 3x'd IPC," not
            // a 3% one; --real medians, read by a human, are the fine signal. See notes/benchmarks.md.
            let slack = (base / 10).max(64);
            let (lo, hi) = (base.saturating_sub(slack), base + slack);
            if *cur < lo || *cur > hi {
                let delta = *cur as i64 - base as i64;
                eprintln!(
                    "bench: CHECK FAIL {name}: {cur} vs baseline {base} ({delta:+} ticks,                      allowed +-{slack})"
                );
                ok = false;
            }
        }
        if ok {
            eprintln!("bench: check passed (all within 10% of baseline; coarse tripwire)");
        } else {
            eprintln!();
            eprintln!(
                "bench: a benchmark moved. If intended, rerun with --save and commit the new                  baseline WITH the change that moved it."
            );
        }
        return ok;
    }

    true
}

/// Boot the kernel with QEMU frozen and a GDB stub listening.
///
/// `-s` opens the stub on :1234, `-S` holds the CPU before the first instruction.
/// The kernel ELF carries symbols and DWARF, so GDB shows Rust source lines rather
/// than raw addresses (notes/elf.md). Point GDB at the **ELF**, even though QEMU is
/// running the flat image: the image has no symbols, and the addresses match.
///
/// This is the tool that will save you at milestone 4, when the MMU comes on and
/// `println!` stops being an option.
fn gdb() -> bool {
    if !build() {
        return false;
    }

    let elf = kernel_elf();
    eprintln!("QEMU is paused, waiting for a debugger on localhost:1234.");
    eprintln!("In another terminal:");
    eprintln!();
    eprintln!("    gdb {elf}");
    eprintln!("    (gdb) target remote :1234");
    eprintln!("    (gdb) break kernel_main");
    eprintln!("    (gdb) continue");
    eprintln!();
    eprintln!("To watch boot.s set up the stack and zero .bss:");
    eprintln!();
    eprintln!("    (gdb) break _boot");
    eprintln!("    (gdb) layout asm");
    eprintln!("    (gdb) si          # step one instruction");
    eprintln!();

    run(RUNNER, &[&elf, "-s", "-S"])
}

fn objdump() -> bool {
    if !build() {
        return false;
    }
    match llvm_tool("llvm-objdump") {
        Some(tool) => run(
            &tool,
            &[
                "-d",
                "--no-show-raw-insn",
                "-M",
                "no-aliases",
                &kernel_elf(),
            ],
        ),
        None => false,
    }
}

/// Build the flat arm64 Image and show its 64-byte header.
///
/// Useful when the header is wrong, which is a failure mode with no diagnostics at
/// all: QEMU simply falls back to treating the file as an anonymous blob, boots it,
/// and hands you a zero in x0. See notes/boot-protocol.md.
fn image() -> bool {
    if !build() {
        return false;
    }
    let Some(objcopy) = llvm_tool("llvm-objcopy") else {
        return false;
    };

    let elf = kernel_elf();
    let img = format!("{elf}.img");
    if !run(&objcopy, &["-O", "binary", &elf, &img]) {
        return false;
    }

    match std::fs::read(&img) {
        Ok(bytes) if bytes.len() >= 64 => {
            let magic = u32::from_le_bytes(bytes[56..60].try_into().unwrap());
            let text_offset = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            let image_size = u64::from_le_bytes(bytes[16..24].try_into().unwrap());

            eprintln!("{img}  ({} bytes)", bytes.len());
            eprintln!();
            eprintln!("  text_offset  {text_offset:#x}");
            eprintln!("  image_size   {image_size:#x}");
            eprintln!(
                "  magic        {magic:#010x}  {}",
                if magic == 0x644d5241 {
                    "ok (\"ARM\\x64\")"
                } else {
                    "WRONG - QEMU will not treat this as a kernel"
                }
            );
            magic == 0x644d5241
        }
        Ok(_) => {
            eprintln!("image is shorter than its own 64-byte header");
            false
        }
        Err(e) => {
            eprintln!("cannot read {img}: {e}");
            false
        }
    }
}

/// Locate an LLVM tool inside the rustup sysroot.
///
/// These ship with the `llvm-tools` component, which `rust-toolchain.toml` pins. We
/// do NOT use the `rust-objdump` / `rust-objcopy` wrappers, because those require a
/// separate `cargo install cargo-binutils` that nothing else in the project needs,
/// and its absence produces a confusing "command not found" rather than a real error.
/// **Read a program ELF for packing, with its debug information removed.**
///
/// The initrd is *reserved RAM*: the frame allocator never owns those pages, so every byte in the
/// archive is a byte the running system does not have. And a debug build is almost entirely debug
/// information: `rust_swappable` is 720 KB, of which **3 KB** is `.text` plus `.rodata` and the
/// other 717 KB is `.debug_*`. Twenty-odd programs like that made a 26 MB archive out of well under
/// a megabyte of code, on a machine with 128 MB.
///
/// Nothing ever read those bytes. `crates/elf` parses **program headers only** (it has no
/// section-header code at all), so the loader cannot see a debug section on either side of the
/// boundary; the kernel prints a raw `pc` on a fault and symbolisation is done offline, against the
/// unstripped binary that is still sitting in `target/`. So this is pure waste, and milestone 23 is
/// where it stopped being free: five more programs pushed the archive 4 MB up and a *later,
/// unrelated* test could no longer find a contiguous eight-megabyte region for init.
///
/// `--strip-debug` rather than `--strip-all`, deliberately: it takes the `.debug_*` sections, which
/// is all of the bulk, and leaves the symbol table for anything that later wants to read it out of
/// the archive rather than out of `target/`.
///
/// A missing `llvm-objcopy` is a hard failure rather than a silent fallback to unstripped bytes,
/// because the measured-boot digest (§26's phase B.1) is taken over what this returns: a build that
/// quietly packed different bytes depending on which tools were installed would be a build whose
/// trust root means something different on each machine.
fn read_stripped(path: &str) -> std::io::Result<Vec<u8>> {
    let objcopy = llvm_tool("llvm-objcopy").ok_or_else(|| {
        std::io::Error::other("llvm-objcopy not found; the llvm-tools rustup component provides it")
    })?;
    let out = workspace_root().join("target/stripped");
    std::fs::create_dir_all(&out)?;
    let stem = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("program");
    // Namespaced by the target directory the binary came from, so the aarch64 and riscv builds of a
    // program cannot overwrite each other's stripped copy.
    let tag = if path.contains(RISCV_TARGET) {
        "riscv"
    } else if path.contains("nife") {
        "std"
    } else {
        "host"
    };
    let dst = out.join(format!("{tag}-{stem}"));
    // Fail before running the tool if the input is missing, so the caller's error message names the
    // binary it wanted rather than objcopy's exit status.
    std::fs::metadata(path)?;
    let status = Command::new(&objcopy)
        .arg("--strip-debug")
        .arg(path)
        .arg(&dst)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "{objcopy} --strip-debug {path} failed ({status})"
        )));
    }
    std::fs::read(&dst)
}

fn llvm_tool(name: &str) -> Option<String> {
    let sysroot = capture("rustc", &["--print", "sysroot"])?;
    let verbose = capture("rustc", &["-vV"])?;
    let host = verbose
        .lines()
        .find_map(|l| l.strip_prefix("host: "))?
        .trim();

    let path = format!("{}/lib/rustlib/{host}/bin/{name}", sysroot.trim());
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        eprintln!("cannot find {name} at {path}");
        eprintln!("the llvm-tools rustup component should provide it (see rust-toolchain.toml)");
        None
    }
}

fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    String::from_utf8(out.stdout).ok()
}

fn kernel_elf() -> String {
    format!("target/{TARGET}/{}/kernel", profile_dir())
}

fn cargo(args: &[&str]) -> bool {
    // The runner needs to know where the initrd is. Set it for every cargo invocation; the
    // script ignores it when the file is not there (which is any build before `user` exists).
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_INITRD", initrd_path()) };
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_DISK", disk_path()) };
    // Attach a virtio-net NIC too (milestone 30): slirp needs no host file, so it is always on for
    // tests, and the net driver's DHCP round-trip test exercises it.
    // SAFETY: `set_var`/`remove_var` became unsafe in edition 2024 because they race other
    // threads. xtask is single-threaded here: this runs on the main thread before the child
    // that reads it is spawned, and the only thread xtask ever starts (the transcript reader
    // in shell_check_leg) copies pipe bytes into a String and never touches the environment.
    unsafe { std::env::set_var("NIFE_NET", "1") };

    run("cargo", args)
}

fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or_else(|e| {
            eprintln!("failed to run {program}: {e}");
            false
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a P6 PPM of the surface's geometry from a per-pixel function, the way QEMU's
    /// `screendump` writes one.
    fn ppm(pixel: impl Fn(u32, u32) -> (u8, u8, u8)) -> Vec<u8> {
        let (w, h) = (gfx_proto::WIDTH, gfx_proto::HEIGHT);
        let mut v = format!("P6\n{w} {h}\n255\n").into_bytes();
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = pixel(x, y);
                v.extend_from_slice(&[r, g, b]);
            }
        }
        v
    }

    fn pattern_rgb(x: u32, y: u32) -> (u8, u8, u8) {
        let w = gfx_proto::pixel(x, y);
        (
            ((w >> 16) & 0xff) as u8,
            ((w >> 8) & 0xff) as u8,
            (w & 0xff) as u8,
        )
    }

    /// **The scanout check accepts the pattern and rejects everything else.**
    ///
    /// This is the negative control for the milestone-29 scanout proof, and it matters: a checker that
    /// accepted anything would report "the pixels reached the device" on every run, which is exactly
    /// the kind of test that is worse than none. Each rejection below is a real failure mode of a
    /// framebuffer driver: a scanout never set (the default console size), a resource that was never
    /// transferred into (black), a channel order mixed up (the single most common framebuffer bug),
    /// and one wrong pixel.
    #[test]
    fn the_scanout_check_accepts_the_pattern_and_rejects_near_misses() {
        assert!(scanout_holds_the_pattern(&ppm(pattern_rgb)).is_ok());

        assert!(
            scanout_holds_the_pattern(&ppm(|_, _| (0, 0, 0))).is_err(),
            "a black scanout was accepted",
        );

        // Red and blue swapped: what a wrong virtio-gpu format code produces, and precisely what the
        // in-guest test cannot see (the guest's own bytes are unchanged).
        assert!(
            scanout_holds_the_pattern(&ppm(|x, y| {
                let (r, g, b) = pattern_rgb(x, y);
                (b, g, r)
            }))
            .is_err(),
            "a red/blue-swapped scanout was accepted: the format check is not doing anything",
        );

        // Shifted one row: a stride bug.
        assert!(
            scanout_holds_the_pattern(&ppm(|x, y| pattern_rgb(x, (y + 1) % gfx_proto::HEIGHT)))
                .is_err(),
            "a scanout shifted by one row was accepted",
        );

        // Exactly one wrong pixel, in the middle.
        assert!(
            scanout_holds_the_pattern(&ppm(|x, y| {
                if (x, y) == (64, 32) {
                    (1, 2, 3)
                } else {
                    pattern_rgb(x, y)
                }
            }))
            .is_err(),
            "a scanout with one wrong pixel was accepted",
        );

        // QEMU's default console size, i.e. a scanout that was never set.
        let mut wrong_geometry = b"P6\n640 480\n255\n".to_vec();
        wrong_geometry.extend(std::iter::repeat_n(0u8, 640 * 480 * 3));
        assert!(
            scanout_holds_the_pattern(&wrong_geometry).is_err(),
            "the default 640x480 console was accepted as our 128x64 surface",
        );

        // A dump caught mid-write is not a failure, but it must not be a pass either.
        let short = &ppm(pattern_rgb)[..1000];
        assert!(scanout_holds_the_pattern(short).is_err());
    }

    fn composed_rgb(x: u32, y: u32) -> (u8, u8, u8) {
        let w = compositor::expected_screen_pixel(compositor::SCENE.len(), x, y);
        (
            ((w >> 16) & 0xff) as u8,
            ((w >> 8) & 0xff) as u8,
            (w & 0xff) as u8,
        )
    }

    /// **The composed-screen check accepts the compositor's screen and rejects the ways a compositor
    /// goes wrong** (milestone 33).
    ///
    /// The negative control for the rung-two half of the scanout proof, and the failure modes are
    /// different from rung one's, which is why it needs its own. In particular a **z-order inversion**
    /// and a **missing window** are both pictures made entirely of correct pixels in almost the right
    /// places: exactly the sort of thing a checker written as "is it not black?" would wave through,
    /// and exactly what a compositor gets wrong.
    #[test]
    fn the_composed_check_accepts_the_screen_and_rejects_the_compositors_own_bugs() {
        assert!(scanout_holds_the_composed_screen(&ppm(composed_rgb)).is_ok());

        assert!(
            scanout_holds_the_composed_screen(&ppm(|_, _| (0, 0, 0))).is_err(),
            "a black screen was accepted",
        );

        // Rung one's pattern is not rung two's screen. Both are 128x64 and both are legitimate
        // pictures, so this pins that the two checks cannot be satisfied by the same dump: if they
        // could, the ordering the poll loop relies on would be meaningless.
        assert!(
            scanout_holds_the_composed_screen(&ppm(pattern_rgb)).is_err(),
            "rung one's test pattern was accepted as the composed screen",
        );
        assert!(
            scanout_holds_the_pattern(&ppm(composed_rgb)).is_err(),
            "the composed screen was accepted as rung one's test pattern",
        );

        // **Stacking order inverted**: the bottom-most window covering a pixel wins instead of the top.
        // Every pixel is a real window pixel; only the order is wrong.
        assert!(
            scanout_holds_the_composed_screen(&ppm(|x, y| {
                for (i, win) in compositor::SCENE.iter().enumerate() {
                    if win.rect().contains(x as i32, y as i32) {
                        let w = compositor::window_pixel(
                            i as u32,
                            (x as i32 - win.origin_x) as u32,
                            (y as i32 - win.origin_y) as u32,
                        );
                        return (
                            ((w >> 16) & 0xff) as u8,
                            ((w >> 8) & 0xff) as u8,
                            (w & 0xff) as u8,
                        );
                    }
                }
                composed_rgb(x, y)
            }))
            .is_err(),
            "a screen with the windows stacked in the wrong order was accepted",
        );

        // **One window missing**: the picture as if the last client never committed. This is what a
        // compositor that dropped a commit, or never mapped a surface, produces.
        assert!(
            scanout_holds_the_composed_screen(&ppm(|x, y| {
                let w = compositor::expected_screen_pixel(compositor::SCENE.len() - 1, x, y);
                (
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    (w & 0xff) as u8,
                )
            }))
            .is_err(),
            "a screen missing its top window was accepted",
        );

        // **Windows placed one pixel off**: the classic clipping error, and the reason the crate's
        // rectangle math is host-tested.
        assert!(
            scanout_holds_the_composed_screen(&ppm(|x, y| composed_rgb(
                (x + 1) % compositor::SCREEN_W,
                y
            )))
            .is_err(),
            "a screen shifted one pixel left was accepted",
        );

        // Red and blue swapped, the format bug the guest cannot see.
        assert!(
            scanout_holds_the_composed_screen(&ppm(|x, y| {
                let (r, g, b) = composed_rgb(x, y);
                (b, g, r)
            }))
            .is_err(),
            "a red/blue-swapped composed screen was accepted",
        );
    }

    fn text_rgb(x: u32, y: u32) -> (u8, u8, u8) {
        let w = video_terminal::script::full_screen().pixel(x, y);
        (
            ((w >> 16) & 0xff) as u8,
            ((w >> 8) & 0xff) as u8,
            (w & 0xff) as u8,
        )
    }

    /// **The text check accepts the terminal's screen and rejects text that is wrong** (milestone
    /// 29's remaining increment).
    ///
    /// The negative control that makes the glyph proof mean anything, and its failure modes are the
    /// *terminal's* rather than the compositor's or the driver's. The one that matters most is the
    /// first: **one letter changed**. Everything else on that screen is identical, every glyph is a
    /// real glyph, the layout is right, and the picture is wrong. A checker that could not tell the
    /// difference would report "readable text reached the scanout" for a terminal that drew the wrong
    /// text, which is the failure this whole increment is about not having.
    #[test]
    fn the_scanout_check_rejects_text_that_is_one_letter_wrong() {
        assert!(scanout_holds_the_terminals_text(&ppm(text_rgb)).is_ok());

        // One letter. `glyphs_ok` against `glyphs_0k`: an `o` for a zero, which is the closest pair
        // of glyphs in the font and therefore the hardest case, deliberately.
        let mut typo =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        typo.feed(video_terminal::script::GREETING_TYPO);
        typo.feed(video_terminal::script::TYPED);
        assert!(
            scanout_holds_the_terminals_text(&ppm(|x, y| {
                let w = typo.pixel(x, y);
                (
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    (w & 0xff) as u8,
                )
            }))
            .is_err(),
            "a screen with one letter wrong was accepted as the terminal's text",
        );

        // **The typing never arrived.** A terminal that rendered an application's output but dropped
        // the keystrokes routed to it draws a picture that is correct as far as it goes.
        let mut no_input =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        no_input.feed(video_terminal::script::GREETING);
        assert!(
            scanout_holds_the_terminals_text(&ppm(|x, y| {
                let w = no_input.pixel(x, y);
                (
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    (w & 0xff) as u8,
                )
            }))
            .is_err(),
            "a screen missing the typed input was accepted",
        );

        // **The rendition ignored.** Every glyph in the right cell, drawn in the default colours: a
        // terminal that parsed SGR as an unknown sequence and swallowed it. The picture is *nearly*
        // right, which is the point.
        let mut plain =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        for &b in video_terminal::script::GREETING
            .iter()
            .chain(video_terminal::script::TYPED)
        {
            // Strip the escape sequences by feeding only what a colour-blind terminal would keep.
            if b != 0x1b {
                plain.feed(&[b]);
            }
        }
        assert!(
            scanout_holds_the_terminals_text(&ppm(|x, y| {
                let w = plain.pixel(x, y);
                (
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    (w & 0xff) as u8,
                )
            }))
            .is_err(),
            "a screen that ignored every rendition was accepted",
        );

        // A blank terminal, which is what a component that came up and drew nothing leaves.
        let blank =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        assert!(
            scanout_holds_the_terminals_text(&ppm(|x, y| {
                let w = blank.pixel(x, y);
                (
                    ((w >> 16) & 0xff) as u8,
                    ((w >> 8) & 0xff) as u8,
                    (w & 0xff) as u8,
                )
            }))
            .is_err(),
            "a blank terminal was accepted as text",
        );

        // And the three pictures on this one scanout are mutually exclusive, which is what makes the
        // poll loop's ordering a real assertion rather than three chances to match once.
        assert!(scanout_holds_the_terminals_text(&ppm(pattern_rgb)).is_err());
        assert!(scanout_holds_the_terminals_text(&ppm(composed_rgb)).is_err());
        assert!(scanout_holds_the_pattern(&ppm(text_rgb)).is_err());
        assert!(scanout_holds_the_composed_screen(&ppm(text_rgb)).is_err());
    }
}
