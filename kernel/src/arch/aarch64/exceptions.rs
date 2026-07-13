//! Exception handling.
//!
//! One mechanism serves three purposes on aarch64, and it is worth seeing that they
//! are the same thing before we build the other two:
//!
//!   - a **fault** (bad memory access, illegal instruction)  <- milestone 2, here
//!   - an **interrupt** (the timer, the UART)                <- milestone 5
//!   - a **syscall** (`svc` from userspace)                  <- milestone 7
//!
//! All three suspend the current instruction stream, switch to EL1, and jump to an
//! address the kernel chose. Only the reason differs. Build the plumbing once.
//!
//! See notes/exceptions.md.

use crate::println;
use aarch64_cpu::asm::barrier;
use aarch64_cpu::registers::{ESR_EL1, FAR_EL1, VBAR_EL1};
use core::sync::atomic::{AtomicUsize, Ordering};
use tock_registers::interfaces::{Readable, Writeable};

/// The interrupted CPU state, as saved by `SAVE_CONTEXT` in `vectors.s`.
///
/// This layout is a **contract with assembly**. The compiler cannot check it for us,
/// so there is a size assertion below, which catches about half of the ways to get it
/// wrong. The other half (reordering two fields of the same type) it cannot catch, so
/// be careful.
#[repr(C)]
pub struct TrapFrame {
    /// `x0` through `x30`. `x30` is the link register.
    pub x: [u64; 31],

    /// Where the interrupted code will resume.
    ///
    /// **Writable, and that matters.** Advancing this is how we step past a `brk`.
    /// Milestone 7 will use it to skip past an `svc`. The hardware reloads the program
    /// counter from here on `eret`, so whatever we leave in this field is where the
    /// world continues.
    pub elr: u64,

    /// The processor state (condition flags, exception level, interrupt masks) the
    /// interrupted code was in. `eret` restores it.
    pub spsr: u64,

    /// The frame must be a multiple of 16 bytes, because `sp` must stay 16-byte
    /// aligned. See notes/stack.md.
    _pad: u64,
}

// If this fails, `SAVE_CONTEXT` and `TrapFrame` have drifted apart, and the Rust side
// is about to read the wrong bytes.
const _: () = assert!(size_of::<TrapFrame>() == 272);

/// How many `brk` instructions we have caught and stepped over.
///
/// Exists so the tests can prove the handler actually ran, rather than proving only
/// that we didn't crash.
pub static BRK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Exception Class: `ESR_EL1` bits 31:26.
///
/// The single most useful field in the machine when something has gone wrong. It says
/// *what kind* of thing happened, and everything else is detail.
mod ec {
    pub const UNKNOWN: u64 = 0x00;
    pub const TRAPPED_WFI_WFE: u64 = 0x01;
    pub const ILLEGAL_EXECUTION_STATE: u64 = 0x0e;
    pub const SVC64: u64 = 0x15;
    pub const TRAPPED_MSR_MRS: u64 = 0x18;
    pub const INSTRUCTION_ABORT_LOWER_EL: u64 = 0x20;
    pub const INSTRUCTION_ABORT_SAME_EL: u64 = 0x21;
    pub const PC_ALIGNMENT_FAULT: u64 = 0x22;
    pub const DATA_ABORT_LOWER_EL: u64 = 0x24;
    pub const DATA_ABORT_SAME_EL: u64 = 0x25;
    pub const SP_ALIGNMENT_FAULT: u64 = 0x26;
    pub const SERROR: u64 = 0x2f;
    pub const BREAKPOINT_LOWER_EL: u64 = 0x30;
    pub const BREAKPOINT_SAME_EL: u64 = 0x31;
    pub const BRK64: u64 = 0x3c;
}

fn ec_name(class: u64) -> &'static str {
    match class {
        ec::UNKNOWN => "Unknown reason",
        ec::TRAPPED_WFI_WFE => "Trapped WFI/WFE",
        ec::ILLEGAL_EXECUTION_STATE => "Illegal execution state",
        ec::SVC64 => "SVC (syscall) from AArch64",
        ec::TRAPPED_MSR_MRS => "Trapped system register access",
        ec::INSTRUCTION_ABORT_LOWER_EL => "Instruction abort from a lower EL",
        ec::INSTRUCTION_ABORT_SAME_EL => "Instruction abort from the same EL",
        ec::PC_ALIGNMENT_FAULT => "PC alignment fault",
        ec::DATA_ABORT_LOWER_EL => "Data abort from a lower EL",
        ec::DATA_ABORT_SAME_EL => "Data abort from the same EL",
        ec::SP_ALIGNMENT_FAULT => "SP alignment fault",
        ec::SERROR => "SError",
        ec::BREAKPOINT_LOWER_EL => "Breakpoint from a lower EL",
        ec::BREAKPOINT_SAME_EL => "Breakpoint from the same EL",
        ec::BRK64 => "BRK instruction",
        _ => "unrecognized exception class",
    }
}

