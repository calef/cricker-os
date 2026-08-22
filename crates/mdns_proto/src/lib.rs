#![no_std]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 41-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]
//! **The mDNS/DNS-SD wire format** (RFC 6762, RFC 6763), as pure computation, for milestone 55's
//! Time Machine advertisement.
//!
//! A Mac will not offer a server in its backup-disk list unless the server announces itself over
//! multicast DNS: `_smb._tcp` (the file service), `_adisk._tcp` (the Time Machine flags, which are
//! what populate the list), and `_device-info._tcp` (the model string, which picks the icon). This
//! crate is the bytes and the decisions over them, with no socket, no timer and no policy: the same
//! split `ntp_proto` and `fs_proto` make for their contracts, so the parser, the name compression
//! handling and the response logic are host-tested in milliseconds and Kani-checked, rather than
//! debugged inside a QEMU boot against a live Mac. The responder program that joins `224.0.0.251`
//! and carries these bytes needs network-stack surface the tree does not have yet; notes/mdns.md
//! records exactly what is missing and why the program is a later phase.
//!
//! # The reference is measured, not derived
//!
//! calef's router (GL.iNet GL-BE9300, the working family Time Machine server) was queried on
//! 2026-08-15 and its actual response bytes are this crate's primary test vectors; the hex lives in
//! the tests and in notes/mdns.md. Three facts from the capture that reading the RFCs would not
//! give:
//!
//! - **One `_adisk._tcp` instance carries every share.** The disks are enumerated *inside* its TXT
//!   record (`dk0=adVN=corinne,adVF=0x82`, `dk1=adVN=chris,adVF=0x82`, `sys=adVF=0x100`), never one
//!   announcement per share. Two disks is the measured truth of the reference as of 2026-08-15.
//! - **The `_adisk` and `_device-info` SRV records advertise port 0.** They carry flags and a model
//!   string, not a service; the connectable port (445) is `_smb._tcp`'s.
//! - **The model string is `MacSamba`, not `TimeCapsule`.** The router's Samba config says
//!   `fruit:model = TimeCapsule`, and its mDNS `_device-info` TXT says `model=MacSamba` anyway:
//!   the SMB-side knob and the mDNS advertisement are separate mechanisms, and a working family
//!   backup runs with them disagreeing. So the model is data here, never a constant.
//!
//! The flag values (`0x82`, `0x100`) and every TXT key are **data supplied by the caller**, with
//! the captured values used as test vectors. The capture is the authority; when a future capture
//! disagrees with a convenience function here, [`txt_rdata`] takes arbitrary entries and the
//! convenience is the thing to fix.
//!
//! # Layering
//!
//! ```text
//!   &[u8] ──Reader──► Header, Question*, Record*        (decodes compressed names)
//!   Builder ──question/answer/additional──► &[u8]       (emits uncompressed names only)
//!   Advertisement ──respond(query)──► Option<response>  (the responder's whole decision)
//!   Advertisement ──announcement(svc)──► response       (unsolicited, RFC 6762 §8.3)
//! ```
//!
//! [`respond`] is the function the eventual responder program wraps in a socket loop: it takes one
//! received message and the advertisement's data, and produces either the full response datagram or
//! the decision to stay silent. Everything RFC 6762 makes conditional on the *source* of the query
//! (a legacy querier's port is not 5353) is a [`QuerySource`] the caller supplies, because the
//! crate never sees a socket.
//!
//! # Examples
//!
//! The whole responder, minus the socket. A Mac browsing for backup targets sends a PTR query for
//! `_adisk._tcp.local`, and what comes back is what populates its list:
//!
//! ```
//! use mdns_proto::{
//!     Advertisement, Builder, CLASS_IN, Disk, QuerySource, Reader, SERVICE_ADISK, respond, rrtype,
//!     name_from_dotted, txt_strings,
//! };
//!
//! // The two shares the family actually backs up, and the reference's measured flag values.
//! let disks = [
//!     Disk { volume: "corinne", flags: "0x82" },
//!     Disk { volume: "chris", flags: "0x82" },
//! ];
//! let adv = Advertisement {
//!     host: "patagonia",
//!     smb_port: 445,
//!     disks: &disks,
//!     sys_flags: "0x100",
//!     model: "MacSamba",
//!     ipv4: Some([192, 168, 1, 20]),
//! };
//!
//! // What a browsing Mac puts on the wire.
//! let mut qbuf = [0u8; 512];
//! let mut q = Builder::query(&mut qbuf).unwrap();
//! q.question(name_from_dotted(SERVICE_ADISK).unwrap().as_bytes(), rrtype::PTR, CLASS_IN).unwrap();
//! let qlen = q.finish();
//!
//! let mut rbuf = [0u8; 1472];
//! let n = respond(&adv, &qbuf[..qlen], &mut rbuf, QuerySource::Multicast)
//!     .unwrap()
//!     .expect("we are authoritative for _adisk._tcp");
//!
//! // Both shares ride inside ONE instance's TXT record. That is the measured shape of the
//! // reference, and the thing reading RFC 6763 would not tell you.
//! let mut r = Reader::new(&rbuf[..n]).unwrap();
//! let mut entries = 0;
//! for _ in 0..r.header.qdcount {
//!     r.question().unwrap();
//! }
//! let records = r.header.ancount + r.header.nscount + r.header.arcount;
//! for _ in 0..records {
//!     let rec = r.record().unwrap();
//!     if rec.rrtype == rrtype::TXT {
//!         for s in txt_strings(rec.rdata) {
//!             if s.unwrap().starts_with(b"dk") {
//!                 entries += 1;
//!             }
//!         }
//!     }
//! }
//! assert_eq!(entries, 2);
//! ```
//!
//! Silence is an answer, and it is the common one: a responder says nothing about a name it is not
//! authoritative for, rather than replying that it does not know.
//!
//! ```
//! # use mdns_proto::{
//! #     Advertisement, Builder, CLASS_IN, QuerySource, respond, rrtype, name_from_dotted,
//! # };
//! # let adv = Advertisement {
//! #     host: "patagonia", smb_port: 445, disks: &[], sys_flags: "0x100", model: "MacSamba",
//! #     ipv4: None,
//! # };
//! let mut qbuf = [0u8; 512];
//! let mut q = Builder::query(&mut qbuf).unwrap();
//! let name = name_from_dotted("_ipp._tcp.local").unwrap();
//! q.question(name.as_bytes(), rrtype::PTR, CLASS_IN).unwrap();
//! let qlen = q.finish();
//!
//! let mut rbuf = [0u8; 1472];
//! assert_eq!(respond(&adv, &qbuf[..qlen], &mut rbuf, QuerySource::Multicast).unwrap(), None);
//! ```
//!
//! # BUGS
//!
//! - **Names are emitted uncompressed.** Legal: RFC 1035 §4.1.4 makes decompression mandatory for
//!   receivers and compression optional for senders. It costs bytes (the reference router's 199-byte
//!   `_adisk` response would be ~280 here), well inside the 1472-byte mDNS budget for this record
//!   set, and it keeps the emitter simple enough to prove things about. Decompression is fully
//!   handled on the read side.
//! - **Known-answer suppression is implemented for the PTR browse answers only** (the records a
//!   browsing Mac re-queries every few minutes, which is the traffic that matters). A known answer
//!   for a SRV/TXT/A record is ignored and the record re-sent, which is chatty, not wrong.
//! - **[`respond`] handles one advertisement.** Three shares is one advertisement (measured, above);
//!   a second *server identity* on one responder is out of scope.
//! - **Probing and announcing are the bytes, not the state machine.** [`probe_query`],
//!   [`conflicts`], [`tiebreak`] and [`announcement`] are the wire-format halves of RFC 6762 §8;
//!   the 250 ms probe timing, the three-probe count, the two-to-eight repeats of an announcement
//!   at doubling intervals, and the rate limiting all live with whoever owns a clock.
//! - **[`announcement`] emits one service type per call**, because this tree's 576-byte MTU cannot
//!   carry all three in one datagram. Legal (a responder may announce in as many messages as it
//!   likes) and slightly chattier than a fatter transport would need.
//! - **TC-bit handling is absent.** A query with the truncation bit set asks the responder to delay
//!   for more known answers; this crate answers it immediately. Harmless (an extra response), and
//!   recorded so nobody mistakes the omission for a decision.
//!
//! Name: unrecorded. Provisional, minted by milestone 55's mDNS lane on 2026-08-15. The `_proto`
//! suffix is the tree's rule for a wire contract (notes/naming.md); the stem is the protocol's own
//! name (RFC 6762), the same derivation that produced `ntp_proto`. Not yet put to calef.

