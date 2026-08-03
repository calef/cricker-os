//! Fuzz the device-tree parser with arbitrary bytes.
//!
//! **Why this target exists.** The device tree is the one parse in this system that runs before
//! anything else: `kernel/src/main.rs` reads it from the pointer firmware left in a register, on
//! both ISAs, before there is a frame allocator, a scheduler, or a way to report a failure. The
//! bytes are written by QEMU, by OpenSBI, or by a board's firmware, and none of those is us. A panic
//! here is a kernel that cannot boot and cannot say why.
//!
//! **What it adds over the Kani proofs.** `crates/dtb`'s four harnesses prove the *leaf readers*
//! total: `be32` and `be64` never panic for any offset into any buffer. They deliberately do not
//! reach the seven walkers above them, because a walker is an unbounded loop over a symbolic blob
//! and that is where bounded model checking stops. The walkers are where the state lives: a depth
//! counter, two 16-entry per-depth cell-count stacks, a "which node am I inside" slot. Every one of
//! those is indexed by a number the blob controls.
//!
//! That is not a hypothetical division of labour. This target found a real out-of-bounds index in
//! `node_reg` within seconds of its first run; see `crates/dtb/tests/hostile.rs`.
//!
//! **Every accessor, not just `from_bytes`.** A blob that parses is not a blob that is safe to walk,
//! and the kernel calls all seven of these on the same blob during boot.

#![no_main]

use dtb::{Dtb, Region};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(dtb) = Dtb::from_bytes(data) else {
        return;
    };

    // Small on purpose. A caller in the kernel passes a fixed array off its own stack, so the
    // interesting paths are the ones where the output runs out and the parser has to say
    // `TooManyRegions` instead of writing past the end.
    let mut out = [Region { start: 0, size: 0 }; 8];

    let _ = dtb.total_size();
    let _ = dtb.reserved_regions(&mut out);
    let _ = dtb.memory_regions(&mut out);
    let _ = dtb.reserved_memory_regions(&mut out);
    let _ = dtb.initrd();

    // The three lookups that take a caller-supplied needle. The kernel's real needles are what the
    // arch layer asks for: `intc`/`plic` for the interrupt controller, `pl031`/`goldfish-rtc` for
    // the clock, `virtio_mmio` for the transport. The empty prefix is the cheapest way to reach the
    // decode path at all, because it matches the first node at depth >= 2 whatever the blob calls
    // it.
    //
    // **It is also the needle that cannot reach the deep-nesting bug**, and that is worth stating
    // where somebody might otherwise assume a fixed list is thorough: matching stops at the first
    // candidate, so an empty prefix always matches at depth 2. Reaching depth 17 needs a name that
    // matches ONLY down there, which means the fuzzer has to build seventeen levels of nesting and
    // name the bottom one `intc...`. Ten minutes did not get there; a reader did. See
    // notes/fuzzing.md's BUGS section.
    for prefix in [&b""[..], b"intc", b"plic", b"memory", b"virtio_mmio"] {
        let _ = dtb.node_reg(prefix, &mut out);
        let _ = dtb.node_prop(prefix, b"reg");
        let _ = dtb.node_prop(prefix, b"compatible");
    }
    for compat in [
        &b""[..],
        b"arm,pl031",
        b"google,goldfish-rtc",
        b"riscv,plic0",
    ] {
        let _ = dtb.node_reg_compatible(compat, &mut out);
    }

    // `Region::end()` is a `pub fn` on a `pub struct` whose fields came out of the blob, so it has
    // to hold on its own. Called on whatever the walkers actually produced rather than on invented
    // numbers, so a region the parser is willing to return is a region this is willing to add up.
    if let Ok(n) = dtb.memory_regions(&mut out) {
        for r in &out[..n] {
            let _ = r.end();
        }
    }
});
