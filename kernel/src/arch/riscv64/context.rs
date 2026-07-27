//! **The saved register context of a thread, RISC-V.** The Rust half of `context.s`, and the
//! second implementation of the seam introduced when HAL leak #1 was closed (notes/riscv-port.md):
//! `thread.rs` asks for a context "for a kernel thread" or "for a user thread" and never names a
//! register; this module owns which RISC-V register carries the closure, the entry, or the return.
//!
//! Where aarch64 saves `x19`..`x30`, RISC-V saves its own callee-saved set: `ra` (the return
//! address, where `switch_to`'s `ret` lands) and `s0`..`s11`. `sp` is the context pointer itself,
//! so it is not a field. See context.s for the matching push/pop order.

/// The callee-saved registers, as `switch_to` pushes them.
///
/// **This layout is a contract with `context.s`.** Fourteen `u64` slots, 112 bytes (a multiple of
/// 16, so `sp` stays aligned as the RISC-V calling convention requires), in exactly this order.
/// Thirteen are live registers; the last is padding for that alignment. Reorder a field and the
/// assembly restores the wrong register.
///
/// The fields are private: the two ways a fresh context is ever built live here, so which register
/// carries what never escapes `arch/`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// `ra`, the return address. **Where `switch_to`'s `ret` jumps.** For a new thread this is a
    /// trampoline, so a thread that has never run starts by the same instruction that resumes one
    /// that has.
    ra: u64,
    /// `s0`, the frame pointer. **Doubles as the first payload for a brand-new thread** (the closure
    /// pointer, or the U-mode entry address); zero once a thread is running, as the bottom of the
    /// backtrace. A callee-saved register, restored by `switch_to` on the way in.
    s0: u64,
    s1: u64,
    s2: u64,
    s3: u64,
    s4: u64,
    s5: u64,
    s6: u64,
    s7: u64,
    s8: u64,
    s9: u64,
    s10: u64,
    s11: u64,
    /// Padding to 112 bytes so the saved frame keeps `sp` 16-byte aligned. Never read.
    _pad: u64,
}

const _: () = assert!(size_of::<Context>() == 112);

impl Context {
    /// The initial context for a brand-new **kernel** thread. `switch_to`'s first `ret` lands in
    /// `thread_trampoline`, which reads the closure pointer out of `s0` and the monomorphized caller
    /// out of `s1` (the closure's concrete type was erased, so the address says *where* and the shim
    /// says *how*), then calls the portable `thread_entry`. See context.s.
    pub fn for_kernel_thread(closure_at: u64, call_shim: u64) -> Self {
        Context {
            ra: thread_trampoline as *const () as u64,
            s0: closure_at,
            s1: call_shim,
            s2: 0,
            s3: 0,
            s4: 0,
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            s9: 0,
            s10: 0,
            s11: 0,
            _pad: 0,
        }
    }

    /// The initial context for a brand-new **user** thread. `switch_to`'s first `ret` lands in
    /// `user_entry_trampoline`, which moves `s0` to the U-mode entry, `s1` to the user `sp`, and
    /// `s2`..`s4` to the child's initial `a0`..`a2`, then drops to U-mode via the portable
    /// `user_thread_entry`.
    pub fn for_user_thread(entry: u64, user_sp: u64, args: [u64; 3]) -> Self {
        Context {
            ra: user_entry_trampoline as *const () as u64,
            s0: entry,
            s1: user_sp,
            s2: args[0],
            s3: args[1],
            s4: args[2],
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            s9: 0,
            s10: 0,
            s11: 0,
            _pad: 0,
        }
    }
}

unsafe extern "C" {
    /// Save our callee-saved registers, swap `sp`, restore theirs, and `ret` into **their** `ra`.
    /// See context.s: the last instruction returns to a different thread.
    pub fn switch_to(prev_context: *mut *mut Context, next_context: *mut Context);

    /// The first-run landing pad for a kernel thread. Referenced only by [`Context::for_kernel_thread`].
    fn thread_trampoline();

    /// The first-run landing pad for a user thread. Referenced only by [`Context::for_user_thread`].
    fn user_entry_trampoline();
}
