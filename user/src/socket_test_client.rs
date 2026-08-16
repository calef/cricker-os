//! A client of the net server's socket contract (milestone 30, piece 3 phase B).
//!
//! It exercises the capability-shaped contract from the outside: mint a shared frame from its own
//! untyped budget, delegate it to the net server, and drive real network exchanges through socket
//! ids on the `Stack` endpoint, no ambient network anywhere. It holds a capability to the stack or
//! it does not; here it was granted one.
//!
//! The exchanges, selected by the entry role, all against QEMU user-mode networking with zero host
//! setup:
//!   - `TEST_UDP_TFTP`: a UDP request/response round trip against **slirp's own built-in TFTP
//!     server** (10.0.2.2:69), which libslirp answers itself with no host network involved. This is
//!     the gating UDP test: deterministic and offline, the UDP twin of the guestfwd echo peer.
//!   - `TEST_UDP_DNS`: a real DNS query for `example.com` via 10.0.2.3:53. **This leaves the
//!     machine.** 10.0.2.3 is not a resolver; libslirp NATs anything sent there to the *host's*
//!     configured nameserver (`get_dns_addr_libresolv`), so this exchange depends on the developer's
//!     DNS working at that instant. It is therefore **non-gating**: a host resolver that does not
//!     answer reports `NO_ANSWER` and the kernel test skips loudly. A malformed or mismatched
//!     response still fails, because that would be our bug. See notes/net.md.
//!   - `TEST_TCP_ECHO`: a full TCP round trip to slirp's guestfwd echo peer (10.0.2.9:7777 -> a
//!     `/bin/cat`): connect (handshake), send, receive the echo, close (teardown).
//!   - `TEST_TCP_ACCEPT`: **the inbound half** (milestone 107), and the only exchange here that is
//!     not the guest as a client. A port outside the stack's listen grant is refused as a matter of
//!     authority, the granted one binds and is exclusive, and then a *host* process connects to it
//!     through QEMU's `hostfwd` twice, which proves the listener re-arms.
//!     The same spawn then carries **the UDP bind grant's refusals** (milestone 55), because a
//!     second net server does not fit the aarch64 boot (the memory receipt in notes/net.md; that
//!     lane re-measured it: an eleventh spawn died as `Unmappable(OutOfFrames)` in an unrelated
//!     later test). A fixed port outside the grant is refused as authority, a granted one binds
//!     and is exclusive, which incidentally proves the two grant halves compose in one word on the
//!     machine. The traffic that used to ride here belongs to `mdns_responder` now: it is a third
//!     client of this same stack, it holds 5353 for the whole run, and it proves the multicast
//!     path with real DNS rather than with marker payloads.
//!
//! On success it reports `OK`; any failure reports a stage code, so the kernel test fails loudly
//! with a hint rather than hanging.
//!
//! This is a **module of the `net_stack` binary** (dispatched by its entry role), not a separate binary,
//! because the initrd archive's directory holds at most 15 files; folding the client in keeps the
//! entry count under that ceiling (see xtask mkinitrd).
//!
//! # Capability contract (when entered as the client)
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the `Stack` endpoint (WRITE)
//! - slot 2: an untyped budget (to mint and map the shared frame)
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `netcli`. Refused `netcli` (squished)
//! and `socket_client`, which belongs to the real clients milestone 54 will need. This file is a
//! single-consumer `#[path]` module rather than a `[[bin]]`.

use abi::{endpoint, frame as fr, rights, untyped as ut};
use socket_proto::*;
use user_rt::{call, exit, invoke, send};

const REPORT: u64 = 0;
const STACK: u64 = 1;
const UNTYPED: u64 = 2;

