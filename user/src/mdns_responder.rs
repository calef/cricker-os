//! **The mDNS responder** (milestone 55): what makes this machine appear in a Mac's Time Machine
//! list at all.
//!
//! A Mac does not browse SMB shares to find a backup destination; it asks multicast DNS, and it
//! offers only what answers. So this program is the difference between a working file service
//! nobody can find and a server that shows up in the UI. It is small on purpose: **every decision
//! about what to say is `mdns_proto`'s** (host-tested against real bytes from the reference router,
//! Kani-checked at the parser), **everything it says is `mdns_config`'s** (a document a person
//! edits), and what is left here is the IO. Bind the port, receive, decide, send.
//!
//! # What it does, in order
//!
//! 1. Parses its configuration document. A mistake in it stops the program with the line number,
//!    rather than putting a half-formed advertisement on the network.
//! 2. Claims UDP 5353 through the socket contract's `BIND_UDP`, which is an **authority it was
//!    granted** rather than a port it took: the spawn site packs the range with
//!    `socket_proto::udp_bind_grant`, and a responder spawned without it is refused (see
//!    notes/mdns.md).
//! 3. **Announces** all three service types (RFC 6762 §8.3), so a Mac already browsing learns about
//!    this machine without waiting to ask again.
//! 4. Answers queries until it has served `arg0` of them, or forever when that is zero. Each answer
//!    is one `mdns_proto::respond` call: multicast queries are answered to the group, legacy
//!    queriers (a source port that is not 5353) get a unicast response with the ID echoed and the
//!    TTLs capped, which is RFC 6762 §6.7 and is why the socket contract reports a datagram's
//!    source endpoint at all.
//!
//! # Capability contract
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the `Stack` endpoint (WRITE), shared with the stack's other clients
//! - slot 2: an untyped budget (to mint and delegate the shared frame)
//! - arg0: queries to answer before reporting OK; 0 means serve forever
//! - arg1: the IPv4 address this machine holds, as the DHCP lease reported it (`u32` of the
//!   octets, big-endian, 0 for "not known"). **Not configuration**: which address a machine has is
//!   a run-time fact, so whoever knows it passes it in, and with 0 the responder simply announces
//!   no A record.
//!
//! And **nothing else**, which is the point worth reading twice. This program holds no filesystem,
//! no disk, no console, and no listening TCP port. It cannot serve a file, cannot read one, and
//! cannot be talked into either: the SMB share it advertises is a *different* process holding a
//! *different* capability. Discovery and service are separate authorities here, and on the
//! reference implementation (one Samba, one config file, `vfs_fruit` plus Avahi in the same trust
//! domain) they are not.
//!
//! # Socket ids
//!
//! Sid 4, not 0. A stack endpoint's socket table is per-endpoint (`socket_proto`'s BUGS: clients
//! sharing an endpoint are indistinguishable to the server), and in the combined gate this program
//! shares one with `socket_test_client` (ids 0 and 1) and `smb_server` (2 and 3).
//!
//! # BUGS
//!
//! - **The configuration is compiled in, not read from a file.** `mdns_config`'s document is
//!   `include_str!`d here, so changing what this machine advertises means a rebuild. The reason is
//!   that reading a file needs a file capability wired through the spawn and a fixture the QEMU
//!   gate cannot seed today, and the fix is a `FileSpec` grant plus an `fs_proto` open-and-read at
//!   startup. Nothing about the format or the parser changes when that lands.
//! - **One announcement, not the RFC's two to eight.** RFC 6762 §8.3 asks for repeats at doubling
//!   intervals, so a Mac that missed the first datagram waits until its next browse rather than
//!   hearing us again. The repeats need a timer this program does not hold.
//! - **No probing.** RFC 6762 §8 says claim a name only after probing for a conflict, and
//!   `mdns_proto` has the wire halves (`probe_query`, `conflicts`, `tiebreak`) ready. This
//!   responder announces straight away, so two nife machines with the same `host` in one
//!   configuration would fight over the name and neither would notice.
//! - **A response larger than [`RESPONSE_MAX`] is not sent at all.** A query that asks for
//!   everything at once (`ANY` on the enumeration name, say) can compose a response past the
//!   transport's MTU, and this program stays silent rather than sending a datagram the 576-byte
//!   MTU would drop. RFC 6762's answer is the TC bit and a follow-up message, which `mdns_proto`
//!   does not implement either.
//! - **It never leaves.** A responder that stops should send goodbye records (TTL 0) so caches
//!   drop it immediately; this one exits after its rounds and its records age out on their own.
//! - **IPv4 only.** No AAAA, so a Mac on an IPv6-only segment finds nothing. `mdns_proto`'s
//!   `Advertisement` carries no IPv6 address yet.
//!
//! Name: unrecorded, and explicitly **provisional**, minted by milestone 55's responder lane on
//! 2026-08-16. `mdns_responder` is what macOS calls its own service (`mDNSResponder`), which is
//! the argument for it (a reader knows what it does before reading a word) and against it (it is
//! somebody else's product name, and this tree's `net_stack` deliberately does not spell itself
//! `netd`). Not put to calef.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::{endpoint, frame as fr, rights, untyped as ut};
use mdns_config::Config;
use mdns_proto::{GROUP_V4, PORT, QuerySource, Service, announcement, respond};
use socket_proto::{
    DATA_MAX, LISTEN_DENIED, LISTEN_GRANTED, LISTEN_IN_USE, OFF_DST_IP, OFF_DST_PORT, OFF_PAYLOAD,
    OP_ATTACH_FRAME, OP_BIND_UDP, OP_RECV, OP_SENDTO, REP_ERR, req,
};
use user_rt::{call, exit, invoke, send};

