//! A client of the net server's socket contract (milestone 30, piece 3 phase B).
//!
//! It exercises the capability-shaped contract from the outside: mint a shared frame from its own
//! untyped budget, delegate it to the net server, and drive real network exchanges through socket
//! ids on the `Stack` endpoint, no ambient network anywhere. It holds a capability to the stack or
//! it does not; here it was granted one.
//!
//! Two exchanges, selected by the entry role, both against QEMU user-mode networking with zero host
//! setup:
//!   - `TEST_UDP_DNS`: a real DNS query to slirp's built-in resolver (10.0.2.3:53), verifying the
//!     response is ours (transaction id) and is a response (QR bit).
//!   - `TEST_TCP_ECHO`: a full TCP round trip to slirp's guestfwd echo peer (10.0.2.9:7777 -> a
//!     `/bin/cat`): connect (handshake), send, receive the echo, close (teardown).
//!
//! On success it reports `OK`; any failure reports a stage code, so the kernel test fails loudly
//! with a hint rather than hanging.
//!
//! This is a **module of the `netd` binary** (dispatched by its entry role), not a separate binary,
//! because the initrd archive's directory holds at most 15 files; folding the client in keeps the
//! entry count under that ceiling (see xtask mkinitrd).
//!
//! # Capability contract (when entered as the client)
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the `Stack` endpoint (WRITE)
//! - slot 2: an untyped budget (to mint and map the shared frame)

use abi::{endpoint, frame as fr, rights, untyped as ut};
use user_rt::{call, invoke, send};

use super::netproto::*;

const REPORT: u64 = 0;
const STACK: u64 = 1;
const UNTYPED: u64 = 2;

/// Test selectors (the entry role), and the success word the kernel test asserts.
pub const TEST_UDP_DNS: u64 = 1;
pub const TEST_TCP_ECHO: u64 = 2;
const OK: u64 = 1;

/// Where the client maps its shared frame.
const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

/// slirp's built-in DNS resolver, and the guestfwd echo peer the runners attach.
const DNS_IP: [u8; 4] = [10, 0, 2, 3];
const DNS_PORT: u16 = 53;
const ECHO_IP: [u8; 4] = [10, 0, 2, 9];
const ECHO_PORT: u16 = 7777;

const DNS_TXID: u16 = 0x1234;

fn w8(va: u64, v: u8) {
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}
fn w16le(va: u64, v: u16) {
    unsafe { core::ptr::write_volatile(va as *mut u16, v) }
}
fn r8(va: u64) -> u8 {
    unsafe { core::ptr::read_volatile(va as *const u8) }
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
    loop {
        core::hint::spin_loop();
    }
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

fn udp_dns() -> ! {
    attach_frame(0);
    if call(STACK, req(OP_OPEN_UDP, 0), 0).0 != REP_OK {
        done(0xE010);
    }

    // Build a DNS A-record query for "example.com" into the frame payload.
    let mut p = FRAME_VA + OFF_PAYLOAD;
    let put8 = |v: u8, at: &mut u64| {
        w8(*at, v);
        *at += 1;
    };
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
    let qlen = p - (FRAME_VA + OFF_PAYLOAD);

    set_dst(DNS_IP, DNS_PORT);
    if call(STACK, req(OP_SENDTO, 0), qlen).0 != REP_OK {
        done(0xE011);
    }

    let (rlen, _) = call(STACK, req(OP_RECV, 0), 0);
    if rlen < 12 || rlen == REP_ERR {
        done(0xE012); // no response, or too short to be a DNS message
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

fn tcp_echo() -> ! {
    const MSG: &[u8] = b"cricker-net!";

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

/// Run the selected client exchange. Entered from `netd`'s `_start` when the entry role is nonzero.
pub fn run(test: u64) -> ! {
    match test {
        TEST_UDP_DNS => udp_dns(),
        TEST_TCP_ECHO => tcp_echo(),
        _ => done(0xE0FF),
    }
}
