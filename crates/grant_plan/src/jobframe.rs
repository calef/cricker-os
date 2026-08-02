//! **The job frame: the shell's shared-memory channel to a supervised foreground child** (m24).
//!
//! The cooperative interrupt tier (DECISIONS §24) needs the shell to signal a *running* child and
//! the child to notice *between work units*. An endpoint cannot do this: there is no non-blocking
//! receive, so a child busy computing cannot poll one, and a blocking receive would stall the very
//! computation the user wants to interrupt. So the cooperative channel is a single page the shell
//! and the child both map, and the signal is a word the child reads with a plain load. Control by
//! shared memory here, deliberately, because the primitive that would carry it as a message
//! (a non-blocking notification) does not exist yet. See notes/grant-expression.md.
//!
//! The layout is four `u64` words at fixed byte offsets. The shell writes [`INTERRUPT`]; the child
//! writes [`DONE`], [`STATUS`], and [`HEARTBEAT`]. One writer per word, so no locking is needed:
//! each side reads the other's words and writes only its own.
//!
//! ```text
//!   byte 0   INTERRUPT   shell -> child   set to 1 on the cooperative ^C; the child polls it
//!   byte 8   DONE        child -> shell   set to 1 when the child has finished and is about to exit
//!   byte 16  STATUS      child -> shell   why it finished: STATUS_INTERRUPTED or STATUS_DONE
//!   byte 24  HEARTBEAT   child -> shell   incremented each work unit: liveness the shell can show
//! ```

/// Byte offset of the interrupt flag (shell writes 1, child polls).
pub const INTERRUPT: usize = 0;
/// Byte offset of the done flag (child writes 1 as it exits; the shell watches it).
pub const DONE: usize = 8;
/// Byte offset of the exit-status word (child writes one of the `STATUS_*` codes).
pub const STATUS: usize = 16;
/// Byte offset of the heartbeat counter (child increments each work unit).
pub const HEARTBEAT: usize = 24;

/// The child stopped because it saw the cooperative interrupt and cleaned up. The graceful case.
pub const STATUS_INTERRUPTED: u64 = 1;
/// The child ran to natural completion without being interrupted.
pub const STATUS_DONE: u64 = 2;
