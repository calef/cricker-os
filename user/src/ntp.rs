//! **The NTP client, and the test server that answers it** (milestone 51; DECISIONS §43 and §44,
//! notes/ntp.md).
//!
//! The component that turns `ntp_proto`'s wire format into an actual clock correction. It holds a
//! **network capability** and a capability to **propose** a time. It does not hold the clock.
//!
//! # The endowment is the argument
//!
//! ```text
//!   entropy ──an endpoint──►┌──────────────┐──an endpoint──► net_stack ──► the network
//!   (8 random bytes,        │  ntp client  │ (the socket contract, UDP 123)
//!    the nonce)             └──────┬───────┘
//!                                  │ an endpoint: PROPOSE
//!                          ┌───────▼────────┐
//!                          │ clock service  │──writes──► the clock page
//!                          └────────────────┘   (this process holds NO mapping of it,
//!                                                read-only or otherwise)
//! ```
//!
//! Five slots, and the interesting one is the slot that is missing. There is no clock page here,
//! writable or not, so "set the time" is not an operation this process can express: the only thing
//! it can do with the wall clock is ask, and `clock_proto::policy` answers. **A compromised NTP
//! client can lie inside the service's bounds and can do nothing else.** In Unix `ntpd` runs as root
//! and may set the clock to anything at all. `an_ntp_client_holds_no_writable_clock_page` in
//! `kernel/src/user.rs` proves the claim the way the machine proves things: the same binary, given
//! the same five slots plus the address at which a *setter* maps the clock page, writes there and
//! dies of a fault.
//!
//! # The nonce comes from the entropy service, or the client stops
//!
//! `Query::with_nonce` exists because a server never interprets the client's transmit timestamp; it
//! copies it into the origin field and does nothing else. So 64 random bits on the wire, with the
//! true send time kept locally, take an off-path attacker from about twelve bits of guesswork to
//! sixty-four (notes/ntp.md).
//!
//! That is worth exactly as much as the bits are unguessable, and until 2026-07-30 the only source
//! here was splitmix64 seeded off the virtual counter, which notes/entropy.md calls "predictable to
//! anyone who could guess boot-relative time". So the nonce is drawn from the entropy service
//! (DECISIONS §44), and a client with no entropy capability **refuses to send anything**: it reports
//! [`RPT_NO_ENTROPY`] and exits. Falling back to the weak stream would be §42's silent degradation
//! in the one place where the whole point of the value is that nobody can predict it, and it is the
//! same call `SystemRng` makes when it panics rather than degrading.
//!
//! # One shot, because there is no timed wait
//!
//! The kernel's syscall surface is `EXIT`, `YIELD`, `INVOKE`, `CAP_DELETE`. There is no sleep, no
//! timeout, and no deadline anywhere in it (the milestone 51 block's open fork), so a poll interval
//! is a yield-spin: a thread that stays runnable for the whole interval and costs scheduler work in
//! proportion to it. At NTP's ordinary 64-second poll that is not a service anybody should ship, so
//! this is a **one-shot synchroniser**: up to [`ATTEMPTS`] requests a few milliseconds apart, one
//! proposal, exit. A long-running client is the timed-wait fork's to build, not a workaround to
//! invent here.
//!
//! **A kiss-o'-death is not retried.** Stratum 0 is an instruction rather than a time (`RATE` means
//! back off, `DENY` means go away), and a client that retries into one is the abusive client the
//! packet exists to stop.
//!
//! # Roles
//!
//! One binary, three roles selected by `arg0`, which is how every other multi-part program here is
//! packed and keeps the initrd's directory small:
//!
//! | role | what it is | `arg1` | `arg2` |
//! |---|---|---|---|
//! | [`ROLE_CLIENT`] | the client above | the server's IPv4, packed big-endian | the server's UDP port |
//! | [`ROLE_SERVER`] | the test server (below) | one of the `SRV_*` variants | the wall-clock nanoseconds it claims |
//! | [`ROLE_PROBE_CLOCK`] | the client's endowment, pointed at the clock page | the address to write | unused |
//!
//! # The test server, and what it does and does not prove
//!
//! [`ROLE_SERVER`] is **both an NTP server and the network**: it holds `READ` on an endpoint the
//! client was given `WRITE` on, and it speaks the same socket contract (`crates/socket_proto/src/lib.rs`)
//! `net_stack` does. The client cannot tell, and that is the point rather than a convenience: the
//! client's network path is one endpoint capability, so substituting the peer at that boundary runs
//! the client's **real, unmodified code**. There is no test-only branch anywhere in the client.
//!
//! What that proves: the socket-contract glue (minting a frame, delegating it, the destination
//! header, `SENDTO`/`RECV` framing), that the 48 bytes on the wire are a well-formed NTPv4 client
//! packet addressed to port 123, that the nonce is unpredictable, that a reply failing
//! `Query::accept` moves nothing, and that an accepted sample becomes a *proposal* the clock service
//! judges.
//!
//! What it does not prove: that smoltcp, UDP, IPv4 and the NIC carry those bytes. Milestone 30's
//! socket-contract tests prove that path with a real datagram, and this lane deliberately does not
//! re-prove it. Nor does it prove anything about a real internet time server: nothing in QEMU's
//! slirp answers UDP 123, and pointing the gate at a public server would make it depend on somebody
//! else's network. The honest summary is that the client is proven against a server we wrote, over a
//! network we wrote, and the parts we did not write are proven elsewhere.
//!
//! Name: unrecorded. Introduced 2026-07-31 with milestone 55; the protocol's own name, which would
//! put it in the tenet's protected group had anyone filed it there.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::{endpoint, frame as fr, rights, untyped as ut};
use clock_proto::propose;
use ntp_proto::{Packet, Query, Reject, Short, Timestamp, leap, mode};
// The socket contract, verbatim from the file `net_stack` compiles, so the client and the test
// server cannot drift from the real server's idea of the wire format.
// An NTP client speaks the UDP half of the contract and never the TCP half, so the rest of the
// file is dead here. Allowed rather than trimmed: the value of compiling the *same file* net_stack
// does is that the two cannot drift, and a per-consumer subset would throw that away.
#[allow(dead_code)]
use socket_proto::*;
use user_rt::{call, cap_delete, cntfrq, exit, invoke, now, recv_cap, reply, send, yield_now};

