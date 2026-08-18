//! The `getrandom` backend for nife (milestone 64, **rank 1** of the measured gap list).
//!
//! # The name, and the three that lost
//!
//! **`entropy_backend`**, calef's, ruled 2026-08-18 after asking what the tree's other entropy
//! crates are called. They are `entropy` (the service that holds the virtio-rng capability) and
//! `entropy_proto` (its contract), so **entropy** is this tree's word for the domain and a third
//! participant takes the same stem.
//!
//! - **`getrandom_backend`** was the lane's provisional name and the strongest rival: it is
//!   upstream's own word for the slot, spelled exactly that way in the `--cfg` flag, so the reader
//!   who arrives here is searching for it. It lost because it names the slot rather than the thing,
//!   and because every other crate in this tree is named for what it is.
//! - **`getrandom_nife`** was considered while this lived under `patches/`, where `std-nife` set an
//!   `<upstream>-<ours>` precedent. It lost with the directory: see below.
//! - **`entropy_shim`** is accurate about the shape and `shim` is a word this tree does not
//!   otherwise use, so it would have been the only one.
//!
//! # Why this is not in `patches/`, and not in `crates/` either
//!
//! It sat in `patches/getrandom-nife/` until 2026-08-18, and that was the actual defect rather than
//! the name. `patches/README.md` states what that directory is for: **patches carried against
//! upstream projects, one file per patch, in `git format-patch` form, each existing to be
//! upstreamed**, leaving when a pin advances past a release containing the fix. This crate changes
//! nothing of `getrandom`'s and there is nothing to upstream, so it was never a patch. (`std-nife`
//! is the directory's one other exception and it *is* modifying upstream source, as an overlay.)
//!
//! `crates/` is wrong too, for a duller reason: everything there is a workspace member, and this
//! cannot be one. It needs `std`, so it builds only for a `*-unknown-nife` target under the linked
//! `nife-dev` toolchain with build-std, and it needs `--cfg getrandom_backend="custom"`, which the
//! workspace crates must never inherit.
//!
//! So it sits at the tree root as its own workspace, beside `std_exerciser`, which is already
//! exactly that shape: our code, its own workspace, outside the kernel's shipping graph.
//!
//! # BUGS
//!
//! - **`script/names` cannot see this crate, and could not see it under `patches/` either.** That
//!   tool derives its table from three locations: `crates/<name>/src/lib.rs`, `user/src/<name>.rs`
//!   and `script/<name>`. A root-level workspace is none of them, so `script/names entropy_backend`
//!   answers "neither a name in the tree nor a recorded refusal", and it says the same of
//!   `std_exerciser`, which has sat there far longer. So the provenance block above is the whole
//!   record, and the naming worklist will never list this crate as unratified. Pre-existing and
//!   wider than this crate; recorded here because milestone 115's point was that a name's
//!   provenance lives beside the name, and here it is the *only* place it lives.
//!
//! # Why a whole crate exists for eleven lines
//!
//! `getrandom` is the crate the Rust ecosystem gets its randomness from, and almost nothing that
//! needs random bytes reaches past it: `rand`, `uuid`, `ring`, every `gix-*` crate that hashes an
//! object, `zip`. It picks its source by `target_os`, in one `cfg_if!` ladder in its own
//! `src/backends.rs`, and the ladder ends in `compile_error!` rather than a fallback.
//!
//! There is no `nife` arm, so **eight of the eleven crates.io probes that failed in milestone 64's
//! measurement failed here and nowhere else** (notes/crates-io-on-nife.md). Not one of them failed
//! for a reason that had anything to do with this operating system: `std::random::SystemRng` has
//! worked since milestone 56 (DECISIONS §44), reaching a real virtio-rng through the entropy
//! service. The bytes were there the whole time; the ecosystem just had no name for them.
//!
//! # What this is
//!
//! `getrandom` documents an escape hatch for exactly this case: build with
//! `--cfg getrandom_backend="custom"` and define one `extern "Rust"` function. This crate is that
//! function, and its body is a `SystemRng` draw. So the chain a crate like `uuid` ends up on is:
//!
//! ```text
//! uuid -> getrandom -> __getrandom_v03_custom (here) -> std::random::SystemRng
//!      -> the entropy service (one endpoint, naming no device) -> virtio-rng
//! ```
//!
//! Every link is one this project already owns except the middle one, which is why the fix is this
//! small.
//!
//! # How to use it
//!
//! **Three** things in the consuming crate, and the third is the one that is easy to miss. Depend on
//! this crate:
//!
//! ```toml
//! [dependencies]
//! entropy_backend = { path = "../entropy_backend" }
//! ```
//!
//! select the backend in the workspace's own `.cargo/config.toml`:
//!
//! ```toml
//! [build]
//! rustflags = ["--cfg", "getrandom_backend=\"custom\""]
//! ```
//!
//! and **name the crate in the binary, for its side effect**:
//!
//! ```text
//! use entropy_backend as _;
//! ```
//!
//! That last line is not tidiness, and this file said the opposite until the linker corrected it. An
//! rlib nothing references is not linked, so without it the build gets all the way to
//! `rust-lld: error: undefined symbol: __getrandom_v03_custom` and stops. It is the same shape as a
//! panic handler or a global allocator in a `no_std` binary: a crate that exists only to define a
//! symbol somebody else declares has to be pulled in on purpose. A crate that happens to call
//! `getrandom` on a path the linker keeps will link it anyway, which is why two of the eight probes
//! passed without the line and six did not; do not rely on that.
//!
//! The measurement note called the `RUSTFLAGS` route "a setting every consumer has to remember and
//! that a workspace cannot express per-dependency", and half of that was wrong: a workspace states
//! it **once, in a file**, and per-workspace is the right granularity anyway, because which entropy
//! source exists is a property of the target rather than of any one dependency.
//!
//! # Why not vendor a patched `getrandom` instead
//!
//! It was the other candidate, and it loses on the thing this tree cares about. A
//! `[patch.crates-io]` fork works with no `rustflags` at all, which is genuinely nicer at the call
//! site, and costs a fork of a crate that moves (`getrandom` was mid-transition across 0.2, 0.3 and
//! 0.4 in a single dependency graph when this was measured). DECISIONS §46's rule is that we vendor
//! when correctness is won by exposure and write when we would otherwise be maintaining someone
//! else's code; a backend hook the upstream crate designed for this purpose is neither, and it is
//! eleven lines that upstream cannot break without also breaking Hermit's.
//!
//! **The right long-term fix is upstream**, and it is a smaller diff than this file: `getrandom`
//! already carries a `hermit.rs` arm selected on `target_os = "hermit"`, which is the same shape
//! this project's `std` took from Hermit in the first place. That is a pull request against
//! `getrandom`, not a change to this tree, and until it lands this is what makes `rand` build.
//!
//! # BUGS
//!
//! - **`getrandom` 0.2 is not covered.** It selects a custom backend through the
//!   `register_custom_getrandom!` macro rather than a bare symbol, so the two shapes cannot be
//!   satisfied by one definition. `ring` 0.17 is the probe that pulls 0.2, and it fails on C sources
//!   before this would matter to it, so nothing currently needs the second shape. Something will.
//! - **The symbol is `__getrandom_v03_custom` for both 0.3 and 0.4**, which reads like a typo and is
//!   not: `getrandom` 0.4's own `backends/custom.rs` still declares the v03 name. A graph holding
//!   both versions therefore resolves both to this one definition, and their `Error` types are
//!   layout-identical, so it links and works. It is not something either crate promises.
//! - **A draw panics when the process holds no entropy capability**, because `SystemRng` does
//!   (milestone 56 chose a panic over a silently weaker stream, deliberately). `getrandom`'s
//!   signature can carry an error and this cannot produce one, so the panic passes through. A
//!   program that uses `rand` must be granted entropy, and there is no degraded mode.
//! - **Nothing in this tree's gate builds this crate**, because building it needs the `nife-dev`
//!   toolchain, a custom target, and a `getrandom` dependency. It is exercised by the probe harness
//!   in notes/crates-io-on-nife.md and by whatever program takes the dependency.

#![feature(random)]

use core::slice;
use std::random::{Rng, SystemRng};

/// `getrandom`'s custom-backend hook, filling `len` bytes at `dest` from [`SystemRng`].
///
/// # Safety
///
/// `getrandom`'s contract: `dest` is valid for writes of `len` bytes, and on `Ok(())` every one of
/// them must be initialized. `fill_bytes` writes the whole slice or panics, so there is no partial
/// path that could return `Ok` over uninitialized memory.
///
/// [`SystemRng`]: std::random::SystemRng
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // SAFETY: the caller's contract, restated above. `getrandom` never passes a null pointer with a
    // non-zero length, and a zero length yields an empty slice, which `from_raw_parts_mut` allows
    // for any aligned non-null pointer; `getrandom` passes the address of a real buffer either way.
    let buf = unsafe { slice::from_raw_parts_mut(dest, len) };
    SystemRng.fill_bytes(buf);
    Ok(())
}
