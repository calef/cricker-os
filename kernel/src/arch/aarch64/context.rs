//! **The saved register context of a thread, and how a new one is faked.** The Rust half of
//! `context.s`.
//!
//! This is arch-specific by nature: a saved context *is* a particular CPU's callee-saved register
//! set, and starting a new thread means writing those registers by hand so the first `switch_to`
//! into it lands running. The portable `thread.rs` names none of this; it asks for a context "for a
//! kernel thread" or "for a user thread" and gets back an opaque [`Context`] it only ever stores and
//! hands to [`switch_to`]. That is the seam that lets a second architecture (RISC-V, with a
//! different callee-saved set and different trampolines) drop in without touching `thread.rs`. See
//! notes/riscv-port.md, the `Context` leak.

/// The callee-saved registers, as `switch_to` pushes them.
///
/// **This layout is a contract with `context.s`.** Twelve `u64`s, 96 bytes, in exactly this
/// order. Reorder a field here and the assembly restores the wrong register into the wrong
/// place, and the thread resumes with a frame pointer where its return address should be.
///
/// The fields are deliberately private: the two ways a fresh context is ever built ([`for_kernel_thread`](Context::for_kernel_thread),
/// [`for_user_thread`](Context::for_user_thread)) live here, so which register carries the closure,
/// the entry, or the trampoline is knowledge that never escapes `arch/`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// `x19`. **Doubles as the first argument to a brand-new thread**: each trampoline reads its
    /// entry payload out of it. A callee-saved register, chosen exactly because `switch_to`
    /// restores it for us on the way in.
    x19: u64,
    x20: u64,
    x21: u64,
    x22: u64,
    x23: u64,
    x24: u64,
    x25: u64,
    x26: u64,
    x27: u64,
    x28: u64,
    /// `x29`, the frame pointer. Zero for a new thread: it is the bottom of the backtrace.
    x29: u64,
    /// `x30`, the link register. **Where `switch_to`'s `ret` jumps.** For a new thread this is a
    /// trampoline, which is how a thread that has never run gets started by the same instruction
    /// that resumes one that has.
    x30: u64,
}

const _: () = assert!(size_of::<Context>() == 96);

impl Context {
    /// The initial context for a brand-new **kernel** thread. `switch_to`'s first `ret` lands in
    /// `thread_trampoline`, which reads the closure pointer out of `x19` and the monomorphized
    /// caller out of `x20` (the closure's concrete type was erased, so the address says *where* and
    /// the shim says *how*). See `Thread::spawn` and context.s.
    pub fn for_kernel_thread(closure_at: u64, call_shim: u64) -> Self {
        Context {
            x19: closure_at,
            x20: call_shim,
            x21: 0,
            x22: 0,
            x23: 0,
            x24: 0,
            x25: 0,
            x26: 0,
            x27: 0,
            x28: 0,
            x29: 0, // no caller: the backtrace ends here
            x30: thread_trampoline as *const () as u64,
        }
    }

    /// The initial context for a brand-new **user** thread (milestone 19c.3). `switch_to`'s first
    /// `ret` lands in `user_entry_trampoline`, which moves `x19` to the EL0 entry, `x20` to the user
    /// `sp`, and `x21`..`x23` to the child's initial `x0`..`x2`, then drops to EL0. See
    /// `Thread::arm_for_start`.
    pub fn for_user_thread(entry: u64, user_sp: u64, args: [u64; 3]) -> Self {
        Context {
            x19: entry,
            x20: user_sp,
            x21: args[0],
            x22: args[1],
            x23: args[2],
            x24: 0,
            x25: 0,
            x26: 0,
            x27: 0,
            x28: 0,
            x29: 0,
            x30: user_entry_trampoline as *const () as u64,
        }
    }
}

unsafe extern "C" {
    /// Save our callee-saved registers, swap `sp`, restore theirs, and `ret` into **their**
    /// `x30`. See context.s: the last instruction returns to a different thread.
    pub fn switch_to(prev_context: *mut *mut Context, next_context: *mut Context);

    /// The first-run landing pad for a kernel thread. Referenced only by [`Context::for_kernel_thread`];
    /// the register contract it depends on lives entirely in this module and context.s.
    fn thread_trampoline();

    /// The first-run landing pad for a user thread (the EL0 mirror). Referenced only by
    /// [`Context::for_user_thread`].
    fn user_entry_trampoline();
}
