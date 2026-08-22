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
        // The tree-wide search (milestone 40, script/apropos): the same index and the same reader,
        // pointed at this repository instead of at what the image installs. See `tree_apropos`.
        "apropos" => tree_apropos(std::env::args().nth(2)),
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
        // The abort sweep (milestone 64): which std calls kill a nife process instead of refusing
        // it. Runs at the end of `std-exerciser` (and so inside `script/test`); exposed on its own
        // because re-reading the list after a nightly bump should not need a rebuild.
        "std-aborts" => std_aborts(),
        "shell-check" => shell_check(),
        "test" => test(),
        "undefined-behavior-check" => undefined_behavior_check(),
        "bench" => bench(),
        // The instruction-count instrument (milestone 78): the two timing claims a wall clock
        // cannot make, on both ISAs. See script/icount.
        "icount" => icount(),
        "gdb" => gdb(),
        "objdump" => objdump(),
        "image" => image(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!(
                "usage: cargo xtask <build|run|shell|shell-check|initboot|smb-serve|initrd-riscv|manual|apropos|std-src|std-stamp|std-exerciser|std-aborts|test|undefined-behavior-check|bench|icount|gdb|objdump|image> [--hvf]"
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
            eprintln!("       cargo xtask icount [--arch aarch64|riscv64]");
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
const STD_SRC_PATCH_VERSION: u32 = 8;

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

/// Where the machine-global `nife-dev` name currently resolves, if it is a link we can read.
///
/// `rustup toolchain link` writes a symlink under `$RUSTUP_HOME/toolchains`, so the target is
/// readable without shelling out. `None` covers every shape we cannot interpret (no such link, a
/// real directory rather than a symlink, an unreadable home), and the caller treats `None` as
/// "cannot prove it is ours", which relinks. Relinking when it was already correct costs one
/// idempotent `rustup` call; assuming it was correct costs a silently wrong build.
fn linked_farm() -> Option<PathBuf> {
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".rustup")))?;
    std::fs::read_link(home.join("toolchains").join(NIFE_TOOLCHAIN)).ok()
}

