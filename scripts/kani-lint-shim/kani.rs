//! Enough of Kani's API for `clippy` to type-check the proof harnesses (milestone 113).
//!
//! `cfg(kani)` is set by the model checker and by nothing else, so until this existed the tree's
//! `#[cfg(kani)]` verification modules were compiled by no gate at all: not by `script/lint`'s
//! thirteen clippy passes, and not by `cargo test`. The consequence was measured rather than
//! guessed: **13 undocumented `unsafe` sites and 13 other clippy warnings**, in code that exists to
//! decide what the proofs are about. See notes/unsafe-obligations.md.
//!
//! `script/lint` compiles this with `rustc`, then runs a fourteenth clippy pass with `--cfg kani`
//! and `--extern kani=` pointing here. The alternative was `-D warnings` on the `script/verify`
//! build, which is correct by construction and finds **none of the 26**: `cargo kani` drives a
//! rustc, not a clippy-driver, so no `clippy::` lint can fire there at all.
//!
//! # What this is not
//!
//! **It is not Kani, and a clean lint pass is not a proof.** The surface below is the five things
//! the harnesses use ([`any`], [`assume`], `cover!`, `proof`, `unwind`), with the semantics deleted:
//! [`any`] returns nothing, [`assume`] constrains nothing.
//!
//! **It is deliberately looser than the real thing in one place.** Kani's `any` requires
//! `T: Arbitrary`; this one takes any `T` at all, because a lint gate must never reject code the
//! model checker accepts. The direction of the error matters: a harness that only *this* accepts
//! fails under `cargo kani`, loudly, at the one place anybody would look.
//!
//! # BUGS
//!
//! **A harness that reaches for Kani API this does not have breaks the lint pass, not the proof.**
//! `kani::proof_for_contract`, `#[derive(kani::Arbitrary)]` and `kani::any_where` are all real and
//! all absent here. The failure is a compile error naming the missing item, which is a fine way to
//! find out; the fix is to add the item below rather than to drop the pass. The surface was five
//! items across 21 crates and 108 harnesses when this was written, so growth is expected to be rare
//! and cheap.
//!
//! **Names are provisional**, per CLAUDE.md's naming tenet, except for `kani` itself: the harnesses
//! write `kani::any`, so the crate has exactly one possible name.

#![no_std]

pub use kani_attributes::{proof, unwind};

/// A symbolic value. Under Kani this is the whole point; here it only has to type-check.
///
/// Unbounded in `T` on purpose: see the module's "looser than the real thing" note.
pub fn any<T>() -> T {
    unimplemented!("the lint shim's `kani::any` is never executed")
}

/// A constraint on the symbolic state. Under Kani it prunes the search; here it does nothing.
pub fn assume(_condition: bool) {}

/// `kani::cover!(cond)`: an assertion that the state is *reachable*, which is how the harnesses
/// check that a bound has not quietly made a proof vacuous. Evaluated and discarded here, so the
/// expression is still type-checked and its variables still count as used.
#[macro_export]
macro_rules! cover {
    ($condition:expr $(, $message:literal)? $(,)?) => {
        let _ = $condition;
    };
}
