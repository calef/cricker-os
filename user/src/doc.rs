//! **`doc`: the viewer** (milestone 40, notes/manual.md).
//!
//! It reads markdown on its input and writes it rendered on its output. That is the whole program.
//! The rendering is `crates/manual`, host-tested in milliseconds; what lives here is the two ends of
//! the sink contract and eight kilobytes of `.bss`.
//!
//! Name: unrecorded. Provisional, minted by milestone 40's lane on 2026-08-04 and not yet put to
//! calef. It is the roadmap's own word for this program. `man` is the live alternative and carries
//! the stronger argument that a reader already knows it from outside this project; the counter is
//! that `man` names a *format* elsewhere and this reads plain markdown. Deliberately not the same
//! name as `crates/manual`, though the two are the usual crate-and-program pair: see that crate's
//! header for why. See notes/naming.md.
//!
//! # It cannot name a page, and that is the demonstration
//!
//! A Unix `man` opens the file it prints, which means it can open any file its caller can. This one
//! is handed bytes. `doc notes/glob.md` is the *shell* resolving that name against the directory
//! capability it holds and streaming what it finds; nothing in this program's cspace names a file, a
//! directory or the filesystem, and there is no message it can send to find out what it is reading.
//!
//! That is the same claim `wc` makes (notes/pipes.md) and it lands harder here, because a
//! documentation viewer is exactly the program a reader would expect to go and fetch things. It
//! does not. `doc < notes/glob.md`, `doc notes/glob.md` and `something | doc` are one behaviour
//! with three sources.
//!
//! # Capability contract
//!
//! | slot | what | why |
//! |---|---|---|
//! | 0 | the output sink, `WRITE` | where the rendered bytes go |
//! | 1 | the input source, `READ` | where the markdown comes from |
//!
//! Two capabilities. No memory grant, because the renderer never allocates; no file, no directory,
//! no shared page, no clock.
//!
//! # Colour is a capability question, answered by the shell
//!
//! This program cannot tell whether its sink is a terminal, a file or another program, and the sink
//! contract is built so that it cannot: that is what makes redirection one grant rather than a
//! negotiation. So it does not sniff the way `isatty` sniffs. It emits **plain text**, and the
//! terminal-bound case is the shell's to arrange.
//!
//! **This is the honest half of a limitation, not a design win**: it means `doc notes/glob.md` at
//! the prompt is currently monochrome even though the renderer can colour it, because the shell has
//! no way to say "this stage ends at the terminal" in the spawn protocol. `BUGS` records it.
//!
//! # EXAMPLES
//!
//! On the host, where the same renderer runs over the same bytes:
//!
//! ```text
//! $ cargo xtask manual capability
//! search: capability
//!      7  The manual: documentation as a system service  notes/manual.md
//!     31  Pipes and redirection: `>`, `<` and `|` are one   notes/pipes.md
//! ```
//!
//! At the prompt, this is the whole of what works today, and `BUGS` says why:
//!
//! ```text
//! $ doc
//!   doc: reads an input stream: name a file, redirect with '<', or pipe into it
//! ```
//!
//! # BUGS
//!
//! - **`doc <page>` on its own deadlocks at the interactive prompt, and needs a `|` or a `>` after
//!   it.** This is a property of the shell rather than of this program, found by running it: `swish`
//!   sends a stage its **whole** input and only then drains the stage's output, and `sink_proto` is
//!   a rendezvous `SEND`. So a program that writes while it is still reading blocks against a shell
//!   that is still writing, and both stop. `wc` never meets this because it produces nothing until
//!   end of stream; a renderer cannot work that way without holding the document, which is the
//!   memory grant this program exists without. With anything downstream the reader is a separate
//!   process and the deadlock is gone. `swish`'s `MAX_TEXT_CHUNKS = 32` is the second half of the
//!   same problem: even without the deadlock the prompt would print only the first 512 bytes.
//! - **And neither workaround for that deadlock delivers the file.** `doc gate.txt | wc` and
//!   `doc gate.txt > page.txt` both answer `0 0 0`, which is this program rendering an *empty*
//!   input: the named file reaches a stage only on the plain `doc gate.txt` path, the one that
//!   deadlocks. So today there is no line a person can type that shows a rendered page. All three
//!   limitations are the shell's, they are one lane's work, and the renderer underneath is proven
//!   on this repository's own pages by `every_character_survives`.
//! - **No pager, and the reason is authority rather than effort.** Paging needs a keypress, a
//!   keypress needs `line_editor::proto::OP_READLINE`, and that opcode rides on the terminal
//!   endpoint whose read side *is* the keyboard. The spawn protocol has no way to hand a child the
//!   right to read one line without handing it the terminal, which is the exact thing
//!   `terminal_sink_caretaker` exists to prevent. So a long page scrolls off, and the fix is a
//!   design decision about the spawn protocol rather than a hundred lines here.
//! - **Eighty columns, always.** The renderer takes a width and this program does not ask for one,
//!   because there is nobody to ask: the terminal contract has no "how wide are you" verb, and the
//!   graphical terminal is 32 columns while the serial one is whatever the host window is.
//! - **Plain text only**, per the section above.
//! - **A source line longer than `manual::LINE_MAX` loses its tail**, and this program does not say
//!   so even though `manual::Renderer::truncated` would tell it, because its only output channel is
//!   the rendered document and a diagnostic in the middle of one is worse than the loss.