/// Point `nife-dev` at *this* worktree's farm, loudly, if it currently points anywhere else.
///
/// Called on the warm-farm path, which is the one that used to trust the name without checking it.
/// See the comment at that call site for the failure this closes.
fn relink_farm_if_stolen() -> bool {
    let farm = farm_dir();
    // Canonicalize both sides: a worktree reached through a symlinked path (/tmp on macOS is one)
    // would otherwise compare unequal to the same directory recorded literally, and relink on every
    // single call. Falling back to the uncanonicalized path keeps a missing directory readable in
    // the message rather than swallowing it.
    let canon = |p: &PathBuf| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
    if linked_farm().map(|l| canon(&l)) == Some(canon(&farm)) {
        return true;
    }
    eprintln!(
        "--- std-src: `{NIFE_TOOLCHAIN}` did not point at this worktree's farm; relinking ---"
    );
    match linked_farm() {
        Some(other) => eprintln!("std-src:   it pointed at {}", other.display()),
        None => eprintln!("std-src:   it pointed at nothing this tool could read"),
    }
    eprintln!("std-src:   now {}", farm.display());
    eprintln!(
        "std-src: if another lane is mid-gate it has just lost the link, which is how this shared \
         name has always worked (AGENTS.md). The integrator relinks from the main checkout at merge."
    );
    if !run("rustup", &["toolchain", "link", NIFE_TOOLCHAIN, &s(farm)]) {
        eprintln!("std-src: `rustup toolchain link {NIFE_TOOLCHAIN}` failed");
        return false;
    }
    true
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
        // A warm, correctly-stamped farm is not enough, and this early return used to be the whole
        // check. The stamp says *this worktree's farm is built*; it says nothing about where the
        // machine-global `nife-dev` name currently points, and every build downstream of here
        // resolves std through that name rather than through `farm_dir()`.
        //
        // So two lanes gating at once silently built each other's std. That is not hypothetical:
        // on 2026-08-18 lane `55-durability` relinked mid-run and lane `64-more`'s `std_exerciser`
        // compiled against 55's farm, caught only by a person reading the `Compiling std` path out
        // of the build output. AGENTS.md predicted this failure in prose and nothing looked for it.
        //
        // Relink rather than refuse. The lane calling this is about to build and needs the name to
        // mean its own farm, taking the link is what every lane already does by design, and failing
        // here would only convert a silent wrong build into a stopped gate. What changes is that
        // the theft is now deliberate and printed, so `Compiling std` from a foreign path cannot
        // happen without a line above it saying who took what.
        if !relink_farm_if_stolen() {
            return false;
        }
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

/// Remove `# Examples` sections from the doc comments of a file about to be copied into the patched
/// std sysroot.
///
/// A doctest in one of these crates says `use entropy_proto::...`, and in the copy there is no such
/// crate: the file arrives as `sys/pal/nife/entropyproto.rs`, an inner module of `std`. So the
/// example is *false* in its destination, in the specific way milestone 68 cares about, which is
/// that it teaches a reader of the PAL something that is not true of the code they are reading.
/// Nothing runs std's doctests here, so this is a documentation fix rather than a build fix; it is
/// done at the copy because the alternative is refusing the workspace crates real examples, and the
/// workspace is where the example is checked.
///
/// Prose and `text` blocks survive: this drops a `# Examples` heading and everything under it, up to
/// the next heading at the same level or the end of the doc block. Fence state is tracked, so a
/// hidden doctest line (`# use ...`) inside a code block is not mistaken for that next heading.
fn strip_doc_examples(body: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        let Some(content) = trimmed
            .strip_prefix("//!")
            .or_else(|| trimmed.strip_prefix("///"))
        else {
            // Any non-doc line ends the doc block, and with it the section being skipped.
            skipping = false;
            in_fence = false;
            out.push(line);
            continue;
        };
        let content = content.trim();
        if skipping {
            if content.starts_with("```") {
                in_fence = !in_fence;
            } else if !in_fence && content.starts_with("# ") {
                skipping = false;
                out.push(line);
            }
            continue;
        }
        if content == "# Examples" || content == "# Example" {
            skipping = true;
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

/// Generate `abi.rs` and `user_heap.rs` verbatim from the host-tested crates, so the ABI numbers and
/// the heap algorithm have exactly one definition. The transform strips crate-level inner
/// attributes (`#![no_std]`, illegal in a non-root module), any trailing `#[cfg(test)]` module, and
/// any `# Examples` section (see [`strip_doc_examples`] for why that last one).
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
        let body = format!("{}\n", strip_doc_examples(&body).trim_end());
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
        // `pub(crate) mod` rather than `mod`: `sys/paths/nife.rs` asks `fs::nife::reachable()`
        // whether this process holds a directory capability, because `current_dir` must refuse for
        // a process that holds none rather than name a place it cannot reach (milestone 47).
        &sys.join("fs/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        pub(crate) mod nife;\n        use nife as imp;\n    }",
    ) && patch_after(
        // env: a process-local variable table (milestone 64, rank 4). The arm precedes the `_ =>`
        // unsupported fallback, whose `env()` is `panic!("not supported on this platform")`: without
        // this, `std::env::vars()` aborted the process rather than yielding nothing. `sys/env/nife.rs`
        // defines its own `Env` instead of reusing `sys/env/common.rs`, so this is the only anchor
        // env costs us; `common` is gated on a `#[cfg(any(...))]` platform list that would have been
        // a second one to keep in step across nightlies.
        &sys.join("env/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        pub use nife::*;\n    }",
    ) && patch_after(
        // paths: `temp_dir`, `split_paths` and `join_paths` (milestone 64). The arm precedes the
        // `_ =>` unsupported fallback, whose `temp_dir()` is `panic!("no filesystem on this
        // platform")` and whose `split_paths()` is `panic!("unsupported")`: without this,
        // `std::env::temp_dir()` aborted the process, which is what `tempfile` reached before it
        // ever got to its own "not supported" arm. `getcwd`, `chdir`, `current_exe` and `home_dir`
        // keep refusing, in `sys/paths/nife.rs` rather than by falling through, so one file holds
        // both halves and a reader meets the reasons together.
        &sys.join("paths/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        mod nife;\n        use nife as imp;\n    }",
    ) && patch_after(
        // process: `getpid` only (milestone 64). Everything else stays the shared `unsupported`
        // backend, which refuses honestly; `getpid` alone was `panic!("no pids on this platform")`,
        // so `std::process::id()` killed the program. The arm is spelled as a split `imp` rather
        // than a whole nife backend because `unsupported.rs` opens with `use super::env::...`, so
        // it cannot be pulled in through a `#[path]` module the way `sys/fs/nife.rs` does.
        &sys.join("process/mod.rs"),
        "cfg_select! {",
        "    target_os = \"nife\" => {\n        #[allow(dead_code)]\n        mod unsupported;\n        mod nife;\n        mod imp {\n            pub use super::nife::getpid;\n            pub use super::unsupported::{\n                ChildPipe, Command, CommandArgs, EnvKey, ExitCode, ExitStatus, ExitStatusError,\n                Process, Stdio, output, read_output,\n            };\n        }\n    }",
    ) && patch_after(
        // **exit: `std::process::exit` was a trap instruction** (milestone 64, fourth pass).
        //
        // `sys/exit.rs` is not a `sys/<module>/mod.rs` backend dispatcher; it is one file whose
        // `cfg_select!` sits *inside* `pub fn exit`, and its `_ =>` arm is
        // `crate::intrinsics::abort()`. So a nife program calling `std::process::exit(0)` compiled
        // perfectly and then executed `brk`, which the kernel reports as `EVENT_FAULT` with a pc
        // and an address: a clean exit arriving at its supervisor as a crash, and a fault report on
        // the console for a program that did nothing wrong.
        //
        // Nothing noticed because the normal path never goes through here. `sys/pal/nife/mod.rs`'s
        // `_start` calls `rt::exit` on `main`'s return value directly, and `std::process::exit` is
        // the *only* caller of `sys::exit::exit` in the whole of std. The two ways a Rust program
        // ends took different exits, and only one of them was wired.
        //
        // The arm is what `_start` already does, which is why this needs no new decision: the same
        // `SYS_EXIT` with the same code. The kernel discards the code (`sched::exit` is
        // `depart(EVENT_EXIT, 0, 0)`), which is a real limitation recorded in notes/std.md rather
        // than something this arm can fix; what it fixes is exit-versus-fault, which is observable
        // today and which `a_whole_std_program_runs_on_the_native_abi` now asserts.
        &sys.join("exit.rs"),
        "pub fn exit(code: i32) -> ! {\n    cfg_select! {",
        "        target_os = \"nife\" => {\n            crate::sys::pal::nife::rt::exit(code as i64)\n        }",
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
    // The build just produced the dep-info the sweep reads, so this costs a few file reads and
    // nothing else. Running it here rather than in `script/lint` is deliberate: the sweep's input
    // is "which std sources did rustc actually compile for nife", which only exists after a build.
    std_aborts()
}

// ===========================================================================================
// The abort sweep (milestone 64, fourth pass).
// ===========================================================================================

/// **Every std call that kills a nife process instead of refusing it** (milestone 64).
///
/// This exists because milestone 64's own `BUGS` section said it did not, and named the cost:
/// *"Nothing runs the sweep that found the three aborts. It is a person reading every module the
/// PAL falls through and asking what its neighbours do, which is rung four of AGENTS.md's ladder.
/// The three found so far were each found by accident or by one deliberate pass, and a fourth
/// would be found the same way."* It was, and the fourth (`std::process::exit`) is the one that
/// argues hardest for a gate: it does not live in a `sys/<module>/mod.rs` backend at all, so the
/// by-hand method of reading module dispatchers would not have reached it however carefully
/// somebody ran it.
///
/// **The method, and why it is exact rather than a grep over std.** The prioritised gap list in
/// notes/crates-io-on-nife.md is built from PAL functions that answer `Unsupported`, and a
/// function that aborts never answers, so it is structurally invisible there. This asks the
/// complementary question directly: of the std sources rustc **actually compiled for this
/// target**, which ones contain a body that terminates the process? The compiled set comes from
/// cargo's own dep-info rather than from reading `cfg_select!` arms, so it is what the compiler
/// did and not what we believe it did; nothing here has to model `cfg` evaluation.
///
/// **What it deliberately does not do.** It does not judge. Most of what it finds is correct
/// (`Once::wait` cannot work without threads; a recursive `RwLock` on a single-threaded target is
/// a deadlock either way), so the output is a set compared against [`ABORTS_ACCEPTED`], where each
/// entry carries the reason it is allowed. A new one fails the build and has to be answered:
/// either bind it in the PAL, or add it with its reason. That is the whole mechanism, and it is
/// rung two of AGENTS.md's ladder where the milestone had rung four.
fn std_aborts() -> bool {
    let compiled = compiled_std_sources();
    if compiled.is_empty() {
        eprintln!(
            "std-aborts: found no compiled std sources in the dep-info under std_exerciser/target.\n\
             std-aborts: this check is meaningless without them; run `cargo xtask std-exerciser` first."
        );
        return false;
    }

    let mut found = Vec::new();
    for path in &compiled {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        // The path as std names it (`sys/exit.rs`), which is what a reader greps for and what the
        // accepted list below is written in.
        let rel = std_relative(path);
        for (n, line) in text.lines().enumerate() {
            if let Some(what) = abort_shaped(line) {
                found.push((
                    rel.clone(),
                    n + 1,
                    what.to_string(),
                    line.trim().to_string(),
                ));
            }
        }
    }

    let mut unexpected = Vec::new();
    for (file, line, _what, text) in &found {
        if !ABORTS_ACCEPTED
            .iter()
            .any(|(f, needle, _why)| f == file && text.contains(needle))
        {
            unexpected.push((file, line, text));
        }
    }

    if unexpected.is_empty() {
        println!(
            "std-aborts: {} process-ending bodies across {} compiled std sources, all accounted for",
            found.len(),
            compiled.len()
        );
        return true;
    }

    eprintln!("std-aborts: a std source compiled for nife ends the process somewhere new:");
    for (file, line, text) in &unexpected {
        eprintln!("  {file}:{line}: {text}");
    }
    eprintln!(
        "\nstd-aborts: each of these is one of two things, and the difference is milestone 64's whole line.\n\
         If a nife program can REACH it, it is a defect: the call compiles, then kills the process,\n\
         which is what `env::vars`, `env::temp_dir`, `env::split_paths`, `process::id` and\n\
         `process::exit` each were. Bind it in patches/std-nife (and add a `target_os = \"nife\"` arm\n\
         in `std_patch_dispatch` if the fallback is a dispatcher's).\n\
         If it cannot be reached, or if ending the process is the honest answer, add it to\n\
         `ABORTS_ACCEPTED` in xtask/src/main.rs WITH THE REASON. An entry with no reason is the\n\
         thing this check exists to stop.\n\
         See notes/std.md, \"What still ends a nife process\"."
    );
    false
}

/// Is this line a body that ends the process, rather than a mention of one?
///
/// Returns the shape it matched, or `None`. Doc comments and `//` comments are skipped, because
/// this tree's PAL files talk *about* the panics they replaced at length, and a check that could
/// not tell a fix from its own explanation would be useless the day it was written.
fn abort_shaped(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("//") || t.starts_with("///") || t.starts_with("*") {
        return None;
    }
    // `unreachable!` is not here: it asserts an invariant of the code rather than declaring a
    // platform's answer, and including it would bury the signal under std's own assertions.
    for pat in [
        "panic!(",
        "unimplemented!(",
        "todo!(",
        "rtabort!(",
        "intrinsics::abort()",
        "panic_nounwind(",
    ] {
        if t.contains(pat) {
            return Some(match pat {
                "panic!(" => "panic",
                "unimplemented!(" => "unimplemented",
                "todo!(" => "todo",
                "rtabort!(" => "rtabort",
                "intrinsics::abort()" => "abort",
                _ => "panic_nounwind",
            });
        }
    }
    None
}

/// Every process-ending body a nife build compiles today, with the reason it stays.
///
/// `(file as std names it, a substring of the line, why it is allowed)`. Matching on a substring
/// rather than a line number is what keeps this from being rewritten by every nightly that adds a
/// blank line; it still moves when upstream rewords the panic, which is a rebuild-and-reread this
/// check is *for*.
///
/// **Read the third column before adding a fourth entry.** Three distinct reasons appear, and only
/// one of them is a licence:
///
///   - *unreachable on nife*: the body sits behind a `cfg` nife does not satisfy, so it is
///     compiled-adjacent rather than compiled. These are the safe ones.
///   - *no answer exists*: single-threaded, so the call can only deadlock or end. Upstream chose
///     to end, and there is no third option to build.
///   - *ours, and deliberate*: the PAL's own, where ending the process is the honest report.
const ABORTS_ACCEPTED: &[(&str, &str, &str)] = &[
    // ---- unreachable on nife ------------------------------------------------------------------
    (
        "sys/alloc/mod.rs",
        "add a value for MIN_ALIGN",
        "a const-eval arm for architectures with no known minimum alignment; aarch64 and riscv64 both have one",
    ),
    (
        "sys/exit.rs",
        "std::process::exit called re-entrantly",
        "inside the `target_os = \"linux\"` arm of `unique_thread_exit`",
    ),
    (
        "sys/exit.rs",
        "rtabort!(\"exit({}) called\", code)",
        "the `solid_asp3` arm of `exit`",
    ),
    (
        "sys/exit.rs",
        "TA should not call `exit`",
        "the `teeos` arm of `exit`",
    ),
    (
        "sys/exit.rs",
        "crate::intrinsics::abort()",
        "two sites: the `uefi` arm's last resort, and the `_ =>` arm nife USED to take. Milestone 64 \
         added a nife arm above it, so the fallback is no longer ours; the line stays compiled \
         because `cfg_select!` keeps every arm's source in the file",
    ),
    (
        "sys/pipe/unsupported.rs",
        "creating pipe on this platform is unsupported!",
        "inside `mod unix_traits`, gated `#[cfg(any(unix, hermit, wasi))]`; nife is none of them. \
         The reachable half of this backend refuses honestly: `pipe()` returns `UNSUPPORTED_PLATFORM` \
         and `Pipe` is uninhabited",
    ),
    (
        "sys/process/unsupported.rs",
        "no pids on this platform",
        "`getpid` here is the one item the nife arm of `sys/process/mod.rs` does NOT re-export; it \
         takes `sys/process/nife.rs`'s instead. The module is pulled in `#[allow(dead_code)]` for \
         everything else, so this body is compiled and unreachable",
    ),
    (
        "sys/personality/mod.rs",
        "core::intrinsics::abort()",
        "the `msvc`/`wasm` arm's stub personality routine",
    ),
    (
        "sys/path/mod.rs",
        "path_separator_bytes must be ASCII bytes",
        "a `const` assertion inside the separator macro, evaluated at compile time",
    ),
    // ---- no answer exists: single-threaded ----------------------------------------------------
    (
        "sys/sync/condvar/no_threads.rs",
        "condvar wait not supported",
        "a wait with no other thread to notify it can only block forever. Upstream ends the process \
         instead, and there is no third answer to build until milestone 64's `thread::spawn` fork is \
         decided. Recorded in notes/std.md rather than fixed",
    ),
    (
        "sys/sync/once/no_threads.rs",
        "not implementable on this target",
        "`Once::wait` waits for another thread's initialisation; same reason as the condvar above",
    ),
    (
        "sys/sync/once/no_threads.rs",
        "Once instance has previously been poisoned",
        "poison propagation, which is `Once`'s documented behaviour on every platform",
    ),
    (
        "sys/sync/once/no_threads.rs",
        "one-time initialization may not be performed recursively",
        "a recursive `call_once`, which is a bug in the caller on every platform",
    ),
    (
        "sys/sync/rwlock/no_threads.rs",
        "rwlock locked for writing",
        "taking a read lock while this same thread holds the write lock. On a threaded platform it \
         deadlocks; here it is caught and named, which is strictly better",
    ),
    (
        "sys/sync/rwlock/no_threads.rs",
        "rwlock locked for reading",
        "the mirror case, and the same argument",
    ),
    (
        "sys/thread_local/mod.rs",
        "thread local panicked on drop",
        "a destructor that panicked; unwinding out of TLS teardown is undefined on every platform",
    ),
    (
        "sys/thread_local/no_threads.rs",
        "Attempted to initialize thread-local while it is being dropped",
        "a TLS access from inside TLS teardown, a caller bug on every platform",
    ),
    (
        "sys/os_str/bytes.rs",
        "is not an OsStr boundary",
        "a slicing bounds assertion, the `OsStr` twin of `str`'s",
    ),
    // ---- ours, and deliberate ------------------------------------------------------------------
    (
        "sys/random/nife.rs",
        "panic!(",
        "the entropy service's own refusals: `std::random` promises cryptographic strength, so a \
         service that cannot deliver it must not return bytes (DECISIONS §44, milestone 56)",
    ),
    (
        "sys/time/nife.rs",
        "panic!(",
        "the clock page's refusals: a wall clock that reads a torn or unrecognised page must not \
         invent a time (milestone 51)",
    ),
    (
        "sys/pal/nife/clockproto.rs",
        "panic!(",
        "the clock contract's own host-side test assertions, generated verbatim from \
         crates/clock_proto and unreachable in a target build",
    ),
];

/// Every `library/std/src/**` source cargo recorded as an input to this target's builds.
///
/// **From the dep-info, not from reading `cfg_select!`.** Every `.d` file under the `std_exerciser`
/// target directories is scanned and the std paths unioned, which makes this robust to cargo
/// moving where it files dep-info and to the two ISAs compiling slightly different sets: a union
/// over both targets is exactly the set the sweep wants, since a body reachable on either ISA is
/// reachable.
fn compiled_std_sources() -> Vec<PathBuf> {
    let mut deps = Vec::new();
    for triple in STD_TARGETS {
        collect_files(
            &workspace_root().join(format!("std_exerciser/target/{triple}")),
            &mut deps,
        );
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for d in deps {
        if d.extension().and_then(|e| e.to_str()) != Some("d") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&d) else {
            continue;
        };
        for tok in text.split_whitespace() {
            // **`sys/` only, and the boundary is the claim rather than a convenience.** `sys` IS
            // std's platform abstraction layer: everything under it is one platform's answer, and
            // everything above it is portable code that behaves the same here as on Linux. A panic
            // in `sys/` says "this platform has nothing to offer"; a panic in `thread/scoped.rs`
            // or `path.rs` says "you called this wrong", and it says it identically everywhere.
            // Sweeping all of std mixes the two and buries about forty of the second under none of
            // the first, which is what the first version of this check did. The limit is recorded
            // in notes/std.md's BUGS, because it is a real gap: portable std code that is only
            // *reachable* on a platform this thin would not be caught here.
            if tok.contains("library/std/src/sys/") && tok.ends_with(".rs") {
                let p = PathBuf::from(tok);
                if p.is_file() && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// `.../library/std/src/sys/exit.rs` as `sys/exit.rs`, which is how std's own source refers to it.
fn std_relative(p: &Path) -> String {
    let s = p.display().to_string();
    match s.split_once("library/std/src/") {
        Some((_, rest)) => rest.to_string(),
        None => s,
    }
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

/// **The host's load average across one emulated leg**, so a red timing assertion says whether the
/// machine was busy (milestone 117's third stranger run, 2026-08-18).
///
/// # Why the harness and not the guest
///
/// The whole family of assertions in notes/load-sensitive-assertions.md has one signature: a claim
/// whose truth depends on the host, written from inside a guest that cannot see the host. The guest
/// can measure how late it was; it cannot know whether eleven other QEMUs were on the same eight
/// cores. **This process can**, and it is the only participant that can, which is why the number is
/// printed here rather than woven into a panic message.
///
/// The run that asked for it: `script/test` went red in 2 of 13 aarch64 legs while other lanes had
/// this laptop at a one-minute load average of 45 to 63, and nothing in the transcript said so, so
/// an hour went into a defect that was not there. One line at the failure would have decided it.
///
/// # What it does and does not claim
///
/// It **suggests**, and it says so in its own output. A loaded host does not make a failure
/// spurious; a quiet host does not make it real. What it removes is the reader having to guess,
/// and the peak matters as much as the number at the end: a suite runs for minutes and the
/// one-minute average decays, so a burst of contention halfway through is invisible by the time
/// the leg fails.
///
/// min/mean/peak is `script/repeat-under-load`'s vocabulary, deliberately: the acceptance harness
/// already reports contention in those three numbers, and a reader who has seen one table should
/// recognise this line without learning a second shape.
///
/// # BUGS
///
/// Sampled only while a leg is *running*. A host that was quiet during the leg and thrashing during
/// the build before it produces an honest, unhelpful line. The subprocess is one `uptime` every
/// five seconds, which is free next to QEMU, but it is a subprocess: on a host where `uptime` is
/// missing or prints an unfamiliar shape, every field stays `None` and the report says "unavailable"
/// rather than guessing.
struct HostLoad {
    min: f64,
    max: f64,
    total: f64,
    samples: u32,
    last: std::time::Instant,
}

impl HostLoad {
    /// Every five seconds. The callers poll on a 100 ms cadence for the scanout referee, and one
    /// `fork`/`exec` per poll would be 3,000 of them over a five-minute leg to resolve a number
    /// that moves on a sixty-second decay.
    const EVERY: std::time::Duration = std::time::Duration::from_secs(5);

    /// Start sampling, taking the first reading now so a leg that fails in its first second still
    /// reports something.
    fn new() -> Self {
        let mut load = Self {
            min: f64::INFINITY,
            max: 0.0,
            total: 0.0,
            samples: 0,
            // Back-dated so the first `sample()` call fires rather than waiting out the interval.
            // `checked_sub` rather than `-`: `Instant` counts from boot on both our platforms, and
            // subtracting past zero is a panic. A machine that booted four seconds ago is a real
            // CI shape, and a harness that panicked there would be a mystery worth more than the
            // one sample it costs to fall back to waiting the interval out.
            last: std::time::Instant::now()
                .checked_sub(Self::EVERY)
                .unwrap_or_else(std::time::Instant::now),
        };
        load.sample();
        load
    }

    /// Take a reading if the interval has elapsed. Cheap enough to call from a 100 ms poll loop or
    /// from a line-at-a-time transcript reader, which is what the two legs do.
    fn sample(&mut self) {
        if self.last.elapsed() < Self::EVERY {
            return;
        }
        self.last = std::time::Instant::now();
        let Some(now) = one_minute_load_average() else {
            return;
        };
        self.min = self.min.min(now);
        self.max = self.max.max(now);
        self.total += now;
        self.samples += 1;
    }

    /// Say what the host was doing, but only when the leg went red. On a green leg this is noise,
    /// and a diagnostic that prints on every run is a diagnostic readers learn to skip.
    fn report_if_failed(&self, ok: bool, arch: &str) {
        if ok {
            return;
        }
        eprintln!();
        if self.samples == 0 {
            eprintln!(
                "host load ({arch}): unavailable (`uptime` did not answer in a shape this parses)"
            );
            return;
        }
        let mean = self.total / f64::from(self.samples);
        let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
        eprintln!(
            "host load ({arch}): 1-minute average {:.2} / {:.2} / {:.2} (min/mean/peak over {} \
             samples), on {} cores",
            self.min, mean, self.max, self.samples, cores,
        );
        if cores > 0 && self.max > cores as f64 {
            eprintln!(
                "  {:.1}x oversubscribed at the peak. A timing assertion that failed above may be \
                 measuring this machine rather than this kernel; `script/icount` asserts the timer \
                 claims in instructions, which nothing the host does can move. See \
                 notes/load-sensitive-assertions.md.",
                self.max / cores as f64,
            );
        } else {
            eprintln!(
                "  Not oversubscribed, so contention is the less likely explanation for a failure \
                 above. See notes/load-sensitive-assertions.md."
            );
        }
    }
}

/// The host's one-minute load average, from `uptime`.
///
/// `uptime` rather than `getloadavg(3)` because reaching the libc call means taking the `libc`
/// crate, and §46 makes a dependency a decision rather than a convenience: this is one number, read
/// once every five seconds, on a machine that is already running an emulator. `script/repeat-under-load`
/// parses the same command with the same trick, and this is deliberately the same parse in Rust:
/// macOS prints `load averages: 4.14 4.86 4.29` and Linux prints `load average: 0.50, 0.40, 0.30`,
/// so stripping commas first lets one scan serve both.
fn one_minute_load_average() -> Option<f64> {
    let out = Command::new("uptime").output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_load_average(std::str::from_utf8(&out.stdout).ok()?)
}

/// The pure half of [`one_minute_load_average`], split out so it can be tested on the host without
/// an `uptime` to run.
///
/// **The two formats are not both reachable from one machine**, which is what makes the test worth
/// having rather than filler: development is macOS and CI is `ubuntu-24.04-arm`, so a parse that
/// only understood the shape in front of its author would keep working here and quietly report
/// "unavailable" on every CI run, which is exactly the silence this whole feature exists to end.
fn parse_load_average(uptime_output: &str) -> Option<f64> {
    // Commas out first: Linux separates the three figures with them and macOS does not, so one
    // scan serves both once they are gone. This is `script/repeat-under-load`'s `load_now` in Rust.
    let text = uptime_output.replace(',', " ");
    let mut fields = text.split_whitespace();
    while let Some(f) = fields.next() {
        if f == "average:" || f == "averages:" {
            return fields.next()?.parse().ok();
        }
    }
    None
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

    // **The suite's verdict is collected, not returned early.** An early return here skipped every
    // prober's report exactly when a guest-side assertion had failed, which is the run where their
    // findings matter most: milestone 55's responder lane spent two five-minute suites learning
    // nothing, because the guest said "nobody ever asked me anything" and the host side, which
    // knew precisely why it had stopped asking, was never given the chance to say so.
    let mut child_ok = false;
    // Sampled here, in the loop that already exists, because the number worth having is the one
    // from *while the leg ran*: a suite takes minutes and the one-minute average has decayed by the
    // time the verdict is in. Reported only if something below goes red.
    let mut load = HostLoad::new();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                child_ok = status.success();
                break;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("waiting for the test child failed: {e}");
                break;
            }
        }
        referee.poll();
        load.sample();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // All four, and not short-circuited: a run that lost the scanout AND a network answer should
    // say so once rather than making the reader run it again to find the next failure.
    let scanout = referee.report();
    let inbound = prober.report();
    let multicast = mcast.report();
    let smb = smb_prober.report();
    let ok = child_ok && scanout && inbound && multicast && smb;
    load.report_if_failed(ok, arch);
    ok
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
/// match `socket_proto::fixture` (`IN_MSG`/`OUT_MSG`), which is what the two guest programs read
/// them from, and they are deliberately different strings: an echo would pass even if the guest
/// were only reflecting our own bytes, and the point of this gate is that the guest **composed** an
/// answer to a connection it did not make.
///
/// **Spelled again here rather than imported**, which is the same call the pinned [MS-NLMP] vectors
/// beside the credential tests make: the claim is that two independently-written sides agree, and a
/// shared constant would let one edit move both. A drift is loud, because the guest's answer would
/// not match and the prober says exactly what came back instead.
const INBOUND_IN: &[u8] = b"nife-in!";
const INBOUND_OUT: &[u8] = b"nife-out!";
/// How many connections the guest **offers** over a whole boot: two each from the two programs
/// that listen on the forwarded port, one after the other (`socket_test_client`'s hand-written
/// accept role from milestone 107, and `std_exerciser`'s `std::net::TcpListener` half from
/// milestone 64). The prober keeps connecting until it has this many or the run ends, and it must
/// keep going even once it has enough to pass, because a guest sitting in `ACCEPT` needs a peer:
/// stopping early would hang the guest's own test rather than the host's.
const INBOUND_OFFERED: usize = 4;

/// How many of those the prober must actually **collect** for the leg to pass, which is not the
/// same number, and the gap is deliberate.
///
/// **Three of four is a provable claim, not a fudge.** Neither program can supply more than two, so
/// a host that collected three collected at least one from *each* of them. That is exactly the half
/// no in-guest assertion can make: the bytes reached a process outside the machine, for both
/// listeners.
///
/// **What the fourth round would have added, and why it is not worth what it costs.** Requiring all
/// four would also confirm the *re-arm* host-side, which is what milestone 107's `2` did when there
/// was one listening window. But the re-arm is asserted where it is actually checkable, inside each
/// guest test: `serve_one_inbound` fails the run if a second `accept` does not return with the right
/// payload, and a guest cannot fake that. Paying for a duplicate of it with a flaky leg is a bad
/// trade.
///
/// **And it was measured rather than assumed.** With four required, run 32195227733's riscv64 leg
/// reported "the guest served 3 of 4" while **all 279 guest tests passed**, on a runner this
/// script's own load instrument called not oversubscribed. So the guest served four and the host
/// collected three: one answer went somewhere the prober was not reading. Five local riscv boots did
/// not reproduce it, which puts it around one in six on that runner class, and it is exactly the
/// kind of intermittent red that notes/net.md records misleading three separate milestones.
///
/// **The mechanism is still not identified**, and a second lane went looking on 2026-08-19 without
/// finding it. What that lane did establish is worth having before you start: the failure is
/// host-side by elimination (every guest path that serves fewer than two rounds is loud), the
/// teardown race is ruled out by timing, and it does not reproduce on macOS because CI is
/// `ubuntu-24.04-arm` and this is an emulator-timing failure. It also left the prober's transcript
/// printing on green runs, so the next red one has a known-good shape to be read against. The whole
/// finding is in notes/net.md; read it before touching this number. This constant makes the gate
/// robust to losing one round without letting it claim less than it proves.
const INBOUND_REQUIRED: usize = 3;

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
                    "inbound check ({arch}): host connections to 127.0.0.1:{port} were forwarded \
                     into the guest, accepted, and answered with the guest's own bytes, by both \
                     listeners: the hand-written one and `std::net::TcpListener`. At least \
                     {INBOUND_REQUIRED} of the {INBOUND_OFFERED} offered, which is the floor that \
                     proves each of the two answered, since neither can supply more than two."
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

/// One prober thread: connect, speak, and require the guest's answer, up to `INBOUND_OFFERED`
/// times, passing at `INBOUND_REQUIRED`.
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
    let mut trace = InboundTrace::new();

    while done < INBOUND_OFFERED && !stop.load(Ordering::Relaxed) {
        let opened = std::time::Instant::now();
        let mut s = match std::net::TcpStream::connect_timeout(
            &addr,
            std::time::Duration::from_millis(500),
        ) {
            Ok(s) => s,
            Err(e) => {
                last = format!("could not connect to 127.0.0.1:{port}: {e}");
                trace.note("connect-failed", opened, 0);
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
            trace.note("write-failed", opened, 0);
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        // What ended this connection, for the tally the failure text prints. The read loop has
        // five exits and they mean different things: only two of them are "nothing was consumed",
        // and telling them apart is the whole diagnosis when a round goes missing.
        let mut outcome = "answered";
        while got.len() < INBOUND_OUT.len() {
            match s.read(&mut buf) {
                Ok(0) => {
                    // The guest had no listener and closed; nothing was consumed. Unless bytes had
                    // already arrived, in which case a round WAS served and we lost the tail of it.
                    outcome = if got.is_empty() {
                        "closed-empty"
                    } else {
                        "closed-partial"
                    };
                    break;
                }
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
                        outcome = "stopped-while-waiting";
                        break;
                    }
                }
                Err(e) => {
                    last = format!("reading the guest's answer failed: {e}");
                    outcome = match e.kind() {
                        ErrorKind::ConnectionReset => "reset",
                        ErrorKind::ConnectionAborted => "aborted",
                        ErrorKind::BrokenPipe => "broken-pipe",
                        _ => "read-failed",
                    };
                    break;
                }
            }
        }
        drop(s);

        if got == INBOUND_OUT {
            done += 1;
            trace.note("answered", opened, got.len());
            continue;
        }
        if outcome == "answered" {
            // The loop filled its quota without matching: bytes that are not the guest's answer.
            outcome = "wrong-bytes";
        }
        trace.note(outcome, opened, got.len());
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

    if done >= INBOUND_REQUIRED {
        // Printed on a GREEN run too, and that is the point: a red run is only diagnosable against
        // a known-good shape, and the first two failures of this check had none to compare with.
        // It is one line and at most four more.
        eprintln!("inbound prober (port {port}): {}", trace.summary());
        Ok(())
    } else {
        Err(format!(
            "the guest served {done} of the {INBOUND_OFFERED} inbound connections it offers on the \
             port forwarded to 127.0.0.1:{port}, and {INBOUND_REQUIRED} is the floor that proves \
             both listeners answered; last attempt: {last}\n  what the {} attempts did: {}\n  \
             READ THE TIMESTAMPS FIRST. The four rounds come from two listeners in two separate \
             windows about eight seconds apart, two rounds in each, so the answers cluster. Two \
             clusters and a missing round means the host lost one that the guest served, and the \
             outcome beside it names how. ONE cluster means a whole listener never ran: look for \
             `(no virtio-net device attached; skipping)` in the transcript, which is the only way \
             either guest test passes without offering its two. See notes/net.md.",
            trace.attempts,
            trace.summary(),
        ))
    }
}

/// **What every prober connection did, kept so a failure can name a mechanism instead of a count.**
///
/// The check has failed twice with nothing to go on but "the guest served 3 of 4", which does not
/// distinguish an abandoned connection from a lost answer from a teardown race, and each of those
/// wants a different fix. The read loop in `probe_inbound` has five exits; this records which one
/// each connection took, how long it was held, and how many bytes it had collected when it ended.
///
/// **Deliberately cheap and unconditional.** A boot makes a few hundred attempts at most, the
/// counters are increments, and the per-connection lines are kept only for the ones that carried
/// bytes or were held long enough to be interesting. The summary prints on a **passing** run too,
/// which is the half that was missing: a red run is only readable against a known-good shape, and
/// the two failures on record had none to compare with.
#[derive(Default)]
struct InboundTrace {
    attempts: usize,
    counts: std::collections::BTreeMap<&'static str, usize>,
    /// `(ms since the prober started, outcome, held ms, bytes)` for connections worth a line: the
    /// ones that collected bytes, and the ones held over a second. A connection that was reset in
    /// under a millisecond with nothing on it is the boring majority and is only counted.
    events: Vec<(u128, &'static str, u128, usize)>,
    started: Option<std::time::Instant>,
}

impl InboundTrace {
    fn new() -> Self {
        Self {
            started: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    fn note(&mut self, outcome: &'static str, opened: std::time::Instant, bytes: usize) {
        self.attempts += 1;
        *self.counts.entry(outcome).or_insert(0) += 1;
        let held = opened.elapsed().as_millis();
        if bytes > 0 || held >= 1000 || outcome == "answered" {
            let at = self
                .started
                .map(|s| s.elapsed().as_millis())
                .unwrap_or_default();
            // Bounded, so a pathological run cannot grow this without limit.
            if self.events.len() < 64 {
                self.events.push((at, outcome, held, bytes));
            }
        }
    }

    fn summary(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.counts {
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&format!("{k} x{v}"));
        }
        if out.is_empty() {
            out.push_str("no attempts");
        }
        for (at, outcome, held, bytes) in &self.events {
            out.push_str(&format!(
                "\n    +{at} ms: {outcome} after {held} ms, {bytes} bytes"
            ));
        }
        out
    }
}

/// **The mDNS gate's constants.** RFC 6762's group and port, the ethernet address IPv4 multicast
/// maps onto (01:00:5e plus the group's low 23 bits), and the spoofed source this prober injects
/// from. The address is on slirp's subnet and held by nothing, so a datagram the guest sends back
/// to it can only be a reply to what arrived; the MAC is locally administered and never claimed.
const MDNS_GROUP: [u8; 4] = [224, 0, 0, 251];
const MDNS_PORT: u16 = 5353;
const MDNS_GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];
const MDNS_PROBER_IP: [u8; 4] = [10, 0, 2, 99];
const MDNS_PROBER_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x99, 0x99, 0x99];
/// The source port and transaction id of the **legacy** query. Not 5353, which is the whole point:
/// RFC 6762 §6.7 makes a querier whose source port is something else a one-shot resolver that
/// cannot receive multicast, so the response must come back **unicast to this port** with the id
/// echoed, the question repeated and every TTL capped at 10. A responder that ignored the source
/// endpoint would answer the group and this leg would time out.
const MDNS_LEGACY_PORT: u16 = 5399;
const MDNS_LEGACY_ID: u16 = 0x4321;
/// The service type the gate browses. `_adisk._tcp` is the one that matters: its TXT record is what
/// puts a server in a Mac's backup-disk list, and it is the record with content worth asserting.
const MDNS_BROWSE: &str = "_adisk._tcp.local";

/// **The guest's own configuration document**, so the gate's expectations and the responder's
/// behaviour have one source. Editing `user/mdns_responder.conf` moves both; a value asserted here
/// as a literal would be a second copy of a measurement.
const RESPONDER_CONFIG: &str = include_str!("../../user/mdns_responder.conf");

/// **The host side of the mDNS gate** (milestone 55): the peer on the frame-level hub the runner
/// wires beside slirp when `NIFE_MCAST_PORT` is set.
///
/// It exists because slirp cannot carry multicast in either direction, so no exchange through it
/// can prove the thing the `multicast` feature was enabled for: that a datagram addressed to a
/// *group*, not to the guest, is accepted once the guest has joined. This prober speaks QEMU's
/// socket-netdev protocol (each ethernet frame prefixed with a 4-byte big-endian length, over one
/// TCP connection) and therefore sees and injects raw frames, below every slirp limitation.
///
/// **What it proves, which is more than carriage** (milestone 55's responder lane; the stack half
/// traded marker payloads and proved protocol nowhere). It waits for the responder's unsolicited
/// announcement, asks it a **real DNS question** twice, and decodes both answers with a parser of
/// its own:
///
/// 1. A multicast browse for `_adisk._tcp.local` must come back to the group with the PTR in the
///    answer section, the instance's SRV, TXT and the host's A riding as additionals (RFC 6763
///    §12.1), cache-flush set on the three this responder owns and never on the shared PTR.
/// 2. A **legacy** query from an ephemeral source port must come back **unicast to that port**,
///    with the id echoed, the question repeated, everything in the answer section and every TTL
///    capped at 10 (RFC 6762 §6.7).
///
/// And the record contents are checked against `user/mdns_responder.conf`, so what is asserted is
/// that the machine advertises what it was configured to advertise.
///
/// Same shape and lifecycle as [`InboundProber`]: constructed before the child so the runner
/// inherits the port, running for the whole boot because nothing here knows when the mDNS test
/// starts, stopped and reported after the suite. It is passive until the guest announces itself.
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

    /// Stop listening, and say whether the whole exchange happened: the guest's announcement seen
    /// raw on the wire, both injected queries answered, and both answers carrying the records
    /// `user/mdns_responder.conf` describes. The guest's own verdict covers that it answered
    /// something; this covers what it said.
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
                    "multicast check ({arch}): the guest announced itself on {}.{}.{}.{}, answered \
                     a multicast browse for {MDNS_BROWSE} to the group, and answered a legacy \
                     query unicast to the port it came from. Both carried the PTR, SRV, TXT and A \
                     records user/mdns_responder.conf describes.",
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

/// How far the exchange has got. The prober injects the query for the stage it is in, and advances
/// only when it has *verified* the answer, so a lost datagram is retried rather than skipped: the
/// responder re-announces whenever a receive times out, and every announcement re-triggers the
/// current stage's injection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MdnsStage {
    /// Nothing yet. Waiting for the responder's unsolicited announcement, which is also where the
    /// guest's own address is learned.
    Announced,
    /// The multicast browse is out; waiting for the group-addressed answer.
    Browse,
    /// The legacy query is out; waiting for the unicast answer.
    Legacy,
}

/// One prober thread: connect to the hub's socket backend, wait for the guest to announce itself,
/// then ask it two real questions and check both answers.
///
/// Passive until spoken to, deliberately: the hub floods every frame of the whole boot here (DHCP
/// for every net test, TFTP, the TCP exchanges, the SMB session), and this thread answers only
/// two things, an ARP request for the address it spoofs and an mDNS message on the group.
fn probe_multicast(
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use std::io::{ErrorKind, Read, Write};
    use std::sync::atomic::Ordering;

    // The expectations, derived from the document the guest ships rather than written out again.
    let config = mdns_config::Config::parse(RESPONDER_CONFIG)
        .map_err(|e| format!("the responder's own configuration document does not parse: {e:?}"))?;
    let adv = config.advertisement(None);
    // Lower-cased, because `dns_name` normalises what it decodes: DNS names compare
    // case-insensitively (RFC 6762 §9.2 keeps that for mDNS), and the configuration's `GL-BE9300`
    // is the same name as the wire's `gl-be9300`. Comparing the two forms directly is a gate that
    // fails on a difference the protocol says is not one.
    let instance = format!("{}.{MDNS_BROWSE}", adv.host).to_lowercase();
    let hostname = format!("{}.local", adv.host).to_lowercase();
    let mut txt_entries: Vec<String> = adv
        .disks
        .iter()
        .enumerate()
        .map(|(i, d)| format!("dk{i}=adVN={},adVF={}", d.volume, d.flags))
        .collect();
    txt_entries.push(format!("sys=adVF={}", adv.sys_flags));

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

    // Tracing, off unless `NIFE_MCAST_DEBUG` is set. This exchange happens inside a boot, on a
    // hub, between two programs that cannot print, and its failure mode is silence; the first
    // debugging session without this spent a five-minute suite run learning nothing.
    let debug = std::env::var_os("NIFE_MCAST_DEBUG").is_some();
    if debug {
        eprintln!("multicast prober: attached to the hub on 127.0.0.1:{port}");
    }

    let mut stage = MdnsStage::Announced;
    let mut guest_ip: Option<[u8; 4]> = None;
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let inject = |s: &mut std::net::TcpStream, frame: Vec<u8>| -> Result<(), String> {
        let mut msg = (frame.len() as u32).to_be_bytes().to_vec();
        msg.extend_from_slice(&frame);
        s.write_all(&msg)
            .map_err(|e| format!("injecting a frame failed: {e}"))
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(match stage {
                MdnsStage::Announced => {
                    "the guest never announced itself on the group: either the responder did not \
                     run, or a multicast SENDTO never reached the wire"
                        .to_string()
                }
                MdnsStage::Browse => {
                    "the guest was announced and asked a multicast browse, and never answered it: \
                     the injected datagram was most likely dropped by the IPv4 accept filter, \
                     which is exactly what an unjoined group looks like"
                        .to_string()
                }
                MdnsStage::Legacy => format!(
                    "the multicast browse was answered, but the legacy query from port \
                     {MDNS_LEGACY_PORT} was never answered unicast: the responder either ignored \
                     the datagram's source endpoint or could not reach {}.{}.{}.{}",
                    MDNS_PROBER_IP[0], MDNS_PROBER_IP[1], MDNS_PROBER_IP[2], MDNS_PROBER_IP[3],
                ),
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
            let frame = acc[4..4 + flen].to_vec();
            acc.drain(..4 + flen);

            // Answer an ARP request for the address we spoof. Without this the guest cannot
            // resolve a MAC for it, and the legacy unicast answer is dropped inside its own stack
            // before it reaches the wire.
            if let Some(reply) = arp_reply(&frame) {
                inject(&mut s, reply)?;
                continue;
            }

            let Some(dg) = udp_datagram(&frame) else {
                continue;
            };
            if dg.dst_port != MDNS_PORT && dg.dst_port != MDNS_LEGACY_PORT {
                continue;
            }
            if debug {
                eprintln!(
                    "multicast prober: {} bytes of UDP {:?} -> {:?}:{}",
                    dg.payload.len(),
                    dg.src_ip,
                    dg.dst_ip,
                    dg.dst_port
                );
            }
            // **Only the guest's answers count**, and this filter is not defensive tidiness: it
            // was measured, and by the funniest possible packet. Slirp is on the same hub, and it
            // forwards a group-addressed datagram out to the host's REAL network; the injected
            // browse for `_adisk._tcp.local` therefore reached the developer's own segment, where
            // **the reference router answered it** (192.168.8.1, the GL-BE9300 this whole
            // milestone is measured against), and slirp NATed that answer back onto the virtual
            // network as a unicast to the spoofed source. So one injected query provokes two
            // responses, and the wrong one carries *exactly the records this gate expects*,
            // because the expectations were captured from that very router. Without this filter
            // the gate could go green on the router's answer while the guest said nothing at all.
            if let Some(guest) = guest_ip
                && dg.src_ip != guest
            {
                if debug {
                    eprintln!("multicast prober: ignoring an answer from {:?}", dg.src_ip);
                }
                continue;
            }
            let msg = match parse_dns(&dg.payload) {
                Ok(m) => m,
                Err(e) => {
                    if debug {
                        eprintln!("multicast prober: not a DNS message ({e})");
                    }
                    continue; // not a DNS message, or one this parser cannot read
                }
            };
            if debug {
                eprintln!(
                    "multicast prober: dns id {:#x} flags {:#x} qd {:?} an {:?} ar {:?}",
                    msg.id,
                    msg.flags,
                    msg.questions,
                    msg.answers
                        .iter()
                        .map(|r| (r.name.clone(), r.rrtype, r.ttl))
                        .collect::<Vec<_>>(),
                    msg.additionals
                        .iter()
                        .map(|r| (r.name.clone(), r.rrtype, r.ttl))
                        .collect::<Vec<_>>(),
                );
            }
            if msg.flags & 0x8000 == 0 {
                continue; // a query; the guest sends none, and nothing else here should
            }

            if dg.dst_ip == MDNS_GROUP && msg.questions.is_empty() && msg.additionals.is_empty() {
                // An unsolicited announcement (RFC 6762 §8.3): no question, and nothing riding in
                // additionals, which is what separates it from the answer to a browse.
                if !msg.answers.iter().any(|r| r.name == MDNS_BROWSE) {
                    continue; // one of the other two service types' announcements
                }
                if guest_ip.is_none() {
                    guest_ip = msg
                        .answers
                        .iter()
                        .find(|r| r.rrtype == RR_A && r.rdata.len() == 4)
                        .map(|r| [r.rdata[0], r.rdata[1], r.rdata[2], r.rdata[3]]);
                }
                if let Some(ip) = guest_ip
                    && ip != dg.src_ip
                {
                    return Err(format!(
                        "the guest announces an A record of {ip:?} and is speaking from {:?}; a Mac \
                         that resolved the name would then connect to the wrong address",
                        dg.src_ip
                    ));
                }
                let Some(ip) = guest_ip else {
                    return Err(
                        "the announcement carried no A record, so a Mac would discover a name it \
                         cannot resolve. The responder announces one when its spawn hands it the \
                         DHCP lease; is the lease reaching it?"
                            .to_string(),
                    );
                };
                // Ask the guest for its OWN address, which is what fills its neighbour cache with
                // ours: smoltcp fills only from an ARP packet whose target is an address it holds
                // (`process_arp`), so a gratuitous announcement of our address would be discarded,
                // and smoltcp drops the datagram that triggers a neighbour resolution rather than
                // queueing it. Without this the legacy leg loses its first answer.
                inject(&mut s, arp_request_frame(ip))?;
                // An announcement means the responder's last receive timed out, so re-inject the
                // query for the stage we are in: whatever we sent before did not arrive. The stage
                // advances here rather than after the batch, because the answer can be in the same
                // read as the announcement that provoked it.
                if stage == MdnsStage::Announced {
                    stage = MdnsStage::Browse;
                }
                if debug {
                    eprintln!("multicast prober: announcement seen; injecting the stage's query");
                }
                inject(&mut s, mdns_query_frame(stage_port(stage), stage_id(stage)))?;
                continue;
            }

            match stage {
                MdnsStage::Announced => {} // nothing has been asked yet
                MdnsStage::Browse if dg.dst_ip == MDNS_GROUP => {
                    check_browse_answer(&msg, &instance, &hostname, &txt_entries, guest_ip)?;
                    stage = MdnsStage::Legacy;
                    inject(&mut s, mdns_query_frame(MDNS_LEGACY_PORT, MDNS_LEGACY_ID))?;
                }
                MdnsStage::Legacy
                    if dg.dst_ip == MDNS_PROBER_IP && dg.dst_port == MDNS_LEGACY_PORT =>
                {
                    check_legacy_answer(&msg, &instance, &hostname, &txt_entries, guest_ip)?;
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}

/// Which source port and id the query for `stage` carries. The browse comes from 5353 (a full mDNS
/// querier) and the legacy one does not, which is the whole distinction RFC 6762 §6.7 draws.
fn stage_port(stage: MdnsStage) -> u16 {
    match stage {
        MdnsStage::Legacy => MDNS_LEGACY_PORT,
        _ => MDNS_PORT,
    }
}
fn stage_id(stage: MdnsStage) -> u16 {
    match stage {
        MdnsStage::Legacy => MDNS_LEGACY_ID,
        _ => 0,
    }
}

// DNS record types, by their IANA numbers. Spelled here rather than imported from `mdns_proto`
// deliberately: this prober is the independent half of the gate, and a check that shared its
// vocabulary with the code under test could agree with it about a number that was wrong.
const RR_A: u16 = 1;
const RR_PTR: u16 = 12;
const RR_TXT: u16 = 16;
const RR_SRV: u16 = 33;
/// The top bit of a record's class: "cache flush" in a response (RFC 6762 §10.2).
const CACHE_FLUSH: u16 = 0x8000;
/// The cap RFC 6762 §6.7 puts on every TTL in a legacy unicast response.
const LEGACY_TTL_CAP: u32 = 10;

/// One decoded record. `name` and any name inside `rdata` are decoded to lower-case dotted form,
/// following compression pointers, so an assertion never depends on how the sender chose to encode.
struct DnsRecord {
    name: String,
    rrtype: u16,
    class: u16,
    ttl: u32,
    rdata: Vec<u8>,
    /// Where the rdata starts in the whole message, so a name inside it can be decompressed.
    rdata_at: usize,
}

struct DnsMessage {
    id: u16,
    flags: u16,
    questions: Vec<(String, u16)>,
    answers: Vec<DnsRecord>,
    additionals: Vec<DnsRecord>,
    raw: Vec<u8>,
}

/// Decode a DNS name at `at`, following compression pointers. Returns the name in lower-case
/// dotted form and the offset just past the name **in the section being read** (a pointer ends the
/// name in two bytes however far it points).
fn dns_name(msg: &[u8], at: usize) -> Result<(String, usize), String> {
    let mut name = String::new();
    let mut here = at;
    let mut after = None;
    // Bounded: every pointer must go strictly backwards in a well-formed message, and a message is
    // at most 65535 bytes, so this cannot loop forever even on a malicious one.
    for _ in 0..256 {
        let len = *msg
            .get(here)
            .ok_or("a name ran off the end of the message")? as usize;
        match len & 0xc0 {
            0 => {
                here += 1;
                if len == 0 {
                    return Ok((name, after.unwrap_or(here)));
                }
                let end = here + len;
                let label = msg
                    .get(here..end)
                    .ok_or("a label ran off the end of the message")?;
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&String::from_utf8_lossy(label).to_lowercase());
                here = end;
            }
            0xc0 => {
                let lo = *msg.get(here + 1).ok_or("a truncated compression pointer")? as usize;
                let target = ((len & 0x3f) << 8) | lo;
                after.get_or_insert(here + 2);
                if target >= here {
                    return Err("a compression pointer that does not go backwards".to_string());
                }
                here = target;
            }
            _ => return Err("a reserved label length".to_string()),
        }
    }
    Err("a name with too many labels or pointers".to_string())
}

/// Decode a whole DNS message. Deliberately a second implementation, not `mdns_proto`'s: a gate
/// that decoded the guest's bytes with the guest's own parser would pass on any bug the two share.
fn parse_dns(msg: &[u8]) -> Result<DnsMessage, String> {
    if msg.len() < 12 {
        return Err("shorter than a DNS header".to_string());
    }
    let u16at = |i: usize| u16::from_be_bytes([msg[i], msg[i + 1]]);
    let (id, flags) = (u16at(0), u16at(2));
    let (qd, an, ns, ar) = (u16at(4), u16at(6), u16at(8), u16at(10));
    let mut at = 12;
    let mut questions = Vec::new();
    for _ in 0..qd {
        let (name, next) = dns_name(msg, at)?;
        at = next + 4;
        if at > msg.len() {
            return Err("a question ran off the end".to_string());
        }
        questions.push((name, u16::from_be_bytes([msg[next], msg[next + 1]])));
    }
    let mut records = Vec::new();
    for _ in 0..(an as usize + ns as usize + ar as usize) {
        let (name, next) = dns_name(msg, at)?;
        if next + 10 > msg.len() {
            return Err("a record header ran off the end".to_string());
        }
        let rrtype = u16::from_be_bytes([msg[next], msg[next + 1]]);
        let class = u16::from_be_bytes([msg[next + 2], msg[next + 3]]);
        let ttl = u32::from_be_bytes([msg[next + 4], msg[next + 5], msg[next + 6], msg[next + 7]]);
        let rdlen = u16::from_be_bytes([msg[next + 8], msg[next + 9]]) as usize;
        let rdata_at = next + 10;
        let rdata = msg
            .get(rdata_at..rdata_at + rdlen)
            .ok_or("rdata ran off the end")?
            .to_vec();
        at = rdata_at + rdlen;
        records.push(DnsRecord {
            name,
            rrtype,
            class,
            ttl,
            rdata,
            rdata_at,
        });
    }
    let mut it = records.into_iter();
    let answers: Vec<DnsRecord> = it.by_ref().take(an as usize).collect();
    let rest: Vec<DnsRecord> = it.collect();
    let additionals = rest.into_iter().skip(ns as usize).collect();
    Ok(DnsMessage {
        id,
        flags,
        questions,
        answers,
        additionals,
        raw: msg.to_vec(),
    })
}

/// Find one record by name and type across the sections the caller hands over.
fn dns_find<'a>(rs: &[&'a [DnsRecord]], name: &str, rrtype: u16) -> Option<&'a DnsRecord> {
    rs.iter()
        .flat_map(|s| s.iter())
        .find(|r| r.rrtype == rrtype && r.name == name)
}

/// The records both answers must carry, whatever section they are in: the service PTR pointing at
/// the instance, the instance's SRV and TXT, and the host's A. **Every value comes from
/// `user/mdns_responder.conf`**, so this asserts that the machine advertises what it was
/// configured to advertise rather than what somebody typed here twice.
fn check_records(
    msg: &DnsMessage,
    sections: &[&[DnsRecord]],
    instance: &str,
    hostname: &str,
    txt_entries: &[String],
    guest_ip: Option<[u8; 4]>,
) -> Result<(), String> {
    let ptr = dns_find(sections, MDNS_BROWSE, RR_PTR)
        .ok_or_else(|| format!("no PTR for {MDNS_BROWSE} in the response"))?;
    let (target, _) = dns_name(&msg.raw, ptr.rdata_at)?;
    if target != instance {
        return Err(format!("the PTR points at {target}, wanted {instance}"));
    }
    if ptr.class & CACHE_FLUSH != 0 {
        return Err(
            "the shared service PTR carries the cache-flush bit, which tells every cache on the \
             segment to discard other responders' answers for this service type"
                .to_string(),
        );
    }

    let srv = dns_find(sections, instance, RR_SRV)
        .ok_or_else(|| format!("no SRV for {instance} in the response"))?;
    if srv.rdata.len() < 7 {
        return Err("the SRV rdata is too short to hold a port and a target".to_string());
    }
    let port = u16::from_be_bytes([srv.rdata[4], srv.rdata[5]]);
    if port != 0 {
        return Err(format!(
            "the _adisk SRV advertises port {port}; the measured reference advertises 0, because \
             the instance carries flags and not a connectable service"
        ));
    }
    let (srv_target, _) = dns_name(&msg.raw, srv.rdata_at + 6)?;
    if srv_target != hostname {
        return Err(format!("the SRV target is {srv_target}, wanted {hostname}"));
    }

    let txt = dns_find(sections, instance, RR_TXT)
        .ok_or_else(|| format!("no TXT for {instance} in the response"))?;
    let mut got: Vec<String> = Vec::new();
    let mut at = 0;
    while at < txt.rdata.len() {
        let len = txt.rdata[at] as usize;
        let end = at + 1 + len;
        if end > txt.rdata.len() {
            return Err("a TXT string ran past the end of its rdata".to_string());
        }
        got.push(String::from_utf8_lossy(&txt.rdata[at + 1..end]).into_owned());
        at = end;
    }
    if got != txt_entries {
        return Err(format!(
            "the _adisk TXT record says {got:?}, and user/mdns_responder.conf says {txt_entries:?}"
        ));
    }

    if let Some(ip) = guest_ip {
        let a = dns_find(sections, hostname, RR_A)
            .ok_or_else(|| format!("no A record for {hostname} in the response"))?;
        if a.rdata != ip {
            return Err(format!(
                "the A record says {:?} and the announcement said {ip:?}",
                a.rdata
            ));
        }
    }
    Ok(())
}

/// The multicast browse's answer (RFC 6763 §12.1): the PTR answers the question, and the instance's
/// records ride as **additionals**. The unique ones flush; the shared PTR does not.
fn check_browse_answer(
    msg: &DnsMessage,
    instance: &str,
    hostname: &str,
    txt_entries: &[String],
    guest_ip: Option<[u8; 4]>,
) -> Result<(), String> {
    if msg.id != 0 {
        return Err(format!(
            "a multicast response carried transaction id {:#x}; it must be 0",
            msg.id
        ));
    }
    if msg.flags & 0x0400 == 0 {
        return Err("the response is not authoritative (the AA bit is clear)".to_string());
    }
    if !msg.answers.iter().any(|r| r.name == MDNS_BROWSE) {
        return Err("the answer section does not answer the question that was asked".to_string());
    }
    if msg.additionals.is_empty() {
        return Err(
            "a browse answer with no additionals: the instance's SRV, TXT and A must ride along, \
             or a Mac discovers a share it then has to ask about again"
                .to_string(),
        );
    }
    check_records(
        msg,
        &[&msg.answers, &msg.additionals],
        instance,
        hostname,
        txt_entries,
        guest_ip,
    )?;
    for r in msg.additionals.iter() {
        if r.class & CACHE_FLUSH == 0 {
            return Err(format!(
                "the record of type {} is this responder's own and does not set cache-flush",
                r.rrtype
            ));
        }
    }
    Ok(())
}

/// The legacy unicast answer, RFC 6762 §6.7 in full: the id echoed, the question repeated, every
/// record in the **answer** section (a one-shot resolver reads no further), no cache-flush bits,
/// and every TTL capped at 10 so a resolver that cannot hear our updates forgets quickly.
fn check_legacy_answer(
    msg: &DnsMessage,
    instance: &str,
    hostname: &str,
    txt_entries: &[String],
    guest_ip: Option<[u8; 4]>,
) -> Result<(), String> {
    if msg.id != MDNS_LEGACY_ID {
        return Err(format!(
            "the legacy response carries id {:#x}, not the {MDNS_LEGACY_ID:#x} that was asked; a \
             one-shot resolver matches on it and would discard this",
            msg.id
        ));
    }
    if msg.questions.len() != 1 || msg.questions[0].0 != MDNS_BROWSE {
        return Err(format!(
            "the legacy response does not repeat the question it answers (questions: {:?})",
            msg.questions
        ));
    }
    if !msg.additionals.is_empty() {
        return Err(
            "the legacy response puts records in additionals; a one-shot resolver reads the \
             answer section"
                .to_string(),
        );
    }
    check_records(
        msg,
        &[&msg.answers],
        instance,
        hostname,
        txt_entries,
        guest_ip,
    )?;
    for r in msg.answers.iter() {
        if r.class & CACHE_FLUSH != 0 {
            return Err(format!(
                "the legacy response sets cache-flush on the record of type {}; that bit is for \
                 multicast responses only",
                r.rrtype
            ));
        }
        if r.ttl > LEGACY_TTL_CAP {
            return Err(format!(
                "a legacy response's TTL is {} and the cap is {LEGACY_TTL_CAP}",
                r.ttl
            ));
        }
    }
    Ok(())
}

/// A UDP datagram taken off the raw wire.
struct Datagram {
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    dst_port: u16,
    payload: Vec<u8>,
}

/// The UDP datagram inside `frame`, if it is one. `None` for everything else on the hub (ARP,
/// TCP, IPv6), which is most of it.
fn udp_datagram(frame: &[u8]) -> Option<Datagram> {
    let ip = frame.get(14..)?;
    if frame[12..14] != [0x08, 0x00] || ip.first()? >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0xf) as usize) * 4;
    if *ip.get(9)? != 17 {
        return None;
    }
    let mut src_ip = [0u8; 4];
    src_ip.copy_from_slice(ip.get(12..16)?);
    let mut dst_ip = [0u8; 4];
    dst_ip.copy_from_slice(ip.get(16..20)?);
    let udp = ip.get(ihl..)?;
    let dst_port = u16::from_be_bytes([*udp.get(2)?, *udp.get(3)?]);
    let udp_len = u16::from_be_bytes([*udp.get(4)?, *udp.get(5)?]) as usize;
    Some(Datagram {
        src_ip,
        dst_ip,
        dst_port,
        payload: udp.get(8..udp_len)?.to_vec(),
    })
}

/// An ARP reply for [`MDNS_PROBER_IP`], if `frame` is a request asking for it. The guest asks this
/// when it has a unicast datagram for the address we spoof and no MAC for it yet.
fn arp_reply(frame: &[u8]) -> Option<Vec<u8>> {
    let arp = frame.get(14..14 + 28)?;
    if frame[12..14] != [0x08, 0x06] {
        return None;
    }
    // Ethernet over IPv4, opcode 1 (request), asking for the address we hold.
    if arp[0..2] != [0, 1] || arp[2..4] != [0x08, 0x00] || arp[4] != 6 || arp[5] != 4 {
        return None;
    }
    if arp[6..8] != [0, 1] || arp[24..28] != MDNS_PROBER_IP {
        return None;
    }
    let sender_mac: [u8; 6] = arp[8..14].try_into().ok()?;
    let sender_ip: [u8; 4] = arp[14..18].try_into().ok()?;
    let mut f = Vec::with_capacity(42);
    f.extend_from_slice(&sender_mac);
    f.extend_from_slice(&MDNS_PROBER_MAC);
    f.extend_from_slice(&[0x08, 0x06]);
    f.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 2]); // reply
    f.extend_from_slice(&MDNS_PROBER_MAC);
    f.extend_from_slice(&MDNS_PROBER_IP);
    f.extend_from_slice(&sender_mac);
    f.extend_from_slice(&sender_ip);
    Some(f)
}

/// An ARP request for the guest's own address, from the address this prober spoofs.
///
/// **This is what makes the unicast leg work**, and it is not politeness. smoltcp fills its
/// neighbour cache only from an ARP packet whose *target* is an address it holds
/// (`process_arp` in smoltcp 0.13.1 returns early otherwise), so a gratuitous announcement of our
/// own address would be discarded, the guest would have to resolve us when it answered the legacy
/// query, and smoltcp drops the datagram that triggers a resolution rather than queueing it. Asking
/// the guest for its address fills the cache in the same breath.
fn arp_request_frame(guest_ip: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::with_capacity(42);
    f.extend_from_slice(&[0xff; 6]);
    f.extend_from_slice(&MDNS_PROBER_MAC);
    f.extend_from_slice(&[0x08, 0x06]);
    f.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 1]); // request
    f.extend_from_slice(&MDNS_PROBER_MAC);
    f.extend_from_slice(&MDNS_PROBER_IP);
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&guest_ip);
    f
}

/// The frame this prober injects: ethernet to the group's multicast MAC, IPv4 from the spoofed
/// source to the group with TTL 255 (RFC 6762 §11), UDP from `src_port` to 5353 carrying a **real
/// DNS query**, a PTR question for [`MDNS_BROWSE`] with transaction id `id`. Checksums real, so
/// nothing in the guest's stack has a reason to drop it.
fn mdns_query_frame(src_port: u16, id: u16) -> Vec<u8> {
    // The query, encoded by hand: header, then the name as length-prefixed labels, then QTYPE PTR
    // and QCLASS IN. No compression, nothing optional.
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&[0, 0]); // flags: a query, opcode 0
    query.extend_from_slice(&[0, 1]); // one question
    query.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // no answers, authorities or additionals
    for label in MDNS_BROWSE.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0);
    query.extend_from_slice(&RR_PTR.to_be_bytes());
    query.extend_from_slice(&[0, 1]); // class IN, QU bit clear

    let udp_len = 8 + query.len();
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
        (src_port >> 8) as u8,
        src_port as u8,
        (MDNS_PORT >> 8) as u8,
        MDNS_PORT as u8,
        (udp_len >> 8) as u8,
        udp_len as u8,
        0,
        0,
    ];
    udp.extend_from_slice(&query);
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
/// contents), close, **the write leg** ([`smb_write_leg`]), and logoff. That first CREATE also
/// carries the **`AAPL` create context** and its answer is checked ([`smb_apple_leg`]), because
/// that is where a Mac puts it: milestone 55's Time Machine negotiation rides the first open of a
/// tree connect rather than a message of its own. Asserting the *seeded*
/// file is what makes this the RedoxFS-to-TCP gate rather than a wire check against the adapter's
/// baked-in fixture. Same retry discipline, including the never-abandon-a-slow-connection rule
/// the echo prober documents: a connected stream is held until it answers, dies, or the run ends.
///
/// **Bytes cross in both directions and neither is checked by whoever sent them.** The read leg
/// asserts bytes a different in-guest process put on the filesystem; the write leg puts bytes
/// there and a different in-guest process reads them back through the FS server after this
/// prober has finished (the kernel test's `assert_smb_write_landed`). A prober that read back its
/// own write would prove only that the adapter remembers, which is a thing an adapter can do
/// without a filesystem underneath it.
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
                     forwarded into the guest and served end to end: negotiate, an **NTLMv2 \
                     session** (an anonymous login refused, a one-bit forgery refused, and a real \
                     proof computed on this host over the challenge the guest issued accepted, \
                     unflagged as guest), tree connect, a read of the FS-server-seeded file whose \
                     bytes matched (RedoxFS to TCP), a create-write-truncate-close of a second \
                     file (TCP to RedoxFS), and a subdirectory made over the wire with a file \
                     written inside it and listed back by its leaf name. The volume reported real \
                     numbers rather than the nominal constant, and the first CREATE's AAPL create \
                     context came back claiming the Time Machine volume capability (milestone 55). \
                     The guest's own verifier reads both files back through the FS server, \
                     descending into the directory to prove a directory is what landed. The \
                     password exists only on this host: the guest holds it as an NTOWFv2 in a \
                     sealed store the SMB adapter cannot read, and the kernel test looks at the \
                     page between them afterwards. The second session is the proof the adapter \
                     re-arms."
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

        match smb_session(&mut s, &stop, done) {
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

/// **The session-setup leg, and the reason milestone 54's last item is gated rather than asserted**
/// (identity, 2026-08-17).
///
/// Three AUTHENTICATE messages down one connection, in the order that makes each one mean something:
///
/// 1. **Anonymous**, every field empty. This is what `mount_smbfs -N` sends and what this prober
///    itself sent until identity landed, so it is the exact request the guest used to admit. It must
///    now get `STATUS_LOGON_FAILURE`, and a `TREE_CONNECT` after it must fail, because a refusal that
///    only changes a status word is not a gate.
/// 2. **A real NTLMv2 proof with one bit flipped.** The proof is a MAC, so this is what every forgery
///    looks like; it separates "the guest refuses what it cannot verify" from "the guest refuses
///    everything".
/// 3. **The real thing.** A proof this process computes on the host with `ntlm`, over the challenge
///    the *guest* chose, under a key derived from `cred_proto::fixture`'s password. It must succeed
///    and the session must **not** come back flagged guest.
///
/// **What the arrangement proves that a unit test could not.** The password exists only here, on the
/// host. Inside the guest it exists only as an `NTOWFv2` inside a sealed credential store in a
/// process that holds no network; the SMB adapter that answers this exchange holds one endpoint to
/// that store and no key, and cannot compute any of the three proofs above. So a real proof crossing
/// a real TCP connection and being verified is four processes' worth of the claim at once, and the
/// kernel test then looks at the frame the adapter shares with the store and finds nothing in it.
///
/// The ids of the two refused attempts are deliberately out of sequence and high: this server echoes
/// message ids and validates none of them (see the note at the end of `smb_session`), so a refused
/// attempt does not consume one of the numbers the later legs use.
fn smb_authenticate_leg(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
    sid: u64,
    challenge: &[u8; 8],
) -> Result<(), String> {
    use smb_proto::{H_STATUS, HDR_LEN, client, r16, r32};

    let status = |resp: &[u8]| r32(resp, H_STATUS);
    // The blob is entirely the client's. Shaped like a real one (response version, timestamp,
    // client challenge) rather than transcribed, because the server MACs it without reading it.
    let blob: Vec<u8> = {
        let mut b = vec![0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        b.extend_from_slice(&[0u8; 8]); // Time
        b.extend_from_slice(&[0xaa; 8]); // ClientChallenge
        b.extend_from_slice(&[0u8; 4]);
        b.extend_from_slice(&[0u8; 4]); // MsvAvEOL
        b
    };

    // 1. Anonymous, which is what this prober used to send and what a guest share admitted.
    smb_send(s, &client::session_setup_authenticate(90, sid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_LOGON_FAILURE {
        return Err(format!(
            "an anonymous AUTHENTICATE got status {:#x}, wanted LOGON_FAILURE ({:#x}). If it \
             SUCCEEDED, the guest is serving a guest share: is the boot wiring \
             SMB_SHARE_FS_AUTHENTICATED, and did it get a credential endpoint?",
            status(&resp),
            smb_proto::STATUS_LOGON_FAILURE,
        ));
    }
    // A refusal must leave nothing usable behind, which is the half a status word cannot show.
    smb_send(s, &client::tree_connect(91, sid, b"\\\\10.0.2.15\\share"))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_USER_SESSION_DELETED {
        return Err(format!(
            "a TREE_CONNECT after a refused login got status {:#x}, wanted USER_SESSION_DELETED: \
             the guest refused the login and then served the share anyway",
            status(&resp),
        ));
    }

    // The key a holder of the password derives. [MS-NLMP] 3.3.2, and the account is the one the
    // guest's store was provisioned with; `ntlm`'s tests pin every value against the specification.
    let key = ntlm::ntowfv2(
        cred_proto::fixture::SMB_PASSWORD,
        cred_proto::fixture::SMB_USER,
        cred_proto::fixture::SMB_DOMAIN,
    )
    .map_err(|e| format!("the fixture account does not derive a key: {e:?}"))?;
    let good = ntlm::proof(&key, challenge, &blob);

    // 2. One bit off.
    let mut bad = good;
    bad[0] ^= 0x01;
    smb_send(
        s,
        &client::session_setup_authenticate_ntlmv2(
            92,
            sid,
            cred_proto::fixture::SMB_USER,
            cred_proto::fixture::SMB_DOMAIN,
            &bad,
            &blob,
        ),
    )?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_LOGON_FAILURE {
        return Err(format!(
            "an NTLMv2 proof with one bit flipped got status {:#x}, wanted LOGON_FAILURE: the \
             guest accepted a forgery",
            status(&resp),
        ));
    }

    // 3. The real thing, on the same connection and against the same challenge, which is what
    //    macOS does after prompting for a password.
    smb_send(
        s,
        &client::session_setup_authenticate_ntlmv2(
            3,
            sid,
            cred_proto::fixture::SMB_USER,
            cred_proto::fixture::SMB_DOMAIN,
            &good,
            &blob,
        ),
    )?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "session setup (auth leg): status {:#x} for a proof computed over the challenge the \
             guest itself issued. A LOGON_FAILURE here means the guest's credential store does not \
             hold the key for {:?} (is the credentialer wired and provisioned before the SMB \
             spawn?), or the challenge did not survive the two refusals above",
            status(&resp),
            String::from_utf8_lossy(cred_proto::fixture::SMB_RESOURCE),
        ));
    }
    // SessionFlags, at the start of the SESSION_SETUP response body. Bit 0 is IS_GUEST, and it is
    // the only thing on the wire that tells a named session from an anonymous one; macOS shows it to
    // the user. A server that verified a proof and then flagged the session guest would have made
    // the whole exchange decorative.
    let flags = r16(&resp, HDR_LEN + 2);
    if flags & 0x0001 != 0 {
        return Err(format!(
            "the guest verified an NTLMv2 proof and then flagged the session as a GUEST \
             (SessionFlags {flags:#06x})",
        ));
    }
    Ok(())
}

/// One full mount-shaped exchange. Any protocol surprise is an `Err` naming the step, so a
/// failing leg says which part of the mount broke rather than "it did not work".
fn smb_session(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
    round: usize,
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
    let Some(challenge) = client::session_setup_challenge(&resp) else {
        return Err(String::from(
            "session setup carried no NTLMSSP CHALLENGE with 8 challenge bytes in it",
        ));
    };

    smb_authenticate_leg(s, stop, sid, &challenge)?;

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
    // **The Apple leg** (milestone 55), and it rides this create rather than one of its own
    // because that is where a Mac puts it: macOS attaches the AAPL context to the *first* CREATE
    // after a tree connect, whatever that CREATE happens to be opening. Everything asserted below
    // about the seeded file is asserted about the same response.
    let aapl_bitmap = smb_proto::apple::BIT_SERVER_CAPS
        | smb_proto::apple::BIT_VOLUME_CAPS
        | smb_proto::apple::BIT_MODEL_INFO;
    smb_send(s, &client::create_aapl(5, sid, tid, name, aapl_bitmap, 0))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "create {}: status {:#x} (a NOT_FOUND here usually means the guest fell back to its \
             fixture share: was the RedoxFS disk attached, and did the seed role run?)",
            String::from_utf8_lossy(name),
            status(&resp)
        ));
    }
    smb_apple_leg(&resp, aapl_bitmap)?;
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

    smb_write_leg(s, stop, sid, tid)?;

    // **The throughput leg** (milestone 55), off unless asked for: see [`smb_throughput_leg`] on
    // why a measurement is not a gate, and `bench/smb-throughput.sh` for the sweep that reads it.
    // First round only, because it is the round with a quiet machine behind it and because each
    // run of it writes a megabyte into a 16 MiB copy-on-write image.
    if round == 0 && std::env::var_os("NIFE_SMB_THROUGHPUT").is_some() {
        smb_throughput_leg(s, stop, sid, tid)?;
    }

    // Well past the write and subdirectory legs' ids. They are echoed and never validated (the
    // crate BUGS), so a collision would not fail; keeping them distinct is what makes a packet
    // capture readable.
    smb_send(s, &client::logoff(40, sid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("logoff: status {:#x}", status(&resp)));
    }
    Ok(())
}

