//! Build orchestration for cricker-os.
//!
//! A normal Rust binary that runs on the *host*. Building a kernel means a custom
//! target, a linker script, and driving QEMU with the right flags, none of which
//! fits neatly into `cargo build`. This beats a Makefile because it's Rust and it
//! composes. See DECISIONS.md §7.
//!
//!     cargo xtask run      boot the kernel, print to this terminal
//!     cargo xtask test     run the kernel's tests under QEMU
//!     cargo xtask gdb      boot paused, waiting for a debugger on :1234
//!     cargo xtask objdump  disassemble the kernel

use std::process::{Command, ExitCode};

const TARGET: &str = "aarch64-unknown-none-softfloat";

const QEMU_ARGS: &[&str] = &[
    "-machine",
    "virt",
    "-cpu",
    "cortex-a72",
    "-nographic",
    "-semihosting",
];

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    let ok = match cmd.as_str() {
        "build" => cargo(&["build", "-p", "kernel", "--target", TARGET]),
        "run" => cargo(&["run", "-p", "kernel", "--target", TARGET]),
        "test" => cargo(&["test", "-p", "kernel", "--target", TARGET]),
        "objdump" => objdump(),
        "gdb" => gdb(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!("usage: cargo xtask <build|run|test|gdb|objdump>");
            return ExitCode::FAILURE;
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cargo(args: &[&str]) -> bool {
    run("cargo", args)
}

/// Boot the kernel with QEMU frozen and a GDB stub listening.
///
/// `-s` opens the stub on :1234, `-S` holds the CPU before the first instruction.
/// The kernel ELF carries symbols and DWARF, so GDB can show you Rust source lines
/// rather than raw addresses (notes/elf.md). This is the tool that will save you at
/// milestone 4, when the MMU comes on and `println!` stops being an option.
fn gdb() -> bool {
    if !cargo(&["build", "-p", "kernel", "--target", TARGET]) {
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

    let mut args: Vec<&str> = QEMU_ARGS.to_vec();
    args.extend_from_slice(&["-kernel", &elf, "-s", "-S"]);
    run("qemu-system-aarch64", &args)
}

fn objdump() -> bool {
    if !cargo(&["build", "-p", "kernel", "--target", TARGET]) {
        return false;
    }
    run(
        "rust-objdump",
        &["-d", "--no-show-raw-insn", "-M", "no-aliases", &kernel_elf()],
    )
}

fn kernel_elf() -> String {
    format!("target/{TARGET}/debug/kernel")
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