// =================================================================================================
// The roles, the slots, and the words this program reports.
// =================================================================================================

/// The NTP client. `arg1` is the server's IPv4 packed big-endian, `arg2` its UDP port.
pub const ROLE_CLIENT: u64 = 0;
/// The test server and stub network. `arg1` selects an `SRV_*` variant, `arg2` is the wall-clock
/// time in nanoseconds it claims to have.
pub const ROLE_SERVER: u64 = 1;
/// The client's endowment, pointed at the clock page. `arg1` is the address to write.
pub const ROLE_PROBE_CLOCK: u64 = 2;

/// Slot 0: the report endpoint (WRITE). Every role has one.
const REPORT: u64 = 0;
/// Slot 1: the socket contract's endpoint. **The client's whole network authority.** The client
/// holds `WRITE`, the test server `READ`.
const STACK: u64 = 1;
/// Slot 2: an untyped budget, to mint and map the one shared frame.
const UNTYPED: u64 = 2;
/// Slot 3: the clock service's **propose** endpoint (WRITE). Not a page, and not a way to set
/// anything.
const PROPOSE: u64 = 3;
/// Slot 4: the entropy service's endpoint (WRITE): "you may obtain randomness", naming no device.
const ENTROPY: u64 = 4;

/// A sample was accepted and proposed. `w1` is the `clock_proto::status` the service answered with,
/// `w2` the nanoseconds proposed.
pub const RPT_SYNCED: u64 = 1;
/// A reply arrived and `Query::accept` refused it. `w1` is a [`reject_code`], `w2` the number of
/// requests sent. **Nothing was proposed.**
pub const RPT_REJECTED: u64 = 2;
/// No reply after [`ATTEMPTS`] requests. `w1` is the number sent.
pub const RPT_NO_REPLY: u64 = 3;
/// **No unguessable bits, so no request.** `w1` is the entropy service's reply word (or the kernel
/// error from a `CALL` on an empty slot, which is what "no capability" looks like).
pub const RPT_NO_ENTROPY: u64 = 4;
/// The socket contract refused us. `w1` names the step, so a wiring mistake is not a mystery.
pub const RPT_NET_ERROR: u64 = 5;
/// [`ROLE_PROBE_CLOCK`], about to write the clock page at `w1`. The next thing this process does is
/// fault; anything after this report means it did not.
pub const RPT_PROBING: u64 = 6;
/// The test server saw its first request. `w1` is the request's transmit field (the nonce), `w2` is
/// `(dst_port << 32) | (version << 8) | mode`.
pub const RPT_SERVED: u64 = 7;
/// The local wall clock is outside the range an NTP timestamp can hold, so no exchange is possible.
pub const RPT_BAD_LOCAL_TIME: u64 = 8;

