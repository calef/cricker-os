//! The FS-server EL0 binary is an ordinary cricker-os ELF, so it links against the SAME linker
//! script the `user` crate and `user-std` use (linked at 0x40_0000, explicit W^X PHDRS). The
//! link args are scoped to the `fsserver` bin (`rustc-link-arg-bin`), never the lib or the host
//! test binary, which are plain host artifacts and must link the host way.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo::rerun-if-changed=../user/link.ld");
    println!("cargo::rustc-link-arg-bin=fsserver=-T{dir}/../user/link.ld");
    // `_start` lives in this bin, but forcing it undefined is the same belt-and-braces user-std
    // uses so the entry survives whatever the linker's ENTRY timing.
    println!("cargo::rustc-link-arg-bin=fsserver=-u_start");
    // Keep the ELF an ELF: the kernel's loader wants program headers, not a build-id note.
    println!("cargo::rustc-link-arg-bin=fsserver=--build-id=none");
}
