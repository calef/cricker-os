//! What a capability in cricker-os can point at.
//!
//! The table itself is `crates/caps`, which is pure logic and knows nothing about this file.
//! This is the kernel's half: **the set of nouns.**
//!
//! DECISIONS.md §10 and notes/capabilities.md.

/// Every kind of thing a process can be handed.
///
/// **One entry, today.** That is not a placeholder, it is the shape of the argument: a process
/// that has been handed a `Console` and nothing else can print, and **cannot express** anything
/// further. There is no `open`, no path, no uid, and so no second thing for it to reach for.
///
/// The list grows deliberately, and each addition is a decision:
///
/// - `Endpoint` at 7e, which is where IPC arrives and where processes can start talking to each
///   other instead of only to the kernel.
/// - `Frame` when shared memory does, because **IPC carries control and shared memory carries
///   data** (§10), and a frame capability in a message is how the data moves without a copy.
/// - `Untyped` at milestone 11, if we take §10's deferred axis, at which point the kernel stops
///   allocating and this enum stops being the interesting part of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    /// The console.
    ///
    /// **Kernel-served, and only until milestone 8.** Today invoking this capability lands in the
    /// kernel, which owns the PL011. At milestone 8 the driver leaves, this becomes an `Endpoint`
    /// to a userspace console *server*, and **the kernel stops knowing what a UART is.**
    ///
    /// That is the milestone that proves §10 was real rather than a syscall table with an unusual
    /// shape, and this variant is the thing it has to delete.
    Console,
}

pub type Cap = caps::Cap<Object>;
pub type CSpace = caps::CSpace<Object>;

pub use caps::{Error, Rights};

/// What a process gets when it is trusted with the console and nothing else.
pub fn console_cap() -> Cap {
    Cap {
        object: Object::Console,
        // WRITE, and **not GRANT**: it may print, and it may not lend the right to print to
        // anyone else. Which is a distinction Unix cannot even express.
        rights: Rights::WRITE,
    }
}
