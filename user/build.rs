//! The user program gets its own linker script, and it must NOT be the kernel's.
//!
//! `cargo:rustc-link-arg` is per-package, which is the whole reason this is a separate crate
//! rather than another binary in `kernel/`.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo::rerun-if-changed=link.ld");
    println!("cargo::rustc-link-arg=-T{dir}/link.ld");

    // Keep the ELF an ELF. The kernel's loader wants program headers, not a flat blob.
    println!("cargo::rustc-link-arg=--build-id=none");
}