/// Test selectors (the entry role), and the success word the kernel test asserts.
pub const TEST_UDP_DNS: u64 = 1;
pub const TEST_TCP_ECHO: u64 = 2;
pub const TEST_TCP_REOPEN: u64 = 3;
pub const TEST_UDP_TFTP: u64 = 4;
pub const TEST_TCP_ACCEPT: u64 = 5;
const OK: u64 = 1;
/// Reported when an exchange could not be completed **for an environmental reason** rather than a
/// defect in our stack: today only the real-DNS check, whose upstream is the host's resolver. The
/// kernel test prints and skips on this instead of failing, so the gate never depends on the
/// developer's network. Distinct from `OK` and from every `0xE0xx` protocol failure.
const NO_ANSWER: u64 = 2;

/// Where the client maps its shared frame.
const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

/// slirp's guest-visible nameserver address (a NAT to the *host's* resolver, not a resolver), the
/// gateway that hosts slirp's own TFTP server, and the guestfwd echo peer the runners attach.
const DNS_IP: [u8; 4] = [10, 0, 2, 3];
const DNS_PORT: u16 = 53;
const GW_IP: [u8; 4] = [10, 0, 2, 2];
const TFTP_PORT: u16 = 69;
const ECHO_IP: [u8; 4] = [10, 0, 2, 9];
const ECHO_PORT: u16 = 7777;

const DNS_TXID: u16 = 0x1234;

/// **The inbound half** (milestone 107). The guest listens on `LISTEN_PORT`; the host reaches it
/// because the runners add a QEMU `hostfwd` from a host port to this one, the mirror of the
/// `guestfwd` the outbound gate uses. `DENIED_PORT` is deliberately *outside* the listen grant the
/// spawn service hands this stack, so asking for it proves the grant refuses rather than that
/// nothing happened to bind.
const LISTEN_PORT: u64 = 7778;
const DENIED_PORT: u64 = 8080;
/// The listener's socket id and the accepted connection's. Two ids, because they are two objects:
/// the listener never carries a byte and never gets a frame, and the connection is where the frame
/// is. Keeping them apart is the contract, not a convenience.
const LISTEN_SID: u64 = 0;
const CONN_SID: u64 = 1;
/// What the host sends in and what the guest answers with. Different strings on purpose: an echo
/// would pass even if the guest were somehow reflecting the host's own bytes, and the point of this
/// gate is that the guest *composed* an answer to a connection it did not make.
const IN_MSG: &[u8] = b"nife-in!";
const OUT_MSG: &[u8] = b"nife-out!";

/// The fixture the runners put in slirp's TFTP directory, and its exact contents. Both sides are
/// fixed so the round trip is asserted byte for byte (see scripts/qemu-runner-*.sh).
const TFTP_NAME: &[u8] = b"nife";
const TFTP_BODY: &[u8] = b"nife-tftp!";

/// **The UDP bind grant's refusals** (milestone 55). `MDNS_DENIED_PORT` is deliberately outside the
/// range the kernel test grants this spawn, so asking for it proves the grant *refuses* rather than
/// that nothing happened to bind; `MDNS_GRANTED_PORT` is inside it, and is deliberately **not**
/// 5353.
///
/// The real port belongs to `mdns_responder`, which is a third client of this same stack and holds
/// it for the whole run (milestone 55's responder lane). That is why the traffic half of this
/// exchange is gone: the marker datagrams this test used to trade with xtask's multicast prober
/// proved that a joined group receives, that the source endpoint rides back on RECV, and that a
/// multicast SENDTO reaches the wire, and the responder now proves all three with real DNS
/// messages the prober parses. What cannot be proved by a program holding a granted port is the
/// refusal of one it was not granted, so that half stays here.
const MDNS_DENIED_PORT: u64 = 4444;
const MDNS_GRANTED_PORT: u64 = 5354;

/// How many times the real-DNS check sends its query before giving up. A DNS client retries; UDP has
/// no retransmit of its own and the measured single-query loss to a real resolver was ~2.5%, so one
/// attempt made an environment-dependent test look like a code defect. Three attempts is ordinary
/// resolver behaviour, not a widened timeout.
const DNS_ATTEMPTS: u32 = 3;