use core::cmp::Ordering;

/// The UDP port mDNS runs on, source and destination both. A query whose *source* port is not this
/// one is a legacy querier (RFC 6762 §6.7) and gets [`QuerySource::LegacyUnicast`] treatment.
pub const PORT: u16 = 5353;

/// The IPv4 multicast group, `224.0.0.251`. The responder must join it (IGMP) or the queries never
/// arrive; see notes/mdns.md for what that requires of the network stack.
pub const GROUP_V4: [u8; 4] = [224, 0, 0, 251];

/// The IPv6 multicast group, `ff02::fb`.
pub const GROUP_V6: [u8; 16] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfb];

/// The three service types Time Machine discovery uses, and the meta-query that enumerates them.
/// `.local` is part of the name: mDNS owns exactly that domain (RFC 6762 §3).
pub const SERVICE_SMB: &str = "_smb._tcp.local";
/// The one that matters: its TXT record's `adVF` flags are what put a server in the backup list.
pub const SERVICE_ADISK: &str = "_adisk._tcp.local";
/// The model string, which sets the icon macOS shows. Measured value on the reference: `MacSamba`.
pub const SERVICE_DEVICE_INFO: &str = "_device-info._tcp.local";
/// RFC 6763 §9: a PTR query here enumerates every service type a responder offers.
pub const SERVICE_ENUMERATION: &str = "_services._dns-sd._udp.local";

/// Record types DNS-SD needs. Standard IANA numbers; nothing here is ours to choose.
pub mod rrtype {
    pub const A: u16 = 1;
    pub const PTR: u16 = 12;
    pub const TXT: u16 = 16;
    pub const AAAA: u16 = 28;
    pub const SRV: u16 = 33;
    /// `ANY`, which is what a probe asks (RFC 6762 §8.1).
    pub const ANY: u16 = 255;
}

/// The Internet class, the only one mDNS uses.
pub const CLASS_IN: u16 = 1;

/// Top bit of a **question's** class: the querier accepts a unicast response (RFC 6762 §5.4).
pub const QU: u16 = 0x8000;

/// Top bit of a **record's** class in a response: "cache flush", set on records whose name this
/// responder owns exclusively (SRV, TXT, A), never on shared PTR records, and never in a legacy
/// unicast response (RFC 6762 §10.2; the reference capture confirms the legacy half).
pub const CACHE_FLUSH: u16 = 0x8000;

/// TTL for the service records (PTR, SRV, TXT): 75 minutes, RFC 6762 §10's recommendation.
pub const TTL_SERVICE: u32 = 4500;
/// TTL for host address records: 120 seconds, same section (a host's address changes more often
/// than the services it runs).
pub const TTL_HOST: u32 = 120;
/// The cap a legacy unicast response applies to every TTL (RFC 6762 §6.7). The reference router
/// uses exactly this value.
pub const TTL_LEGACY_CAP: u32 = 10;

/// A DNS name may not exceed 255 bytes in wire form (RFC 1035 §2.3.4).
pub const MAX_NAME_LEN: usize = 255;
/// A label may not exceed 63 bytes; the two top bits of a longer count are the compression-pointer
/// tag.
pub const MAX_LABEL_LEN: usize = 63;

/// Everything that can go wrong with these bytes. Decoding errors mean the datagram is not a valid
/// DNS message and the right response is silence; building errors are caller bugs (a buffer too
/// small, a name too long) and are reported rather than panicking, because the responder must not
/// be crashable by its own configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The input ends before the structure it promised.
    Truncated,
    /// A label length byte uses the reserved `0x40`/`0x80` tag bits.
    BadLabel,
    /// A label exceeds 63 bytes (only the encoder can produce this; on the wire such a byte parses
    /// as a pointer or as [`BadLabel`](Error::BadLabel)).
    LabelTooLong,
    /// A name exceeds 255 bytes in uncompressed wire form.
    NameTooLong,
    /// A compression pointer that does not point strictly backwards. RFC-valid messages always
    /// point at earlier bytes; enforcing it is also the termination proof for the decoder.
    PointerForward,
    /// The output buffer is full.
    Overflow,
    /// A TXT entry exceeds the 255 bytes its length prefix can carry.
    EntryTooLong,
    /// RDATA exceeds the 65535 bytes its length field can carry.
    RdataTooLong,
    /// [`Builder`] sections must be appended in question, answer, authority, additional order.
    SectionOrder,
}

pub type Result<T> = core::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// The header.
// ---------------------------------------------------------------------------

/// The 12-byte DNS header, in front of every mDNS packet.
pub const HEADER_LEN: usize = 12;

/// Response bit.
pub const FLAG_QR: u16 = 0x8000;
/// Opcode field; mDNS uses only opcode 0 and ignores messages with any other (RFC 6762 §18.3).
pub const OPCODE_MASK: u16 = 0x7800;
/// Authoritative answer. Every mDNS response sets it (RFC 6762 §18.4).
pub const FLAG_AA: u16 = 0x0400;
/// Truncated.
pub const FLAG_TC: u16 = 0x0200;