const REPORT: u64 = 0;
const STACK: u64 = 1;
const UNTYPED: u64 = 2;

/// This program's socket id; see the module header for why it is not 0.
const SID: u64 = 4;

/// Where the shared frame is mapped. Every program in this tree picks its own VA and the kernel's
/// spawn service must agree; this one matches nothing else because nothing else maps it.
const FRAME_VA: u64 = 0x0000_0000_00A0_0000;

/// **The advertisement, as a document rather than as constants.** See `mdns_config`: the values are
/// measured off a working Time Machine server, and they are data every step of the way from that
/// file to the wire.
const CONFIG: &str = include_str!("../mdns_responder.conf");

/// The largest response this responder will send, and the largest query it will read. The virtio
/// transport's MTU is 576 bytes (`net_transport::MTU`), which leaves 548 for a UDP payload after
/// the IP and UDP headers; smoltcp does not fragment, so a larger datagram is not sent at all.
const RESPONSE_MAX: usize = 548;

/// How many times to re-announce while waiting for a query. Each empty `RECV` has already waited
/// out the net server's full bounded wait, so this is a small number by necessity: the announcement
/// is unsolicited, and a responder that shouted more often would still be answering nobody.
const ANNOUNCE_TRIES: u32 = 3;

/// How many datagrams one round will look at before giving up. Datagrams that want no answer are
/// free (they do not spend an announce try) but they are not unbounded: a segment full of other
/// responders' chatter would otherwise keep a one-shot responder receiving forever.
const RECEIVES_MAX: u32 = 12;

/// The success word, the same one every other client of this stack reports.
const OK: u64 = 1;

// Failure codes. 0xE2xx is this program's range: `socket_test_client` owns 0xE0xx and `smb_server`
// 0xE1xx, and a verdict has to name which program refused.
const E_CONFIG: u64 = 0xE200; // the configuration document; the low byte is the line number
const E_FRAME_MINT: u64 = 0xE210;
const E_FRAME_MAP: u64 = 0xE211;
const E_FRAME_DELEGATE: u64 = 0xE212;
const E_BIND_DENIED: u64 = 0xE220; // spawned without the UDP bind grant this needs
const E_BIND_IN_USE: u64 = 0xE221;
const E_BIND_FAILED: u64 = 0xE222;
const E_ANNOUNCE_BUILD: u64 = 0xE230;
const E_ANNOUNCE_SEND: u64 = 0xE231;
const E_NO_QUERY: u64 = 0xE240; // nothing ever asked: the group join, or the host side
const E_ANSWER_SEND: u64 = 0xE241;

fn w8(va: u64, v: u8) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}
fn r8(va: u64) -> u8 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}
fn w16le(va: u64, v: u16) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u16, v) }
}
fn r16le(va: u64) -> u16 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u16) }
}

