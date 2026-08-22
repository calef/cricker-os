//! The aarch64 half of E3's padding (see `kernel/src/fastpath_pad.rs`): one function, nothing
//! but `nop`, sized to roughly double `bench/fastpath-aarch64.txt`'s recorded `ipc_fastpath`
//! (5,792 bytes at the time this was written). 1,448 `nop`s at 4 bytes each is 5,792 bytes plus
//! one `ret`, so the padded closure lands within a few bytes of exactly double; re-measure with
//! `script/fastpath-footprint --features fastpath_pad --arch aarch64` after touching this file or
//! the fastpath it is attached to, since the target is the *current* baseline, not a constant.
//!
//! Written in `global_asm!` rather than a Rust loop because the whole point is *static* footprint:
//! a `for` loop over `nop`s is a handful of instructions regardless of the count, and would pad
//! nothing this tool can see. A `.rept` block is bytes on disk, which is what
//! `script/fastpath-footprint` sums.
use core::arch::global_asm;

unsafe extern "C" {
    /// The nop sled defined below. Never actually called (see `crate::fastpath_pad::maybe_pad`);
    /// declared so the reachable-but-dead call exists in the compiled binary for
    /// `script/fastpath-footprint`'s static closure walk to find.
    pub fn fastpath_pad_body();
}

global_asm!(
    "
    .pushsection .text.fastpath_pad_body,\"ax\",%progbits
    .global fastpath_pad_body
    .type fastpath_pad_body, %function
fastpath_pad_body:
    .rept 1448
    nop
    .endr
    ret
    .popsection
    "
);
