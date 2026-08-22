//! E3's footprint-perturbation experiment (milestone 134,
//! design/roadmap/134-the-measurements-that-decide.md): pad the IPC fastpath's call graph with a
//! large, reachable-but-never-taken function, so the fastpath's *measured* footprint
//! (`script/fastpath-footprint`) roughly doubles while the *executed* work on a benchmarked round
//! trip stays exactly what it was. Liedtke's claim is that footprint costs cache, not that any
//! particular byte is slow; padding tests that without a cache counter, by making the tool that
//! already measures footprint (milestone 132) disagree or agree with a real latency change.
//!
//! Gated on the `fastpath_pad` Cargo feature (kernel/Cargo.toml): this module, the call into it,
//! and the per-arch nop sleds it calls do not exist in an ordinary build, so nothing here costs a
//! normal boot anything. `crate::sched::ipc_send` calls [`maybe_pad`] unconditionally when the
//! feature is on; whether that call actually reaches the padding is decided at runtime by
//! [`core::hint::black_box`], not by a flag anyone sets, which is the property that makes the
//! padding *resident* (present in `.text`, walked by `script/fastpath-footprint`'s static closure)
//! while staying *dead* (never executed, so a benchmark run with the feature on does exactly the
//! IPC work a run without it does, plus this one guard).
//!
//! The guard itself is not free: `black_box(false)` still costs a real compare-and-branch on every
//! `ipc_send`, in both the padded and the un-padded-by-flag sense, because the feature being *on*
//! is what adds it, whether or not the branch is ever taken. That is a real, if small, confound on
//! E3's comparison and it is named in this milestone's roadmap doc rather than hidden: a purer
//! design would need the compiler to see "always false" at compile time, which is exactly the
//! condition LLVM is licensed to delete, undoing the whole point. `black_box` is the standard way
//! to keep a dead branch reachable without also making it free; the residual cost is on the order
//! of a nanosecond, which the observed IPC round trip (low microseconds under HVF) comfortably
//! dwarfs. See this file's callers for where the guard sits.

/// Call from a root the fastpath-footprint closure walks (today: [`crate::sched::ipc_send`]).
/// Branches into the arch's nop sled only if [`core::hint::black_box`] fails to prove its input
/// false, which it never does, so this costs one untaken branch and nothing else at runtime.
#[inline(never)]
pub fn maybe_pad() {
    if core::hint::black_box(false) {
        // SAFETY: the arch nop sled is a leaf function (no memory access, no stack frame beyond
        // its own `ret`) reached with the platform's ordinary call ABI; it touches nothing this
        // caller owns. It is also never actually called (see the module doc), so its body has no
        // runtime effect to be unsound about; this `unsafe` covers the FFI call shape only.
        unsafe { crate::arch::fastpath_pad_body() };
    }
}