fn w8(va: u64, v: u8) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}
fn w16le(va: u64, v: u16) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u16, v) }
}
fn r8(va: u64) -> u8 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}
fn r16le(va: u64) -> u16 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u16) }
}

/// The source endpoint a UDP RECV reply left in the frame header (`socket_proto`'s layout note).
fn recv_source() -> ([u8; 4], u16) {
    let mut ip = [0u8; 4];
    for (i, b) in ip.iter_mut().enumerate() {
        *b = r8(FRAME_VA + OFF_DST_IP + i as u64);
    }
    (ip, r16le(FRAME_VA + OFF_DST_PORT))
}

/// Set the shared frame's destination header.
fn set_dst(ip: [u8; 4], port: u16) {
    for (i, &b) in ip.iter().enumerate() {
        w8(FRAME_VA + OFF_DST_IP + i as u64, b);
    }
    w16le(FRAME_VA + OFF_DST_PORT, port);
}

/// Report `code` and stop.
fn done(code: u64) -> ! {
    send(REPORT, code, 0, 0);
    // Exit so the kernel reaps this one-shot client rather than leaving it spinning on a run queue
    // forever. Leaked net-client spinners accumulate across the socket-contract tests and starve the
    // later std_net test on core 0 (the same test-thread-starvation finding that made the driver
    // roles exit; nothing balances threads across cores yet, DECISIONS Open design ideas). A
    // one-shot role must exit, not spin.
    exit();
}

/// Mint a frame from our untyped, map it writable, and delegate it to socket `sid`.
fn attach_frame(sid: u64) {
    // SAFETY: `svc`. RETYPE returns the new frame capability's slot, or a negative error.
    let frame = unsafe { invoke(UNTYPED, ut::RETYPE, 0, 0, 0) };
    if frame < 0 {
        done(0xE001);
    }
    let frame = frame as u64;
    // SAFETY: `svc`. Map it writable; page tables come from our untyped.
    if unsafe { invoke(frame, fr::MAP, FRAME_VA, 1, UNTYPED) } < 0 {
        done(0xE002);
    }
    // Delegate it (narrowed to read/write) with the ATTACH request. SAFETY: `svc`.
    if unsafe {
        invoke(
            STACK,
            endpoint::SEND_CAP,
            frame,
            rights::READ | rights::WRITE,
            req(OP_ATTACH_FRAME, sid),
        )
    } < 0
    {
        done(0xE003);
    }
}

/// Write a byte at `*at` and advance it.
fn put8(v: u8, at: &mut u64) {
    w8(*at, v);
    *at += 1;
}

/// Build a DNS A-record query for "example.com" into the frame payload. Returns its length.
fn build_dns_query() -> u64 {
    let mut p = FRAME_VA + OFF_PAYLOAD;
    // header: id, flags(0x0100 recursion desired), qd=1, an=ns=ar=0
    put8((DNS_TXID >> 8) as u8, &mut p);
    put8(DNS_TXID as u8, &mut p);
    put8(0x01, &mut p);
    put8(0x00, &mut p);
    put8(0x00, &mut p);
    put8(0x01, &mut p);
    for _ in 0..6 {
        put8(0x00, &mut p);
    }
    // qname: 7 "example" 3 "com" 0
    for &(len, label) in &[(7u8, b"example" as &[u8]), (3, b"com")] {
        put8(len, &mut p);
        for &c in label {
            put8(c, &mut p);
        }
    }
    put8(0x00, &mut p); // root label
    put8(0x00, &mut p); // qtype A = 0x0001
    put8(0x01, &mut p);
    put8(0x00, &mut p); // qclass IN = 0x0001
    put8(0x01, &mut p);
    p - (FRAME_VA + OFF_PAYLOAD)
}