/// The header fields, decoded. mDNS repurposes almost none of them: the ID is 0 in multicast
/// messages and echoed in legacy unicast responses, and the counts are counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub id: u16,
    pub flags: u16,
    pub qdcount: u16,
    pub ancount: u16,
    pub nscount: u16,
    pub arcount: u16,
}

impl Header {
    /// Decode the first 12 bytes. Total on any input of sufficient length: judging the flags is
    /// the caller's business.
    pub fn parse(msg: &[u8]) -> Result<Header> {
        if msg.len() < HEADER_LEN {
            return Err(Error::Truncated);
        }
        let f = |i: usize| u16::from_be_bytes([msg[i], msg[i + 1]]);
        Ok(Header {
            id: f(0),
            flags: f(2),
            qdcount: f(4),
            ancount: f(6),
            nscount: f(8),
            arcount: f(10),
        })
    }

    /// Encode into the first 12 bytes of `out`.
    pub fn write(&self, out: &mut [u8]) -> Result<()> {
        if out.len() < HEADER_LEN {
            return Err(Error::Overflow);
        }
        for (i, v) in [
            self.id,
            self.flags,
            self.qdcount,
            self.ancount,
            self.nscount,
            self.arcount,
        ]
        .into_iter()
        .enumerate()
        {
            out[2 * i..2 * i + 2].copy_from_slice(&v.to_be_bytes());
        }
        Ok(())
    }

    /// Is this a query a responder should read? QR clear and opcode 0 (RFC 6762 §18.3 says to
    /// ignore anything else silently).
    pub fn is_query(&self) -> bool {
        self.flags & (FLAG_QR | OPCODE_MASK) == 0
    }
}

// ---------------------------------------------------------------------------
// Names: the fixed buffer, the decoder (compression handled), the encoders.
// ---------------------------------------------------------------------------

/// A name in uncompressed wire form (length-prefixed labels, root terminated), in a fixed buffer.
/// 255 bytes is the protocol's own maximum, so this type cannot fail to hold a valid name.
#[derive(Clone, Copy)]
pub struct NameBuf {
    bytes: [u8; MAX_NAME_LEN],
    len: u8,
}

impl NameBuf {
    pub const fn empty() -> NameBuf {
        NameBuf {
            bytes: [0; MAX_NAME_LEN],
            len: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    fn push(&mut self, b: u8) -> Result<()> {
        if (self.len as usize) >= MAX_NAME_LEN {
            return Err(Error::NameTooLong);
        }
        self.bytes[self.len as usize] = b;
        self.len += 1;
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<()> {
        for &b in bytes {
            self.push(b)?;
        }
        Ok(())
    }
}

/// Equality is over the wire bytes, case-sensitive: [`names_equal`] is the DNS comparison. This
/// one exists so a test can `assert_eq!` and get a readable diff.
impl PartialEq for NameBuf {
    fn eq(&self, other: &NameBuf) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for NameBuf {}

impl core::fmt::Debug for NameBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        let mut rest = self.as_bytes();
        while let Some((&n, tail)) = rest.split_first() {
            if n == 0 {
                break;
            }
            if !first {
                write!(f, ".")?;
            }
            first = false;
            let n = n as usize;
            for &b in tail.get(..n).unwrap_or(tail) {
                if b.is_ascii_graphic() {
                    write!(f, "{}", b as char)?;
                } else {
                    write!(f, "\\{b:03}")?;
                }
            }
            rest = tail.get(n..).unwrap_or(&[]);
        }
        Ok(())
    }
}

/// Decode one possibly compressed name starting at `off` in the whole message `msg`.
///
/// Returns the uncompressed name and the offset of the first byte *after* the name in the original
/// stream (after the first pointer, or after the root label if there was none), which is where the
/// caller keeps reading.
///
/// Compression pointers must point **strictly backwards**, each target earlier than the last. Every
/// message a correct encoder can produce satisfies that (a pointer refers to a name that already
/// appeared), and it is what makes this loop provably terminate on arbitrary input: the pointer
/// budget strictly decreases and labels strictly consume output, so a pointer loop is an
/// [`Error::PointerForward`] rather than a hang. That claim is Kani-checked, not just argued.
pub fn decode_name(msg: &[u8], off: usize) -> Result<(NameBuf, usize)> {
    let mut out = NameBuf::empty();
    let (len, resume) = decode_name_into(msg, off, &mut out.bytes)?;
    // The engine wrote at most the array's 255 bytes, so the length fits its u8.
    out.len = len as u8;
    Ok((out, resume))
}

/// The decoder's engine, over a caller-supplied output slice; [`decode_name`] instantiates it at
/// the protocol's 255-byte maximum, and the capacity decides nothing except where
/// [`Error::NameTooLong`] fires. It is a separate function so the Kani harness can run the same
/// code against a small buffer: proving termination over a 255-byte symbolic output was measured
/// at twenty-plus solver minutes, and over a small one it is CI-fast, while the loop being proved
/// is identical (src/proofs.rs carries the measurements).
fn decode_name_into(msg: &[u8], off: usize, out: &mut [u8]) -> Result<(usize, usize)> {
    let mut len = 0usize;
    let mut pos = off;
    // Where the caller resumes: set at the first pointer, else after the root label.
    let mut resume: Option<usize> = None;
    // Every pointer must target an offset strictly below this.
    let mut fence = off;
    loop {
        let b = *msg.get(pos).ok_or(Error::Truncated)?;
        match b & 0xc0 {
            0xc0 => {
                let b2 = *msg.get(pos + 1).ok_or(Error::Truncated)?;
                let target = ((b as usize & 0x3f) << 8) | b2 as usize;
                if resume.is_none() {
                    resume = Some(pos + 2);
                }
                if target >= fence {
                    return Err(Error::PointerForward);
                }
                fence = target;
                pos = target;
            }
            0x00 => {
                if b == 0 {
                    if len >= out.len() {
                        return Err(Error::NameTooLong);
                    }
                    out[len] = 0;
                    return Ok((len + 1, resume.unwrap_or(pos + 1)));
                }
                let l = b as usize;
                let label = msg.get(pos + 1..pos + 1 + l).ok_or(Error::Truncated)?;
                let end = len + 1 + l;
                if end > out.len() {
                    return Err(Error::NameTooLong);
                }
                out[len] = b;
                out[len + 1..end].copy_from_slice(label);
                len = end;
                pos += 1 + l;
            }
            // 0x40 and 0x80 are reserved (RFC 1035 §4.1.4); nothing valid emits them.
            _ => return Err(Error::BadLabel),
        }
    }
}

/// Encode a dotted name (`"_adisk._tcp.local"`, trailing dot optional) into wire form. No escape
/// syntax: a dot always separates labels here. A label that must *contain* a dot (a DNS-SD
/// instance name may) goes through [`name_under`], which takes the label as raw bytes.
pub fn name_from_dotted(dotted: &str) -> Result<NameBuf> {
    let mut out = NameBuf::empty();
    let s = dotted.strip_suffix('.').unwrap_or(dotted);
    for label in s.split('.') {
        if label.is_empty() {
            return Err(Error::BadLabel);
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(Error::LabelTooLong);
        }
        out.push(label.len() as u8)?;
        out.extend(label.as_bytes())?;
    }
    out.push(0)?;
    Ok(out)
}

/// Prepend one label (raw bytes, so a DNS-SD instance name containing dots or spaces is fine) to
/// an already-encoded parent name: `name_under(b"GL-BE9300", &service)` is the instance name.
pub fn name_under(label: &[u8], parent: &NameBuf) -> Result<NameBuf> {
    if label.is_empty() {
        return Err(Error::BadLabel);
    }
    if label.len() > MAX_LABEL_LEN {
        return Err(Error::LabelTooLong);
    }
    let mut out = NameBuf::empty();
    out.push(label.len() as u8)?;
    out.extend(label)?;
    out.extend(parent.as_bytes())?;
    Ok(out)
}

/// DNS name comparison is ASCII case-insensitive (RFC 6762 §9.2 keeps that for mDNS). Both sides
/// are uncompressed wire form. Length bytes are below 64, so folding them is harmless.
pub fn names_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// ---------------------------------------------------------------------------
// Reading a message.
// ---------------------------------------------------------------------------

/// A decoded question.
#[derive(Debug, Clone, Copy)]
pub struct Question {
    pub name: NameBuf,
    pub qtype: u16,
    /// Raw class field, QU bit included; see [`Question::class`] and [`Question::unicast_requested`].
    pub qclass: u16,
}

impl Question {
    /// The class with the QU bit masked off.
    pub fn class(&self) -> u16 {
        self.qclass & !QU
    }

    /// RFC 6762 §5.4: the querier is willing to take the response unicast.
    pub fn unicast_requested(&self) -> bool {
        self.qclass & QU != 0
    }
}

/// A decoded resource record: the fixed fields plus the raw RDATA. RDATA that contains names
/// (PTR, SRV) may be compressed against the whole message, so [`ptr_target`] and [`srv_fields`]
/// take the message too; `rdata_off` is where the RDATA sits in it.
#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub name: NameBuf,
    pub rrtype: u16,
    /// Raw class field, cache-flush bit included.
    pub class: u16,
    pub ttl: u32,
    pub rdata: &'a [u8],
    pub rdata_off: usize,
}

impl Record<'_> {
    pub fn class_masked(&self) -> u16 {
        self.class & !CACHE_FLUSH
    }

