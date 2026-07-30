//! Build orchestration for cricker-os.
//!
//! A normal Rust binary that runs on the *host*. Building a kernel means a custom
//! target, a linker script, and driving QEMU with the right flags, none of which fits
//! neatly into `cargo build`. This beats a Makefile because it's Rust and it composes.
//! See DECISIONS.md §7.
//!
//!     cargo xtask run      boot the kernel (the milestone tour), print to this terminal
//!     cargo xtask shell    boot straight to the interactive shell (add --hvf for the real core)
//!     cargo xtask test     host tests (milliseconds), then the kernel under QEMU
//!     cargo xtask gdb      boot paused, waiting for a debugger on :1234
//!     cargo xtask objdump  disassemble the kernel
//!     cargo xtask image    build the flat arm64 Image and dump its header
//!
//! Note that `run` and `test` do NOT invoke QEMU themselves. They just call cargo,
//! which invokes `scripts/qemu-runner.sh` via the runner setting in
//! `.cargo/config.toml`. That script is the single source of truth for how the kernel
//! gets booted, so there is exactly one place to get the QEMU flags wrong.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const RUNNER: &str = "scripts/qemu-runner.sh";

/// The RISC-V target, for the second-architecture initrd (milestone 20). The kernel itself is built
/// and run through cargo + `scripts/qemu-runner-riscv.sh` directly, not this xtask; this const exists
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
            eprintln!(
                "--- booting cricker-os to an interactive shell (type `help`, Ctrl-C to quit) ---"
            );
            mkdisk()
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
            eprintln!("--- booting cricker-os via userspace init (Ctrl-C to quit) ---");
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
        "initrd-riscv" => initrd_riscv(),
        "std-src" => std_src(),
        "user-std" => user_std(),
        "test" => test(),
        "bench" => bench(),
        "gdb" => gdb(),
        "objdump" => objdump(),
        "image" => image(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!(
                "usage: cargo xtask <build|run|shell|initboot|initrd-riscv|std-src|user-std|test|bench|gdb|objdump|image> [--hvf]"
            );
            eprintln!(
                "       cargo xtask bench [--riscv] [--real] [--release] [--smp] [--check] [--save]"
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
// std's Platform Abstraction Layer for cricker-os lives in patches/std-cricker (the Hermit shape:
// a `sys` backend on the capability ABI, not a libc shim). `std-src` materializes a patched
// rust-src into a linked `cricker-dev` toolchain; `user-std` builds the `hellostd` demo for the
// custom targets with -Zbuild-std against it. See notes/std.md.
// ===========================================================================================

/// The custom-target triples the std demo builds for, one per supported ISA. The name is the
/// JSON spec's file stem, which is also cargo's target-dir subdirectory.
const STD_TARGETS: [&str; 2] = ["aarch64-unknown-cricker", "riscv64-unknown-cricker"];

/// The linked toolchain name (`rustup toolchain link`) whose rust-src carries the cricker PAL.
const CRICKER_TOOLCHAIN: &str = "cricker-dev";

/// Bump to force every farm to rebuild after a change to the patch logic itself (not the inputs).
const STD_SRC_PATCH_VERSION: u32 = 4;

fn farm_dir() -> PathBuf {
    workspace_root().join("target/cricker-farm")
}

/// The real nightly sysroot the farm is hardlink-cloned from.
fn real_sysroot() -> Option<PathBuf> {
    capture("rustc", &["--print", "sysroot"]).map(|s| PathBuf::from(s.trim()))
}

/// The farm's patched std source root (`.../library/std/src`).
fn farm_std_src() -> PathBuf {
    farm_dir().join("lib/rustlib/src/rust/library/std/src")
}

/// The `hellostd` ELF for a given custom-target triple. user-std is its own workspace, so its
/// artifacts land under `user-std/target/<triple>/release/`.
fn hellostd_elf(triple: &str) -> PathBuf {
    workspace_root().join(format!("user-std/target/{triple}/release/hellostd"))
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
        root.join("crates/uheap/src/lib.rs"),
        // The net PAL generates its wire constants verbatim from the netd contract; a change to it
        // must rebuild the farm just like a change to the ABI crate.
        root.join("user/src/netproto.rs"),
        // Likewise the FS-service contract: `std::fs` is a client of it (milestone 27 phase two),
        // and its wire constants are generated verbatim into the PAL.
        root.join("crates/fs_proto/src/lib.rs"),
        root.join("targets/aarch64-unknown-cricker.json"),
        root.join("targets/riscv64-unknown-cricker.json"),
    ];
    collect_files(&root.join("patches/std-cricker/overlay"), &mut files);
    files.sort();
    for f in files {
        h = fnv(h, f.to_string_lossy().as_bytes());
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

/// **Materialize the patched `cricker-dev` toolchain** (milestone 27).
///
/// build-std reads std's source from the sysroot of the rustc it invokes, so a patched std means
/// a toolchain whose sysroot IS patched. We hardlink-clone the real nightly (`cp -al`, near-zero
/// disk since blocks are shared) so rustc resolves *this* directory as its sysroot, then replace
/// the `src` subtree with a real (independent-inode) copy and patch that copy: the overlay PAL
/// files, the ABI/heap crates generated verbatim, and a `target_os = "cricker"` arm inserted into
/// std's `cfg_select!` dispatchers. The real toolchain is never touched.
///
/// Idempotent: a stamp of all inputs guards the rebuild, so a warm farm (and its build-std cache)
/// survives across runs and only a PAL change forces std to recompile.
fn std_src() -> bool {
    let stamp = std_inputs_stamp();
    let stamp_file = farm_dir().join(".cricker-stamp");
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
    eprintln!("--- std-src: building the patched cricker-dev toolchain (this recompiles std) ---");

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

    // Link (or relink) the farm as `cricker-dev`. Idempotent: rustup replaces an existing link to
    // the same path.
    if !run(
        "rustup",
        &["toolchain", "link", CRICKER_TOOLCHAIN, &s(farm.clone())],
    ) {
        eprintln!("std-src: `rustup toolchain link {CRICKER_TOOLCHAIN}` failed");
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

/// Copy the PAL overlay (`patches/std-cricker/overlay/std/src/...`) over the farm's std source.
fn std_apply_overlay() -> bool {
    let overlay = workspace_root().join("patches/std-cricker/overlay/std/src");
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

/// Generate `abi.rs` and `uheap.rs` verbatim from the host-tested crates, so the ABI numbers and
/// the heap algorithm have exactly one definition. The transform strips crate-level inner
/// attributes (`#![no_std]`, illegal in a non-root module) and any trailing `#[cfg(test)]` module.
fn std_generate_modules() -> bool {
    let root = workspace_root();
    let jobs = [
        (
            root.join("crates/abi/src/lib.rs"),
            farm_std_src().join("sys/pal/cricker/abi.rs"),
        ),
        (
            root.join("crates/uheap/src/lib.rs"),
            farm_std_src().join("sys/alloc/cricker/uheap.rs"),
        ),
        // The netd socket-contract wire format, verbatim, so the net PAL cannot drift from the
        // server it talks to (same discipline as the ABI and heap crates above).
        (
            root.join("user/src/netproto.rs"),
            farm_std_src().join("sys/pal/cricker/netproto.rs"),
        ),
        // The FS-service wire protocol (DECISIONS §27), so `std::fs`'s PAL cannot drift from the
        // server it opens files through. Same discipline as the three above.
        (
            root.join("crates/fs_proto/src/lib.rs"),
            farm_std_src().join("sys/pal/cricker/fsproto.rs"),
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

/// Add a `target_os = "cricker"` arm to std's `cfg_select!` dispatchers so they pick the cricker
/// backend, and add cricker to std's `build.rs` known-platform chain (so std is not
/// `restricted_std` and ordinary programs need no `#![feature]`). These string anchors couple us
/// to the pinned nightly's std internals; a rustc bump that reshapes them fails loudly here, which
/// is the intended tripwire (see notes/std.md).
fn std_patch_dispatch() -> bool {
    let sys = farm_std_src().join("sys");
    patch_after(
        &sys.join("pal/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        pub(crate) mod cricker;\n        pub use self::cricker::*;\n    }",
    ) && patch_after(
        &sys.join("alloc/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        use cricker as imp;\n    }",
    ) && patch_after(
        &sys.join("stdio/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        pub use cricker::*;\n    }",
    ) && patch_after(
        &sys.join("random/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        pub use cricker::fill_bytes;\n    }",
    ) && patch_after(
        &sys.join("thread/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        pub use cricker::{Thread, available_parallelism, current_os_id, set_name, sleep, yield_now, DEFAULT_MIN_STACK_SIZE};\n    }",
    ) && patch_after(
        &sys.join("time/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        use cricker as imp;\n    }",
    ) && patch_after(
        // net: TcpStream + outbound UdpSocket over the netd socket contract (milestone 27 phase
        // two). The first cfg_select in connection/mod.rs is the backend dispatcher; the cricker
        // arm precedes the `_ =>` unsupported fallback that phase one used. hostname has its own
        // `_ =>` fallback to unsupported, so it needs no arm.
        &sys.join("net/connection/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        pub use cricker::*;\n    }",
    ) && patch_after(
        // fs: File open/read/metadata over the FS-service contract (milestone 27 phase two). The
        // arm precedes the `_ =>` unsupported fallback phase one used, and mirrors the shape of
        // the other single-backend arms (`use cricker as imp`).
        &sys.join("fs/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod cricker;\n        use cricker as imp;\n    }",
    ) && patch_after(
        // io/error has no fallback arm; route cricker to the generic backend.
        &sys.join("io/error/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod generic;\n        pub use generic::*;\n    }",
    ) && patch_after(
        // Single-threaded, no native TLS: storage is a plain static (no_threads).
        &sys.join("thread_local/mod.rs"),
        "cfg_select! {",
        "    target_os = \"cricker\" => {\n        mod no_threads;\n        pub use no_threads::{EagerStorage, LazyStorage, thread_local_inner};\n        pub(crate) use no_threads::{LocalPointer, local_pointer};\n    }",
    ) && patch_after(
        // ... and the TLS-destructor guard is a no-op.
        &sys.join("thread_local/mod.rs"),
        "pub(crate) mod guard {\n    cfg_select! {",
        "        target_os = \"cricker\" => {\n            pub(crate) fn enable() {}\n        }",
    ) && patch_after(
        // std::env::consts::OS. `cfg_unordered!` turns each arm's cfg into the fallback's
        // exclusion set, so adding a cricker arm both defines OS and keeps the fallback off it.
        &sys.join("env_consts.rs"),
        "cfg_unordered! {",
        "#[cfg(target_os = \"cricker\")]\npub mod os {\n    pub const FAMILY: &str = \"\";\n    pub const OS: &str = \"cricker\";\n    pub const DLL_PREFIX: &str = \"\";\n    pub const DLL_SUFFIX: &str = \"\";\n    pub const DLL_EXTENSION: &str = \"\";\n    pub const EXE_SUFFIX: &str = \"\";\n    pub const EXE_EXTENSION: &str = \"\";\n}",
    ) && patch_after(
        // cricker has a real PAL: not restricted_std.
        &farm_std_src().parent().unwrap().join("build.rs"),
        "        || target_os == \"vexos\"\n",
        "        || target_os == \"cricker\"",
    )
}

/// **Build the `hellostd` demo for both custom targets** (milestone 27), via -Zbuild-std against
/// the patched `cricker-dev` toolchain. panic=abort and singlethread come from the target specs;
/// `compiler-builtins-mem` supplies memcpy/memset for the bare target.
///
/// `RUSTUP_TOOLCHAIN` is set explicitly rather than via `+cricker-dev`, because the cargo proxy
/// that launched this xtask already exports `RUSTUP_TOOLCHAIN=nightly`, which would override a
/// `+` selector and silently build std from the *unpatched* sysroot.
fn user_std() -> bool {
    if !std_src() {
        return false;
    }
    let manifest = s(workspace_root().join("user-std/Cargo.toml"));
    for triple in STD_TARGETS {
        let spec = s(workspace_root().join(format!("targets/{triple}.json")));
        let ok = Command::new("cargo")
            .env("RUSTUP_TOOLCHAIN", CRICKER_TOOLCHAIN)
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
            eprintln!("user-std: building hellostd for {triple} failed");
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
/// `builder`) and its shell boot boots `sysinit`.
fn boot_programs(arch: &str) -> &'static [&'static str] {
    match arch {
        "riscv64" => &["init", "sysinit"],
        _ => &["init"],
    }
}

/// Where the measurement manifest for an architecture is written. `kernel/build.rs` derives exactly
/// this path from `CARGO_CFG_TARGET_ARCH`, so the two stay in lockstep without an env var to forget.
fn measure_manifest_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/init-measure-{arch}.txt"))
}

/// Hash the boot-program entries out of the archive we just packed and write the manifest.
///
/// It parses the packed image back with `crickerfs` rather than hashing the input file, deliberately:
/// what must be measured is the bytes **the kernel will read out of the archive**, not the bytes we
/// meant to put in. If packing ever mangled an entry, this measures the mangling and the boot fails,
/// which is the correct direction to be wrong in.
fn write_measure_manifest(arch: &str, image: &[u8]) -> bool {
    let fs = match crickerfs::Fs::parse(image) {
        Ok(fs) => fs,
        Err(e) => {
            eprintln!("measure: the archive we just packed does not parse: {e:?}");
            return false;
        }
    };
    let mut text = format!(
        "# generated by cargo xtask; the boot programs this {arch} kernel image is built against\n"
    );
    for name in boot_programs(arch) {
        let Some(bytes) = fs.read(name) else {
            // Not every archive carries every boot program (the aarch64 one has no `sysinit`). A
            // name that is absent simply gets no measurement, and the kernel refuses to enter a
            // program it has no measurement for, so nothing is quietly waved through.
            continue;
        };
        let digest = measure::sha256(bytes);
        let hex = measure::hex(&digest);
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
// scanout even with no display backend. So the runners take a monitor socket (CRICKER_GPU_MON), and
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
    format!("/tmp/cricker-gpu-{arch}-{}.sock", std::process::id())
}

fn gpu_shot_path(arch: &str) -> PathBuf {
    workspace_root().join(format!("target/gpu-scanout-{arch}.ppm"))
}

/// Does this PPM hold the pattern the guest painted?
///
/// Compares against `gfx_proto::pixel`, the same definition the client painted from and the kernel
/// test digested against, so the host cannot disagree with the guest about what the pattern is. The
/// geometry must match too: a scanout of the wrong size is a `SET_SCANOUT` bug, not a near miss.
///
/// Returns `Err(reason)` rather than a bool so a mismatch says which pixel and what it should have
/// been, since "the screen is wrong" is otherwise the least actionable failure in graphics.
fn scanout_holds_the_pattern(ppm: &[u8]) -> Result<(), String> {
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
            let want = gfx_proto::pixel(x, y);
            let (r, g, b) = (
                ((want >> 16) & 0xff) as u8,
                ((want >> 8) & 0xff) as u8,
                (want & 0xff) as u8,
            );
            if (pixels[o], pixels[o + 1], pixels[o + 2]) != (r, g, b) {
                return Err(format!(
                    "pixel ({x},{y}) is rgb({},{},{}), the pattern says rgb({r},{g},{b})",
                    pixels[o],
                    pixels[o + 1],
                    pixels[o + 2],
                ));
            }
        }
    }
    Ok(())
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

/// **Run the kernel test suite for `arch` and prove the scanout while it runs.** `test_args` is the
/// cargo invocation the caller would otherwise have handed to [`run`].
///
/// The child inherits stdio, so the suite's output streams exactly as before; this only adds a poll
/// loop beside it. Returns false if the suite failed OR if the scanout never showed the pattern.
fn cargo_test_with_scanout_check(arch: &str, test_args: &[&str]) -> bool {
    let sock = gpu_mon_socket(arch);
    let shot = gpu_shot_path(arch);
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(&shot);

    unsafe { std::env::set_var("CRICKER_GPU_MON", &sock) };
    let mut child = match Command::new("cargo").args(test_args).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to run cargo: {e}");
            return false;
        }
    };

    let mut matched: Option<String> = None;
    let mut last_reason = String::from("no screendump was ever taken (did QEMU get a monitor?)");
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
        if matched.is_none()
            && screendump(&sock, &shot)
            && let Ok(bytes) = std::fs::read(&shot)
        {
            match scanout_holds_the_pattern(&bytes) {
                Ok(()) => matched = Some(format!("{}", shot.display())),
                Err(reason) => last_reason = reason,
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&sock);

    match matched {
        Some(path) => {
            eprintln!(
                "scanout check ({arch}): the {}x{} pattern reached the DEVICE's scanout, verified \
                 pixel for pixel against gfx_proto::pixel ({path})",
                gfx_proto::WIDTH,
                gfx_proto::HEIGHT,
            );
            true
        }
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
            false
        }
    }
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
/// child it loads) for the riscv target, and packs them into a crickerfs archive: `builder` under
/// the name `init` (the entry the kernel loads first), `worker` under `worker` (the one init loads by
/// name). Point `CRICKER_INITRD` at the result and boot the riscv kernel, e.g.:
///
/// ```text
/// cargo xtask initrd-riscv
/// CRICKER_INITRD=target/initrd-riscv.img cargo run -p kernel --target riscv64imac-unknown-none-elf
/// ```
fn initrd_riscv() -> bool {
    // Only the portable bins: hello/console/input/shell are aarch64-wired and do not build here.
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
            "elbench",
            "--bin",
            "coremark",
            "--bin",
            "sysinit",
            "--bin",
            "console",
            "--bin",
            "input",
            "--bin",
            "shell",
            "--bin",
            "termd",
            "--bin",
            "blk",
            "--bin",
            "allocdemo",
            "--bin",
            "netd",
            "--bin",
            "budgeter",
            "--bin",
            "fsclient",
            "--bin",
            "heeder",
            "--bin",
            "spinner",
            "--bin",
            "rootsup",
            "--bin",
            "spawner",
            "--bin",
            "subsup",
            "--bin",
            "flaky",
            "--bin",
            "gpud",
            "--bin",
            "painter",
            "--bin",
            "cwarden",
            "--bin",
            "cshim",
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
    // names. `sysinit`/`console`/`input`/`shell` are the interactive-shell system (parity D).
    let entries: &[(&str, &str)] = &[
        ("init", "builder"),
        ("worker", "worker"),
        ("driver", "driver"),
        ("elbench", "elbench"),
        ("coremark", "coremark"),
        ("sysinit", "sysinit"),
        ("console", "console"),
        ("input", "input"),
        ("shell", "shell"),
        ("termd", "termd"),
        ("blk", "blk"),
        ("allocdemo", "allocdemo"),
        ("netd", "netd"),
        ("budgeter", "budgeter"),
        ("fsclient", "fsclient"),
        ("heeder", "heeder"),
        ("spinner", "spinner"),
        // The authority-shrinking supervision tree (milestone 22 phase B.2): an init that hands its
        // construction authority to a spawner and its restart policy to a supervisor, then drops the
        // budget. Portable, so both archives carry all four.
        ("rootsup", "rootsup"),
        ("spawner", "spawner"),
        ("subsup", "subsup"),
        ("flaky", "flaky"),
        // The display pair (milestone 29): the confined virtio-gpu driver and the client that draws
        // into the surface it serves. Portable, so both archives carry both.
        ("gpud", "gpud"),
        ("painter", "painter"),
        // The C seam (milestone 36): the warden and the Rust shell that links user/c/cseam.c. The C
        // is compiled for this ISA by user/build.rs, so the riscv shell carries riscv C.
        ("cwarden", "cwarden"),
        ("cshim", "cshim"),
    ];
    let mut blobs: Vec<(&str, Vec<u8>)> = Vec::new();
    for &(archive_name, bin_name) in entries {
        match std::fs::read(bin(bin_name)) {
            Ok(b) => blobs.push((archive_name, b)),
            Err(e) => {
                eprintln!("initrd-riscv: cannot read {}: {e}", bin(bin_name));
                return false;
            }
        }
    }
    // The std demo (milestone 27), built through the cricker-dev toolchain for the riscv custom
    // target, rides along when present, exactly as on aarch64. `test` builds it first.
    if let Ok(bytes) = std::fs::read(hellostd_elf("riscv64-unknown-cricker")) {
        blobs.push(("hellostd", bytes));
    }
    // The FS server (milestone 32 phase 2), built for the riscv bare target, rides along when
    // present, exactly as hellostd does; `test` builds it first.
    if let Ok(bytes) = std::fs::read(fsserver_elf(RISCV_TARGET)) {
        blobs.push(("fsserver", bytes));
    }
    let files: Vec<(&str, &[u8])> = blobs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let size = crickerfs::image_size(&files);
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
        eprintln!("initrd-riscv: could not build the archive");
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
/// The initrd is a **crickerfs image**, the same format the virtio disk uses, so one parser serves
/// both the RAM archive and the disk. It holds `init` (the `hello` binary, which the kernel loads
/// and init re-enters at its remaining roles) plus the distinct binaries lifted out of hello:
/// `worker` (19f.2) and `console` (19f.3). The kernel reads the `init` entry to boot; init loads the
/// rest by name. Generated, not checked in, exactly like the disk and the flat kernel image: a blob
/// in git is a blob nobody can review.
fn mkinitrd() -> bool {
    let hello = match std::fs::read(user_elf()) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", user_elf());
            return false;
        }
    };
    let worker = match std::fs::read(bin_elf("worker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("worker"));
            return false;
        }
    };
    let console = match std::fs::read(bin_elf("console")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("console"));
            return false;
        }
    };
    let input = match std::fs::read(bin_elf("input")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("input"));
            return false;
        }
    };
    let shell = match std::fs::read(bin_elf("shell")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("shell"));
            return false;
        }
    };
    let coremark = match std::fs::read(bin_elf("coremark")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("coremark"));
            return false;
        }
    };
    let elbench = match std::fs::read(bin_elf("elbench")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("elbench"));
            return false;
        }
    };
    let termd = match std::fs::read(bin_elf("termd")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("termd"));
            return false;
        }
    };
    let allocdemo = match std::fs::read(bin_elf("allocdemo")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("allocdemo"));
            return false;
        }
    };
    let netd = match std::fs::read(bin_elf("netd")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("netd"));
            return false;
        }
    };
    let budgeter = match std::fs::read(bin_elf("budgeter")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("budgeter"));
            return false;
        }
    };
    let fsclient = match std::fs::read(bin_elf("fsclient")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("fsclient"));
            return false;
        }
    };
    let heeder = match std::fs::read(bin_elf("heeder")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("heeder"));
            return false;
        }
    };
    let spinner = match std::fs::read(bin_elf("spinner")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("spinner"));
            return false;
        }
    };
    // "init" is the hello binary (the kernel loads it, init re-enters it at its remaining roles);
    // "worker", "console", "input", "shell" are the split system binaries (19f.2-5), "termd" is
    // the line discipline between them (milestone 28), "coremark" is the compute workload (19e),
    // "elbench" is the EL0 microbenchmark program (primitive suite), and "allocdemo" proves the
    // user_rt heap (milestone 27). init (and the bench boot) load each by name. All are entries
    // in the one archive.
    // The authority-shrinking supervision tree (milestone 22 phase B.2), read as a group: four small
    // portable programs that share one module, packed under their own names for both ISAs.
    // The display pair (milestone 29) reads as a group for the same reason: the confined virtio-gpu
    // driver and the client that draws into the surface it serves, both portable.
    // The C seam (milestone 36) reads as a group too: the warden that builds, supervises, and checks
    // the foreign component, and the Rust shell that links it. Both portable.
    let mut tree: Vec<(&str, Vec<u8>)> = Vec::new();
    for name in [
        "rootsup", "spawner", "subsup", "flaky", "gpud", "painter", "cwarden", "cshim",
    ] {
        match std::fs::read(bin_elf(name)) {
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
        ("shell", &shell),
        ("termd", &termd),
        ("coremark", &coremark),
        ("elbench", &elbench),
        ("allocdemo", &allocdemo),
        ("netd", &netd),
        ("budgeter", &budgeter),
        ("fsclient", &fsclient),
        ("heeder", &heeder),
        ("spinner", &spinner),
    ];
    for (name, bytes) in &tree {
        files.push((name, bytes.as_slice()));
    }
    // The std demo (milestone 27) rides along IFF it has been built (`cargo xtask user-std`, which
    // `test` runs). It builds through a separate toolchain and target, so an interactive `run` that
    // never built it simply ships an initrd without it; nothing loads it there.
    let hellostd = std::fs::read(hellostd_elf("aarch64-unknown-cricker")).ok();
    if let Some(bytes) = &hellostd {
        files.push(("hellostd", bytes.as_slice()));
    }
    // The FS server (milestone 32 phase 2) rides along IFF built (its own workspace/target; `test`
    // builds it). Absent for a plain interactive boot, which simply skips the FS-server test.
    let fsserver = std::fs::read(fsserver_elf(TARGET)).ok();
    if let Some(bytes) = &fsserver {
        files.push(("fsserver", bytes.as_slice()));
    }
    let size = crickerfs::image_size(&files);
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
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

/// The packed initrd archive ([`initrd_path`]) is what `scripts/qemu-runner.sh` passes to QEMU as
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
        unsafe { std::env::set_var("CRICKER_ACCEL", "hvf") };
        eprintln!("--- on the real Apple Silicon core via Hypervisor.framework ---");
    }
}

