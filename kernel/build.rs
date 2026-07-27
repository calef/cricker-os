//! Tell the linker to use our layout instead of the default one.
//!
//! `cargo:rustc-link-arg` applies to binaries *and* test binaries, which is what we
//! want: the test build has to boot in QEMU too, so it needs the same layout.

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    // Each architecture has its own memory map (aarch64 links high with an AT()-relocated load
    // address; riscv links at the OpenSBI payload address). The linker script is arch-specific, so
    // it is selected here rather than hard-coded. See notes/riscv-port.md.
    let (link_script, boot_asm) = match arch.as_str() {
        "aarch64" => ("link.ld", "src/arch/aarch64/boot.s"),
        "riscv64" => ("link-riscv.ld", "src/arch/riscv64/boot.s"),
        other => panic!("cricker-os has no linker script for target arch {other}"),
    };

    println!("cargo::rerun-if-changed={link_script}");
    println!("cargo::rerun-if-changed={boot_asm}");
    println!("cargo::rustc-link-arg=-T{manifest_dir}/{link_script}");
}