/// **The Apple leg** (milestone 55): check the `AAPL` create context the guest hung off its CREATE
/// response, which is the exchange macOS negotiates Time Machine through.
///
/// What this proves that a host test cannot: the context survived the whole adapter. The state
/// machine's answer had to be chunked out through the socket contract, reassembled by a real TCP
/// stack, and arrive with its `CreateContextsOffset` still measured from the right place, on a
/// response that also had to carry a real file's size. A host test drives the state machine
/// directly and would not notice a framing bug between it and the wire.
///
/// What it cannot prove is the only thing that finally matters, which is that **macOS** accepts
/// these bytes. This prober is a client this tree wrote, so it agrees with the server about
/// everything by construction; see `crates/smb_proto/src/apple.rs`'s BUGS.
fn smb_apple_leg(resp: &[u8], bitmap: u64) -> Result<(), String> {
    use smb_proto::{apple, client, r64};

    let Some(reply) = client::create_context_data(resp, apple::TAG) else {
        return Err(String::from(
            "the CREATE carried an AAPL create context and the answer carried none: macOS will \
             mount this share and never offer it as a Time Machine destination",
        ));
    };
    if r64(reply, 8) != bitmap {
        return Err(format!(
            "the AAPL reply bitmap is {:#x}, the request asked {bitmap:#x}",
            r64(reply, 8)
        ));
    }
    // Each claim named on its own, so a bit that goes missing says which one it was.
    let server_caps = r64(reply, 16);
    if server_caps & apple::SERVER_UNIX_BASED == 0 {
        return Err(format!(
            "the AAPL server capabilities {server_caps:#x} do not claim UNIX_BASED"
        ));
    }
    if server_caps & apple::SERVER_READ_DIR_ATTR != 0 {
        return Err(String::from(
            "the AAPL server capabilities claim READ_DIR_ATTR, which promises Finder info this \
             server has none of",
        ));
    }
    let volume_caps = r64(reply, 24);
    if volume_caps & apple::VOLUME_FULL_SYNC == 0 {
        return Err(format!(
            "the AAPL volume capabilities {volume_caps:#x} do not claim FULL_SYNC, which is the \
             Time Machine bit (fruit:time machine = yes on the reference)"
        ));
    }
    let mut want = [0u8; 64];
    let n = smb_proto::ascii_to_utf16le(apple::MODEL, &mut want);
    if apple::model_utf16le(reply) != Some(&want[..n]) {
        return Err(format!(
            "the AAPL model string is not {}",
            String::from_utf8_lossy(apple::MODEL)
        ));
    }
    Ok(())
}

