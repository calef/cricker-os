//! **The byte-sink contract** (milestone 50): one framing for *write these bytes there*.
//!
//! Before this crate there were four of them. std's `println!` SENT three registers on the
//! endpoint in slot 1; `line_editor` CALLed with an opcode and a shared page; `fs_proto` CALLed with
//! a handle, an offset and a shared page; the console server took a length on one endpoint and
//! acknowledged on another. Four shapes for one verb, which is exactly as many as it takes to make
//! a program's output destination something the program has to *know about*. This is the one
//! shape, and the point of having it is negative: a writer stops being able to tell what is
//! underneath.
//!
//! # The shape, and why it is this one
//!
//! **A sink is an `Endpoint` capability with `WRITE`, and nothing else.** Not an endpoint plus a
//! shared page, not an endpoint plus an acknowledgement channel. That restriction is the whole
//! design, because milestone 50's finding is that redirection is *putting a different capability in
//! a slot*: the moment a sink needs a page mapped at an agreed address, substituting one for
//! another stops being one grant and starts being a spawn-time negotiation, and the finding is
//! gone.
//!
//! It follows that the payload rides in registers. A message carries up to [`INLINE_MAX`] bytes in
//! two words, with the opcode and the length in the first, which is the three-word fastpath
//! notes/abi.md §2 describes and the one std's stdout already used.
//!
//! **`SEND`, not `CALL`.** A `CALL` would tell the writer how many bytes the sink took, and it
//! would cost a round trip on the hottest path in the system to say "all of them", which is the
//! only answer a self-framing message has. Back-pressure is not lost by dropping the reply,
//! because `SEND` blocks until a receiver takes the message: the rendezvous *is* the flow control,
//! which is the property `line_editor::proto::OP_BYTES` already documented. And it is what lets the
//! reader of a pipe be an ordinary program that does nothing but `recv`, with no reply to send and
//! no protocol knowledge at all.
//!
//! # What a writer learns, which is the one thing it must
//!
//! A sink that has ended is not the same fact as a sink that was never granted, and [`Sent`] is
//! that distinction:
//!
//! - **[`Sent::NoSink`]**: the slot is empty (or holds something that is not a writable endpoint).
//!   This program was not given an output destination. Writing into the void is correct; every OS
//!   does it to a process whose stdout is closed, and the program keeps running.
//! - **[`Sent::Gone`]**: there *was* a sink and it has been destroyed. The reader exited, its
//!   endpoint died with it (DECISIONS §16 revocation), and the bytes have nowhere to go. **The
//!   writer must end.** This is Unix's `SIGPIPE`, arriving as a return code rather than a signal,
//!   for the same reason milestone 48 retired `SIGTTIN`: the question the signal answered is
//!   already answered by who holds what.
//!
//! Unix needs a signal here because an anonymous file descriptor gives the kernel no other way to
//! reach a writer that is not making a syscall. A capability system has a better channel: the
//! writer *is* making a syscall, on a capability the kernel can see is dead.
//!
//! # Examples
//!
//! A writer chunks; the sink reassembles. Sixteen bytes per message is not a tuning parameter, so
//! an example that writes more than that is the honest one:
//!
//! ```
//! use sink_proto::{INLINE_MAX, Msg, eof, pack, unpack};
//!
//! let line = b"the quick brown fox\n"; // 20 bytes, so two messages and an EOF
//! let mut messages = 0;
//! let mut received = [0u8; 32];
//! let mut got = 0;
//! let mut ended = false;
//!
//! let mut rest = &line[..];
//! while !rest.is_empty() {
//!     let (w0, w1, w2, taken) = pack(rest);
//!     messages += 1;
//!     rest = &rest[taken..]; // `pack` never drops a tail silently; the count says what it took
//!
//!     // The other end of the SEND.
//!     let mut buf = [0u8; INLINE_MAX];
//!     match unpack(w0, w1, w2, &mut buf) {
//!         Msg::Bytes(n) => {
//!             received[got..got + n].copy_from_slice(&buf[..n]);
//!             got += n;
//!         }
//!         Msg::Eof | Msg::Malformed => unreachable!(),
//!     }
//! }
//! // The writer is finished, and says so rather than leaving it to be inferred.
//! let (w0, w1, w2) = (eof(), 0, 0);
//! if let Msg::Eof = unpack(w0, w1, w2, &mut [0u8; INLINE_MAX]) {
//!     ended = true;
//! }
//!
//! assert_eq!(messages, 2); // 16 + 4
//! assert_eq!(&received[..got], line);
//! assert!(ended);
//! ```
//!
//! Two claims in the constants are worth checking in code rather than trusting in prose. The first
//! is why [`OP_BYTES`] is zero: with that opcode the first word **is** the byte count, which is
//! bit-for-bit the framing std's stdout sent before this crate existed, so unifying the protocol
//! cost no instruction on the hottest path in the system. The second is that a length the message
//! cannot hold is refused rather than obeyed, because obeying it is a read past the end of a
//! buffer:
//!
//! ```
//! use sink_proto::{INLINE_MAX, Msg, OP_BYTES, len, op, pack, req, unpack};
//!
//! let (w0, ..) = pack(b"hi");
//! assert_eq!(w0, 2); // the whole word, not just its low bits
//! assert_eq!(op(w0), OP_BYTES);
//! assert_eq!(len(w0), 2);
//!
//! // A writer claiming more bytes than three words can carry is malformed, not generous.
//! let lying = req(OP_BYTES, INLINE_MAX as u64 + 1);
//! assert_eq!(unpack(lying, 0, 0, &mut [0u8; INLINE_MAX]), Msg::Malformed);
//! ```
//!
//! # What this contract deliberately does not carry
//!
//! **A per-write error code.** A sink that cannot accept bytes any more says so by ceasing to
//! exist, and a disk-full file sink is required to destroy its receiving end rather than reply
//! with an errno. That collapses "the reader exited", "the device is gone" and "the file system is
//! full" into one fact, and the honest caveat is that the writer loses the *reason*: it learns that
//! this sink is over, not why. That is the right trade for a byte stream, whose only available
//! response to any of the three is to stop, and it is the trade Unix makes too (a writer gets
//! `EPIPE`, never the reader's reason). A sink whose failures a client must distinguish is not a
//! sink; it is a service, and it should be a `CALL` protocol like `fs_proto`.
//!
//! **Types.** This carries bytes. A method implies an object with a type, and typed pipelines are a
//! separate and larger fork (design/roadmap/50-pipes-and-redirection.md records it as one); nothing in
//! this framing should be read as a step toward one.
//!
//! **Seeking, truncating, re-reading, or stat.** A sink appends. That is the payoff milestone 50
//! claims over Unix: `> report.txt` hands a program strictly less than fd 1 with full file
//! semantics does, and it is this crate's opcode list that makes the claim true rather than
//! policy.
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! The stem is the contract's own word (DECISIONS §51), which that section uses and never defends.