/// **Real DNS resolution, and therefore NOT a gate.** The query goes to 10.0.2.3, which libslirp
/// NATs to the *host's* nameserver, so whether it is answered is a fact about the developer's
/// machine. Retries like any resolver client, then reports `NO_ANSWER` if the host never answered,
/// which the kernel test turns into a loud skip. A response that arrives but is not ours, or is not
/// a response, still fails: that would be a defect in the socket contract, not in the network.
fn udp_dns() -> ! {
    attach_frame(0);
    if call(STACK, req(OP_OPEN_UDP, 0), 0).0 != REP_OK {
        done(0xE010);
    }

    let mut got = 0u64;
    for _ in 0..DNS_ATTEMPTS {
        let qlen = build_dns_query();
        set_dst(DNS_IP, DNS_PORT);
        if call(STACK, req(OP_SENDTO, 0), qlen).0 != REP_OK {
            done(0xE011);
        }
        let (rlen, _) = call(STACK, req(OP_RECV, 0), 0);
        if rlen != REP_ERR && rlen >= 12 {
            got = rlen;
            break;
        }
    }
    if got == 0 {
        // The host's resolver never answered. Environmental, not ours.
        done(NO_ANSWER);
    }

    // Verify it is a response to our query: transaction id matches, and the QR bit is set.
    let rid = ((r8(FRAME_VA + OFF_PAYLOAD) as u16) << 8) | r8(FRAME_VA + OFF_PAYLOAD + 1) as u16;
    let qr = r8(FRAME_VA + OFF_PAYLOAD + 2) & 0x80;
    if rid != DNS_TXID {
        done(0xE013);
    }
    if qr == 0 {
        done(0xE014);
    }

    let _ = call(STACK, req(OP_CLOSE, 0), 0);
    done(OK);
}

/// **The gating UDP test: a round trip against slirp's own TFTP server.** libslirp implements TFTP
/// internally (enabled by `tftp=` on the netdev), so this request and its reply never leave the
/// emulator: no host resolver, no internet, no packet that can be dropped by somebody else's router.
/// It proves exactly what the DNS test was there to prove about *our* code, and nothing about the
/// host: a client holding only a `Stack` endpoint and a shared frame can open a UDP socket by id,
/// send a datagram to a chosen address, and read the reply back through the same frame.
///
/// Send a read request (opcode 1, `octet` mode) for the fixture the runners planted, and require the
/// first data packet back: opcode 3, block 1, and the fixture's bytes exactly.
fn udp_tftp() -> ! {
    attach_frame(0);
    if call(STACK, req(OP_OPEN_UDP, 0), 0).0 != REP_OK {
        done(0xE040);
    }

    // RRQ: { u16 opcode = 1 } filename 0 "octet" 0
    let mut p = FRAME_VA + OFF_PAYLOAD;
    put8(0x00, &mut p);
    put8(0x01, &mut p);
    for &c in TFTP_NAME {
        put8(c, &mut p);
    }
    put8(0x00, &mut p);
    for &c in b"octet" {
        put8(c, &mut p);
    }
    put8(0x00, &mut p);
    let qlen = p - (FRAME_VA + OFF_PAYLOAD);

    set_dst(GW_IP, TFTP_PORT);
    if call(STACK, req(OP_SENDTO, 0), qlen).0 != REP_OK {
        done(0xE041);
    }

    // DATA: { u16 opcode = 3 }{ u16 block = 1 } body. The fixture is one short block, so the whole
    // file arrives in this first packet and no ACK/continuation is needed.
    let (rlen, _) = call(STACK, req(OP_RECV, 0), 0);
    if rlen == REP_ERR || rlen < 4 + TFTP_BODY.len() as u64 {
        done(0xE042);
    }
    let opcode = ((r8(FRAME_VA + OFF_PAYLOAD) as u16) << 8) | r8(FRAME_VA + OFF_PAYLOAD + 1) as u16;
    let block =
        ((r8(FRAME_VA + OFF_PAYLOAD + 2) as u16) << 8) | r8(FRAME_VA + OFF_PAYLOAD + 3) as u16;
    if opcode != 3 {
        done(0xE043); // an ERROR packet (opcode 5) means the fixture is missing: see the runners
    }
    if block != 1 {
        done(0xE044);
    }
    for (i, &b) in TFTP_BODY.iter().enumerate() {
        if r8(FRAME_VA + OFF_PAYLOAD + 4 + i as u64) != b {
            done(0xE045); // the bytes came back changed
        }
    }

    // The RECV reply now carries the DATA packet's source endpoint in the frame header (milestone
    // 55's stack half), and this is the slirp-provable check of it: the DATA came from the gateway,
    // from a real port. The port is deliberately not pinned to 69: TFTP's own protocol has the
    // server answer from a transfer-id port of its choosing (RFC 1350 §4), so asserting 69 would
    // pin a libslirp implementation detail.
    let (src_ip, src_port) = recv_source();
    if src_ip != GW_IP {
        done(0xE046); // the reported source is not the server that answered
    }
    if src_port == 0 {
        done(0xE047); // no source port arrived at all
    }

    // ACK block 1, which ends the transfer properly: { u16 opcode = 4 }{ u16 block = 1 }. The fixture
    // is one short block, so this is the last packet of the exchange. Without it the server would sit
    // retransmitting its DATA at a socket we are about to close, which is rude to the next test that
    // brings this NIC up even though libslirp eventually gives up on its own.
    //
    // Addressed to the DATA's reported source rather than to :69, which is what TFTP's TID scheme
    // asks for and is the first real consumer of the source endpoint: replying to the querier is
    // exactly the move an mDNS legacy-unicast responder makes.
    let mut a = FRAME_VA + OFF_PAYLOAD;
    put8(0x00, &mut a);
    put8(0x04, &mut a);
    put8(0x00, &mut a);
    put8(0x01, &mut a);
    set_dst(src_ip, src_port);
    let _ = call(STACK, req(OP_SENDTO, 0), a - (FRAME_VA + OFF_PAYLOAD));

    let _ = call(STACK, req(OP_CLOSE, 0), 0);
    done(OK);
}