/// **How many bytes the throughput leg moves in each direction.** 1 MiB, the same figure
/// `fs_proto::fixture::throughput::TOTAL` holds for the in-guest benchmark, so a MiB/s from this
/// leg and a MiB/s from that phase are measuring the same amount of work through two different
/// depths of the stack. Bounded by the fixture image (16 MiB) and by RedoxFS being copy-on-write,
/// exactly as that constant's doc explains.
const SMB_THROUGHPUT_BYTES: usize = 1024 * 1024;

/// **The throughput leg** (milestone 55): move [`SMB_THROUGHPUT_BYTES`] through a mounted share, in
/// each direction, and print what it cost.
///
/// # Why this is measured here and nowhere else
///
/// Milestone 138 step 3 grew the file contract's transfer from 4 KiB to 64 KiB and measured 8.02x
/// on a sequential write. **That number was measured against `fs_proto`**, by a client that speaks
/// the contract directly. The thing a customer runs does not: it is a Mac, and everything it asks
/// for arrives through TCP, the socket contract, a reassembly buffer and an SMB2 state machine
/// before any of it reaches an `fs::WRITE`. So the only honest way to say whether the speedup
/// reached the customer path is to measure it *on* the customer path, which is what this is: a host
/// process, over a real forwarded TCP connection, timing the same bytes a backup would send.
///
/// Each direction is issued in `smb_proto::MAX_TRANSACT`-sized messages, which is the largest the
/// server negotiates (`MaxReadSize` and `MaxWriteSize`, both 64 KiB) and therefore the largest a
/// real client will use. That is the ceiling this measurement is bounded by however large the file
/// contract's transfer becomes, and it is the number to quote beside any speedup claimed here.
///
/// # Why it is not part of the gate
///
/// A gate says pass or fail and a timing is neither: the host is a laptop, the guest is an emulator
/// with no cycle-accurate clock behind this path, and a threshold would fail on a busy afternoon
/// and pass on a slow regression. So it runs only when `NIFE_SMB_THROUGHPUT` is set, and what makes
/// it a record rather than a number somebody once saw is `bench/smb-throughput.sh`, which sweeps
/// the contract's transfer size around it and prints a before against an after.
///
/// The correctness of what it moved is still checked, because a fast wrong answer is the failure
/// mode a throughput test invites: the read-back is compared byte for byte against what was sent.
/// It is the one leg that reads back its own write, which [`smb_write_leg`] refuses to do for good
/// reasons; those reasons are about proving the filesystem is real, and that is already proven by
/// the legs above. Here the read-back is guarding the measurement, not the store.
fn smb_throughput_leg(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
    sid: u64,
    tid: u32,
) -> Result<(), String> {
    use smb_proto::{H_STATUS, client, r32};
    let status = |resp: &[u8]| r32(resp, H_STATUS);

    let unit = smb_proto::MAX_TRANSACT as usize;
    // A cheap non-constant pattern, so a server that answered a read with zeroes or with a
    // previous chunk would be caught by the compare below rather than flattered by it.
    let payload: Vec<u8> = (0..SMB_THROUGHPUT_BYTES)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761) as u8)
        .collect();

    let name = b"throughput.bin";
    let mut msg_id = 200u64;

    // FILE_OVERWRITE_IF = 5, as the write leg uses: create, or replace what an earlier run left.
    smb_send(s, &client::create_disposition(msg_id, sid, tid, name, 5))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "throughput: create {}: status {:#x}",
            String::from_utf8_lossy(name),
            status(&resp)
        ));
    }
    let fid = client::create_file_id(&resp);
    msg_id += 1;

    // The builders return a fixed-capacity `Msg` because `smb_proto` has no allocator, and a 64 KiB
    // message does not fit one. `write_at_into` is the same builder writing into a buffer the
    // caller sized, which is this one.
    let mut req = vec![0u8; client::WRITE_OVERHEAD + unit];

    let started = std::time::Instant::now();
    let mut off = 0usize;
    while off < payload.len() {
        let n = unit.min(payload.len() - off);
        let len = client::write_at_into(
            &mut req,
            msg_id,
            sid,
            tid,
            &fid,
            off as u64,
            &payload[off..off + n],
        );
        smb_send(s, &req[..len])?;
        let resp = smb_recv(s, stop)?;
        if status(&resp) != smb_proto::STATUS_SUCCESS {
            return Err(format!(
                "throughput: write of {n} bytes at {off}: status {:#x} (STATUS_DISK_FULL here                  means the fixture image cannot hold {SMB_THROUGHPUT_BYTES} more bytes; RedoxFS is                  copy-on-write, so an overwrite costs space until the old blocks are freed)",
                status(&resp)
            ));
        }
        let took = client::write_count(&resp) as usize;
        if took != n {
            return Err(format!(
                "throughput: write at {off} took {took} of {n} bytes"
            ));
        }
        off += n;
        msg_id += 1;
    }
    let write_elapsed = started.elapsed();

    // FLUSH inside the timing would measure a different thing (durability, which the write leg
    // already gates); outside it, this is what makes the read below come from the store rather
    // than from anything the server is still holding.
    smb_send(s, &client::flush(msg_id, sid, tid, &fid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("throughput: flush: status {:#x}", status(&resp)));
    }
    msg_id += 1;

    let started = std::time::Instant::now();
    let mut back = Vec::with_capacity(payload.len());
    while back.len() < payload.len() {
        let n = unit.min(payload.len() - back.len());
        smb_send(
            s,
            &client::read(msg_id, sid, tid, &fid, back.len() as u64, n as u32),
        )?;
        let resp = smb_recv(s, stop)?;
        if status(&resp) != smb_proto::STATUS_SUCCESS {
            return Err(format!(
                "throughput: read of {n} bytes at {}: status {:#x}",
                back.len(),
                status(&resp)
            ));
        }
        let data = client::read_data(&resp);
        if data.is_empty() {
            return Err(format!(
                "throughput: read at {} came back empty with {} bytes still to go",
                back.len(),
                payload.len() - back.len()
            ));
        }
        back.extend_from_slice(data);
        msg_id += 1;
    }
    let read_elapsed = started.elapsed();

    if back != payload {
        let at = back
            .iter()
            .zip(payload.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(payload.len().min(back.len()));
        return Err(format!(
            "throughput: the file came back wrong, first difference at byte {at} of {}",
            payload.len()
        ));
    }

    smb_send(s, &client::close(msg_id, sid, tid, &fid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("throughput: close: status {:#x}", status(&resp)));
    }

    // One line per direction, tagged so bench/smb-throughput.sh can grep them out of a test run
    // whose stderr carries a great deal else. `unit` is on the line because it is the ceiling the
    // numbers are bounded by, and a reader who has only the output should not have to look it up.
    let mib = SMB_THROUGHPUT_BYTES as f64 / (1024.0 * 1024.0);
    eprintln!(
        "smb-throughput: write {:.2} MiB/s ({} bytes in {:.1} ms, {} per SMB2 message)",
        mib / write_elapsed.as_secs_f64(),
        SMB_THROUGHPUT_BYTES,
        write_elapsed.as_secs_f64() * 1000.0,
        unit
    );
    eprintln!(
        "smb-throughput: read {:.2} MiB/s ({} bytes in {:.1} ms, {} per SMB2 message)",
        mib / read_elapsed.as_secs_f64(),
        SMB_THROUGHPUT_BYTES,
        read_elapsed.as_secs_f64() * 1000.0,
        unit
    );
    Ok(())
}