#![no_std]

/// The opcode's position in the first word: bits 63:56, the same place `fs_proto` and
/// `line_editor::proto` put theirs. One spelling for "the wire contract" across the tree.
pub const OP_SHIFT: u32 = 56;

/// **Bytes.** The low 32 bits are the count (1 to [`INLINE_MAX`]); the bytes themselves are in the
/// second and third words, little-endian, low word first.
///
/// It is **zero on purpose.** With the opcode at zero the first word is exactly the byte count,
/// which is bit-for-bit the framing std's stdout already sent before this crate existed. So
/// unifying the protocol changed no instruction on the fastpath and cost no message, and the
/// benchmark that prices `println!` cannot tell that anything happened. An opcode is a claim about
/// what a message means; the cheapest claim to make is the one the wire was already making.
pub const OP_BYTES: u64 = 0;

/// **End of stream.** The writer is finished; no more bytes will come. The low 32 bits are zero.
///
/// A sink acts on this rather than merely noticing it: a file sink closes its handle, a terminal
/// sink may draw a prompt, and the reader of a pipe stops reading instead of blocking forever on a
/// writer that has exited. Without it "the producer is done" would have to be inferred from a death
/// notification the reader may not be the supervisor for, which is a fact about process supervision
/// standing in for a fact about a stream.
pub const OP_EOF: u64 = 1;

/// The most bytes one message carries: two 64-bit words. Not a tuning parameter. It is what fits in
/// the registers a `SEND` already has, and enlarging it means giving a sink a shared page, which is
/// the thing the module note explains this contract exists to avoid.
pub const INLINE_MAX: usize = 16;

/// The kernel's `abi::Error::Gone`, restated so this crate stays dependency-free.
///
/// It has to be restated because `cargo xtask std-src` copies this file verbatim into the patched
/// std sysroot, where there is no `abi` crate to link against. The test module below pins the two
/// numbers together and is truncated out of that copy, so the drift this duplication invites is
/// caught in the workspace where it can be.
pub const GONE: i64 = -11;

/// Pack an opcode and a length into a message's first word.
pub const fn req(op: u64, len: u64) -> u64 {
    (op << OP_SHIFT) | (len & 0xffff_ffff)
}

/// The opcode of a message's first word.
pub const fn op(w0: u64) -> u64 {
    w0 >> OP_SHIFT
}

/// The byte count of a message's first word.
pub const fn len(w0: u64) -> usize {
    (w0 & 0xffff_ffff) as usize
}