/// Which reply the test server sends.
pub mod srv {
    /// A correct, acceptable reply at the claimed time.
    pub const GOOD: u64 = 0;
    /// The origin field is the nonce with its low bit flipped: the reply of an off-path attacker
    /// who guessed wrong, and the check the whole of plain NTP's spoofing resistance rests on.
    pub const BAD_ORIGIN: u64 = 1;
    /// Stratum 0 with the kiss code `RATE`. An instruction to back off, not a time.
    pub const KISS_OF_DEATH: u64 = 2;
    /// Twenty bytes: something that is not an NTP packet arriving on our socket.
    pub const SHORT: u64 = 3;
}

/// How many requests one synchronisation makes before giving up. Three is a client's ordinary
/// behaviour on a lossy path, not a widened timeout; the DNS check in `socket_test_client` settled on the same
/// number for the same reason.
pub const ATTEMPTS: u32 = 3;

/// The gap between attempts, as a yield-spin, because there is no timed wait (see the module docs).
/// Milliseconds rather than NTP's 64 seconds: the interval a real client wants is precisely the
/// thing this kernel cannot yet express without keeping a thread runnable for the whole of it.
const RETRY_GAP_NANOS: u64 = 2 * 1_000_000;

/// The server's turnaround in the test server's reply: T3 - T2. Small but not zero, so the delay
/// arithmetic is exercised on a value that has to be subtracted rather than one that cannot go
/// wrong. Far under the client's own round trip, which is two IPC exchanges.
const SERVER_TURNAROUND_NANOS: u64 = 1_000;

/// Where both roles map the shared socket frame in their own address spaces. Above the program's
/// segments; the same address `socket_test_client` uses, and each address space is its own.
const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

/// The one socket id this client uses.
const SID: u64 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, a1: u64, a2: u64) -> ! {
    match role {
        ROLE_SERVER => server(a1, a2),
        ROLE_PROBE_CLOCK => probe_clock(a1),
        _ => client(a1, a2),
    }
}

// =================================================================================================
// The client
// =================================================================================================

/// Report and stop. One-shot, so it **exits** rather than parking: a role left spinning on a run
/// queue starves later tests on the same core, which is the finding `socket_test_client`'s `done` records.
fn done(w0: u64, w1: u64, w2: u64) -> ! {
    send(REPORT, w0, w1, w2);
    exit();
}

