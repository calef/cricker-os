//! The two Kani attribute macros, as pass-throughs, so `script/lint` can compile the proof
//! harnesses without Kani installed (milestone 113). Half of the shim; the other half is `kani.rs`
//! beside it, which re-exports these and supplies `any`, `assume` and `cover!`.
//!
//! **Names are provisional** (CLAUDE.md: Chris names crates, programs and modules). The `kani` crate
//! next door has no choice about its name, since `#[kani::proof]` is what the harnesses write; this
//! one is internal to the shim and could be called anything.
//!
//! An attribute macro can only come from a proc-macro crate, which is the whole reason the shim is
//! two crates rather than one. Registering `kani` as a tool namespace instead
//! (`-Zcrate-attr=register_tool(kani)`) does not work: the tool name loses to the extern crate the
//! harnesses also need for `kani::any`, and rustc then reports `cannot find proof in kani`. Measured,
//! not assumed.
//!
//! Compiled by `script/lint` with a plain `rustc` invocation, not by cargo. It is deliberately not a
//! workspace package: nothing should be able to depend on it, and a dependency named `kani` in a
//! harness crate's manifest would collide with the real library that `cargo kani` injects.

extern crate proc_macro;

use proc_macro::TokenStream;

/// `#[kani::proof]`: keep the function, and silence the dead-code warning it would otherwise earn.
///
/// A harness is called by nothing (Kani finds it by attribute), so under `-D warnings` every one of
/// the 108 harnesses in the tree would fail the lint pass as dead code. The cost of the blanket is
/// stated plainly: this pass cannot see a genuinely unused *harness* either. It can still see an
/// unused helper beside one, which is the case worth catching.
#[proc_macro_attribute]
pub fn proof(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut out: TokenStream = "#[allow(dead_code)]".parse().unwrap();
    out.extend(item);
    out
}

/// `#[kani::unwind(N)]`: the loop bound, which only the model checker has any use for.
#[proc_macro_attribute]
pub fn unwind(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