/// **The other direction** (milestone 54's write path): put a file on the guest's filesystem over
/// SMB2 and leave it there for the guest's own verifier to find.
///
/// Every step is one a real client takes when it saves a file, in the order it takes them: open
/// with `FILE_OVERWRITE_IF` (which both creates and replaces, so the second round is not a
/// collision), write in two chunks at two offsets, write a tail, shorten with `SET_INFO` so the
/// truncate leg is exercised on something that had bytes past the cut, stamp the timestamps a
/// copy ends with, and close.
///
/// **This function deliberately does not read the file back.** A client believing its own write
/// is exactly what the gate must not accept, so the read-back is a different process on the other
/// side of the machine: `fs_test_client`'s verify role, spawned by the kernel test once the
/// adapter has stopped serving, reading through the FS server with no network in its cspace.
fn smb_write_leg(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
    sid: u64,
    tid: u32,
) -> Result<(), String> {
    use smb_proto::{H_STATUS, client, r32};
    let status = |resp: &[u8]| r32(resp, H_STATUS);

    let name = fs_proto::fixture::SMB_WROTE_NAME.as_bytes();
    let want = fs_proto::fixture::SMB_WROTE;
    // A tail past the payload, so the truncate below has something to remove. Without it a
    // successful truncate and a truncate that did nothing look identical.
    let tail = b"...and this tail must not survive the truncate...";

    // FILE_OVERWRITE_IF = 5: create it, or replace what a previous round left.
    smb_send(s, &client::create_disposition(10, sid, tid, name, 5))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "create {} for writing: status {:#x} (ACCESS_DENIED means the guest wired the share \
             read-only; NOT_FOUND means the fixture share, so no RedoxFS disk)",
            String::from_utf8_lossy(name),
            status(&resp)
        ));
    }
    let fid = client::create_file_id(&resp);
    if client::create_end_of_file(&resp) != 0 {
        return Err(format!(
            "the overwrite-if create left {} bytes; it must truncate what it replaced",
            client::create_end_of_file(&resp)
        ));
    }

    // Two chunks at two offsets: an offset bug in the write path shows up as a scrambled file
    // rather than as a missing one, and only a multi-chunk write can catch it.
    let cut = want.len() / 2;
    let mut msg_id = 11u64;
    for (offset, chunk) in [
        (0u64, &want[..cut]),
        (cut as u64, &want[cut..]),
        (want.len() as u64, &tail[..]),
    ] {
        smb_send(s, &client::write_at(msg_id, sid, tid, &fid, offset, chunk))?;
        let resp = smb_recv(s, stop)?;
        if status(&resp) != smb_proto::STATUS_SUCCESS {
            return Err(format!(
                "write of {} bytes at offset {offset}: status {:#x}",
                chunk.len(),
                status(&resp)
            ));
        }
        let count = client::write_count(&resp);
        if count as usize != chunk.len() {
            return Err(format!(
                "write at offset {offset} took {count} of {} bytes",
                chunk.len()
            ));
        }
        msg_id += 1;
    }

    // **FLUSH** (milestone 55's durability half), where a backup client puts it: after the write
    // and before it believes the write. Until this milestone the guest answered it with a polite
    // yes; now it is a `fs_proto::fs::SYNC` and a `VIRTIO_BLK_T_FLUSH` the device completes, and
    // this is the only leg that drives that chain from outside the guest.
    //
    // **A success here is necessary and nowhere near sufficient**, which is why the real assertion
    // is somewhere else. This side cannot see whether anything reached the device; all it can say
    // is that the server did not refuse. The in-guest witness
    // (`fs_proto::fixture::durability`, read by the kernel's `assert_smb_write_landed`) is what
    // says the flush actually happened, and it says so by finding the block server's flush count
    // already advanced before it ran. That count can only have been moved by this command.
    smb_send(s, &client::flush(msg_id, sid, tid, &fid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "FLUSH after writing: status {:#x}. UNEXPECTED_IO_ERROR here is the durability chain              refusing rather than lying, which is the correct failure: it means either the device              offers no VIRTIO_BLK_F_FLUSH or it rejected the flush. See fs_proto::fs::SYNC",
            status(&resp)
        ));
    }
    msg_id += 1;

    // SET_INFO / FileEndOfFileInformation: cut the tail off.
    smb_send(
        s,
        &client::set_end_of_file(msg_id, sid, tid, &fid, want.len() as u64),
    )?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("set end of file: status {:#x}", status(&resp)));
    }
    msg_id += 1;

    // SET_INFO / FileBasicInformation: the timestamp stamp every copy ends with. The guest
    // accepts and discards it (smb_proto's BUGS); what is proven here is that it does not refuse,
    // because a client that meets a refusal here reports the whole copy as failed.
    smb_send(s, &client::set_basic(msg_id, sid, tid, &fid, 0x80))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "set basic information: status {:#x}",
            status(&resp)
        ));
    }
    msg_id += 1;

    smb_send(s, &client::close(msg_id, sid, tid, &fid))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!("close after writing: status {:#x}", status(&resp)));
    }
    msg_id += 1;

    smb_subdirectory_leg(s, stop, sid, tid, msg_id)?;
    Ok(())
}