fn tcp_echo() -> ! {
    const MSG: &[u8] = b"nife-net!";

    attach_frame(0);
    if call(STACK, req(OP_OPEN_TCP, 0), 0).0 != REP_OK {
        done(0xE020);
    }

    set_dst(ECHO_IP, ECHO_PORT);
    let (outcome, _) = call(STACK, req(OP_CONNECT, 0), 0);
    if outcome != CONNECT_ESTABLISHED {
        done(0xE021); // handshake did not complete (refused/reset)
    }

    for (i, &b) in MSG.iter().enumerate() {
        w8(FRAME_VA + OFF_PAYLOAD + i as u64, b);
    }
    let (sent, _) = call(STACK, req(OP_SEND, 0), MSG.len() as u64);
    if sent != MSG.len() as u64 {
        done(0xE022);
    }

    let (rlen, _) = call(STACK, req(OP_RECV, 0), 0);
    if rlen != MSG.len() as u64 {
        done(0xE023); // the echo did not come back whole
    }
    for (i, &b) in MSG.iter().enumerate() {
        if r8(FRAME_VA + OFF_PAYLOAD + i as u64) != b {
            done(0xE024); // the echoed bytes differ
        }
    }

    let _ = call(STACK, req(OP_CLOSE, 0), 0);
    done(OK);
}