fn client(server_ip: u64, server_port: u64) -> ! {
    // The wall clock, read the only way this process can read it: by asking the service. A reader
    // would hold the clock page and do two loads and an add, but the whole point of this process is
    // that it holds no mapping of that page, so `propose::STATE` is its channel. It costs one round
    // trip, and it is taken once: the ambient monotonic counter carries the time from here.
    let (_state, wall0) = call(PROPOSE, propose::req(propose::STATE), 0);
    let local = LocalClock {
        wall0,
        mono0: monotonic_nanos(),
    };

    // **The nonce before the network.** A client that cannot obtain unguessable bits has nothing to
    // send, so it finds that out before it exercises any network authority at all, rather than
    // reaching the same refusal one socket later.
    let mut nonce = match nonce_bits() {
        Ok(bits) => bits,
        Err(word) => done(RPT_NO_ENTROPY, word, 0),
    };

    attach_frame();
    if call(STACK, req(OP_OPEN_UDP, SID), 0).0 != REP_OK {
        done(RPT_NET_ERROR, 2, 0);
    }

    let mut sent = 0u32;
    let mut last_reject = 0u64;
    while sent < ATTEMPTS {
        // A fresh nonce every attempt after the first. Reusing one would let a reply to an earlier
        // request answer a later one, which is the stale-reply half of the origin check's job.
        if sent > 0 {
            nonce = match nonce_bits() {
                Ok(bits) => bits,
                Err(word) => done(RPT_NO_ENTROPY, word, 0),
            };
        }

        // T1 as late as it can honestly be taken. The bytes on the wire do not depend on it (a
        // request carries the nonce, not the send time), so everything that can be done first is,
        // and what remains between this reading and the datagram leaving is one IPC. What is left
        // over lands in the measured delay, which is the same path asymmetry NTP already lives with.
        write_dst(server_ip, server_port);
        let Some(t1) = stamp(local.now()) else {
            done(RPT_BAD_LOCAL_TIME, local.now(), 0);
        };
        let query = Query::with_nonce(t1, Timestamp::from_bits(nonce));
        write_payload(&query.request());
        sent += 1;
        if call(STACK, req(OP_SENDTO, SID), ntp_proto::PACKET_LEN as u64).0 != REP_OK {
            done(RPT_NET_ERROR, 3, sent as u64);
        }

        let (n, _) = call(STACK, req(OP_RECV, SID), 0);
        if n == REP_ERR || n == 0 {
            poll_gap();
            continue;
        }

        // T4, read as close to the arrival as this side can manage: the RECV reply is the first
        // instruction after the bytes landed.
        let Some(t4) = stamp(local.now()) else {
            done(RPT_BAD_LOCAL_TIME, local.now(), 0);
        };
        let mut wire = [0u8; ntp_proto::PACKET_LEN];
        let n = read_payload(n as usize, &mut wire);

        match query.accept(&wire[..n], t4) {
            Ok(sample) => {
                // The correction, applied to the local clock as it is now rather than as it was at
                // T4: the proposal is a claim about the present, and the service will compare it
                // against the present.
                let corrected = local.now() as i128 + sample.offset.nanos();
                let proposed = corrected.clamp(0, u64::MAX as i128) as u64;
                let (status, _wall_after) = call(PROPOSE, propose::req(propose::PROPOSE), proposed);
                let _ = call(STACK, req(OP_CLOSE, SID), 0);
                done(RPT_SYNCED, status, proposed);
            }
            Err(reject) => {
                last_reject = reject_code(reject);
                // A kiss-o'-death is an instruction, and the instruction is to stop.
                if matches!(reject, Reject::KissOfDeath(_)) {
                    break;
                }
                poll_gap();
            }
        }
    }

    let _ = call(STACK, req(OP_CLOSE, SID), 0);
    if last_reject != 0 {
        done(RPT_REJECTED, last_reject, sent as u64);
    }
    done(RPT_NO_REPLY, sent as u64, 0)
}

/// **Eight unguessable bytes, or an error word that says why not.**
///
/// `entropy_proto::delivered` is what makes the two failures distinguishable with no probe: a byte
/// count is `0..=8`, and every error a `CALL` can return is one of the kernel's small negatives,
/// which read as enormous `u64`s. So `Err(0)` is "the service has no entropy" and `Err(huge)` is
/// "there is no entropy service in slot 4". Both stop the client; only the report tells them apart.
fn nonce_bits() -> Result<u64, u64> {
    let (r0, r1) = call(ENTROPY, entropy_proto::req(entropy_proto::GET, 8), 0);
    let Some(n) = entropy_proto::delivered(r0) else {
        return Err(r0);
    };
    if n < 8 {
        return Err(r0);
    }
    let mut bytes = [0u8; 8];
    entropy_proto::take(n, r1, &mut bytes);
    Ok(u64::from_le_bytes(bytes))
}

/// The local wall clock, anchored once against the monotonic counter.
///
/// `wall0` is what the clock service said at `mono0`; everything after that is counter arithmetic,
/// which is ambient and costs one instruction. When the service says the clock is `UNKNOWN` it
/// answers 0, and the arithmetic still works out: T1 and T4 are then measured from 1970, the offset
/// comes out as the whole distance to the server's clock, and the proposal lands on the server's
/// time. That is the bootstrap case, and it is the case a machine with no RTC is in.
struct LocalClock {
    wall0: u64,
    mono0: u64,
}

impl LocalClock {
    fn now(&self) -> u64 {
        self.wall0
            .saturating_add(monotonic_nanos().saturating_sub(self.mono0))
    }
}

/// Monotonic nanoseconds since boot, from the ambient counter.
///
/// Whole seconds and a remainder, not `ticks * 1e9`: at 62.5 MHz the naive product overflows a
/// `u64` about five minutes into a boot, and the clock built on it jumps backwards by six centuries.
/// The clock service's copy of this arithmetic carries the same note.
fn monotonic_nanos() -> u64 {
    let freq = cntfrq();
    let ticks = now();
    let secs = ticks / freq;
    let rem = ticks % freq;
    secs * clock_proto::NANOS_PER_SEC + rem * clock_proto::NANOS_PER_SEC / freq
}