/// Report `code` and stop. A one-shot role must exit rather than spin: a leaked spinner starves
/// later tests on the same core (`socket_test_client` carries the finding).
fn done(code: u64) -> ! {
    send(REPORT, code, 0, 0);
    exit();
}

/// Mint a frame from our untyped, map it, and delegate it to our socket id.
fn attach_frame() {
    // SAFETY: `svc`. RETYPE returns the new frame capability's slot, or a negative error.
    let frame = unsafe { invoke(UNTYPED, ut::RETYPE, 0, 0, 0) };
    if frame < 0 {
        done(E_FRAME_MINT);
    }
    let frame = frame as u64;
    // SAFETY: `svc`. Map it writable; page tables come from our untyped.
    if unsafe { invoke(frame, fr::MAP, FRAME_VA, 1, UNTYPED) } < 0 {
        done(E_FRAME_MAP);
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
        done(E_FRAME_DELEGATE);
    }
}

/// Set the shared frame's destination header, which is where the next `SENDTO` sends.
fn set_dst(ip: [u8; 4], port: u16) {
    for (i, &b) in ip.iter().enumerate() {
        w8(FRAME_VA + OFF_DST_IP + i as u64, b);
    }
    w16le(FRAME_VA + OFF_DST_PORT, port);
}

/// The source endpoint a UDP `RECV` reply left in the frame header. **The whole reason mDNS needs
/// it**: RFC 6762 §6.7 decides a response's shape from the querier's source port, and a unicast
/// answer has to go back to the address that asked.
fn recv_source() -> ([u8; 4], u16) {
    let mut ip = [0u8; 4];
    for (i, b) in ip.iter_mut().enumerate() {
        *b = r8(FRAME_VA + OFF_DST_IP + i as u64);
    }
    (ip, r16le(FRAME_VA + OFF_DST_PORT))
}

/// Copy `msg` into the frame's payload and send it to `(ip, port)`.
fn send_to(msg: &[u8], ip: [u8; 4], port: u16) -> bool {
    if msg.len() > DATA_MAX {
        return false;
    }
    for (i, &b) in msg.iter().enumerate() {
        w8(FRAME_VA + OFF_PAYLOAD + i as u64, b);
    }
    set_dst(ip, port);
    call(STACK, req(OP_SENDTO, SID), msg.len() as u64).0 != REP_ERR
}

/// Announce all three service types to the group. Three datagrams rather than one, because the
/// transport's MTU cannot carry the whole record set (see [`mdns_proto::announcement`]).
fn announce(adv: &mdns_proto::Advertisement<'_>) {
    let mut out = [0u8; RESPONSE_MAX];
    for svc in [Service::Smb, Service::Adisk, Service::DeviceInfo] {
        let Ok(n) = announcement(adv, svc, &mut out) else {
            // The advertisement does not fit a datagram this transport can carry. A configuration
            // fault, not a network one, and silence would leave it looking like a lost packet.
            done(E_ANNOUNCE_BUILD);
        };
        if !send_to(&out[..n], GROUP_V4, PORT) {
            done(E_ANNOUNCE_SEND);
        }
    }
}

/// What one receive amounted to. The three cases call for different things, and collapsing the
/// last two is what makes a responder on a busy segment announce itself into the ground: **a
/// datagram that deserved silence is not a timeout.** Most of what arrives on 5353 belongs to
/// other machines, and mDNS's own rule is to say nothing about names you do not own.
enum Received {
    /// A query we are authoritative for; the response is on its way.
    Answered,
    /// Something arrived and wanted no answer: another responder's announcement, a query about
    /// somebody else's name, or a datagram this responder could not parse.
    Silence,
    /// Nothing arrived before the receive gave up.
    Nothing,
}