/// Where the crickerfs disk image is written.
fn disk_path() -> String {
    workspace_root()
        .join("target/crickerfs.img")
        .display()
        .to_string()
}

/// The PCIe transport's copy of the disk image, a sibling of [`disk_path`]. Two files because
/// both transports are now attached **writable** (milestone 32's write path) and QEMU's image
/// locking refuses to attach one file to two devices once either attachment can write. The
/// runner derives this name from `CRICKER_DISK`, so the two stay in lockstep.
fn disk_pci_path() -> String {
    workspace_root()
        .join("target/crickerfs-pci.img")
        .display()
        .to_string()
}

/// Build the crickerfs disk images the virtio-blk driver will read and write.
///
/// **The disk is generated, not checked in**, the same way the flat kernel image is: a binary
/// blob in git is a blob nobody can review. The contents are a couple of tiny files, written
/// through the same `crickerfs::write_image` the userspace filesystem server reads back, so the
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
            b"cricker-os: read from a virtio disk, by a driver at EL0.\n",
        ),
        (
            "readme",
            b"this file came off a real block device through a userspace driver.\n",
        ),
        ("scratch", &[0u8; 512]),
    ];
    let size = crickerfs::image_size(&files).max(64 * 1024); // pad to a friendly size
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
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
// release so the initrd stays small. The test image is made HOST-side by the redoxfs-host tool,
// the same engine the server opens it with; the server never creates. See notes/fs-server.md.
// ===========================================================================================