/// **Regression: reusing a socket id is safe.** Open a TCP socket on id 0, connect to the echo peer,
/// close it, then reopen the *same* id and connect again. Before `net_stack` assigned ephemeral local ports
/// independent of the socket id, the reopen reused the exact local port, and the second connect on a
/// 4-tuple whose slirp flow had not yet cleared stalled `net_stack`'s bounded poll forever (found by the
/// `std::net` PAL, notes/net.md). With the rotating allocator the reopen gets a fresh port, so both
/// connects complete.
fn tcp_reopen() -> ! {
    attach_frame(0);
    set_dst(ECHO_IP, ECHO_PORT);

    // First connection on socket id 0.
    if call(STACK, req(OP_OPEN_TCP, 0), 0).0 != REP_OK {
        done(0xE030);
    }
    if call(STACK, req(OP_CONNECT, 0), 0).0 != CONNECT_ESTABLISHED {
        done(0xE031);
    }
    let _ = call(STACK, req(OP_CLOSE, 0), 0);

    // Reopen the SAME socket id and connect again. This is the exact path that hung before the fix.
    if call(STACK, req(OP_OPEN_TCP, 0), 0).0 != REP_OK {
        done(0xE032);
    }
    if call(STACK, req(OP_CONNECT, 0), 0).0 != CONNECT_ESTABLISHED {
        done(0xE033);
    }
    let _ = call(STACK, req(OP_CLOSE, 0), 0);

    done(OK);
}

/// **The inbound gate: a granted port, and the guest connected TO through it, twice** (milestone
/// 107).
///
/// Everything else in this file is the guest as a client. Here it is the server: listen on a port it
/// was granted, accept a connection a *host* process opened through QEMU's `hostfwd`, read what
/// arrived, answer it, and then do the whole thing again on the same listener. The second round is
/// the load-bearing one: a listener that can accept exactly one connection is a listener a file
/// server cannot use, and nothing but a second accept proves the re-arm.
///
/// **The grant checks ride in the same exchange rather than in a test of their own, and that is the
/// machine's call, not a preference.** A second net server costs a 192-page untyped region that is
/// never reclaimed (nothing unregisters a transport or reaps `net_stack`), and the aarch64 test boot
/// has no room for one: with two, a later test asking for 128 contiguous pages found 137 free frames
/// and no run that long. So one spawn proves both halves, with distinct stage codes standing in for
/// the separate test names.
///
/// The frame is attached to the *connection* id and never to the listener, and it is attached only
/// after the listener is bound. That ordering is the two-object split made visible: the whole grant
/// half runs with no shared frame anywhere, because a listener carries no bytes.
fn tcp_accept_inbound() -> ! {
    // A port outside the grant is refused as a matter of AUTHORITY, which is a different answer from
    // "somebody has it" and calls for a different response from a client.
    match call(STACK, req(OP_LISTEN, LISTEN_SID), DENIED_PORT).0 {
        LISTEN_DENIED => {}
        LISTEN_GRANTED => done(0xE050), // bound a port nothing granted: the whole point, lost
        LISTEN_IN_USE => done(0xE051),
        _ => done(0xE052),
    }

    // The granted one binds, and this listener is the one the rest of the exchange accepts on.
    match call(STACK, req(OP_LISTEN, LISTEN_SID), LISTEN_PORT).0 {
        LISTEN_GRANTED => {}
        LISTEN_DENIED => done(0xE053), // the spawn service granted the wrong range
        LISTEN_IN_USE => done(0xE054),
        _ => done(0xE055),
    }

    // And it is exclusive, which is the property that makes a port grantable rather than merely a
    // number. Asking again on a second socket id must collide.
    match call(STACK, req(OP_LISTEN, CONN_SID), LISTEN_PORT).0 {
        LISTEN_IN_USE => {}
        LISTEN_GRANTED => done(0xE056), // two listeners on one port
        _ => done(0xE057),
    }

    // Only now a frame, and only for the connection.
    attach_frame(CONN_SID);

    // Two connections in a row, each with its own stage codes so a failure names which one.
    serve_one_inbound(0xE060);
    serve_one_inbound(0xE070);

    let _ = call(STACK, req(OP_CLOSE, LISTEN_SID), 0);

    // The mDNS half rides in this same spawn (milestone 55's stack half), because a second net
    // server does not fit the aarch64 boot: the spawn is ~154 frames nothing ever reclaims, and
    // this lane measured the eleventh one dying as `Unmappable(OutOfFrames)` in an unrelated later
    // test, the exact failure notes/net.md's memory receipt predicted. Milestone 107 folded its
    // grant half for the same reason; the stage codes stand in for the separate test's name.
    udp_mdns_half();
    done(OK);
}