/// The sixteen slots in the vector table, in hardware order. See `vectors.s`.
const VECTOR_NAMES: [&str; 16] = [
    "Current EL, SP_EL0, Synchronous",
    "Current EL, SP_EL0, IRQ",
    "Current EL, SP_EL0, FIQ",
    "Current EL, SP_EL0, SError",
    "Current EL, SP_ELx, Synchronous",
    "Current EL, SP_ELx, IRQ",
    "Current EL, SP_ELx, FIQ",
    "Current EL, SP_ELx, SError",
    "Lower EL, AArch64, Synchronous",
    "Lower EL, AArch64, IRQ",
    "Lower EL, AArch64, FIQ",
    "Lower EL, AArch64, SError",
    "Lower EL, AArch32, Synchronous",
    "Lower EL, AArch32, IRQ",
    "Lower EL, AArch32, FIQ",
    "Lower EL, AArch32, SError",
];

/// Install the vector table.
///
/// After this returns, a fault produces a legible report instead of a silent death.
/// Until it returns, it doesn't.
pub fn init() {
    unsafe extern "C" {
        static exception_vectors: core::ffi::c_void;
    }

    let base = (&raw const exception_vectors) as u64;
    VBAR_EL1.set(base);

    // An Instruction Synchronization Barrier makes the CPU discard everything it has
    // already fetched or speculated past this point and start again.
    //
    // Without it, the write to VBAR_EL1 is not architecturally guaranteed to be in
    // effect for the *very next instruction*. And "the very next instruction" is
    // exactly when a fault might arrive. This is one line, it is easy to leave out,
    // and leaving it out produces a bug that appears only under timing you cannot
    // reproduce.
    barrier::isb(barrier::SY);
}

/// Called from every vector entry, with the saved state and which slot fired.
///
/// `extern "C"` because assembly calls it: `frame` arrives in `x0`, `index` in `x1`,
/// per AAPCS64. See notes/registers.md.
#[unsafe(no_mangle)]
extern "C" fn exception_dispatch(frame: &mut TrapFrame, index: u64) {
    let esr = ESR_EL1.get();
    let class = (esr >> 26) & 0x3f;

    match class {
        ec::BRK64 => {
            // `brk` is a deliberate trap: a breakpoint the program asked for.
            //
            // The subtlety: ELR_EL1 points AT the `brk` instruction, not past it.
            // (Compare `svc`, where the hardware already advances it for you.) So if
            // we just `eret`, we execute the `brk` again, forever.
            //
            // Stepping over it means advancing ELR by one instruction, and every
            // aarch64 instruction is exactly 4 bytes. That fixed-width design we
            // liked in notes/aarch64.md is what makes this a `+= 4` instead of a
            // decode.
            BRK_COUNT.fetch_add(1, Ordering::Relaxed);
            frame.elr += 4;
        }

        // Everything else is, for now, fatal. As the kernel grows, cases move out of
        // `fatal` and into real handlers: IRQs at milestone 5, `svc` at milestone 7,
        // and data aborts become page faults at milestone 4 (where most of them will
        // be *recoverable*: the page was swapped out, or copy-on-write needs to
        // duplicate it).
        _ => fatal(frame, index, esr),
    }
}

/// Print everything we know and stop.
///
/// A kernel with a violated invariant has no business continuing, so this does not
/// return. But it prints first, because a silent death teaches you nothing.
fn fatal(frame: &TrapFrame, index: u64, esr: u64) -> ! {
    let class = (esr >> 26) & 0x3f;
    let name = VECTOR_NAMES
        .get(index as usize)
        .copied()
        .unwrap_or("<vector index out of range>");

    println!();
    println!("[EXCEPTION]  {name}");
    println!("             {} (EC {:#04x})", ec_name(class), class);
    println!();
    println!("  ESR_EL1   {esr:#018x}   what happened");

    // FAR_EL1 only holds a meaningful address for aborts and alignment faults. For
    // anything else it is stale garbage from an earlier fault, and printing it as if
    // it meant something would be a lie.
    let far_is_meaningful = matches!(
        class,
        ec::INSTRUCTION_ABORT_LOWER_EL
            | ec::INSTRUCTION_ABORT_SAME_EL
            | ec::DATA_ABORT_LOWER_EL
            | ec::DATA_ABORT_SAME_EL
            | ec::PC_ALIGNMENT_FAULT
    );
    if far_is_meaningful {
        println!(
            "  FAR_EL1   {:#018x}   the address that faulted",
            FAR_EL1.get()
        );
    } else {
        println!("  FAR_EL1   (not meaningful for this exception class)");
    }

    println!("  ELR_EL1   {:#018x}   the instruction that did it", frame.elr);
    println!("  SPSR_EL1  {:#018x}   the state it was in", frame.spsr);
    println!();

    for row in 0..8 {
        let a = row * 4;
        print_reg_row(frame, a);
    }
    println!();

    panic!("unhandled exception: {}", ec_name(class));
}

/// Four registers per line, x28..x30 on the short last row.
fn print_reg_row(frame: &TrapFrame, first: usize) {
    use crate::print;
    print!("  ");
    for i in first..(first + 4).min(31) {
        print!("x{:<2} {:#018x}  ", i, frame.x[i]);
    }
    println!();
}
