//! What a capability in cricker-os can point at.
//!
//! The table itself is `crates/caps`, which is pure logic and knows nothing about this file.
//! This is the kernel's half: **the set of nouns.**
//!
//! DECISIONS.md §10 and notes/capabilities.md.

/// Every kind of thing a process can be handed.
///
/// **One entry, and that is the milestone-8 result rather than a stub.** There used to be a
/// `Console` variant: the kernel owned the PL011 and printed on a user's behalf. Milestone 8
/// deleted it. The console is now a userspace server reached by `SEND` on an endpoint, so
/// everything a process can name is an endpoint, and **the kernel no longer knows what a UART
/// is** on any path a user program can take.
///
/// The list grows deliberately, and each addition is a decision:
///
/// - `Frame`, when shared memory gets a capability of its own, because **IPC carries control and
///   shared memory carries data** (§10). Today a shared buffer is mapped in at spawn time rather
///   than handed over as a capability; a `Frame` object is what makes delegating memory a
///   runtime operation.
/// - `Untyped` at milestone 11, if we take §10's deferred axis, at which point the kernel stops
///   allocating and this enum stops being the interesting part of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    /// An IPC endpoint, by id (an index into the scheduler's endpoint table).
    ///
    /// Invoking it is a `SEND` or a `RECV` (which one you may do is a matter of rights). Since
    /// milestone 8 this is how a process reaches the console: it holds a `WRITE` capability on
    /// the console server's endpoint, and printing is sending.
    Endpoint(usize),
}

pub type Cap = caps::Cap<Object>;
pub type CSpace = caps::CSpace<Object>;

pub use caps::{Error, Rights};

/// A capability naming an endpoint, with the given rights.
///
/// **`WRITE` lets the holder `SEND`; `READ` lets it `RECV`.** Hand the two ends of one endpoint
/// out with opposite rights and you have a one-way pipe that neither side can run backwards.
pub fn endpoint_cap(ep: usize, rights: Rights) -> Cap {
    Cap {
        object: Object::Endpoint(ep),
        rights,
    }
}