/// Accept one inbound connection, check what the host sent, answer it, and close. Reports through
/// `done` on any failure, so `base` distinguishes the first connection from the second.
fn serve_one_inbound(base: u64) {
    if call(STACK, req(OP_ACCEPT, LISTEN_SID), CONN_SID).0 != REP_OK {
        done(base); // nobody connected within the server's bounded wait
    }

    let (rlen, _) = call(STACK, req(OP_RECV, CONN_SID), 0);
    if rlen != IN_MSG.len() as u64 {
        done(base + 1);
    }
    for (i, &b) in IN_MSG.iter().enumerate() {
        if r8(FRAME_VA + OFF_PAYLOAD + i as u64) != b {
            done(base + 2); // something connected and said something else
        }
    }

    for (i, &b) in OUT_MSG.iter().enumerate() {
        w8(FRAME_VA + OFF_PAYLOAD + i as u64, b);
    }
    let (sent, _) = call(STACK, req(OP_SEND, CONN_SID), OUT_MSG.len() as u64);
    if sent != OUT_MSG.len() as u64 {
        done(base + 3);
    }

    if call(STACK, req(OP_CLOSE, CONN_SID), 0).0 != REP_OK {
        done(base + 4);
    }
}

/// **A fixed UDP port is an authority, and this is the half that proves the refusals** (milestone
/// 55; runs inside the accept test's spawn, see `tcp_accept_inbound`).
///
/// - **A port outside the grant is `LISTEN_DENIED`**, which is the capability answer and a
///   different one from "somebody has it": no retry helps, and no other port will do unless the
///   spawn site granted it. Since this spawn's word also carries the TCP listen grant the accept
///   half just spent, the machine is exercising the *composed* word rather than one half alone.
/// - **A granted port binds**, and asking for it again on a second socket id collides, which is
///   the exclusivity that makes a port a grantable thing rather than a number.
///
/// **What is deliberately not here any more**: the marker-payload exchange with xtask's multicast
/// prober. It proved that a joined group receives, that a multicast `SENDTO` reaches the wire, and
/// that a datagram's source endpoint rides back on `RECV`; `mdns_responder`, a third client of this
/// same stack, now proves all three with real DNS messages the prober decodes as records
/// (notes/mdns.md). Two programs sending markers past each other proved carriage twice and protocol
/// never.
fn udp_mdns_half() {
    match call(STACK, req(OP_BIND_UDP, LISTEN_SID), MDNS_DENIED_PORT).0 {
        LISTEN_DENIED => {}
        LISTEN_GRANTED => done(0xE080), // bound a port nothing granted: the whole point, lost
        _ => done(0xE081),
    }

    match call(STACK, req(OP_BIND_UDP, CONN_SID), MDNS_GRANTED_PORT).0 {
        LISTEN_GRANTED => {}
        LISTEN_DENIED => done(0xE082), // the spawn granted the wrong range
        _ => done(0xE083),
    }

    // Exclusive, the property that makes a fixed port grantable rather than merely a number.
    match call(STACK, req(OP_BIND_UDP, LISTEN_SID), MDNS_GRANTED_PORT).0 {
        LISTEN_IN_USE => {}
        LISTEN_GRANTED => done(0xE084), // two sockets on one fixed port
        _ => done(0xE085),
    }

    let _ = call(STACK, req(OP_CLOSE, CONN_SID), 0);
}

/// Run the selected client exchange. Entered from `net_stack`'s `_start` when the entry role is nonzero.
pub fn run(test: u64) -> ! {
    match test {
        TEST_UDP_DNS => udp_dns(),
        TEST_UDP_TFTP => udp_tftp(),
        TEST_TCP_ECHO => tcp_echo(),
        TEST_TCP_REOPEN => tcp_reopen(),
        TEST_TCP_ACCEPT => tcp_accept_inbound(),
        _ => done(0xE0FF),
    }
}
