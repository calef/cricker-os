//! stdout/stderr over the console endpoint, per the std slot convention (pal/cricker/rt.rs):
//! SEND on slot 1, 16 bytes per message, w0 = byte count, w1|w2 = the bytes little-endian.
//!
//! Bulk data over a register-only IPC is deliberate for phase one: the three-word message is
//! the ABI's whole fastpath (notes/abi.md §2), std's own `LineWriter` batches user writes, and
//! the receiver reassembles. A shared-page protocol belongs to milestone 28's terminal
//! contract, not here. stdin is honest EOF: nothing grants a std program input yet.
//!
//! stderr shares the endpoint, so out and err interleave by 16-byte chunk. Recorded as a
//! caveat in notes/std.md; one endpoint is what the contract grants today.

use crate::io;
use crate::sys::pal::cricker::rt;

pub struct Stdin;
pub struct Stdout;
pub type Stderr = Stdout;

fn send_bytes(buf: &[u8]) {
    for chunk in buf.chunks(16) {
        let mut words = [0u8; 16];
        words[..chunk.len()].copy_from_slice(chunk);
        let w1 = u64::from_le_bytes(words[..8].try_into().unwrap());
        let w2 = u64::from_le_bytes(words[8..].try_into().unwrap());
        // A failed SEND (no endpoint in slot 1, wrong rights) is deliberately swallowed: a
        // program without a console still runs, it just prints into the void, which is what
        // every OS does to a process whose stdout is closed.
        rt::send(rt::STDOUT_SLOT, chunk.len() as u64, w1, w2);
    }
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
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        send_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub const STDIN_BUF_SIZE: usize = 0;

pub fn is_ebadf(_err: &io::Error) -> bool {
    true
}

pub fn panic_output() -> Option<impl io::Write> {
    Some(Stdout::new())
}