#![no_std]
#![no_main]

use manual::{Renderer, Sink, Style};
use user_rt::{exit, recv, send};

/// The output sink: where the rendered bytes go, in the sink contract's framing.
const SINK: u64 = 0;
/// The input source: an endpoint we hold `READ` on, carrying markdown inbound.
const SOURCE: u64 = 1;

/// The width the renderer wraps to. See `BUGS`: there is nothing to ask.
const WIDTH: u16 = 80;

/// The renderer lives here rather than on the stack because it is about eleven kilobytes and a user
/// process gets one 4 KiB stack page. `display_terminal` keeps its `Vt` the same way and for the
/// same reason.
static mut RENDERER: Renderer = Renderer::new(Style {
    width: WIDTH,
    color: false,
});

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    // SAFETY: this process is single-threaded (`thread::spawn` is `Unsupported` here and nothing in
    // this program creates one), so there is exactly one live reference to this static for the whole
    // run. It is taken once, here, and passed down by reference. `Renderer::new` is `const` for the
    // same reason the reference is taken this way: building one on the stack would overflow the
    // single stack page before it was ever used.
    let renderer = unsafe { &mut *core::ptr::addr_of_mut!(RENDERER) };

    let mut out = Out::default();
    loop {
        let (w0, w1, w2) = recv(SOURCE);
        let mut buf = [0u8; sink_proto::INLINE_MAX];
        let n = match sink_proto::unpack(w0, w1, w2, &mut buf) {
            sink_proto::Msg::Bytes(n) => n,
            // A malformed message ends the document rather than being skipped, for the reason `wc`
            // gives: a page that is silently missing a paragraph is indistinguishable from a page
            // that never had one.
            sink_proto::Msg::Eof | sink_proto::Msg::Malformed => break,
        };
        renderer.feed(&buf[..n], &mut out);
        if out.gone {
            break;
        }
    }
    renderer.finish(&mut out);
    out.end();
    exit();
}

/// The output side of the sink contract, as a [`Sink`] the renderer can write into.
///
/// It buffers to [`sink_proto::INLINE_MAX`] and sends a message whenever it is full, because the
/// renderer emits in runs as short as one escape sequence and a message per run would be one IPC
/// round trip per attribute change.
#[derive(Default)]
struct Out {
    buf: [u8; sink_proto::INLINE_MAX],
    used: usize,
    /// The sink has been destroyed and there is nowhere left to write. Unix's `SIGPIPE`, arriving
    /// as a return code (notes/pipes.md).
    gone: bool,
}

impl Out {
    fn flush(&mut self) {
        if self.used == 0 || self.gone {
            self.used = 0;
            return;
        }
        let (w0, w1, w2, _) = sink_proto::pack(&self.buf[..self.used]);
        self.used = 0;
        if !matches!(
            sink_proto::classify(send(SINK, w0, w1, w2)),
            sink_proto::Sent::Ok
        ) {
            self.gone = true;
        }
    }

    fn end(&mut self) {
        self.flush();
        if !self.gone {
            send(SINK, sink_proto::eof(), 0, 0);
        }
    }
}

impl Sink for Out {
    fn put(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.used == sink_proto::INLINE_MAX {
                self.flush();
            }
            if self.gone {
                return;
            }
            self.buf[self.used] = b;
            self.used += 1;
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `brk` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: `ebreak` traps; the kernel turns a trap from userspace into a kill.
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