/// The first word of an end-of-stream message.
pub const fn eof() -> u64 {
    req(OP_EOF, 0)
}

/// Pack up to [`INLINE_MAX`] bytes into the three words of a `SEND`. Bytes past the sixteenth are
/// **not** silently dropped: the caller chunks, and the returned count says how many were taken, so
/// a caller that ignores it writes a short message rather than losing a tail without knowing.
pub fn pack(bytes: &[u8]) -> (u64, u64, u64, usize) {
    let n = if bytes.len() < INLINE_MAX {
        bytes.len()
    } else {
        INLINE_MAX
    };
    let mut words = [0u8; INLINE_MAX];
    words[..n].copy_from_slice(&bytes[..n]);
    let mut lo = [0u8; 8];
    let mut hi = [0u8; 8];
    lo.copy_from_slice(&words[..8]);
    hi.copy_from_slice(&words[8..]);
    (
        req(OP_BYTES, n as u64),
        u64::from_le_bytes(lo),
        u64::from_le_bytes(hi),
        n,
    )
}

/// What one received message was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    /// [`OP_BYTES`]: this many bytes were written into the caller's buffer, 0 to [`INLINE_MAX`].
    Bytes(usize),
    /// [`OP_EOF`]: the writer is finished.
    Eof,
    /// An opcode this contract does not define, or a byte count past [`INLINE_MAX`].
    ///
    /// A sink refuses it rather than guessing, and refusing loudly is the point: a length larger
    /// than the message can hold is the shape of a confused or hostile writer, and the read it
    /// would authorise is a read past the end of a buffer.
    Malformed,
}

/// Unpack a received message into `out`. See [`Msg`].
pub fn unpack(w0: u64, w1: u64, w2: u64, out: &mut [u8; INLINE_MAX]) -> Msg {
    match op(w0) {
        OP_BYTES => {
            let n = len(w0);
            if n > INLINE_MAX {
                return Msg::Malformed;
            }
            out[..8].copy_from_slice(&w1.to_le_bytes());
            out[8..].copy_from_slice(&w2.to_le_bytes());
            Msg::Bytes(n)
        }
        OP_EOF => Msg::Eof,
        _ => Msg::Malformed,
    }
}

/// What a writer learned from a `SEND`'s return value. See the module note: this is the one thing
/// the contract must tell a writer, and the two failures are not the same fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// The bytes are at the sink.
    Ok,
    /// **There was a sink and it is destroyed.** Stop writing; end the program.
    Gone,
    /// **There is no sink and there never was one.** Keep running; the bytes go nowhere.
    NoSink,
}

/// Classify a `SEND`'s return value.
///
/// Every refusal other than [`GONE`] is [`Sent::NoSink`], and that is deliberate rather than lazy.
/// An empty slot, a slot holding a `Frame` instead of an `Endpoint`, and an endpoint held without
/// `WRITE` are all the same fact from the writer's side: *this program was not given a working
/// output destination*, which is a wiring error its parent made and which the program has no
/// channel to report on, its only channel being the broken one. The conservative reading keeps it
/// running; the alternative kills a program because its spawner was careless.
pub const fn classify(ret: i64) -> Sent {
    if ret >= 0 {
        Sent::Ok
    } else if ret == GONE {
        Sent::Gone
    } else {
        Sent::NoSink
    }
}

/// **Shared fixtures for the sink tests**: the bytes an indifferent writer writes, the file a file
/// sink writes them into, and the sentinels its report carries. One definition, so the component
/// and the kernel-side test cannot disagree about what "the same bytes" means. Same split
/// `fs_proto::fixture` makes for the filesystem contract.
pub mod fixture {
    use super::Sent;

    /// What `user/src/sink.rs`'s writer role writes. Long enough to span several messages and to
    /// end mid-message, so a decoder that only ever sees full 16-byte chunks is not being flattered
    /// by the fixture.
    pub const TRANSCRIPT: &[u8] =
        b"a program does not know what its output slot holds, and that is the point.\n";

    /// The name the file-sink role writes into. Created on first use rather than shipped in the
    /// image, because the file's existence is part of what the test proves.
    pub const SINK_NAME: &str = "sinkout";

    /// A sink reports this once its destination is open and before it takes a byte, so a hang in
    /// opening the file is distinguishable from a hang in the serve path. Same discipline as
    /// `fs_proto::fixture::READY`.
    pub const READY: u64 = 0x5142_0000_0000_0001;

    /// A sink reports this after end-of-stream, with the byte total in the second word.
    pub const DONE: u64 = 0x5142_0000_0000_0002;