/// Unix nanoseconds to an NTP timestamp. `None` outside the crate's representable window, which the
/// clock service's 2100 ceiling keeps us well inside; it is checked rather than assumed because the
/// alternative to checking is a timestamp 136 years out.
fn stamp(unix_nanos: u64) -> Option<Timestamp> {
    Timestamp::from_unix(
        unix_nanos / clock_proto::NANOS_PER_SEC,
        (unix_nanos % clock_proto::NANOS_PER_SEC) as u32,
    )
}

/// The poll interval, as the only thing this kernel can express: a yield-spin. See the module docs;
/// keeping a thread runnable for the whole interval is why a continuously polling client waits on
/// the timed-wait fork rather than being written around it.
fn poll_gap() {
    let deadline = monotonic_nanos() + RETRY_GAP_NANOS;
    while monotonic_nanos() < deadline {
        yield_now();
    }
}

/// A stable small integer for each `Reject`, so a test (and one day a log) can name which check
/// refused a packet. The enum is the security story of unauthenticated NTP; collapsing it to a bool
/// at the first opportunity would throw that away.
fn reject_code(r: Reject) -> u64 {
    match r {
        Reject::Length(_) => 1,
        Reject::Version(_) => 2,
        Reject::Mode(_) => 3,
        Reject::KissOfDeath(_) => 4,
        Reject::Stratum(_) => 5,
        Reject::Unsynchronised => 6,
        Reject::OriginMismatch => 7,
        Reject::TransmitZero => 8,
        Reject::ServerOrderReversed => 9,
        Reject::ClientOrderReversed => 10,
        Reject::NegativeDelay => 11,
        Reject::RootDistance => 12,
    }
}

/// Mint one frame from our own budget, map it, and delegate it to the socket contract's server.
/// Exactly what `socket_test_client` does, because it is exactly the same contract.
fn attach_frame() {
    // SAFETY: `svc`. RETYPE answers with the new frame capability's slot, or a negative error.
    let frame = unsafe { invoke(UNTYPED, ut::RETYPE, 0, 0, 0) };
    if frame < 0 {
        done(RPT_NET_ERROR, 0, 0);
    }
    let frame = frame as u64;
    // SAFETY: `svc`. Map it writable; the page tables come from our untyped.
    if unsafe { invoke(frame, fr::MAP, FRAME_VA, 1, UNTYPED) } < 0 {
        done(RPT_NET_ERROR, 1, 0);
    }
    // SAFETY: `svc`. Delegate it, narrowed to read/write, with the ATTACH request.
    if unsafe {
        invoke(
            STACK,
            endpoint::SEND_CAP,
            frame,
            rights::READ | rights::WRITE,
            req(OP_ATTACH_FRAME, SID),
        )
    } < 0
    {
        done(RPT_NET_ERROR, 1, 0);
    }
}

// =================================================================================================
// The shared frame. Absolute-VA volatile access, the same shape net_stack and socket_test_client use.
// =================================================================================================

fn r8(va: u64) -> u8 {
    // SAFETY: inside the one frame this process mapped at FRAME_VA.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}
fn w8(va: u64, v: u8) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}
fn r16le(va: u64) -> u16 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile(va as *const u16) }
}
fn w16le(va: u64, v: u16) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(va as *mut u16, v) }
}

/// Write the destination header: the address and port the *wiring* named, not one this program
/// chose. Which server to ask is an endowment, the same way which network to use is.
fn write_dst(ip: u64, port: u64) {
    for i in 0..4 {
        w8(FRAME_VA + OFF_DST_IP + i, (ip >> (24 - 8 * i)) as u8);
    }
    w16le(FRAME_VA + OFF_DST_PORT, port as u16);
}

fn write_payload(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        w8(FRAME_VA + OFF_PAYLOAD + i as u64, b);
    }
}

fn read_payload(n: usize, out: &mut [u8]) -> usize {
    let n = n.min(out.len());
    for (i, b) in out[..n].iter_mut().enumerate() {
        *b = r8(FRAME_VA + OFF_PAYLOAD + i as u64);
    }
    n
}

// =================================================================================================
// The probe: the client's endowment, pointed at the clock page.
// =================================================================================================

/// **Everything an NTP client holds, and the clock page is still not reachable.**
///
/// Spawned with the client's five slots and told the address at which a process holding the *set*
/// authority maps the clock page. It reports the address, writes there, and faults, because its
/// address space has no mapping of that frame at any address. The boundary is the mapping, not the
/// layout, and knowing where to look buys nothing.
fn probe_clock(va: u64) -> ! {
    send(REPORT, RPT_PROBING, va, 0);
    // SAFETY: deliberately not safe. This is the assertion: the write must fault. If it does not,
    // the process survives to send the report below, and the test fails on that.
    unsafe {
        core::ptr::write_volatile(va as *mut u64, clock_proto::state::SET);
    }
    done(RPT_PROBING, va, 1)
}

