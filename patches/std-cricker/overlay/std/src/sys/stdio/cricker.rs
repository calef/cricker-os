//! stdout/stderr over the **sink contract** (`crates/sink_proto`, milestone 50), on the endpoint
//! the std slot convention puts in slot 1 (see `pal/cricker/rt.rs`): SEND, register-only, up to 16
//! bytes per message.
//!
//! Bulk data over a register-only IPC is deliberate, and since milestone 50 it is forced rather
//! than merely cheap. The three-word message is the ABI's whole fastpath (notes/abi.md §2) and
//! std's own `LineWriter` batches user writes, but the real argument is substitutability: a sink
//! that also required a page mapped at an agreed address could not be swapped for another by
//! putting a different capability in this slot, and *that* is what redirection is here. See
//! notes/sink-protocol.md.
//!
//! stdin is honest EOF: nothing grants a std program input yet. The sink contract is one-directional
//! by construction, and a source contract is its own design.
//!
//! stderr shares the endpoint, so out and err interleave by 16-byte chunk. Recorded as a caveat in
//! notes/std.md; one endpoint is what the contract grants today.
//!
//! # A dead reader ends this program, and a missing one does not
//!
//! This used to swallow every failed SEND, on the grounds that "a program without a console still
//! runs, it just prints into the void, which is what every OS does to a process whose stdout is
//! closed". That is right for a program with no console and wrong for a pipeline, where `yes | head`
//! must end when `head` exits. `sink_proto::classify` splits the two, and each half is routed
//! through the seam std already has for exactly this:
//!
//! - **No sink** ([`sink_proto::Sent::NoSink`]) is [`io::ErrorKind::Unsupported`], which
//!   [`is_ebadf`] accepts, so `io::stdio`'s `handle_ebadf` swallows it and the program keeps
//!   running. It is also the same answer every other ungranted capability gives in this PAL.
//! - **A destroyed sink** ([`sink_proto::Sent::Gone`]) is [`io::ErrorKind::BrokenPipe`], which
//!   `is_ebadf` rejects, so it propagates and `println!` panics. The target is panic=abort, so the
//!   process dies and the kernel attributes the fault. That is what a Rust program on Linux does
//!   when its reader exits, and it is milestone 50's "`SIGPIPE` becomes a return code".

use crate::io;
use crate::sys::pal::cricker::{rt, sinkproto};

pub struct Stdin;
pub struct Stdout;
pub type Stderr = Stdout;

/// The refusal that means "this program holds no output stream". Not an error the caller should
/// act on; `is_ebadf` turns it back into success one layer up.
fn no_sink() -> io::Error {
    io::const_error!(
        io::ErrorKind::Unsupported,
        "this program was not granted an output sink"
    )
}

/// The refusal that must end the program: there *was* a sink and it has been destroyed.
fn sink_gone() -> io::Error {
    io::const_error!(
        io::ErrorKind::BrokenPipe,
        "the sink this program was writing to no longer exists"
    )
}

/// One message on the sink. Returns how many bytes it carried.
fn send_chunk(bytes: &[u8]) -> io::Result<usize> {
    let (w0, w1, w2, n) = sinkproto::pack(bytes);
    match sinkproto::classify(rt::send(rt::STDOUT_SLOT, w0, w1, w2)) {
        sinkproto::Sent::Ok => Ok(n),
        sinkproto::Sent::Gone => Err(sink_gone()),
        sinkproto::Sent::NoSink => Err(no_sink()),
    }
}

/// **Tell the sink the stream is over.** Called from the PAL's `cleanup`, which std's runtime runs
/// after `main` returns and after it has flushed stdout, so this is the last thing on the wire.
///
/// A file sink closes its handle here and the reader of a pipe stops reading; without it a reader
/// would block forever on a writer that has exited. Errors are dropped on purpose: a program on its
/// way out cannot act on one, and a sink that is already gone has already heard everything it needs.
pub fn end_of_stream() {
    let _ = rt::send(rt::STDOUT_SLOT, sinkproto::eof(), 0, 0);
}

impl Stdin {
    pub const fn new() -> Stdin {
        Stdin
    }
}

impl io::Read for Stdin {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl Stdout {
    pub const fn new() -> Stdout {
        Stdout
    }
}

impl io::Write for Stdout {
    /// Writes as much as fits and reports it, which is what `Write` asks for; `write_all` loops.
    /// A partial write that then fails is reported as the bytes that landed, so the caller cannot
    /// mistake a torn write for a whole one.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < buf.len() {
            match send_chunk(&buf[written..]) {
                Ok(n) => written += n,
                Err(e) if written == 0 => return Err(e),
                Err(_) => break,
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub const STDIN_BUF_SIZE: usize = 0;

/// **The seam that decides whether a failed print is fatal.** `io::stdio`'s `handle_ebadf` swallows
/// an error this accepts and propagates everything else, and a propagated error makes `println!`
/// panic. This used to return `true` unconditionally, which is why the old code's "print into the
/// void" applied to a dead sink as well as a missing one.
pub fn is_ebadf(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Unsupported
}

pub fn panic_output() -> Option<impl io::Write> {
    Some(Stdout::new())
}