/// Build the FS-server ELF for `triple`. Its own workspace, so it takes `--manifest-path` and its
/// artifacts land under `fs-server/target/`.
fn fs_server_build(triple: &str) -> bool {
    run(
        "cargo",
        &[
            "build",
            "--manifest-path",
            "fs-server/Cargo.toml",
            "--bin",
            "fsserver",
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
fn fsserver_elf(triple: &str) -> String {
    workspace_root()
        .join(format!("fs-server/target/{triple}/release/fsserver"))
        .display()
        .to_string()
}

/// Where the RedoxFS test image is written. The runners derive exactly this name from
/// `CRICKER_DISK` (`${CRICKER_DISK%.img}-redoxfs.img`), so the two stay in lockstep.
fn redoxfs_disk_path() -> String {
    workspace_root()
        .join("target/crickerfs-redoxfs.img")
        .display()
        .to_string()
}

/// Drive the redoxfs-host tool (its own workspace) by `--manifest-path`, quietly. Returns success.
fn redoxfs_host(args: &[&str]) -> bool {
    let mut v = vec![
        "run",
        "--quiet",
        "--manifest-path",
        "tools/redoxfs-host/Cargo.toml",
        "--",
    ];
    v.extend_from_slice(args);
    run("cargo", &v)
}

/// Build the RedoxFS test image the FS server serves: an empty filesystem with the two fixture
/// files (`motd`, `scratch`) the client reads and writes. Made host-side with the pinned engine, so
/// an image the server opens is proven against exactly the code that opens it. Arch-neutral (the
/// on-disk format does not depend on the CPU), so one image serves both ISA test legs.
fn mkredoxfs() -> bool {
    let img = redoxfs_disk_path();
    // Stage the fixture contents in temp files (the host tool's `put` takes a host file), then load
    // them. The contents live in fs_proto::fixture, shared with the client and the kernel test.
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
    redoxfs_host(&["mkfs", &img, "16"])
        && redoxfs_host(&["put", &img, fs_proto::fixture::MOTD_NAME, &motd])
        && redoxfs_host(&["put", &img, fs_proto::fixture::SCRATCH_NAME, &scratch])
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
}

/// `cat` one file out of the post-run image with the host tool and compare it byte for byte.
fn redoxfs_reads_back(name: &str, want: &[u8]) -> bool {
    let out = capture(
        "cargo",
        &[
            "run",
            "--quiet",
            "--manifest-path",
            "tools/redoxfs-host/Cargo.toml",
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
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has no parent directory")
        .to_path_buf()
}

/// Host tests first, then the kernel under QEMU.
///
/// The host crates (`dtb`, `frames`) hold the pure logic and run in *milliseconds* with no
/// emulator, so they fail fast and cheap. Only once they pass is it worth spending twenty
/// seconds booting QEMU. See DECISIONS.md §7.
fn test() -> bool {
    // Tests always run under TCG. They exit via semihosting, which QEMU only intercepts in the
    // TCG path; under HVF the `hlt #0xf000` traps to the guest and the harness hangs. TCG is also
    // the right place for reproducible tests: deterministic, and identical on any host.
    unsafe { std::env::remove_var("CRICKER_ACCEL") };
    eprintln!("--- host tests (pure logic, no emulator) ---");
    // Every host crate, not just two. `paging`, `heap` and `slab` each carry real tests and
    // were silently not being run here for four milestones.
    if !cargo(&[
        "test",
        "-p",
        "abi",
        "-p",
        "caps",
        "-p",
        "crickerfs",
        "-p",
        "dma_validate",
        "-p",
        "dtb",
        "-p",
        "elf",
        "-p",
        "frames",
        "-p",
        "gfx_proto",
        "-p",
        "xtask",
        "-p",
        "paging",
        "-p",
        "pci",
        "-p",
        "ipc",
        "-p",
        "linedisc",
        "-p",
        "measure",
        "-p",
        "slots",
        "-p",
        "uheap",
        "-p",
        "intrusive",
        "-p",
        "asid",
        "-p",
        "regions",
        "-p",
        "abi",
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
        &["test", "--manifest-path", "tools/redoxfs-host/Cargo.toml"],
    ) {
        return false;
    }
    // The FS server's sans-IO core (fs-server, its own workspace): open, read, write, close against
    // a real RedoxFS image in memory, in milliseconds. This proves the filesystem logic for BOTH the
    // read and write paths on the host, which the on-device test can only do for reads today.
    eprintln!();
    eprintln!("--- fs-server sans-IO core (host, its own workspace) ---");
    if !run(
        "cargo",
        &["test", "--manifest-path", "fs-server/Cargo.toml"],
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

    eprintln!();
    eprintln!("--- kernel tests, aarch64 (QEMU) ---");
    // Build the std demo (milestone 27) for both custom targets first, so both initrds carry it:
    // mkinitrd (inside `user`) packs the aarch64 hellostd, initrd_riscv packs the riscv one.
    if !user_std() {
        return false;
    }
    // The FS server (milestone 32 phase 2), for the aarch64 bare target, before `user()` so mkinitrd
    // packs it; then the RedoxFS test image the runner attaches as the second mmio disk.
    if !fs_server_build(TARGET) {
        return false;
    }
    if !user() || !mkdisk() || !mkredoxfs() {
        return false;
    }
    // Attach a virtio-gpu for the display test (milestone 29). Set here, in `test`, rather than in
    // `cargo()`: the benchmark boot uses the same runner and adding a device to it would change what
    // the icount instrument measures, so the GPU is a test-leg device only. Both ISA legs get it,
    // because parity is the gate (§19), and the display test ASSERTS the device is present rather
    // than skipping, so a leg that lost this line fails loudly.
    unsafe { std::env::set_var("CRICKER_GPU", "1") };
    // `cargo()` only exports the env the runner needs; the test itself runs under the scanout check,
    // which drives QEMU's monitor beside the suite and proves the pixels reached the device's scanout
    // rather than only the driver's frames.
    if !cargo(&["build", "-p", "kernel", "--target", TARGET])
        || !cargo_test_with_scanout_check("aarch64", &["test", "-p", "kernel", "--target", TARGET])
    {
        return false;
    }

    // The same booted kernel test suite on the second architecture (parity workstream B). The
    // portable tests (scheduler, capabilities, revocation, memory, sync) run on RISC-V's real Sv39
    // kernel; the aarch64-specific ones (the userspace-exec suite, the SGI interrupt tests, and SMP)
    // are gated to aarch64. RISC-V exits via the sifive_test finisher, same harness. See
    // notes/riscv-parity-scope.md.
    eprintln!();
    eprintln!("--- kernel tests, riscv64 (QEMU) ---");
    // The riscv userspace tests (parity C) load programs from the initrd and read the disk, so
    // build the riscv archive and point the runner at IT, not at the aarch64 archive `cargo()`
    // exports: the riscv ELF loader must never be handed aarch64 ELFs. The disk is arch-neutral
    // (a crickerfs data image) and was built by mkdisk() above.
    // The riscv FS server, before the riscv archive that packs it.
    if !fs_server_build(RISCV_TARGET) {
        return false;
    }
    if !initrd_riscv() {
        return false;
    }
    unsafe { std::env::set_var("CRICKER_INITRD", riscv_initrd_path()) };
    unsafe { std::env::set_var("CRICKER_DISK", disk_path()) };
    unsafe { std::env::set_var("CRICKER_NET", "1") }; // a virtio-net NIC for the net test (m30)
    if !cargo_test_with_scanout_check(
        "riscv64",
        &["test", "-p", "kernel", "--target", RISCV_TARGET],
    ) {
        return false;
    }

    // FS-level consistency after the runs (milestone 32 phase 2): reopen the RedoxFS image with the
    // host tool and confirm the FS server's write persisted and the filesystem still parses. Both
    // ISA legs wrote the same pattern to the same image; the write survives, so it reads back.
    eprintln!();
    eprintln!("--- redoxfs image consistency after the run (host tool) ---");
    redoxfs_check_after_run()
}

/// The microbenchmarks (milestone 21; design/roadmap.md §21).
///
/// Two instruments:
/// - default: TCG with `-icount`, where virtual time is a deterministic function of instructions
///   executed. Counts are exact and reproducible; `--check` diffs them against
///   `bench/baseline.txt` and fails on drift, `--save` rewrites the baseline (a deliberate act,
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

    // For --smp, build the FS server (before user(), so mkinitrd packs the fsserver ELF) and the
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
        cmd.env("CRICKER_ACCEL", "hvf");
        if smp {
            // The full machine, for the aggregate-throughput bench. The per-core primitive magnitudes
            // in this same run are then NOT per-core clean (the reap-heavy ones, spawn_el0 and
            // spawn_reap, inflate and go noisy under cross-core reap lag); read those from the default
            // single-hart run instead. See notes/benchmarks.md, the multi-hart section.
            // "4" matches the kernel's MAX_CPUS and the runner's default; the throughput bench reads
            // the actual online count at runtime, so this only needs to be more than one.
            cmd.env("CRICKER_SMP", "4");
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
            cmd.env("CRICKER_SMP", "1");
            eprintln!(
                "--- bench: HVF, single hart, per-core magnitudes (statistical; medians matter) ---"
            );
        }
    } else {
        cmd.env_remove("CRICKER_ACCEL");
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
        cmd.env("CRICKER_SMP", "1");
        eprintln!(
            "--- bench: aarch64, single hart, TCG + icount (deterministic instruction counts) ---"
        );
    }
    cmd.env("CRICKER_INITRD", initrd_path());
    cmd.env("CRICKER_DISK", disk_path());

    run_bench(
        cmd,
        real,
        check,
        save,
        workspace_root().join("bench/baseline.txt"),
    )
}

/// **The RISC-V benchmark path** (parity E's follow-up). Same primitive suite, same deterministic
/// icount instrument, on the second architecture, so the tick counts are directly comparable to the
/// aarch64 ones: both are the virtual timer advancing under `-icount`, which is instruction-clocked,
/// not wall-clock. No HVF (there is no RISC-V hypervisor on this host) and no disk (the bench boot
/// runs no virtio); it just needs the riscv initrd carrying `elbench` + `coremark`. Its baseline is a
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

    let mut cmd = Command::new("scripts/qemu-runner-riscv.sh");
    cmd.arg(format!("target/{RISCV_TARGET}/debug/kernel"));
    // icount pins virtual time (rdtime) to the instruction stream; sleep=off so it never waits on the
    // wall clock. This is what makes the riscv counts deterministic and comparable to aarch64's.
    cmd.args(["-icount", "shift=0,sleep=off"]);
    cmd.env("CRICKER_INITRD", riscv_initrd_path());
    // One hart: a primitive benchmark measures per-core cost. With more harts, a thread that waits
    // for a spawned child leaves its hart idling in `wfi`, and under `-icount` a `wfi` jumps virtual
    // time to the next timer tick, inflating the spawn primitives to timer-quantized nonsense. The
    // single-core costs are what compare to aarch64 anyway.
    cmd.env("CRICKER_SMP", "1");
    eprintln!(
        "--- bench: riscv64, single hart, TCG + icount (deterministic instruction counts) ---"
    );

    run_bench(
        cmd,
        false,
        check,
        save,
        workspace_root().join("bench/baseline-riscv.txt"),
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
        let mut out = String::from(
            "# bench/baseline.txt: deterministic icount tick counts (cargo xtask bench --save).
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
        for line in text.lines().filter(|l| !l.starts_with('#')) {
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
    unsafe { std::env::set_var("CRICKER_INITRD", initrd_path()) };
    unsafe { std::env::set_var("CRICKER_DISK", disk_path()) };
    // Attach a virtio-net NIC too (milestone 30): slirp needs no host file, so it is always on for
    // tests, and the net driver's DHCP round-trip test exercises it.
    unsafe { std::env::set_var("CRICKER_NET", "1") };

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
}