    pub fn cache_flush(&self) -> bool {
        self.class & CACHE_FLUSH != 0
    }
}

/// Walks one message: the header, then [`Reader::question`] `qdcount` times, then
/// [`Reader::record`] `ancount + nscount + arcount` times, in wire order. The reader does not
/// count for you; [`respond`] shows the loop.
pub struct Reader<'a> {
    pub header: Header,
    msg: &'a [u8],
    off: usize,
}

impl<'a> Reader<'a> {
    pub fn new(msg: &'a [u8]) -> Result<Reader<'a>> {
        let header = Header::parse(msg)?;
        Ok(Reader {
            header,
            msg,
            off: HEADER_LEN,
        })
    }

    fn u16_at(&self, off: usize) -> Result<u16> {
        let b = self.msg.get(off..off + 2).ok_or(Error::Truncated)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn question(&mut self) -> Result<Question> {
        let (name, off) = decode_name(self.msg, self.off)?;
        let qtype = self.u16_at(off)?;
        let qclass = self.u16_at(off + 2)?;
        self.off = off + 4;
        Ok(Question {
            name,
            qtype,
            qclass,
        })
    }

    pub fn record(&mut self) -> Result<Record<'a>> {
        let (name, off) = decode_name(self.msg, self.off)?;
        let rrtype = self.u16_at(off)?;
        let class = self.u16_at(off + 2)?;
        let ttl_b = self.msg.get(off + 4..off + 8).ok_or(Error::Truncated)?;
        let ttl = u32::from_be_bytes([ttl_b[0], ttl_b[1], ttl_b[2], ttl_b[3]]);
        let rdlen = self.u16_at(off + 8)? as usize;
        let rdata_off = off + 10;
        let rdata = self
            .msg
            .get(rdata_off..rdata_off + rdlen)
            .ok_or(Error::Truncated)?;
        self.off = rdata_off + rdlen;
        Ok(Record {
            name,
            rrtype,
            class,
            ttl,
            rdata,
            rdata_off,
        })
    }
}

/// Decode a PTR record's target name, decompressing against the whole message.
pub fn ptr_target(msg: &[u8], rec: &Record<'_>) -> Result<NameBuf> {
    let (name, end) = decode_name(msg, rec.rdata_off)?;
    // The name must lie within the RDATA it claims to be (its pointers may reach anywhere).
    if end > rec.rdata_off + rec.rdata.len() {
        return Err(Error::Truncated);
    }
    Ok(name)
}

/// SRV RDATA, decoded (RFC 2782 layout: priority, weight, port, target name).
#[derive(Debug, Clone, Copy)]
pub struct Srv {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: NameBuf,
}

/// Decode an SRV record's fields, decompressing the target against the whole message. (RFC 2782
/// forbids compressing SRV targets; RFC 6762 §18.14 re-allows it for mDNS and the reference router
/// does it, so the decoder must cope either way.)
pub fn srv_fields(msg: &[u8], rec: &Record<'_>) -> Result<Srv> {
    let b = rec.rdata.get(..6).ok_or(Error::Truncated)?;
    let (target, end) = decode_name(msg, rec.rdata_off + 6)?;
    if end > rec.rdata_off + rec.rdata.len() {
        return Err(Error::Truncated);
    }
    Ok(Srv {
        priority: u16::from_be_bytes([b[0], b[1]]),
        weight: u16::from_be_bytes([b[2], b[3]]),
        port: u16::from_be_bytes([b[4], b[5]]),
        target,
    })
}

/// Iterate a TXT record's length-prefixed strings. A truncated final entry yields one `Err` and
/// then ends; RFC 6763 §6.1's "a TXT record must contain at least one string, possibly empty" is
/// the *emitter's* obligation ([`txt_rdata`] keeps it), so an empty RDATA here simply yields
/// nothing.
pub struct TxtIter<'a> {
    rest: &'a [u8],
}

pub fn txt_strings(rdata: &[u8]) -> TxtIter<'_> {
    TxtIter { rest: rdata }
}

impl<'a> Iterator for TxtIter<'a> {
    type Item = Result<&'a [u8]>;