/// **The subdirectory leg** (milestone 54's third act): make a directory over the wire, put a file
/// in it, list it back, and leave both for the guest's own verifier.
///
/// This is the shape milestone 55 needs and nothing else is: a sparsebundle is a directory of band
/// files, so "can this share hold a directory with a file in it" is the question, and the answer a
/// flat share gives is a file whose *name* contains a separator. The guest-side verifier is what
/// tells those apart (`fs_test_client`'s `smb_verify`, verdict `DIR_IS_A_FILE`), so this side does
/// not read the file back for the reason [`smb_write_leg`] gives.
///
/// It **does** read the listing back, and that is not the same thing: a listing is a fact about the
/// server's own view, and getting it wrong is a bug this side can see (a leaf name shown as a full
/// path, or a grandchild shown in the parent) that the filesystem cannot.
fn smb_subdirectory_leg(
    s: &mut std::net::TcpStream,
    stop: &std::sync::atomic::AtomicBool,
    sid: u64,
    tid: u32,
    first_msg_id: u64,
) -> Result<(), String> {
    use smb_proto::{H_STATUS, client, r32};
    let status = |resp: &[u8]| r32(resp, H_STATUS);

    let dir = fs_proto::fixture::SMB_DIR_NAME.as_bytes();
    let leaf = fs_proto::fixture::SMB_NESTED_NAME.as_bytes();
    let body = fs_proto::fixture::SMB_NESTED;
    // The path a client actually sends: the components joined by SMB's own separator. One string,
    // built from the two constants the guest checks separately, so the two sides cannot drift.
    let mut nested = dir.to_vec();
    nested.push(b'\\');
    nested.extend_from_slice(leaf);

    // FILE_OPEN_IF (3) plus FILE_DIRECTORY_FILE (0x1): make it, or take the one a previous round
    // left. FILE_CREATE would collide on the second round and on the HVF leg's reboot.
    let mut msg_id = first_msg_id;
    smb_send(
        s,
        &client::create_full(msg_id, sid, tid, dir, 3, 0x0000_0001),
    )?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "create directory {}: status {:#x} (a PATH_NOT_FOUND here means the share is still \
             flat; ACCESS_DENIED means it was wired read-only)",
            String::from_utf8_lossy(dir),
            status(&resp)
        ));
    }
    let dir_fid = client::create_file_id(&resp);
    msg_id += 1;

    // The file inside it, by its full path. FILE_OVERWRITE_IF (5) so a second round replaces.
    smb_send(s, &client::create_disposition(msg_id, sid, tid, &nested, 5))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "create {}: status {:#x} (the directory was made, so this is the path walk failing on \
             the last component)",
            String::from_utf8_lossy(&nested),
            status(&resp)
        ));
    }
    let fid = client::create_file_id(&resp);
    msg_id += 1;

    smb_send(s, &client::write_at(msg_id, sid, tid, &fid, 0, body))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "write into the subdirectory: status {:#x}",
            status(&resp)
        ));
    }
    if client::write_count(&resp) as usize != body.len() {
        return Err(format!(
            "the nested write took {} of {} bytes",
            client::write_count(&resp),
            body.len()
        ));
    }
    msg_id += 1;

    smb_send(s, &client::close(msg_id, sid, tid, &fid))?;
    smb_recv(s, stop)?;
    msg_id += 1;

    // The listing of the directory just filled: `.`, `..`, then the leaf by its own name. A share
    // that returned the whole path here, or that showed the file in the share root instead, is a
    // bug this side can see and the filesystem cannot.
    smb_send(
        s,
        &client::query_directory(msg_id, sid, tid, &dir_fid, 37, b"*"),
    )?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "listing the subdirectory: status {:#x}",
            status(&resp)
        ));
    }
    let names: Vec<Vec<u8>> = client::dir_entries(&resp, 37)
        .map(|n| n.as_bytes().to_vec())
        .collect();
    if !names.iter().any(|n| n == leaf) {
        return Err(format!(
            "the subdirectory's listing is {:?}; it must hold {} by its leaf name",
            names
                .iter()
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .collect::<Vec<_>>(),
            String::from_utf8_lossy(leaf),
        ));
    }
    msg_id += 1;

    // The volume, which is the statfs half arriving on the wire. What is asserted is that it is
    // *not* the constant it used to be: `NOMINAL_VOLUME_BYTES` is what a share with no volume
    // reports, and the fs-backed share has one.
    smb_send(s, &client::query_info(msg_id, sid, tid, &dir_fid, 2, 7))?;
    let resp = smb_recv(s, stop)?;
    if status(&resp) != smb_proto::STATUS_SUCCESS {
        return Err(format!(
            "FileFsFullSizeInformation: status {:#x}",
            status(&resp)
        ));
    }
    let at = smb_proto::HDR_LEN + 8;
    let (total, free, unit) = (
        smb_proto::r64(&resp, at),
        smb_proto::r64(&resp, at + 8),
        smb_proto::r32(&resp, at + 28) as u64,
    );
    let nominal = smb_proto::share::NOMINAL_VOLUME_BYTES / 4096;
    if total == 0 || unit == 0 {
        return Err(format!(
            "the volume reports {total} units of {unit} bytes, which no client can size work \
             against"
        ));
    }
    if total == nominal && free == nominal {
        return Err(String::from(
            "the volume is still the nominal constant (total == free == the fallback), so \
             fs_proto's STATFS is not reaching the wire. Time Machine sizes a sparsebundle \
             against these fields; see notes/smb.md.",
        ));
    }
    if free > total {
        return Err(format!("the volume reports {free} free of {total} total"));
    }
    msg_id += 1;

    smb_send(s, &client::close(msg_id, sid, tid, &dir_fid))?;
    smb_recv(s, stop)?;
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
            "mdns_responder",
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
            // `ps` (milestone 126): the process listing. Both archives, because the claim it
            // makes is about the capability model rather than about a machine.
            "--bin",
            "ps",
            // `pgrep` (milestone 126): the same listing, filtered. Built beside `ps` because the
            // demonstration is the pair: both hold one supervision endpoint with `ENUMERATE`, and
            // there is no `pkill` to build alongside them.
            "--bin",
            "pgrep",
            "--bin",
            "timetable",
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
        // The mDNS responder (milestone 55): the discovery half of the Time Machine target.
        // Portable, so both archives carry it and both ISAs answer the same injected query.
        ("mdns_responder", "mdns_responder"),
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
        // The process listing (milestone 126). Both archives: "a program cannot enumerate the
        // machine" is a claim about this system, not about an instruction set.
        ("ps", "ps"),
        // The filter over that listing (milestone 126). Same reason, and one more: "naming a member
        // confers nothing over it" is a property of the rights model and holds on both.
        ("pgrep", "pgrep"),
        // The scheduler (milestone 129). Both archives: "a scheduled entry can do exactly what it
        // was granted" is a claim about the capability model, and one that held on one instruction
        // set would not be one.
        ("timetable", "timetable"),
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
    // **One table, one loop**, the shape `initrd_riscv` has always had (milestone 130). This
    // function used to do the same job three ways at once: nineteen hand-rolled `let` bindings of
    // seven identical lines each, then a loop over a name array doing exactly the same thing, then
    // a hand-written vector re-listing the nineteen by the same string literals. Adding a program
    // meant editing four places that all said its name, and the two that were prose rather than
    // data were the two that drifted.
    //
    // `(archive_name, bin_name)` because the two differ exactly once: the kernel loads the entry
    // called **`init`**, and on aarch64 that is the `hello` binary, which init then re-enters at
    // its remaining roles (19f). Every other row is a name repeated, and that is fine: the pair is
    // what lets the one exception be data instead of a special case in the loop.
    //
    // Order is preserved from the hand-written vector it replaces. It is not load-bearing (init
    // looks entries up by name) but the measurement table is computed over this sequence, so
    // reordering would churn the manifest for nothing.
    let entries: &[(&str, &str)] = &[
        ("init", "hello"),
        ("worker", "worker"),
        ("console", "console"),
        ("input", "input"),
        ("swish", "swish"),
        // The line discipline between the console and the shell (milestone 28).
        ("line_editor", "line_editor"),
        // The terminal's sink adapter (milestone 50), so a declared second stream has somewhere to
        // go that is not the shell's own output slot.
        ("terminal_sink_caretaker", "terminal_sink_caretaker"),
        // The compute workload (19e) and the EL0 microbenchmark program.
        ("coremark", "coremark"),
        ("os_primitives_benchmarker", "os_primitives_benchmarker"),
        // Proves the user_rt heap (milestone 27).
        ("allocator_exerciser", "allocator_exerciser"),
        ("net_stack", "net_stack"),
        ("smb_server", "smb_server"),
        // The mDNS responder (milestone 55): the discovery half of the Time Machine target.
        ("mdns_responder", "mdns_responder"),
        ("budgeter", "budgeter"),
        ("fs_test_client", "fs_test_client"),
        ("fs_file_caretaker", "fs_file_caretaker"),
        ("fs_subtree_caretaker", "fs_subtree_caretaker"),
        ("heeder", "heeder"),
        ("spinner", "spinner"),
        // The authority-shrinking supervision tree (milestone 22 phase B.2): an init that hands its
        // construction authority to a spawner and its restart policy to a supervisor, then drops
        // the budget.
        ("root_supervisor", "root_supervisor"),
        ("spawner", "spawner"),
        ("sub_server_supervisor", "sub_server_supervisor"),
        ("flaky", "flaky"),
        // The interactive boot's undertaker (milestone 22, the interactive increment): one endpoint
        // capability and nothing else, so a job's region comes back to init's budget.
        ("job_undertaker", "job_undertaker"),
        // The display pair (milestone 29): the confined virtio-gpu driver and the client that draws
        // into the surface it serves.
        ("display", "display"),
        ("painter", "painter"),
        // The C seam (milestone 36): the confiner that builds, supervises and checks the foreign
        // component, and the Rust shell that links it.
        ("c_confiner", "c_confiner"),
        ("c_shim", "c_shim"),
        // The compositor and its window client (milestone 33, rung two).
        ("compositor", "compositor"),
        ("window", "window"),
        // The display terminal (milestone 29's text increment): one binary, two wirings.
        ("display_terminal", "display_terminal"),
        // The keyboard driver (milestone 29's input).
        ("kbd", "kbd"),
        // Live component replacement (milestone 23): the operator, the two instances of the
        // swappable component (the second computes its answers in C), the client that talks across
        // the swap, and the queue broker for the opt-in rung.
        ("swapper", "swapper"),
        ("rust_swappable", "rust_swappable"),
        ("c_swappable", "c_swappable"),
        ("chatty", "chatty"),
        ("broker", "broker"),
        // The clock service (milestone 51) and the program that reads the page it publishes.
        ("clock", "clock"),
        ("date", "date"),
        // `rm` (milestone 47's rmdir lane): the first program endowed a directory capability.
        ("rm", "rm"),
        // The disk surveyor and the partitioner (milestone 57): the same disk authority pointed in
        // each direction, and the partitioner refuses to write without an entropy endpoint.
        ("disk_surveyor", "disk_surveyor"),
        ("disk_partitioner", "disk_partitioner"),
        // The nameset caretaker (milestone 47's globbing lane): a directory capability attenuated
        // to the names a pattern matched.
        ("fs_nameset_caretaker", "fs_nameset_caretaker"),
        ("entropy", "entropy"),
        // The credential service and its clients (milestone 56, the credential half).
        ("credentialer", "credentialer"),
        ("credentialer_test_client", "credentialer_test_client"),
        ("ntp", "ntp"),
        // The outlaw (milestone 19's user-test port): the privilege-boundary programs
        // kernel::user::tests used to hand-assemble.
        ("outlaw", "outlaw"),
        // The sink contract's ends (milestone 50): the indifferent writer and the read-back.
        ("sink", "sink"),
        // `wc` (milestone 50): the right-hand side of a pipe, and the first program that reads a
        // stream.
        ("wc", "wc"),
        // `doc` (milestone 40): the documentation viewer, a filter from markdown to styled text.
        ("doc", "doc"),
        // `ps` (milestone 126): the process listing over a supervision domain.
        ("ps", "ps"),
        // `pgrep` (milestone 126): that listing, filtered to the members a selector names.
        // It must ship with `ps` because the two together are the claim.
        ("pgrep", "pgrep"),
        // `timetable` (milestone 129): scheduled execution whose every entry is a grant.
        ("timetable", "timetable"),
    ];
    let mut blobs: Vec<(&str, Vec<u8>)> = Vec::new();
    for &(archive_name, bin_name) in entries {
        match read_stripped(&bin_elf(bin_name)) {
            Ok(b) => blobs.push((archive_name, b)),
            Err(e) => {
                eprintln!("mkinitrd: cannot read {}: {e}", bin_elf(bin_name));
                return false;
            }
        }
    }
    let mut files: Vec<(&str, &[u8])> = blobs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
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
/// `-initrd` (milestone 19f); the raw user ELFs ([`bin_elf`]) are only the input `mkinitrd` packs.
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
    // **`notes/capabilities.md` is here because of milestone 117 rather than because of symmetry.**
    // Three stranger runs found it unreachable by following the tree, and it is the page that
    // answers what this system's central word means. A store that can be searched from the prompt
    // and does not carry it is a manual with the first chapter missing. It is the kernel's own
    // document, so it is in the kernel's bundle; `script/apropos` is the other half, for the reader
    // who has a checkout rather than a prompt.
    (
        "kernel",
        &[
            "notes/ipc-naming.md",
            "notes/stack.md",
            "notes/capabilities.md",
        ],
    ),
    ("glob", &["notes/glob.md"]),
];

