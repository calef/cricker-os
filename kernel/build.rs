//! Tell the linker to use our layout instead of the default one.
//!
//! `cargo:rustc-link-arg` applies to binaries *and* test binaries, which is what we
//! want: the test build has to boot in QEMU too, so it needs the same layout.

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    println!("cargo::rerun-if-changed=link.ld");
    println!("cargo::rerun-if-changed=src/arch/aarch64/boot.s");
    println!("cargo::rustc-link-arg=-T{manifest_dir}/link.ld");
}