    /// A sink reports this on a message the contract does not define, with the offending first word
    /// in the second position. It refuses rather than guessing (see [`super::Msg::Malformed`]).
    pub const BAD: u64 = 0x5142_0000_0000_0003;

    /// A writer's report of how its last `SEND` classified. A number, because the assertion it
    /// serves is about telling `Gone` from `NoSink` and a boolean cannot carry that.
    pub const fn code(sent: Sent) -> u64 {
        match sent {
            Sent::Ok => 0,
            Sent::Gone => 1,
            Sent::NoSink => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number this crate restates and the kernel's enum are one fact in two files, and this is
    /// the only place that can notice them parting. `abi` is a dev-dependency precisely so this
    /// check exists in the workspace and never travels into the std sysroot copy, which cannot
    /// link it.
    #[test]
    fn the_restated_gone_code_is_the_kernels() {
        assert_eq!(GONE, abi::Error::Gone as i64);
    }

    /// A round trip through the wire words, at the boundaries that matter: a message that fills
    /// both words, one that fills neither, and one that straddles them.
    #[test]
    fn bytes_survive_the_wire_words() {
        for n in 0..=INLINE_MAX {
            let src: [u8; INLINE_MAX] = core::array::from_fn(|i| (i as u8).wrapping_mul(37) ^ 0x5a);
            let (w0, w1, w2, taken) = pack(&src[..n]);
            assert_eq!(taken, n);
            let mut out = [0u8; INLINE_MAX];
            assert_eq!(unpack(w0, w1, w2, &mut out), Msg::Bytes(n));
            assert_eq!(&out[..n], &src[..n]);
        }
    }

    /// A caller handing over more than one message holds gets a full message and an honest count,
    /// not a truncated write it cannot detect.
    #[test]
    fn pack_takes_a_full_message_and_says_so() {
        let src = [7u8; 40];
        let (w0, _, _, taken) = pack(&src);
        assert_eq!(taken, INLINE_MAX);
        assert_eq!(len(w0), INLINE_MAX);
    }

    /// The framing that makes the unification free: with [`OP_BYTES`] at zero, a bytes message's
    /// first word IS its length, which is the wire std's stdout sent before this contract existed.
    #[test]
    fn a_bytes_messages_first_word_is_its_length() {
        let (w0, _, _, _) = pack(b"nife");
        assert_eq!(w0, 4);
    }

    /// EOF is distinguishable from every byte count, including zero, which is the property that
    /// lets one word carry both.
    #[test]
    fn eof_is_not_a_byte_count() {
        let mut out = [0u8; INLINE_MAX];
        assert_eq!(unpack(eof(), 0, 0, &mut out), Msg::Eof);
        let (w0, w1, w2, _) = pack(&[]);
        assert_eq!(unpack(w0, w1, w2, &mut out), Msg::Bytes(0));
        assert_ne!(eof(), w0);
    }

    /// A length past what a message can hold, and an opcode nobody defined, are refused rather than
    /// clamped. Clamping a length would turn a hostile writer's message into a plausible one.
    #[test]
    fn a_malformed_message_is_refused_not_repaired() {
        let mut out = [0u8; INLINE_MAX];
        assert_eq!(
            unpack(req(OP_BYTES, 17), 0, 0, &mut out),
            Msg::Malformed,
            "a byte count past INLINE_MAX must not be clamped",
        );
        assert_eq!(
            unpack(req(OP_BYTES, u32::MAX as u64), 0, 0, &mut out),
            Msg::Malformed,
        );
        assert_eq!(unpack(req(9, 4), 0, 0, &mut out), Msg::Malformed);
    }

    /// The three report codes are distinct, which is the only property of them that matters: a test
    /// that could not tell `Gone` from `NoSink` in a report would not be testing anything.
    #[test]
    fn the_report_codes_tell_the_three_outcomes_apart() {
        use fixture::code;
        assert_ne!(code(Sent::Ok), code(Sent::Gone));
        assert_ne!(code(Sent::Ok), code(Sent::NoSink));
        assert_ne!(code(Sent::Gone), code(Sent::NoSink));
    }

    /// The distinction the whole contract exists to carry. `Gone` must not be reachable from any
    /// other refusal, because a writer turns it into "end this program".
    #[test]
    fn gone_is_distinguishable_from_never_had_one() {
        assert_eq!(classify(0), Sent::Ok);
        assert_eq!(classify(16), Sent::Ok);
        assert_eq!(classify(GONE), Sent::Gone);
        for other in [-1i64, -2, -3, -4, -5, -6, -7, -8, -9, -10, -12, -99] {
            assert_eq!(
                classify(other),
                Sent::NoSink,
                "error {other} must not be read as a dead sink; only {GONE} is",
            );
        }
    }
}
