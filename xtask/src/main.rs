//! Build orchestration for cricker-os.
//!
//! A normal Rust binary that runs on the *host*. Building a kernel means a custom
//! target, a linker script, and driving QEMU with the right flags, none of which fits
//! neatly into `cargo build`. This beats a Makefile because it's Rust and it composes.
//! See DECISIONS.md §7.
//!
//!     cargo xtask run      boot the kernel, print to this terminal
//!     cargo xtask test     host tests (milliseconds), then the kernel under QEMU
//!     cargo xtask gdb      boot paused, waiting for a debugger on :1234
//!     cargo xtask objdump  disassemble the kernel
//!     cargo xtask image    build the flat arm64 Image and dump its header
//!
//! Note that `run` and `test` do NOT invoke QEMU themselves. They just call cargo,
//! which invokes `scripts/qemu-runner.sh` via the runner setting in
//! `.cargo/config.toml`. That script is the single source of truth for how the kernel
//! gets booted, so there is exactly one place to get the QEMU flags wrong.

use std::process::{Command, ExitCode};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const RUNNER: &str = "scripts/qemu-runner.sh";

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    let ok = match cmd.as_str() {
        "build" => build(),
        "run" => {
            // `cargo xtask run --hvf` boots under Apple's Hypervisor.framework instead of TCG.
            if std::env::args().any(|a| a == "--hvf") {
                unsafe { std::env::set_var("CRICKER_ACCEL", "hvf") };
                eprintln!("--- booting under HVF (Apple Silicon hardware virtualization) ---");
            }
            cargo(&["run", "-p", "kernel", "--target", TARGET])
        }
        "test" => test(),
        "gdb" => gdb(),
        "objdump" => objdump(),
        "image" => image(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!("usage: cargo xtask <build|run|test|gdb|objdump|image>");
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
    cargo(&["build", "-p", "user", "--target", TARGET])
}

/// The user ELF, which `scripts/qemu-runner.sh` passes to QEMU as `-initrd`.
///
/// **Deliberately the same road Linux's initramfs travels.** QEMU loads the file into RAM and
/// writes its address into `/chosen/linux,initrd-start` in the device tree; the kernel finds it
/// there (`memory::initrd_region`, built at milestone 3 for exactly this). Nothing about the
/// binary is known to the kernel at build time, which is the entire point of milestone 7c.
/// Where the crickerfs disk image is written.
fn disk_path() -> String {
    workspace_root().join("target/crickerfs.img").display().to_string()
}

/// Build the crickerfs disk image the virtio-blk driver will read.
///
/// **The disk is generated, not checked in**, the same way the flat kernel image is: a binary
/// blob in git is a blob nobody can review. The contents are a couple of tiny files, written
/// through the same `crickerfs::write_image` the userspace filesystem server reads back, so the
/// format has exactly one definition.
fn mkdisk() -> bool {
    let files: [(&str, &[u8]); 2] = [
        ("motd", b"cricker-os: read from a virtio disk, by a driver at EL0.\n"),
        ("readme", b"this file came off a real block device through a userspace driver.\n"),
    ];
    let size = crickerfs::image_size(&files).max(64 * 1024); // pad to a friendly size
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
        eprintln!("mkdisk: could not build the image");
        return false;
    }
    if let Err(e) = std::fs::write(disk_path(), &img) {
        eprintln!("mkdisk: could not write {}: {e}", disk_path());
        return false;
    }
    true
}

fn user_elf() -> String {
    // ABSOLUTE, and that is not fussiness.
    //
    // Cargo runs the runner script with the working directory set to the **package** dir for
    // `cargo test` and the workspace root for `cargo run`. A relative path therefore resolved
    // under `cargo run` and silently did not under `cargo test`, so the tests booted with no
    // initrd at all and the one that noticed was the one that panicked.
    workspace_root()
        .join(format!("target/{TARGET}/debug/hello"))
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
        "test", "-p", "abi", "-p", "caps", "-p", "crickerfs", "-p", "dtb", "-p", "elf", "-p",
        "frames", "-p", "heap", "-p", "paging", "-p", "slab",
    ]) {
        return false;
    }
    eprintln!();
    eprintln!("--- kernel tests (QEMU) ---");
    if !user() || !mkdisk() {
        return false;
    }
    cargo(&["test", "-p", "kernel", "--target", TARGET])
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
    format!("target/{TARGET}/debug/kernel")
}

fn cargo(args: &[&str]) -> bool {
    // The runner needs to know where the initrd is. Set it for every cargo invocation; the
    // script ignores it when the file is not there (which is any build before `user` exists).
    unsafe { std::env::set_var("CRICKER_INITRD", user_elf()) };
    unsafe { std::env::set_var("CRICKER_DISK", disk_path()) };

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