// =================================================================================================
// The test server: an NTP server and the network under it, in one process.
// =================================================================================================

/// Serve the socket contract on [`STACK`], answering each request with an NTP reply built from
/// `variant` at `claimed_nanos`. See the module docs for what this does and does not prove.
///
/// It reports **once**, after the first request it sees, and the report blocks until the test drains
/// it: a report per request would deadlock the exchange the moment nobody was draining, and one is
/// all the assertions need.
fn server(variant: u64, claimed_nanos: u64) -> ! {
    let mut pending = [0u8; ntp_proto::PACKET_LEN];
    let mut pending_len = 0usize;
    let mut reported = false;

    loop {
        let (w0, cap, w1) = recv_cap(STACK);
        match req_op(w0) {
            // A SEND_CAP: the client's shared frame, which we map for ourselves and then drop the
            // capability for, because the mapping outlives it. No reply; nobody is waiting.
            OP_ATTACH_FRAME => {
                // SAFETY: `svc`. The cap is the frame the client delegated.
                let _ = unsafe { invoke(cap, fr::MAP, FRAME_VA, 1, UNTYPED) };
                cap_delete(cap);
            }
            OP_OPEN_UDP | OP_OPEN_TCP => {
                reply(cap, REP_OK, 0);
            }
            OP_SENDTO => {
                // The length the client declared, which is how the real server learns it too.
                let mut wire = [0u8; ntp_proto::PACKET_LEN];
                let n = read_payload((w1 as usize).min(ntp_proto::PACKET_LEN), &mut wire);
                let request = Packet::parse(&wire[..n]).unwrap_or_default();
                pending_len = build_reply(&request, variant, claimed_nanos, &mut pending);
                reply(cap, REP_OK, 0);
                if !reported {
                    reported = true;
                    let dst_port = r16le(FRAME_VA + OFF_DST_PORT) as u64;
                    send(
                        REPORT,
                        RPT_SERVED,
                        request.transmit.bits(),
                        (dst_port << 32) | ((request.version as u64) << 8) | request.mode as u64,
                    );
                }
            }
            OP_RECV => {
                write_payload(&pending[..pending_len]);
                w16le(FRAME_VA + OFF_LEN, pending_len as u16);
                reply(cap, pending_len as u64, 0);
            }
            OP_CLOSE => {
                reply(cap, REP_OK, 0);
            }
            _ => {
                if cap != endpoint::NO_CAP {
                    reply(cap, REP_ERR, 0);
                }
            }
        }
    }
}

/// Build the reply for `variant`. Returns how many bytes of `out` are the reply.
fn build_reply(request: &Packet, variant: u64, claimed_nanos: u64, out: &mut [u8]) -> usize {
    let t2 = stamp(claimed_nanos).unwrap_or(Timestamp::ZERO);
    let t3 = stamp(claimed_nanos + SERVER_TURNAROUND_NANOS).unwrap_or(Timestamp::ZERO);

    let mut p = Packet {
        leap: leap::NONE,
        version: ntp_proto::VERSION,
        mode: mode::SERVER,
        stratum: 2,
        poll: request.poll,
        precision: -20,
        root_delay: Short(0x0000_1000),      // ~62 ms
        root_dispersion: Short(0x0000_2000), // ~125 ms
        reference_id: *b"TEST",
        reference: t2,
        // A server echoes the client's transmit field into origin and interprets it no further.
        // That is what makes a random nonce free, and it is what BAD_ORIGIN below breaks.
        origin: request.transmit,
        receive: t2,
        transmit: t3,
    };

    match variant {
        srv::BAD_ORIGIN => {
            p.origin = Timestamp::from_bits(request.transmit.bits() ^ 1);
        }
        srv::KISS_OF_DEATH => {
            p.stratum = 0;
            p.reference_id = *b"RATE";
        }
        srv::SHORT => {
            let bytes = p.to_bytes();
            out[..20].copy_from_slice(&bytes[..20]);
            return 20;
        }
        _ => {}
    }

    out[..ntp_proto::PACKET_LEN].copy_from_slice(&p.to_bytes());
    ntp_proto::PACKET_LEN
}

user_rt::panic_handler!();
