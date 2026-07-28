//! Same linker contract as the `user` crate: the shared link.ld (linked at 0x40_0000, explicit
//! W^X PHDRS), because a std program is still an ordinary cricker-os ELF the loader maps.
//! `-u _start` belts-and-braces the entry: `_start` lives in std's rlib (the PAL), and forcing
//! it undefined guarantees the archive member is extracted whatever the linker's ENTRY timing.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo::rerun-if-changed=../user/link.ld");
    println!("cargo::rustc-link-arg=-T{dir}/../user/link.ld");
    println!("cargo::rustc-link-arg=-u_start");
    println!("cargo::rustc-link-arg=--build-id=none");
}