/// Receive one datagram and answer it if it is a query we are authoritative for.
fn serve_one(adv: &mdns_proto::Advertisement<'_>) -> Received {
    let (len, _) = call(STACK, req(OP_RECV, SID), 0);
    if len == REP_ERR || len == 0 {
        return Received::Nothing;
    }
    if len as usize > RESPONSE_MAX {
        // A datagram too large to be a query this transport could have carried is not read: the
        // parser needs the whole message, and half of one parses as a different message.
        return Received::Silence;
    }
    let (src_ip, src_port) = recv_source();

    let mut query = [0u8; RESPONSE_MAX];
    for (i, b) in query.iter_mut().take(len as usize).enumerate() {
        *b = r8(FRAME_VA + OFF_PAYLOAD + i as u64);
    }

    // RFC 6762 §6.7: a querier whose source port is not 5353 cannot receive our multicast, so it
    // gets a unicast response with the ID echoed, the question repeated, and the TTLs capped.
    // `mdns_proto` applies every one of those rules; this program's whole part in it is knowing
    // where the datagram came from.
    let src = if src_port == PORT {
        QuerySource::Multicast
    } else {
        QuerySource::LegacyUnicast
    };
    let mut out = [0u8; RESPONSE_MAX];
    let Ok(Some(n)) = respond(adv, &query[..len as usize], &mut out, src) else {
        // Either nothing to say, or the answer would not fit a datagram; both are silence. See
        // this program's BUGS for why the second is not the TC bit yet.
        return Received::Silence;
    };
    let (ip, port) = match src {
        QuerySource::Multicast => (GROUP_V4, PORT),
        QuerySource::LegacyUnicast => (src_ip, src_port),
    };
    if !send_to(&out[..n], ip, port) {
        done(E_ANSWER_SEND);
    }
    Received::Answered
}

/// `rounds` queries to answer (0 = forever), and the address the DHCP lease gave this machine.
#[unsafe(no_mangle)]
pub extern "C" fn _start(rounds: u64, ipv4: u64, _a2: u64) -> ! {
    let config = match Config::parse(CONFIG) {
        Ok(c) => c,
        // The low byte carries the line number, so a wrong document is findable from the verdict
        // alone. `Missing` has no line and reports as 0.
        Err(e) => done(E_CONFIG | (config_error_line(e) as u64 & 0xff)),
    };
    // 0 means "the lease is not known", which announces no A record rather than announcing 0.0.0.0.
    let addr = if ipv4 == 0 {
        None
    } else {
        Some((ipv4 as u32).to_be_bytes())
    };
    let adv = config.advertisement(addr);

    attach_frame();
    match call(STACK, req(OP_BIND_UDP, SID), PORT as u64).0 {
        LISTEN_GRANTED => {}
        // Not a retry: no port but this one will do, and nothing this program can do wins it. The
        // authority is the spawn site's to grant (`socket_proto::udp_bind_grant`).
        LISTEN_DENIED => done(E_BIND_DENIED),
        LISTEN_IN_USE => done(E_BIND_IN_USE),
        _ => done(E_BIND_FAILED),
    }

    announce(&adv);

    if rounds == 0 {
        // The serve-forever wiring: say the advertisement is up, then answer until the machine
        // stops. Re-announcing after an idle stretch is deliberate here, since nothing else will.
        send(REPORT, OK, 0, 0);
        loop {
            if matches!(serve_one(&adv), Received::Nothing) {
                announce(&adv);
            }
        }
    }

    let mut left = rounds;
    while left > 0 {
        let mut answered = false;
        let mut idle = 0;
        // Bounded either way: a round ends when it is answered, when nothing has arrived
        // `ANNOUNCE_TRIES` times, or when this many datagrams have gone by without one of ours
        // among them. The last bound is what keeps a busy segment from holding a one-shot
        // responder open forever.
        for _ in 0..RECEIVES_MAX {
            match serve_one(&adv) {
                Received::Answered => {
                    answered = true;
                    break;
                }
                Received::Silence => {}
                Received::Nothing => {
                    idle += 1;
                    if idle >= ANNOUNCE_TRIES {
                        break;
                    }
                    // A querier that missed the announcement is the likeliest reason nothing has
                    // arrived, so say it again.
                    announce(&adv);
                }
            }
        }
        if !answered {
            done(E_NO_QUERY);
        }
        left -= 1;
    }
    done(OK)
}

/// The line number inside a configuration error, or 0 for the one variant that has none.
fn config_error_line(e: mdns_config::Error) -> usize {
    use mdns_config::Error::*;
    match e {
        Malformed(l) | UnknownKey(l) | Repeated(l) | MalformedDisk(l) | TooManyDisks(l)
        | BadPort(l) | BadValue(l) => l,
        Missing(_) => 0,
    }
}

user_rt::panic_handler!();