    fn next(&mut self) -> Option<Self::Item> {
        let (&n, tail) = self.rest.split_first()?;
        match tail.split_at_checked(n as usize) {
            Some((s, rest)) => {
                self.rest = rest;
                Some(Ok(s))
            }
            None => {
                self.rest = &[];
                Some(Err(Error::Truncated))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Building a message.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Section {
    Question,
    Answer,
    Authority,
    Additional,
}

/// Serialises one message into a caller-supplied buffer: header last (it holds the counts), then
/// questions and records in section order, enforced. Names go out uncompressed (see BUGS above).
pub struct Builder<'a> {
    buf: &'a mut [u8],
    len: usize,
    header: Header,
    section: Section,
}

impl<'a> Builder<'a> {
    pub fn new(buf: &'a mut [u8], id: u16, flags: u16) -> Result<Builder<'a>> {
        if buf.len() < HEADER_LEN {
            return Err(Error::Overflow);
        }
        Ok(Builder {
            buf,
            len: HEADER_LEN,
            header: Header {
                id,
                flags,
                ..Header::default()
            },
            section: Section::Question,
        })
    }

    /// A multicast query: ID 0, no flags (RFC 6762 §18.1).
    pub fn query(buf: &'a mut [u8]) -> Result<Builder<'a>> {
        Builder::new(buf, 0, 0)
    }

    /// A multicast response: ID 0, QR and AA set.
    pub fn response(buf: &'a mut [u8]) -> Result<Builder<'a>> {
        Builder::new(buf, 0, FLAG_QR | FLAG_AA)
    }

    /// A legacy unicast response echoes the querier's ID (RFC 6762 §6.7).
    pub fn legacy_response(buf: &'a mut [u8], id: u16) -> Result<Builder<'a>> {
        Builder::new(buf, id, FLAG_QR | FLAG_AA)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self.len.checked_add(bytes.len()).ok_or(Error::Overflow)?;
        if end > self.buf.len() {
            return Err(Error::Overflow);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn enter(&mut self, s: Section) -> Result<()> {
        if s < self.section {
            return Err(Error::SectionOrder);
        }
        self.section = s;
        Ok(())
    }

    pub fn question(&mut self, name: &[u8], qtype: u16, qclass: u16) -> Result<()> {
        self.enter(Section::Question)?;
        // Only the Question variant equals itself under `<`'s derivation, so re-entry is fine; a
        // question after any record is not, and the count fields would silently lie about order.
        if self.header.ancount + self.header.nscount + self.header.arcount != 0 {
            return Err(Error::SectionOrder);
        }
        self.put(name)?;
        self.put(&qtype.to_be_bytes())?;
        self.put(&qclass.to_be_bytes())?;
        self.header.qdcount += 1;
        Ok(())
    }

    fn rr(&mut self, name: &[u8], rrtype: u16, class: u16, ttl: u32, rdata: &[u8]) -> Result<()> {
        if rdata.len() > u16::MAX as usize {
            return Err(Error::RdataTooLong);
        }
        self.put(name)?;
        self.put(&rrtype.to_be_bytes())?;
        self.put(&class.to_be_bytes())?;
        self.put(&ttl.to_be_bytes())?;
        self.put(&(rdata.len() as u16).to_be_bytes())?;
        self.put(rdata)
    }

    pub fn answer(
        &mut self,
        name: &[u8],
        rrtype: u16,
        class: u16,
        ttl: u32,
        rdata: &[u8],
    ) -> Result<()> {
        self.enter(Section::Answer)?;
        if self.header.nscount + self.header.arcount != 0 {
            return Err(Error::SectionOrder);
        }
        self.rr(name, rrtype, class, ttl, rdata)?;
        self.header.ancount += 1;
        Ok(())
    }

    pub fn authority(
        &mut self,
        name: &[u8],
        rrtype: u16,
        class: u16,
        ttl: u32,
        rdata: &[u8],
    ) -> Result<()> {
        self.enter(Section::Authority)?;
        if self.header.arcount != 0 {
            return Err(Error::SectionOrder);
        }
        self.rr(name, rrtype, class, ttl, rdata)?;
        self.header.nscount += 1;
        Ok(())
    }

    pub fn additional(
        &mut self,
        name: &[u8],
        rrtype: u16,
        class: u16,
        ttl: u32,
        rdata: &[u8],
    ) -> Result<()> {
        self.enter(Section::Additional)?;
        self.rr(name, rrtype, class, ttl, rdata)?;
        self.header.arcount += 1;
        Ok(())
    }

    /// Write the header and hand back the message length. Consumes the builder: the counts are
    /// final.
    pub fn finish(self) -> usize {
        // Infallible: `new` guaranteed HEADER_LEN bytes.
        let _ = self.header.write(self.buf);
        self.len
    }
}

/// Encode SRV RDATA (priority, weight, port, then the target name, uncompressed).
pub fn srv_rdata(
    priority: u16,
    weight: u16,
    port: u16,
    target: &NameBuf,
    out: &mut [u8],
) -> Result<usize> {
    let t = target.as_bytes();
    let need = 6 + t.len();
    if out.len() < need {
        return Err(Error::Overflow);
    }
    out[0..2].copy_from_slice(&priority.to_be_bytes());
    out[2..4].copy_from_slice(&weight.to_be_bytes());
    out[4..6].copy_from_slice(&port.to_be_bytes());
    out[6..need].copy_from_slice(t);
    Ok(need)
}

/// Encode TXT RDATA from entries (each becomes one length-prefixed string, `key=value` by DNS-SD
/// convention, but the bytes are the caller's). No entries encodes as the single empty string RFC
/// 6763 §6.1 requires; the reference router's `_smb._tcp` TXT is exactly that byte.
pub fn txt_rdata(entries: &[&[u8]], out: &mut [u8]) -> Result<usize> {
    if entries.is_empty() {
        if out.is_empty() {
            return Err(Error::Overflow);
        }
        out[0] = 0;
        return Ok(1);
    }
    let mut len = 0;
    for e in entries {
        if e.len() > 255 {
            return Err(Error::EntryTooLong);
        }
        let end = len + 1 + e.len();
        if end > out.len() {
            return Err(Error::Overflow);
        }
        out[len] = e.len() as u8;
        out[len + 1..end].copy_from_slice(e);
        len = end;
    }
    Ok(len)
}

// ---------------------------------------------------------------------------
// The Time Machine advertisement: DNS-SD structuring over the wire layer.
// ---------------------------------------------------------------------------

/// One advertised backup disk. Both fields are data: the volume name is the share's, and the flags
/// are whatever the capture says a Mac accepts (`0x82` on the reference, 2026-08-15).
#[derive(Debug, Clone, Copy)]
pub struct Disk<'a> {
    pub volume: &'a str,
    /// The `adVF` value, spelled as it goes on the wire (`"0x82"`).
    pub flags: &'a str,
}

/// Everything the responder advertises, as data. The captured reference values live in the tests
/// and in notes/mdns.md; nothing here hard-codes them.
#[derive(Debug, Clone, Copy)]
pub struct Advertisement<'a> {
    /// One label, used as the instance name of all three services and as the hostname stem
    /// (`host.local`). The reference uses its model name, `GL-BE9300`.
    pub host: &'a str,
    /// The port `_smb._tcp`'s SRV advertises (445). `_adisk` and `_device-info` advertise port 0,
    /// which is measured behaviour, not an option.
    pub smb_port: u16,
    /// Every share, in `dk0..dkN` order. One advertisement carries all of them.
    pub disks: &'a [Disk<'a>],
    /// The server-wide `sys=adVF=` value (`"0x100"` on the reference).
    pub sys_flags: &'a str,
    /// The `_device-info` `model=` value. Measured on the reference: `MacSamba`, despite the
    /// Samba-side `fruit:model = TimeCapsule`; the two knobs are independent.
    pub model: &'a str,
    /// The host's IPv4 address, if the responder knows it (an A record rides along in responses).
    pub ipv4: Option<[u8; 4]>,
}

/// Write one `dkN=adVN=<volume>,adVF=<flags>` TXT entry. Public so a test or a future responder
/// can emit a single disk's entry; [`adisk_txt_rdata`] does the whole record.
pub fn dk_entry(index: usize, disk: &Disk<'_>, out: &mut [u8]) -> Result<usize> {
    let mut w = SliceWriter { out, len: 0 };
    w.str("dk")?;
    w.decimal(index)?;
    w.str("=adVN=")?;
    w.str(disk.volume)?;
    w.str(",adVF=")?;
    w.str(disk.flags)?;
    Ok(w.len)
}

struct SliceWriter<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl SliceWriter<'_> {
    fn str(&mut self, s: &str) -> Result<()> {
        let end = self.len + s.len();
        if end > self.out.len() {
            return Err(Error::Overflow);
        }
        self.out[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }

    fn decimal(&mut self, mut v: usize) -> Result<()> {
        let mut digits = [0u8; 20];
        let mut n = 0;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            let d = digits[n];
            if self.len >= self.out.len() {
                return Err(Error::Overflow);
            }
            self.out[self.len] = d;
            self.len += 1;
        }
        Ok(())
    }
}

/// The `_adisk._tcp` TXT RDATA: every disk as a `dkN=` entry, then `sys=adVF=<flags>`. This layout
/// (per-server record, disks inside it) is the measured reference structure; emitting one
/// announcement per share would be wrong.
pub fn adisk_txt_rdata(adv: &Advertisement<'_>, out: &mut [u8]) -> Result<usize> {
    let mut len = 0;
    let mut entry = [0u8; 255];
    for (i, disk) in adv.disks.iter().enumerate() {
        let n = dk_entry(i, disk, &mut entry)?;
        len += put_txt_entry(&entry[..n], out, len)?;
    }
    let mut w = SliceWriter {
        out: &mut entry,
        len: 0,
    };
    w.str("sys=adVF=")?;
    w.str(adv.sys_flags)?;
    let n = w.len;
    len += put_txt_entry(&entry[..n], out, len)?;
    Ok(len)
}

/// The `_device-info._tcp` TXT RDATA: `model=<model>`.
pub fn device_info_txt_rdata(adv: &Advertisement<'_>, out: &mut [u8]) -> Result<usize> {
    let mut entry = [0u8; 255];
    let mut w = SliceWriter {
        out: &mut entry,
        len: 0,
    };
    w.str("model=")?;
    w.str(adv.model)?;
    let n = w.len;
    put_txt_entry(&entry[..n], out, 0)
}

fn put_txt_entry(entry: &[u8], out: &mut [u8], at: usize) -> Result<usize> {
    if entry.len() > 255 {
        return Err(Error::EntryTooLong);
    }
    let end = at + 1 + entry.len();
    if end > out.len() {
        return Err(Error::Overflow);
    }
    out[at] = entry.len() as u8;
    out[at + 1..end].copy_from_slice(entry);
    Ok(1 + entry.len())
}

/// Where the query came from, which RFC 6762 §6.7 makes semantically load-bearing: a querier whose
/// source port is not 5353 cannot receive multicast, so it gets a unicast response with the ID
/// echoed, the question included, every TTL capped at [`TTL_LEGACY_CAP`], and no cache-flush bits.
/// The crate never sees the socket, so the caller says which case this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySource {
    /// Source port 5353: a real mDNS querier. Respond to the multicast group.
    Multicast,
    /// Any other source port: a one-shot legacy querier. Respond unicast to the source.
    LegacyUnicast,
}

// The answer units respond() can decide to emit, one bit each.
const U_PTR_SMB: u16 = 1 << 0;
const U_PTR_ADISK: u16 = 1 << 1;
const U_PTR_DEVINFO: u16 = 1 << 2;
const U_ENUM: u16 = 1 << 3;
const U_SRV_SMB: u16 = 1 << 4;
const U_SRV_ADISK: u16 = 1 << 5;
const U_SRV_DEVINFO: u16 = 1 << 6;
const U_TXT_SMB: u16 = 1 << 7;
const U_TXT_ADISK: u16 = 1 << 8;
const U_TXT_DEVINFO: u16 = 1 << 9;
const U_HOST_A: u16 = 1 << 10;

/// The responder's whole decision, as one pure function: one received datagram in, either the
/// response datagram or the decision to stay silent out.
///
/// Handles PTR browses of the three service types, the service-type enumeration (RFC 6763 §9),
/// SRV/TXT/ANY queries on the three instance names, and A/ANY on the hostname. Applies known-answer
/// suppression to the PTR browses (see BUGS for its limits), and every legacy-unicast rule in one
/// place. Anything else in the datagram is silence, which is mDNS's own rule: a responder answers
/// what it is authoritative for and says nothing about the rest.
pub fn respond(
    adv: &Advertisement<'_>,
    query: &[u8],
    out: &mut [u8],
    src: QuerySource,
) -> Result<Option<usize>> {
    let mut r = Reader::new(query)?;
    if !r.header.is_query() {
        return Ok(None);
    }

    let smb = name_from_dotted(SERVICE_SMB)?;
    let adisk = name_from_dotted(SERVICE_ADISK)?;
    let devinfo = name_from_dotted(SERVICE_DEVICE_INFO)?;
    let enumeration = name_from_dotted(SERVICE_ENUMERATION)?;
    let local = name_from_dotted("local")?;
    let hostname = name_under(adv.host.as_bytes(), &local)?;
    let inst_smb = name_under(adv.host.as_bytes(), &smb)?;
    let inst_adisk = name_under(adv.host.as_bytes(), &adisk)?;
    let inst_devinfo = name_under(adv.host.as_bytes(), &devinfo)?;

    let mut units: u16 = 0;
    let mut questions = [None::<Question>; MAX_QUESTIONS];
    let mut nq = 0;
    for _ in 0..r.header.qdcount {
        let q = r.question()?;
        if nq < MAX_QUESTIONS {
            questions[nq] = Some(q);
            nq += 1;
        }
        if q.class() != CLASS_IN && q.class() != 255 {
            continue;
        }
        let name = q.name.as_bytes();
        let wants = |t: u16| q.qtype == t || q.qtype == rrtype::ANY;
        if wants(rrtype::PTR) {
            if names_equal(name, smb.as_bytes()) {
                units |= U_PTR_SMB;
            }
            if names_equal(name, adisk.as_bytes()) {
                units |= U_PTR_ADISK;
            }
            if names_equal(name, devinfo.as_bytes()) {
                units |= U_PTR_DEVINFO;
            }
            if names_equal(name, enumeration.as_bytes()) {
                units |= U_ENUM;
            }
        }
        for (inst, srv_bit, txt_bit) in [
            (&inst_smb, U_SRV_SMB, U_TXT_SMB),
            (&inst_adisk, U_SRV_ADISK, U_TXT_ADISK),
            (&inst_devinfo, U_SRV_DEVINFO, U_TXT_DEVINFO),
        ] {
            if names_equal(name, inst.as_bytes()) {
                if wants(rrtype::SRV) {
                    units |= srv_bit;
                }
                if wants(rrtype::TXT) {
                    units |= txt_bit;
                }
            }
        }
        if wants(rrtype::A) && names_equal(name, hostname.as_bytes()) && adv.ipv4.is_some() {
            units |= U_HOST_A;
        }
    }

    // Known-answer suppression (RFC 6762 §7.1), PTR browses only: a querier that already holds our
    // PTR with at least half its TTL left is not answered again.
    for _ in 0..r.header.ancount {
        let rec = r.record()?;
        if rec.rrtype != rrtype::PTR || rec.ttl < TTL_SERVICE / 2 {
            continue;
        }
        let Ok(target) = ptr_target(query, &rec) else {
            continue;
        };
        for (svc, inst, bit) in [
            (&smb, &inst_smb, U_PTR_SMB),
            (&adisk, &inst_adisk, U_PTR_ADISK),
            (&devinfo, &inst_devinfo, U_PTR_DEVINFO),
        ] {
            if names_equal(rec.name.as_bytes(), svc.as_bytes())
                && names_equal(target.as_bytes(), inst.as_bytes())
            {
                units &= !bit;
            }
        }
    }

    if units == 0 {
        return Ok(None);
    }

    let legacy = src == QuerySource::LegacyUnicast;
    let mut b = if legacy {
        Builder::legacy_response(out, r.header.id)?
    } else {
        Builder::response(out)?
    };
    if legacy {
        // RFC 6762 §6.7: a legacy response carries the question it answers.
        for q in questions.iter().take(nq).flatten() {
            b.question(q.name.as_bytes(), q.qtype, q.qclass & !QU)?;
        }
    }
    let ttl_svc = if legacy {
        TTL_LEGACY_CAP.min(TTL_SERVICE)
    } else {
        TTL_SERVICE
    };
    let ttl_host = if legacy {
        TTL_LEGACY_CAP.min(TTL_HOST)
    } else {
        TTL_HOST
    };
    // Unique (cache-flushable) records: never in a legacy response (RFC 6762 §10.2).
    let unique = if legacy {
        CLASS_IN
    } else {
        CLASS_IN | CACHE_FLUSH
    };

    let mut rdata = [0u8; MAX_TXT_RDATA];

    // The PTR browse answers, each dragging its instance records along. In a multicast response
    // the instance records ride in additionals (RFC 6763 §12.1); a legacy response puts everything
    // in the answer section, which is the shape the reference router produces and the shape a
    // one-shot resolver expects to find.
    let mut trailing: u16 = 0;
    for (svc, inst, bit, srv_bit, txt_bit) in [
        (&smb, &inst_smb, U_PTR_SMB, U_SRV_SMB, U_TXT_SMB),
        (&adisk, &inst_adisk, U_PTR_ADISK, U_SRV_ADISK, U_TXT_ADISK),
        (
            &devinfo,
            &inst_devinfo,
            U_PTR_DEVINFO,
            U_SRV_DEVINFO,
            U_TXT_DEVINFO,
        ),
    ] {
        if units & bit != 0 {
            b.answer(
                svc.as_bytes(),
                rrtype::PTR,
                CLASS_IN,
                ttl_svc,
                inst.as_bytes(),
            )?;
            trailing |= srv_bit | txt_bit | U_HOST_A;
        }
    }
    if units & U_ENUM != 0 {
        for svc in [&smb, &adisk, &devinfo] {
            b.answer(
                enumeration.as_bytes(),
                rrtype::PTR,
                CLASS_IN,
                ttl_svc,
                svc.as_bytes(),
            )?;
        }
    }

    // Directly queried instance records go in answers; browse-implied ones go in additionals
    // (multicast) or also in answers (legacy). Emit the answer-section ones first, then the rest.
    let direct = units;
    let implied = trailing & !direct & if adv.ipv4.is_some() { !0 } else { !U_HOST_A };
    for phase in 0..2 {
        let (set, additional) = if phase == 0 {
            (direct, false)
        } else {
            (implied, !legacy)
        };
        let emit = |b: &mut Builder<'_>,
                    name: &NameBuf,
                    rrtype_: u16,
                    class: u16,
                    ttl: u32,
                    rdata: &[u8]|
         -> Result<()> {
            if additional {
                b.additional(name.as_bytes(), rrtype_, class, ttl, rdata)
            } else {
                b.answer(name.as_bytes(), rrtype_, class, ttl, rdata)
            }
        };
        for (inst, srv_bit, txt_bit, port) in [
            (&inst_smb, U_SRV_SMB, U_TXT_SMB, adv.smb_port),
            (&inst_adisk, U_SRV_ADISK, U_TXT_ADISK, 0u16),
            (&inst_devinfo, U_SRV_DEVINFO, U_TXT_DEVINFO, 0u16),
        ] {
            if set & srv_bit != 0 {
                let n = srv_rdata(0, 0, port, &hostname, &mut rdata)?;
                emit(&mut b, inst, rrtype::SRV, unique, ttl_svc, &rdata[..n])?;
            }
            if set & txt_bit != 0 {
                let n = if txt_bit == U_TXT_SMB {
                    txt_rdata(&[], &mut rdata)?
                } else if txt_bit == U_TXT_ADISK {
                    adisk_txt_rdata(adv, &mut rdata)?
                } else {
                    device_info_txt_rdata(adv, &mut rdata)?
                };
                emit(&mut b, inst, rrtype::TXT, unique, ttl_svc, &rdata[..n])?;
            }
        }
        if set & U_HOST_A != 0
            && let Some(ip) = adv.ipv4
        {
            emit(&mut b, &hostname, rrtype::A, unique, ttl_host, &ip)?;
        }
    }

    Ok(Some(b.finish()))
}

/// One of the three service types an [`Advertisement`] carries, as a selector for
/// [`announcement`]. Not a general service registry: these three are what Time Machine discovery
/// uses, and a fourth would be a different feature rather than another enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `_smb._tcp`, the file service and its port.
    Smb,
    /// `_adisk._tcp`, the Time Machine flags that populate the backup-disk list.
    Adisk,
    /// `_device-info._tcp`, the model string that picks the icon.
    DeviceInfo,
}

/// **Announce one service type**, RFC 6762 §8.3: an unsolicited multicast response carrying this
/// advertisement's records for `svc`, sent when a responder starts up (and, in a full
/// implementation, two to eight times at increasing intervals once probing has claimed the name).
///
/// Two things differ from what [`respond`] builds for a browse of the same type, and both are the
/// section's own rules. **Every record goes in the answer section**, additionals included, because
/// an announcement is not answering a question and has nothing to imply. And the records are the
/// full set for the type: the PTR, the instance's SRV and TXT, and the host's A when the responder
/// knows its address.
///
/// **One service type per call, because of the MTU rather than the RFC.** RFC 6762 permits every
/// record in one message, and a responder on an ordinary ethernet can do that; this tree's virtio
/// transport carries a 576-byte MTU (`user/src/net_transport.rs`, a single-page DMA region), so all
/// three types in one datagram would not fit and would be dropped rather than fragmented. Three
/// calls and three datagrams is the same announcement, and a caller with a real MTU loses nothing
/// by sending them separately.
pub fn announcement(adv: &Advertisement<'_>, svc: Service, out: &mut [u8]) -> Result<usize> {
    let (service, port) = match svc {
        Service::Smb => (SERVICE_SMB, adv.smb_port),
        // Measured, not chosen: the reference advertises port 0 on both of these, because they
        // carry flags and a model string rather than a connectable service.
        Service::Adisk => (SERVICE_ADISK, 0),
        Service::DeviceInfo => (SERVICE_DEVICE_INFO, 0),
    };
    let service = name_from_dotted(service)?;
    let local = name_from_dotted("local")?;
    let hostname = name_under(adv.host.as_bytes(), &local)?;
    let instance = name_under(adv.host.as_bytes(), &service)?;

    let mut b = Builder::response(out)?;
    let mut rdata = [0u8; MAX_TXT_RDATA];

    // The PTR is a SHARED record (several responders may offer the same service type), so it never
    // carries the cache-flush bit; the three below are this responder's alone and do.
    b.answer(
        service.as_bytes(),
        rrtype::PTR,
        CLASS_IN,
        TTL_SERVICE,
        instance.as_bytes(),
    )?;
    let n = srv_rdata(0, 0, port, &hostname, &mut rdata)?;
    b.answer(
        instance.as_bytes(),
        rrtype::SRV,
        CLASS_IN | CACHE_FLUSH,
        TTL_SERVICE,
        &rdata[..n],
    )?;
    let n = match svc {
        Service::Smb => txt_rdata(&[], &mut rdata)?,
        Service::Adisk => adisk_txt_rdata(adv, &mut rdata)?,
        Service::DeviceInfo => device_info_txt_rdata(adv, &mut rdata)?,
    };
    b.answer(
        instance.as_bytes(),
        rrtype::TXT,
        CLASS_IN | CACHE_FLUSH,
        TTL_SERVICE,
        &rdata[..n],
    )?;
    if let Some(ip) = adv.ipv4 {
        b.answer(
            hostname.as_bytes(),
            rrtype::A,
            CLASS_IN | CACHE_FLUSH,
            TTL_HOST,
            &ip,
        )?;
    }
    Ok(b.finish())
}

/// How many questions a legacy response can echo back. More arrive intact (they are read and
/// answered); only the echo is capped, and real queriers send one.
pub const MAX_QUESTIONS: usize = 8;

/// The largest TXT RDATA [`respond`] can assemble. Ten disks' worth with room over; the reference
/// record is 67 bytes.
pub const MAX_TXT_RDATA: usize = 512;

// ---------------------------------------------------------------------------
// Probe-before-claim: the wire-format halves of RFC 6762 §8.
// ---------------------------------------------------------------------------

/// A record a prober proposes to own, carried in the probe's authority section.
#[derive(Debug, Clone, Copy)]
pub struct ProposedRecord<'a> {
    pub rrtype: u16,
    pub ttl: u32,
    pub rdata: &'a [u8],
}

/// Build a probe query for `claim`: one `ANY` question (QU set, per RFC 6762 §8.1's advice for the
/// first probe) plus the proposed records in the authority section, which is what lets a
/// simultaneous prober run the tiebreak instead of both backing off forever.
pub fn probe_query(
    claim: &NameBuf,
    proposed: &[ProposedRecord<'_>],
    out: &mut [u8],
) -> Result<usize> {
    let mut b = Builder::query(out)?;
    b.question(claim.as_bytes(), rrtype::ANY, CLASS_IN | QU)?;
    for p in proposed {
        b.authority(claim.as_bytes(), p.rrtype, CLASS_IN, p.ttl, p.rdata)?;
    }
    Ok(b.finish())
}

/// Does this received message defeat our probe? True when any answer record carries the claimed
/// name (case-insensitive): somebody already owns it, and the prober must rename or tiebreak.
pub fn conflicts(msg: &[u8], claim: &NameBuf) -> Result<bool> {
    let mut r = Reader::new(msg)?;
    for _ in 0..r.header.qdcount {
        r.question()?;
    }
    let records = r.header.ancount as usize + r.header.nscount as usize + r.header.arcount as usize;
    for _ in 0..records {
        let rec = r.record()?;
        if names_equal(rec.name.as_bytes(), claim.as_bytes()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// RFC 6762 §8.2.1's simultaneous-probe tiebreak: compare (class, type, raw RDATA)
/// lexicographically; the greater party wins and the lesser waits and re-probes. Both RDATAs must
/// be in uncompressed form (ours always is; decompress theirs with [`ptr_target`]/[`srv_fields`]
/// first when the type contains a name).
pub fn tiebreak(ours: (u16, u16, &[u8]), theirs: (u16, u16, &[u8])) -> Ordering {
    let key = |(class, rrtype_, _): (u16, u16, &[u8])| (class & !CACHE_FLUSH, rrtype_);
    key(ours)
        .cmp(&key(theirs))
        .then_with(|| ours.2.cmp(theirs.2))
}

#[cfg(test)]
mod tests;

// Machine-checked proofs (Kani). Behind `#[cfg(kani)]`, so an ordinary build or test never sees
// them; `script/verify` runs them.
#[cfg(kani)]
mod proofs;