/// Where the store is staged on the host before it is imported into the filesystem image.
///
/// The last component is [`manual::index::STORE_DIR`] rather than the literal `doc`, because the
/// guest opens that name and this writes it: it is a thing two programs agree on, so it is a
/// constant in the crate they share.
fn doc_store_path() -> std::path::PathBuf {
    workspace_root()
        .join("target/redoxfs-tree")
        .join(manual::index::STORE_DIR)
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
        if std::fs::write(dir.join(manual::index::SHARD), &index).is_err() {
            eprintln!("doc-store: cannot write {bundle}/{}", manual::index::SHARD);
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
    if std::fs::write(root.join(manual::index::MANIFEST), names).is_err() {
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

    // **The bundles come from the manifest the build just wrote**, not from `DOC_BUNDLES`, because
    // that is what the guest reads and the whole value of this query is that it takes the guest's
    // path. A store whose manifest disagreed with the table would answer differently at the prompt
    // than it does here, and this is where that would show.
    let Ok(manifest) = std::fs::read(doc_store_path().join(manual::index::MANIFEST)) else {
        eprintln!("doc-store: the bundle manifest is not there");
        return false;
    };
    let mut ranked = manual::index::Ranked::new();
    let mut bad = Vec::new();
    manual::index::bundles(&manifest, |bundle| {
        let name = String::from_utf8_lossy(bundle).to_string();
        let Ok(bytes) = std::fs::read(doc_store_path().join(&name).join(manual::index::SHARD))
        else {
            bad.push(format!("{name}: no shard"));
            return;
        };
        if let Err(e) = manual::index::search(
            bundle,
            term.as_bytes(),
            &mut manual::index::Slice(&bytes),
            &mut ranked,
        ) {
            bad.push(format!("{name}: {e:?}"));
        }
    });
    for b in &bad {
        println!("  {b}");
    }

    for f in ranked.results() {
        println!(
            "  {:>4}  {:<28}  {:<46}  {}",
            f.count,
            String::from_utf8_lossy(f.location()),
            String::from_utf8_lossy(f.title()),
            String::from_utf8_lossy(f.origin())
        );
    }
    if ranked.offered() == 0 {
        println!("  nothing in the store says that");
    } else if ranked.offered() > ranked.results().len() {
        println!(
            "  {} of {} pages, strongest first",
            ranked.results().len(),
            ranked.offered()
        );
    }
    bad.is_empty()
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

/// The ELF path of a named binary the `user` package builds (milestone 19f.2+): `hello`, `worker`,
/// `console`, and so on. `mkinitrd` packs each into the archive, under that same name for every
/// program but `hello`, which is packed as `init`.
///
/// **The path is ABSOLUTE, and that is not fussiness.** Cargo runs the runner script with the
/// working directory set to the **package** dir for `cargo test` and the workspace root for
/// `cargo run`. A relative path therefore resolved under `cargo run` and silently did not under
/// `cargo test`, so the tests booted with no initrd at all and the one that noticed was the one
/// that panicked.
///
/// That lesson was written on a `user_elf()` helper that computed this same path for `hello`
/// alone, and was `bin_elf("hello")` in every respect but the comment. Milestone 130 folded it in
/// when `mkinitrd` stopped needing a special case for `init`; the warning belongs here, where
/// every caller reads it, rather than on the one caller that happened to earn it.
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
    // Sampled from the transcript reader rather than from the referee thread, because this is the
    // loop that runs for the length of the leg and the sampler is rate-limited anyway. **This leg
    // needs it more than the TCG one does**: HVF runs the guest on the physical cores, so the
    // host's other work competes with it directly rather than through an interpreter.
    let mut load = HostLoad::new();
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
        load.sample();
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

    let ok = match verdict {
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
    };
    load.report_if_failed(ok, "aarch64 --hvf");
    ok
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
const SHELL_CHECK_SCRIPT: [(&str, &[&str]); 55] = [
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
    // **Milestone 40 at the same interface.** `doc` is in the image, is spawnable, and declares that
    // it reads a stream, so bare `doc` is refused at the prompt before anything is spawned, exactly
    // as `wc` is and for the same reason: a viewer that could open the page it renders could open
    // any page.
    ("doc", &["name a file"]),
    // **The named file reaches the viewer and comes back rendered**, which two of this gate's own
    // comments said it did not until 2026-08-18. Both halves of that were fixed elsewhere and the
    // record was never corrected: the input operand now comes off the plan rather than off the
    // `Line` (`user/src/swish.rs`, the same fix `wc gate.txt | wc` above pins), and
    // `MAX_OUTPUT_CHUNKS` is 4096 rather than the 32 that would have truncated a page to 512 bytes.
    //
    // The numbers are the assertion and not decoration. `gate.txt` is 2 lines, 4 words, 24 bytes
    // (`wc gate.txt`, above). What comes back is **1 line, 4 words, 26 bytes**: the two source
    // lines are one paragraph re-flowed to one output line, and the two bytes are the body indent.
    // A viewer handed an empty stream would answer `0 0 0`, which is what this line answered when
    // the operand was being dropped, so the count is what separates rendering from silence.
    ("doc gate.txt | wc", &["1 4 26"]),
    // **And the line a person actually wants is still refused, honestly.** `doc gate.txt` on its
    // own would make this shell both the writer and the reader of one line, and it has one wait
    // point; the refusal names the fix rather than hanging, which is the refusal Unix cannot
    // produce. See `grant_plan::check_chain` and notes/manual.md: closing this needs a place for
    // the viewer's output to go that is not the shell, and that is a spawn-protocol decision.
    ("doc gate.txt", &["writes while it reads"]),
    // **The negative control on the viewer itself**, and it is the whole milestone in one screen: a
    // documentation viewer is exactly the program a reader expects to go and fetch things, and this
    // one is handed a stream. `caps` prints what would be granted before anything is spawned, and
    // there is no file capability, no directory and no filesystem endpoint in it. The manifest is
    // byte-identical to `wc`'s, which is why the assertion is the same string.
    ("caps doc gate.txt", &["input    gate.txt"]),
    // **Milestone 40 phase 2, at the same interface**: the documentation store is installed, and a
    // search of it answers with pages a person can then open.
    //
    // `doc/bundles` is the manifest the search reads, and it is checked as a *file* first, with an
    // ordinary designation, because that is the claim underneath everything below it: the store is
    // real, it is where the reader thinks it is, and nothing special is needed to read it. The
    // numbers are the four bundle names and their newlines, so a bundle added to `DOC_BUNDLES`
    // fails here, which is right: the manifest is what the guest enumerates by.
    ("wc doc/bundles", &["4 4 25"]),
    // **The query a person would type, against shards built from this repository's own markdown.**
    // Two bundles, so the answer is the merge across shards rather than one shard's list, and both
    // named pages are ones a reader wanting to know what a capability is here would want. The
    // counts are deliberately not asserted: they move whenever the notes are edited, and the claim
    // is which pages were found, not how often the word appears in them.
    (
        "apropos capability",
        &["doc/swish/pipes.md", "doc/kernel/ipc-naming.md"],
    ),
    // The negative control, and the word is chosen to appear in **no bundled page**. See
    // notes/manual.md's BUGS for why this one cannot be written into the note that documents it:
    // that note is itself in the store, so a word written there is a word the store then says.
    (
        "apropos photosynthesis",
        &["no page in the store says photosynthesis"],
    ),
    // And a search with nothing to search for is refused, in the same sentence every other verb
    // that needs an operand uses.
    ("apropos", &["name what you mean"]),
    // **The payoff, and the reason a search may be a builtin at all.** The name the search printed
    // is an ordinary designation: this line grants `wc` exactly that one page out of the store and
    // nothing else, resolved by the shell against the directory it holds. So search produced a
    // *name*, and the authority moved on the line where a person typed it. A search that had
    // handed a program the store's directory would have moved it three lines earlier and silently.
    (
        "caps wc doc/kernel/ipc-naming.md",
        &["input    ipc-naming.md"],
    ),
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
    // **`ps`, at the real prompt** (milestone 126). The listing itself: a header, and at least the
    // row for `ps` itself, which is a member of the domain init spawned it into. Asserting the
    // header rather than a tid is deliberate: a tid is a generational name that moves with how many
    // jobs ran before it, and a gate that pinned one would be pinning the boot's history.
    ("ps", &["TID  STATE"]),
    // **`ps` cannot see the machine, and this is the shape of the evidence at the prompt.** The
    // listing above is short: at this line the shell's domain holds `ps` itself and whatever else
    // the shell has running, which is nothing. A `/proc`-shaped `ps` would be listing init, the
    // shell, the terminal, the FS server, the compositor, the net stack and every driver.
    //
    // **The count is deliberately not asserted here.** `ps | wc` answered three lines on one run
    // and two on the next, because a pipeline spawns both stages into the same domain and whether
    // `ps` walks before or after `wc` exists is a race. That is truthful (a survey is a snapshot,
    // notes/process-view.md) and it makes a count a bad gate. The confinement claim is asserted
    // deterministically and on both ISAs in `kernel::user::survey_tests`, which builds the domain
    // it measures instead of inheriting one.
    // And `caps ps` prints the scope **before** anything is spawned, which is the half Linux has no
    // way to express: there, "which processes can this see" has one answer for every program on the
    // machine and no command line chose it.
    ("caps ps", &["cap 7  endpoint  domain"]),
    // **`pgrep`, at the real prompt** (milestone 126), and what is asserted is deliberately not the
    // tids. `pgrep` prints nothing but names, one per line, and a tid is a generational name that
    // moves with how many jobs ran before it; a gate that pinned one would be pinning the boot's
    // history. So the claim is made through the **second stream**, which is the same trick the four
    // `date 2>` lines above use: `pgrep`'s diagnostics carry a sentence in exactly three cases (the
    // walk was refused, the selector can never match, or nothing matched), so an *empty* second
    // stream is the assertion that none of the three happened. This one line fails if init endowed
    // no domain, if it endowed one the kernel refuses, or if the filter came back empty.
    ("pgrep 2> pgrep.txt", &[]),
    ("wc < pgrep.txt", &["0 0 0"]),
    // And the asymmetry, printed before anything is spawned, which is the whole of what milestone
    // 126 has instead of the `pgrep`-beside-`pkill` comparison it originally promised. Two phrases:
    // the right (`ENUMERATE`, not `READ`, so the finder cannot receive a death or collect a corpse)
    // and the sentence that says so in English. There is no `caps pkill` line to put beneath this
    // one, because a tid is a name and no method turns one into authority.
    (
        "caps pgrep",
        &["cap 7  endpoint  domain   ENUMERATE", "do nothing to them"],
    ),
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
    // **Milestone 31 phase 3, at the one interface a human touches** (2026-08-17). Naming a
    // directory in a command IS granting a capability to it, and until this landed the prompt could
    // say so and not do it: init deleted the file service during the boot, so a directory grant had
    // nothing to build a caretaker out of and `rm` was a refusal.
    //
    // Four lines, and they are one argument in order. **The preview first**, because the whole claim
    // of this milestone is that the authority is legible before it moves: the row names the
    // directory the grant is over and the sentence says what `-r` would have added, so a reader can
    // see the narrower of the two capabilities being chosen.
    (
        "caps rm rmtree/rm-solo",
        &[
            "dir      /rmtree  (the directory holding rm-solo)",
            "and nothing under it: no -r, so it cannot even look",
        ],
    ),
    // **The removal, through a caretaker init built for this one command.** `-v` because `rm`'s
    // default is silence and a gate needs something to read; the name it prints is the name the
    // command line designated, which is the whole of the endowment.
    ("rm -v rmtree/rm-solo", &["rm-solo"]),
    // **And exactly that name went.** Two entries left in a directory that had three, so the grant
    // took what was designated and not what it could reach: `rm-keep` and the whole `rm-doomed`
    // subtree were inside the same capability and are still there, because nothing named them.
    // Eleven bytes of `rm-doomed/` and eight of `rm-keep`, newlines included.
    ("ls rmtree | wc", &["2 2 19"]),
    // **The one shape this still cannot deliver, and the refusal now says what is true.** It used to
    // read "needs init to build the caretaker", which stopped being true on the line above. A
    // caretaker's whole attenuation is one `OPENDIR` *into* the granted directory, and a name typed
    // at the top prompt designates the root of this shell's namespace, which has no name to descend
    // into; the contract has no verb for "the directory I already hold, with fewer rights". So this
    // is a refusal at the prompt with **nothing spawned**, which is the one outcome this model must
    // never trade away, and it is a design fork rather than a missing line of code. See
    // design/roadmap/31-capability-shell.md and notes/dir-capability.md's BUGS.
    ("rm gate.txt", &["there is no name here to descend into"]),
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
    // **Fifteen spawns above plus these six are twenty-one jobs through a six-job pool**, so a boot
    // where nothing collected would answer "could not spawn (init is out of memory)" somewhere in
    // here rather than the arithmetic. (Eleven when milestone 22 wrote this line, `2>` added two
    // more spawning lines above, milestone 86's `time` added two more, milestone 67's quoting added
    // three, milestone 40 phase 2's `wc doc/bundles` added one, and milestone 31 phase 3 added two:
    // an `rm` that really runs, and the `wc` that counts what is left. The `rm` is the one worth
    // noticing, because it is the first job whose region holds **two**
    // processes, the program and the `fs_subtree_caretaker` carrying its grant, and it is therefore
    // the first thing in this script that would fail if `job_undertaker`'s retry did not collect
    // both; the count is a fact about the whole script, so it is taken at the merge and not from any
    // one lane.) Six distinct arguments rather than one repeated, because the
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
///
/// # BUGS
///
/// **It flakes anyway, and the thirty seconds is a *per-echo* budget rather than a per-line one.**
/// On 2026-08-18 the riscv64 leg failed with "the prompt never echoed `caps date 2> err.txt`" after
/// echoing thirteen characters of it, on a machine running four other lanes; the same commit passed
/// on a rerun with nothing else changed. So a failure of this shape is a load report and not a
/// finding, and the way to tell them apart is to rerun the one leg before reading the transcript.
/// Raising the number is not obviously the fix: a real hang would then take proportionally longer
/// to report, and the honest measurement (how long an echo actually takes under load, versus the
/// budget) has not been made.
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
             a declared second stream off the redirection, previewed a directory grant and \
             then removed exactly the name it designated through a caretaker init built for \
             that one command, swept a \
             match too large to hand over in batches whose authority is exactly what each was \
             designated, named a file whose name has a space in it, searched an installed \
             documentation store and got back pages a following line could then designate, ran \
             a && past a command that succeeded and not past one it refused, and ran \
             twenty-one jobs through init's six-job pool after init gave its construction \
             budget away"
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

    // E3 (milestone 134, design/roadmap/134-the-measurements-that-decide.md): build with an extra
    // kernel feature alongside `bench`, so the padded-fastpath experiment can be measured with the
    // same harness as everything else rather than a one-off. `--real`-only for the same reason
    // `--release` is: there is no cache under TCG for a footprint change to perturb, so a gated
    // run with this set would just measure instruction-count noise and call it a finding.
    let extra_features = flag_value("--extra-features");
    if extra_features.is_some() && !real {
        eprintln!(
            "bench: --extra-features only makes sense with --real (no cache under TCG for it to move)"
        );
        return false;
    }
    let features = match &extra_features {
        Some(f) => format!("bench,{f}"),
        None => "bench".to_string(),
    };

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
            &features,
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
        // **Diagnostics pass straight through, and deliberately do not become rows.** A probe
        // reports a count or a bitmask, not a duration, so putting it in the table would invite
        // `--check` to police an exact value with a 10% tolerance and would print a meaningless
        // ns/iter beside it. Echoed instead, so a reader of a bench run sees it and the baseline
        // never grows a line that is not a measurement. See `bench::map_new`'s shootdown probe.
        if let Some(probe) = line.strip_prefix("bench-probe: ") {
            eprintln!("  probe: {probe}");
            continue;
        }
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

/// **The instruction-count instrument** (milestone 78;
/// design/roadmap/78-load-sensitive-assertions.md), on both ISAs because parity is a gate (§19).
///
/// Boots a `--features icount` kernel under `-icount shift=0,sleep=off`, where QEMU's virtual clock
/// advances by exactly one nanosecond per guest instruction retired and by nothing else. The guest
/// asserts two claims a wall-clock test cannot make (that the timer fired at the deadline the kernel
/// armed, and that the handler costs fewer than N instructions) and prints what it measured.
///
/// **This is not on the test path and that is the design.** `-icount` changes what QEMU is, and the
/// two ways that matter are not the one the milestone block gave: it is **not** measurably slower on
/// compute (measured, 2026-08-17), but it gives every vCPU **one shared virtual clock**, which forces
/// `-smp 1` and would silently retire every cross-core property the suite proves, and it makes a
/// clock-bound wait cost instructions rather than host time. So the instrument gets its own boot,
/// exactly as `script/bench` does, and the test path is untouched. See notes/instruction-clock.md.
///
/// The verdict arrives the bench boot's way rather than through semihosting: the guest prints
/// `icount: done` and parks in `wfi`, this owns the child and kills it. A panic (a violated claim)
/// prints `[PANIC]` and is a failure; so is reaching end of output with neither.
fn icount() -> bool {
    let legs = match flag_value("--arch").as_deref() {
        None => ArchLegs::Both,
        Some("aarch64") => ArchLegs::Aarch64,
        Some("riscv64") => ArchLegs::Riscv64,
        Some(other) => {
            eprintln!("icount: --arch {other} is not an architecture (aarch64 or riscv64)");
            return false;
        }
    };
    if legs.aarch64() && !icount_leg("aarch64", RUNNER, TARGET) {
        return false;
    }
    if legs.riscv64() && !icount_leg("riscv64", "scripts/qemu-runner-riscv64.sh", RISCV_TARGET) {
        return false;
    }
    true
}

/// One ISA's instrument run: build, boot, read the transcript, report.
fn icount_leg(arch: &str, runner: &str, target: &str) -> bool {
    if !cargo(&[
        "build",
        "-p",
        "kernel",
        "--features",
        "icount",
        "--target",
        target,
    ]) {
        return false;
    }

    eprintln!();
    eprintln!(
        "--- icount: {arch}, single hart, TCG + icount (one instruction = one nanosecond) ---"
    );

    let mut cmd = Command::new(runner);
    cmd.arg(format!("target/{target}/debug/kernel"));
    cmd.args(["-icount", "shift=0,sleep=off"]);
    // One hart, for the reason the bench instrument pins it and the placement probe can never move
    // here: under `-icount` all vCPUs share ONE virtual clock, and an idle secondary parked in `wfi`
    // jumps that clock forward to the next event. A timer measurement on four harts would be
    // measuring three other harts' idle jumps. See notes/benchmarks.md.
    cmd.env("NIFE_SMP", "1");
    // No accelerator: HVF has no icount at all (it runs the physical core, which is the whole point
    // of it), so a stale `NIFE_ACCEL` from the caller's shell would silently produce wall-clock
    // numbers wearing instruction units. The guest's own calibration refuses that case too; this
    // stops it happening rather than catching it.
    cmd.env_remove("NIFE_ACCEL");
    // And no devices. Every one of these adds a source of interrupts, and an interrupt that is not
    // the timer landing inside the measured window would show up as the timer handler being late.
    // The suite attaches them because its tests assert they are present; this boot drives none of
    // them, so a variable left set by an earlier `script/test` in the same shell must not reach it.
    for device in [
        "NIFE_GPU",
        "NIFE_KBD",
        "NIFE_RNG",
        "NIFE_NVME",
        "NIFE_NET",
        "NIFE_DISK",
        "NIFE_INITRD",
        "NIFE_GPU_MON",
    ] {
        cmd.env_remove(device);
    }
    cmd.stdout(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("icount: failed to start the runner: {e}");
            return false;
        }
    };

    // **A deadline, on a thread, because the guest is not obliged to say anything.**
    //
    // The read below blocks, and a QEMU that has wedged before printing (or after panicking, since
    // this kernel's panic handler halts rather than exiting) never closes the pipe. This lane leaked
    // two emulators learning that, at 80% of a core each, on a laptop already carrying four other
    // lanes' gates. AGENTS.md's rule is that every unattended QEMU run is bounded, and a bound that
    // depends on the guest reaching a marker is not a bound.
    //
    // Generous on purpose: the instrument's own work is a few seconds, so this only ever fires on a
    // machine that is not making progress at all.
    const DEADLINE_SECS: u64 = 300;
    const PANIC_GRACE_SECS: u64 = 3;
    let pid = child.id();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let panicked = std::sync::Arc::new(AtomicBool::new(false));
    let watchdog = {
        let finished = std::sync::Arc::clone(&finished);
        let panicked = std::sync::Arc::clone(&panicked);
        std::thread::spawn(move || {
            // Woken in slices so a normal run's thread goes away promptly rather than sleeping out
            // the whole deadline after everything else is done.
            let mut grace: Option<u64> = None;
            for _ in 0..DEADLINE_SECS {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if finished.load(Ordering::Relaxed) {
                    return;
                }
                // A panic shortens the fuse rather than taking a path of its own. The reader below
                // keeps printing for a few seconds, however many lines the message runs to, and then
                // this kill closes the pipe and ends the read. Counting lines instead was the first
                // attempt and it hung: the panic printed two and the reader waited forever for a
                // third that a halted guest was never going to send.
                match grace {
                    _ if !panicked.load(Ordering::Relaxed) => {}
                    None => grace = Some(PANIC_GRACE_SECS),
                    Some(0) => break,
                    Some(n) => grace = Some(n - 1),
                }
            }
            if grace.is_none() {
                eprintln!("icount: no verdict in {DEADLINE_SECS}s; killing QEMU (pid {pid})");
            }
            // `kill(1)` rather than the `Child`, which the reading thread owns.
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        })
    };

    use std::io::BufRead;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::io::BufReader::new(stdout);
    let mut done = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // The guest's own lines, verbatim: the numbers are the deliverable, not a summary of them.
        if let Some(rest) = line.strip_prefix("icount: ") {
            eprintln!("  {rest}");
            if rest == "done" {
                done = true;
                break;
            }
            continue;
        }
        // A violated claim is a panic in the guest, and its message is written to say which claim
        // moved and by how much. Print it and everything after it; the watchdog's grace period ends
        // the read, because this kernel's panic handler halts rather than exiting and the pipe would
        // otherwise never close.
        if panicked.load(Ordering::Relaxed) || line.contains("[PANIC]") {
            eprintln!("  {line}");
            panicked.store(true, Ordering::Relaxed);
        }
    }
    finished.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let _ = child.wait();
    let _ = watchdog.join();
    let panicked = panicked.load(Ordering::Relaxed);

    if panicked {
        eprintln!("icount: {arch} FAILED a claim (the panic above says which)");
        return false;
    }
    if !done {
        eprintln!("icount: {arch} QEMU ended before printing `icount: done`; no verdict");
        return false;
    }
    eprintln!("icount: {arch} claims hold");
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

// ---- the tree-wide search (milestone 40, script/apropos) -----------------------------------

/// **The same index, the same reader, pointed at this repository instead of at the image.**
///
/// Milestone 117 has run three strangers at this tree, and what all three could not do is a list
/// rather than an impression: reach `notes/net.md`, `notes/capabilities.md` or **any**
/// `design/decisions/` file by following the tree while doing ordinary work, and find
/// `crates/abi/src/lib.rs`, which is four syscall numbers and the whole design on one screen. None
/// of those is hidden. They are unreachable in the sense that matters, which is that nothing a
/// person would type leads to them, and a signpost read once does not count.
///
/// Milestone 40 already owns the machinery for that: an inverted index over markdown, a reader that
/// merges shards, and a ranking. It was pointed only at what the filesystem image installs, which
/// is six pages, because that is where a *guest* can search. A person with a checkout is not a
/// guest and has no such limit, so this points the same code at all of it.
///
/// **It is deliberately not a second implementation.** `manual::index::build` writes these shards
/// and `manual::index::search` reads them, exactly as `cargo xtask manual` and the guest's
/// `apropos` builtin do, so a defect in the layout shows up in both places and a fix lands in both.
/// What differs is the corpus and what a result names: a guest result names a page in the store it
/// can open, and a result here names a **path in this repository**, because that is what a person
/// with a checkout opens.
///
/// See notes/manual.md.
fn tree_apropos(term: Option<String>) -> bool {
    let Some(term) = term else {
        eprintln!("usage: script/apropos <word>");
        eprintln!("       searches every markdown page in this repository, and every crate's and");
        eprintln!("       program's own module documentation, and says where each one lives.");
        return false;
    };

    let sections = tree_sections();
    if sections.is_empty() {
        eprintln!("apropos: found no documentation to index, which means this is not a checkout");
        return false;
    }

    let mut ranked = manual::index::Ranked::new();
    let mut pages = 0usize;
    let mut bytes = 0usize;
    let mut long = Vec::new();
    for shelf in &sections {
        pages += shelf.docs.len();
        bytes += shelf.docs.iter().map(|d| d.text.len()).sum::<usize>();
        // A path the record cannot hold is reported rather than silently shortened, because the
        // path is the whole answer here: a result a reader cannot open is worse than no result.
        for d in &shelf.docs {
            if d.path.len() > manual::index::PATH_MAX {
                long.push(d.path.clone());
            }
        }
        let sources: Vec<manual::index::Source<'_>> = shelf
            .docs
            .iter()
            .map(|d| manual::index::Source {
                path: &d.path,
                title: &d.title,
                text: &d.text,
            })
            .collect();
        let shard = manual::index::build(&sources);
        if let Err(e) = manual::index::search(
            shelf.name.as_bytes(),
            term.as_bytes(),
            &mut manual::index::Slice(&shard),
            &mut ranked,
        ) {
            eprintln!("apropos: {}: {e:?}", shelf.name);
            return false;
        }
    }

    println!("{pages} pages, {bytes} bytes of documentation in this repository");
    println!();
    println!("searching for: {term}");
    println!();
    for f in ranked.results() {
        // **The origin, not the location.** A guest result names `doc/<bundle>/<page>`, because
        // that is what a shell there can designate; the reader of this command holds a checkout, so
        // the openable name is the path the page came from.
        println!(
            "  {:>5}  {:<48}  {}",
            f.count,
            String::from_utf8_lossy(f.origin()),
            String::from_utf8_lossy(f.title())
        );
    }
    if ranked.offered() == 0 {
        println!("  nothing in this repository says that");
    } else if ranked.offered() > ranked.results().len() {
        println!();
        println!(
            "  {} of {} pages, strongest first",
            ranked.results().len(),
            ranked.offered()
        );
    }
    for p in &long {
        eprintln!(
            "apropos: {p} is longer than the {} bytes a page record holds, so its result would be \
             truncated",
            manual::index::PATH_MAX
        );
    }
    long.is_empty()
}

/// One document offered to the tree index: where it lives, what it is called, and its text.
struct Doc {
    /// Path relative to the workspace root, which is the openable name a result prints.
    path: String,
    title: String,
    text: Vec<u8>,
}

/// One shard of the tree index, named for the part of the tree it covers.
struct Shelf {
    name: &'static str,
    docs: Vec<Doc>,
}

/// The corpus, in shards, because the merge across shards is what the reader does.
///
/// The shard names are the reader's map of the tree and are the only new vocabulary here; they are
/// the directories a person already sees.
fn tree_sections() -> Vec<Shelf> {
    let root = workspace_root();
    let mut out = Vec::new();

    // The markdown, in the four places this project keeps it. The repository root is included
    // because `README.md` and `DECISIONS.md` are where a stranger starts, and a search that could
    // not return the front page would be odd about it.
    for (shard, dir, recurse) in [
        ("notes", "notes", false),
        ("decisions", "design/decisions", false),
        ("roadmap", "design/roadmap", false),
        ("design", "design", false),
        ("guides", "", false),
    ] {
        let mut docs = Vec::new();
        collect_markdown(&root.join(dir), &root, &mut docs, recurse);
        if !docs.is_empty() {
            out.push(Shelf { name: shard, docs });
        }
    }

    // **And the module documentation, which is the finding this exists for.** A stranger could not
    // find `crates/abi/src/lib.rs`, and no markdown page is going to fix that, because the document
    // it wants *is* that file's header. A `//!` block is markdown already, so it indexes as a page
    // with no conversion and no copy: the result names the source file, which is the thing to open.
    for (shard, dir, file) in [
        ("crates", "crates", "src/lib.rs"),
        ("programs", "user/src", ""),
    ] {
        let mut docs = Vec::new();
        collect_module_docs(&root.join(dir), &root, file, &mut docs);
        if !docs.is_empty() {
            out.push(Shelf { name: shard, docs });
        }
    }

    out
}

/// Every `.md` file directly in `dir`, as `(path relative to the root, title, bytes)`.
fn collect_markdown(
    dir: &std::path::Path,
    root: &std::path::Path,
    out: &mut Vec<Doc>,
    recurse: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // Sorted, so two runs on one checkout offer the same pages in the same order and a tie between
    // two pages breaks the same way twice. `Ranked` keeps ties in offer order by design.
    found.sort();
    for path in found {
        if path.is_dir() {
            if recurse {
                collect_markdown(&path, root, out, recurse);
            }
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.display().to_string();
        let title = manual::index::title_of(&bytes)
            .unwrap_or(&rel)
            .trim()
            .to_string();
        out.push(Doc {
            path: rel,
            title,
            text: bytes,
        });
    }
}

/// The `//!` header of every Rust file under `dir`, as a page.
///
/// `file` is the path inside each subdirectory to read (`src/lib.rs` for a crate), or empty to read
/// the `.rs` files in `dir` itself (the programs).
fn collect_module_docs(
    dir: &std::path::Path,
    root: &std::path::Path,
    file: &str,
    out: &mut Vec<Doc>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    found.sort();
    for entry in found {
        let path = if file.is_empty() {
            if entry.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            entry
        } else {
            entry.join(file)
        };
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let doc = module_doc(&src);
        // A file whose header is a line or two says nothing worth ranking, and indexing it would
        // put noise in front of the pages that do. Three hundred bytes is about a paragraph.
        if doc.len() < 300 {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.display().to_string();
        // **Named for what it is, not for its first heading.** A crate header rarely opens with a
        // level-one heading, and falling back to the path would print the path twice. `crate abi`
        // is what a reader calls the thing.
        let what = if file.is_empty() { "program" } else { "crate" };
        let name = if file.is_empty() {
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("?")
        } else {
            path.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("?")
        };
        out.push(Doc {
            path: rel,
            title: format!("{what} {name}"),
            text: doc.into_bytes(),
        });
    }
}

/// The leading `//!` block of a Rust file, with the markers stripped, which is markdown.
fn module_doc(src: &str) -> String {
    let mut out = String::new();
    for line in src.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("//!") {
            out.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            out.push('\n');
        } else if t.is_empty() && out.is_empty() {
            // Leading blank lines before the header, which nothing in this tree writes but which
            // cost nothing to tolerate.
            continue;
        } else if !out.is_empty() {
            // The header ends at the first line that is not part of it. Attributes and `use` lines
            // below are code, and a searcher that indexed them would rank a crate by its imports.
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Both operating systems' `uptime`, because only one of them is ever in front of you.**
    /// Development happens on macOS and CI runs on `ubuntu-24.04-arm`, so the format not under the
    /// author's nose is the one that breaks silently: a failed parse reports "unavailable" rather
    /// than erroring, which is right at run time and useless as a signal.
    ///
    /// The real strings, copied from each system rather than reconstructed.
    #[test]
    fn the_load_average_parse_serves_macos_and_linux() {
        let macos = "11:07  up 5 days, 22:33, 3 users, load averages: 4.14 4.86 4.29";
        let linux = " 18:02:11 up 12 days,  3:41,  2 users,  load average: 0.50, 0.40, 0.30";
        assert_eq!(parse_load_average(macos), Some(4.14));
        assert_eq!(parse_load_average(linux), Some(0.50));
        // A shape nothing here recognises is `None`, not a wrong number: the report says so out
        // loud, and a made-up load average would be worse than no line at all.
        assert_eq!(parse_load_average("up 3 days"), None);
        assert_eq!(parse_load_average("load average:"), None);
        assert_eq!(parse_load_average("load average: n/a"), None);
    }

    /// The copy into the patched std sysroot must drop a `# Examples` section and keep everything
    /// else, including the `text` diagrams the protocol crates lead with. The two cases worth
    /// pinning are the ones a naive line filter gets wrong: a hidden doctest line (`# use ...`)
    /// looks exactly like a heading, and a section that runs to the end of the doc block has no
    /// following heading to stop at.
    #[test]
    fn the_std_copy_drops_doc_examples_and_keeps_the_prose() {
        let src = "\
//! A contract.
//!
//! ```text
//!   a diagram
//! ```
//!
//! # Examples
//!
//! ```
//! # use entropy_proto::GET;
//! assert_eq!(GET, 1);
//! ```
//!
//! # Nothing here transforms a byte
//!
//! Prose that must survive.

/// An item.
///
/// # Examples
///
/// ```
/// let x = 1;
/// ```
pub const GET: u64 = 1;
";
        let got = strip_doc_examples(src);
        assert!(got.contains("a diagram"), "text blocks are documentation");
        assert!(got.contains("# Nothing here transforms a byte"));
        assert!(got.contains("Prose that must survive."));
        assert!(got.contains("/// An item."));
        assert!(got.contains("pub const GET: u64 = 1;"));
        assert!(!got.contains("# Examples"));
        assert!(
            !got.contains("entropy_proto"),
            "the copy is an inner module of std, where that crate does not exist"
        );
        assert!(
            !got.contains("let x = 1;"),
            "a trailing section, with no heading after it to stop at"
        );
    }

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
