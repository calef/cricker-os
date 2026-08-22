//! **The net server: smoltcp as a userspace TCP/IP stack over the confined NIC** (milestone 30,
//! piece 3).
//!
//! The networking form of the userspace-reuse thesis: the kernel confines the NIC's DMA (pieces 1
//! and 2), and a real, reused TCP/IP stack (smoltcp, not hand-built) runs it entirely at EL0. The
//! kernel knows nothing about TCP, UDP, or DHCP; it owns only the DMA confinement.
//!
//! `net_stack` brings the NIC up, runs DHCP to completion (reporting the lease), then serves a
//! capability-shaped socket contract on a `Stack` endpoint (DECISIONS §25, notes/net.md,
//! `crates/socket_proto/src/lib.rs)`: a socket is a socket id, per-connection bytes cross in a shared frame the
//! client delegates, and every operation is one message. Phase one is single-threaded and
//! synchronous, one exchange per request; the server blocks on the `Stack` endpoint between
//! requests and drives the network inside handling one.
//!
//! # Capability contract
//! - slot 0: the report endpoint (WRITE) for the DHCP lease
//! - slot 1: the NIC interrupt (WAIT / ACK)
//! - slot 2: the confined `Virtio` transport
//! - slot 3: an untyped budget, for the heap and for mapping clients' shared frames
//! - slot 4: the `Stack` endpoint (READ), where clients' requests arrive
//! - arg1: the DMA page's physical address
//! - arg2: the **grant word**: the TCP listen range (milestone 107) in its low half and the
//!   fixed-UDP bind range (milestone 55's mDNS stack half) in its high half, both packed by
//!   `socket_proto`. Zero, the default, means no port anywhere in either protocol: a stack serves
//!   inbound connections, or claims a fixed UDP port, only when whoever spawned it said which.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46), replacing `netd`, and
//! respelled from `netstack` on 2026-08-01 (milestone 63) because `net` is already this tree's word
//! and the two halves are separate concepts. Refused `netd`, the name DECISIONS §39 was written
//! about: it holds five explicit capabilities, cannot name its own callers, is supervised, and can
//! be reaped by something that lacks the authority to build it, which is about as far from the
//! model that suffix claims as a long-running process gets.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

extern crate alloc;

use alloc::vec;

use abi::{frame as fr, irq};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address};
use user_rt::{cap_delete, cntfrq, invoke, now, recv_cap, reply, send};

#[path = "net_transport.rs"]
mod net_transport;
// The socket-contract client rides in this same binary (dispatched by the entry role), because the
// initrd directory holds at most 15 files; see user/src/socket_test_client.rs.
#[path = "socket_test_client.rs"]
mod socket_test_client;
use socket_proto::*;

const REPORT: u64 = 0;
const IRQ: u64 = 1;
const UNTYPED: u64 = 3;
const STACK: u64 = 4;

/// The heap smoltcp allocates against, capped under the granted budget.
///
/// **96 pages, down from 128** (milestone 107), and the two numbers are a pair: the spawn service
/// grants `NET_SERVER_BUDGET_PAGES = 128`, and the difference is what pays for the heap's page
/// tables and for mapping clients' shared frames. The budget came down because ten `net_stack`
/// regions are held for the whole boot and never reclaimed, and the aarch64 suite ran out of memory
/// when an eleventh net test arrived; see `kernel/src/user/virtio_service.rs` for the measurement.
/// If either number moves, both must.
const HEAP_MAX: u64 = 96 * 4096;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

/// Our MAC. Locally administered; slirp routes DHCP regardless.
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// The mDNS group, RFC 6762's 224.0.0.251. The stack joins it at startup (milestone 55's mDNS
/// stack half): joining is what makes smoltcp's IPv4 input path accept datagrams addressed to the
/// group, and an mDNS responder cannot exist without that. Unconditional rather than
/// client-requested because membership is interface state, not socket state, and this stack has
/// exactly one interface; what IS granted per client is the right to bind port 5353
/// (`socket_proto::udp_bind_grant`). See notes/mdns.md.
const MDNS_GROUP: Ipv4Address = Ipv4Address::new(224, 0, 0, 251);

/// Where a client's shared frame for socket `sid` is mapped in `net_stack`'s address space. Above the DMA
/// page (`0x90_0000`) and well below the heap (1 GiB).
fn socket_va(sid: usize) -> u64 {
    0x0000_0000_00A0_0000 + sid as u64 * 0x1000
}

/// smoltcp's clock, from the monotonic counter, in milliseconds.
fn instant() -> Instant {
    let ms = (now() as u128 * 1000 / cntfrq() as u128) as i64;
    Instant::from_millis(ms)
}

// Absolute-VA access to a mapped shared frame (little-endian for the length and port fields).
fn a_r8(va: u64) -> u8 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}
fn a_r16(va: u64) -> u16 {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::read_volatile(va as *const u16) }
}
fn a_w16(va: u64, v: u16) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u16, v) }
}
fn a_w8(va: u64, v: u8) {
    // SAFETY: `va` addresses a field inside a shared frame this process has mapped. Volatile because the peer writes the same frame, so a cached read would be a stale one.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}

/// One open socket: its smoltcp handle, whether it is TCP, where its shared frame is mapped, and the
/// ephemeral local port it was assigned.
///
/// `listen_port` is nonzero on a **listener** and zero on everything else, which is the whole of the
/// distinction inside the server: a listener holds a smoltcp socket parked in `Listen` state, has no
/// shared frame (`va == 0`, and it never needs one, because no bytes cross on a listener), and its
/// port is the one it was granted rather than one the allocator handed out.
#[derive(Clone, Copy)]
struct Sock {
    handle: SocketHandle,
    is_tcp: bool,
    va: u64,
    local_port: u16,
    listen_port: u16,
}

/// The private ephemeral-port range `net_stack` allocates local ports from, and a **rotating** allocator
/// over it. The local port must be independent of the socket id: deriving it from the id (the
/// original bug, found by the `std::net` PAL) means reopening a just-closed id reuses the exact port,
/// and a TCP connect on a 4-tuple whose slirp flow has not yet cleared stalls in the bounded poll
/// forever. Advancing monotonically hands out a fresh port each open, so a closed connection's port
/// is not reused until the whole range has cycled; ports a live socket still holds are skipped
/// outright. See notes/net.md.
const EPHEMERAL_LO: u16 = 49152;
const EPHEMERAL_HI: u16 = 65535;

struct PortAllocator {
    next: u16,
}

impl PortAllocator {
    fn new() -> Self {
        Self { next: EPHEMERAL_LO }
    }

    /// The next free ephemeral port not currently held by a live socket. Bounded by the range size,
    /// so it always terminates; with only `MAX_SOCKETS` sockets ever live, it never exhausts.
    fn alloc(&mut self, socks: &[Option<Sock>; MAX_SOCKETS]) -> u16 {
        for _ in 0..=(EPHEMERAL_HI - EPHEMERAL_LO) {
            let port = self.next;
            self.next = if self.next == EPHEMERAL_HI {
                EPHEMERAL_LO
            } else {
                self.next + 1
            };
            if !socks.iter().flatten().any(|s| s.local_port == port) {
                return port;
            }
        }
        self.next
    }
}

/// The largest datagram/segment buffered per socket.
const SOCK_BUF: usize = 2048;

/// Entry role 0 is the net server; any other role runs the socket-contract client (the same
/// binary, so the initrd stays under its 15-file directory limit).
///
/// `a2` is the server's **grant word**, both port authorities in one spawn argument: the TCP
/// listen range in the low half (milestone 107, `socket_proto::listen_grant`) and the fixed-UDP
/// bind range in the high half (milestone 55's mDNS stack half, `socket_proto::udp_bind_grant`).
/// Whoever spawns the server decides both; a server spawned with zero (`NO_LISTEN_GRANT`, which
/// every outbound-only test uses) refuses every `LISTEN` and every `BIND_UDP`. The client half
/// ignores it.
#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, a2: u64) -> ! {
    if role == 0 {
        server(dma_phys, a2)
    } else {
        socket_test_client::run(role)
    }
}

/// The net server: bring the NIC up, run DHCP, then serve the socket contract.
fn server(dma_phys: u64, grant_word: u64) -> ! {
    HEAP.init(UNTYPED, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    let mut dev = net_transport::VirtioNet::bring_up(dma_phys);
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(MAC)));
    config.random_seed = now();
    let mut iface = Interface::new(config, &mut dev, instant());
    let mut sockets = SocketSet::new(vec![]);

    // --- DHCP: bring the interface up, then report the lease (the phase-A test asserts it). ---
    let dhcp = sockets.add(dhcpv4::Socket::new());
    loop {
        iface.poll(instant(), &mut dev, &mut sockets);
        if let Some(dhcpv4::Event::Configured(cfg)) = sockets.get_mut::<dhcpv4::Socket>(dhcp).poll()
        {
            iface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::Ipv4(cfg.address));
            });
            if let Some(router) = cfg.router {
                let _ = iface.routes_mut().add_default_ipv4_route(router);
            }
            let octets = cfg.address.address().octets();
            send(REPORT, u32::from_be_bytes(octets) as u64, 0, 0);
            break;
        }
        // Same discipline as service_until: block on the interrupt only when smoltcp has no timer
        // pending, so a dropped DHCP OFFER is retried by smoltcp's own DISCOVER retransmit rather
        // than deadlocking on an interrupt that will not come.
        wait_for_nic(&mut iface, &mut dev, &mut sockets);
    }

    // Join the mDNS group, after DHCP so the IGMP membership report carries a real source address.
    // `join_multicast_group` only queues the join; the poll is what emits the report and flips the
    // group to joined, and from then on datagrams to 224.0.0.251 pass the IPv4 accept filter. The
    // error arm is unreachable for a well-formed multicast address unless the group table is full,
    // and this stack joins exactly one group.
    let _ = iface.join_multicast_group(MDNS_GROUP);
    iface.poll(instant(), &mut dev, &mut sockets);

    // --- Serve the socket contract. One synchronous exchange per request. ---
    let mut socks: [Option<Sock>; MAX_SOCKETS] = [None; MAX_SOCKETS];
    let mut frame_va: [u64; MAX_SOCKETS] = [0; MAX_SOCKETS];
    let mut ports = PortAllocator::new();
    loop {
        let (w0, cap_slot, w1) = recv_cap(STACK);
        let op = req_op(w0);
        let sid = req_sid(w0) as usize;
        if sid >= MAX_SOCKETS {
            if cap_slot != abi::endpoint::NO_CAP {
                reply(cap_slot, REP_ERR, 0);
            }
            continue;
        }

        match op {
            OP_ATTACH_FRAME => {
                // cap_slot holds the delegated frame. Map it writable at this socket's VA, paid for
                // from net_stack's untyped; the mapping outlives the cap, so drop the cap after. ATTACH
                // is a SEND_CAP, so there is no reply cap to answer on.
                let va = socket_va(sid);
                // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
                // before acting (user_rt's contract). A caller cannot break an invariant by passing a
                // bad slot or method; it gets an error back.
                let r = unsafe { invoke(cap_slot, fr::MAP, va, 1, UNTYPED) };
                cap_delete(cap_slot);
                if r >= 0 {
                    frame_va[sid] = va;
                }
            }

            OP_OPEN_UDP => {
                let s = udp::Socket::new(
                    udp::PacketBuffer::new(
                        vec![udp::PacketMetadata::EMPTY; 8],
                        vec![0u8; SOCK_BUF],
                    ),
                    udp::PacketBuffer::new(
                        vec![udp::PacketMetadata::EMPTY; 8],
                        vec![0u8; SOCK_BUF],
                    ),
                );
                let handle = sockets.add(s);
                let local_port = ports.alloc(&socks);
                let _ = sockets.get_mut::<udp::Socket>(handle).bind(local_port);
                socks[sid] = Some(Sock {
                    handle,
                    is_tcp: false,
                    va: frame_va[sid],
                    local_port,
                    listen_port: 0,
                });
                reply(cap_slot, REP_OK, 0);
            }

            OP_OPEN_TCP => {
                let s = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0u8; SOCK_BUF]),
                    tcp::SocketBuffer::new(vec![0u8; SOCK_BUF]),
                );
                let handle = sockets.add(s);
                let local_port = ports.alloc(&socks);
                socks[sid] = Some(Sock {
                    handle,
                    is_tcp: true,
                    va: frame_va[sid],
                    local_port,
                    listen_port: 0,
                });
                reply(cap_slot, REP_OK, 0);
            }

            OP_SENDTO => {
                let rep = udp_sendto(&mut iface, &mut dev, &mut sockets, &socks, sid, w1 as usize);
                reply(cap_slot, rep, 0);
            }

            OP_RECV => {
                let rep = sock_recv(&mut iface, &mut dev, &mut sockets, &socks, sid);
                reply(cap_slot, rep, 0);
            }

            OP_CONNECT => {
                let rep = tcp_connect(&mut iface, &mut dev, &mut sockets, &socks, sid);
                reply(cap_slot, rep, 0);
            }

            OP_SEND => {
                let rep = tcp_send(&mut iface, &mut dev, &mut sockets, &socks, sid, w1 as usize);
                reply(cap_slot, rep, 0);
            }

            OP_CLOSE => {
                if let Some(sk) = socks[sid].take() {
                    if sk.is_tcp {
                        let handle = sk.handle;
                        sockets.get_mut::<tcp::Socket>(handle).close();
                        // Drain the close handshake so the peer (and slirp's flow for it) sees a
                        // clean teardown before we drop the socket. Dropping a socket mid-close
                        // leaves the peer's connection half-open, which on the guestfwd echo peer
                        // blocks the next connection to it. Both FINs exchanged (Closed or TimeWait)
                        // is enough; waiting for full Closed would linger in TimeWait's timer.
                        // Bounded, so a peer that never finishes still returns.
                        service_until(&mut iface, &mut dev, &mut sockets, |s| {
                            matches!(
                                s.get_mut::<tcp::Socket>(handle).state(),
                                tcp::State::Closed | tcp::State::TimeWait
                            )
                        });
                    } else {
                        sockets.get_mut::<udp::Socket>(sk.handle).close();
                        iface.poll(instant(), &mut dev, &mut sockets);
                    }
                    sockets.remove(sk.handle);
                }
                reply(cap_slot, REP_OK, 0);
            }

            OP_LISTEN => {
                let rep = tcp_listen(&mut sockets, &mut socks, sid, w1, grant_word);
                reply(cap_slot, rep, 0);
            }

            OP_BIND_UDP => {
                let rep = udp_bind(&mut sockets, &mut socks, frame_va[sid], sid, w1, grant_word);
                reply(cap_slot, rep, 0);
            }

            OP_ACCEPT => {
                // The target id's frame must already be attached: an accepted connection carries
                // bytes, so it needs the resource a listener never did.
                let target_va = if w1 < MAX_SOCKETS as u64 {
                    frame_va[w1 as usize]
                } else {
                    0
                };
                let rep = tcp_accept(
                    &mut iface,
                    &mut dev,
                    &mut sockets,
                    &mut socks,
                    sid,
                    w1,
                    target_va,
                );
                reply(cap_slot, rep, 0);
            }

            _ => {
                if cap_slot != abi::endpoint::NO_CAP {
                    reply(cap_slot, REP_ERR, 0);
                }
            }
        }
    }
}

/// Read the destination (octets + little-endian port) from a shared frame's header.
fn read_dst(va: u64) -> IpEndpoint {
    let ip = Ipv4Address::new(
        a_r8(va + OFF_DST_IP),
        a_r8(va + OFF_DST_IP + 1),
        a_r8(va + OFF_DST_IP + 2),
        a_r8(va + OFF_DST_IP + 3),
    );
    let port = a_r16(va + OFF_DST_PORT);
    IpEndpoint::new(IpAddress::Ipv4(ip), port)
}

/// **Wait for the NIC to need attention again, without stalling smoltcp's own timers.**
///
/// The obvious loop, "block on the NIC interrupt, service on each wakeup," has a hole that SMP timing
/// on riscv exposed and that a lone core hid: smoltcp drives TCP retransmits, delayed ACKs, and DNS
/// timeouts from *its* clock, advanced only when we call `poll`. If we block on the interrupt alone
/// and the exchange is waiting on one of those timers (a segment we must retransmit because its ACK
/// was dropped), no RX interrupt is coming (the peer is waiting for that retransmit), so the block
/// never returns and the whole pipeline deadlocks with every core idle. aarch64 happened never to
/// drop a segment and so never hit it; riscv under the SMP scatter did. See notes/net.md.
///
/// So: ask smoltcp when it next needs to run. When it has **no** timer pending (`poll_delay` is
/// `None`), block on the interrupt, which is the common, efficient case (0% CPU until a frame
/// arrives), and correct, because with nothing of our own to send we are purely waiting on the peer,
/// whose own retransmit will wake us. When it **does** have a timer pending, do not block: yield and
/// let the caller re-`poll`, so the timer actually fires. That confines the busy interval to the
/// short retransmit window, not the whole exchange.
fn wait_for_nic(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
) {
    if iface.poll_delay(instant(), sockets).is_none() {
        // SAFETY: as above: the kernel validates the capability and the method.
        unsafe {
            invoke(IRQ, irq::WAIT, 0, 0, 0);
        }
        dev.ack_irq();
    } else {
        // A smoltcp timer is due: keep the source armed and the device quiet, then yield so the
        // caller re-polls and smoltcp emits the retransmit/ACK. Not a blocking wait, on purpose.
        dev.ack_irq();
        user_rt::yield_now();
    }
}

/// Drive the poll loop until `cond` holds, servicing the NIC on each wakeup. Bounded so a stuck
/// exchange returns rather than spinning forever; a genuinely lost packet still relies on the
/// QEMU-level timeout, the disk driver's discipline.
fn service_until(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    mut cond: impl FnMut(&mut SocketSet) -> bool,
) -> bool {
    let start = instant().total_millis();
    loop {
        iface.poll(instant(), dev, sockets);
        if cond(sockets) {
            return true;
        }
        // A stuck exchange still returns rather than blocking a hart forever; 15 s is far beyond any
        // slirp round trip yet well under the 60 s watchdog. The disk driver's bounded discipline.
        if instant().total_millis() - start > 15_000 {
            iface.poll(instant(), dev, sockets);
            return cond(sockets);
        }
        wait_for_nic(iface, dev, sockets);
    }
}

fn udp_sendto(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    socks: &[Option<Sock>; MAX_SOCKETS],
    sid: usize,
    len: usize,
) -> u64 {
    let Some(sk) = socks[sid] else { return REP_ERR };
    if sk.is_tcp || sk.va == 0 || len > DATA_MAX {
        return REP_ERR;
    }
    let dst = read_dst(sk.va);
    let mut buf = vec![0u8; len];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = a_r8(sk.va + OFF_PAYLOAD + i as u64);
    }
    if sockets
        .get_mut::<udp::Socket>(sk.handle)
        .send_slice(&buf, dst)
        .is_err()
    {
        return REP_ERR;
    }
    iface.poll(instant(), dev, sockets); // push the datagram out
    REP_OK
}

fn sock_recv(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    socks: &[Option<Sock>; MAX_SOCKETS],
    sid: usize,
) -> u64 {
    let Some(sk) = socks[sid] else { return REP_ERR };
    if sk.va == 0 {
        return REP_ERR;
    }
    let handle = sk.handle;
    let ready = if sk.is_tcp {
        service_until(iface, dev, sockets, |s| {
            s.get_mut::<tcp::Socket>(handle).can_recv()
        })
    } else {
        service_until(iface, dev, sockets, |s| {
            s.get_mut::<udp::Socket>(handle).can_recv()
        })
    };
    if !ready {
        return REP_ERR;
    }

    let mut buf = [0u8; DATA_MAX];
    let n = if sk.is_tcp {
        sockets
            .get_mut::<tcp::Socket>(handle)
            .recv_slice(&mut buf)
            .unwrap_or(0)
    } else {
        match sockets.get_mut::<udp::Socket>(handle).recv_slice(&mut buf) {
            Ok((n, meta)) => {
                // The datagram's source endpoint rides back in the frame's dst fields, which are
                // dead space on a reply (socket_proto's layout note). A responder needs it: mDNS's
                // reply semantics turn on the querier's source port (RFC 6762 §6.7), and this used
                // to be discarded here with `.map(|(n, _)| n)`.
                let IpAddress::Ipv4(src) = meta.endpoint.addr;
                for (i, &b) in src.octets().iter().enumerate() {
                    a_w8(sk.va + OFF_DST_IP + i as u64, b);
                }
                a_w16(sk.va + OFF_DST_PORT, meta.endpoint.port);
                n
            }
            Err(_) => 0,
        }
    };
    for (i, &b) in buf[..n].iter().enumerate() {
        // SAFETY: the payload area of this socket's mapped shared frame; `n` is the byte count smoltcp just produced into `buf`, which is sized to that frame's payload capacity.
        unsafe {
            core::ptr::write_volatile((sk.va + OFF_PAYLOAD + i as u64) as *mut u8, b);
        }
    }
    a_w16(sk.va + OFF_LEN, n as u16);
    n as u64
}

fn tcp_connect(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    socks: &[Option<Sock>; MAX_SOCKETS],
    sid: usize,
) -> u64 {
    let Some(sk) = socks[sid] else { return REP_ERR };
    if !sk.is_tcp || sk.va == 0 {
        return REP_ERR;
    }
    let dst = read_dst(sk.va);
    let handle = sk.handle;
    let local = sk.local_port;
    {
        let cx = iface.context();
        if sockets
            .get_mut::<tcp::Socket>(handle)
            .connect(cx, dst, local)
            .is_err()
        {
            return REP_ERR;
        }
    }
    // Drive until the connection settles: established, or back to a closed state (a RST from an
    // unused port, the deterministic refusal outcome).
    service_until(iface, dev, sockets, |s| {
        let st = s.get_mut::<tcp::Socket>(handle).state();
        st == tcp::State::Established || st == tcp::State::Closed
    });
    match sockets.get_mut::<tcp::Socket>(handle).state() {
        tcp::State::Established => CONNECT_ESTABLISHED,
        _ => CONNECT_REFUSED,
    }
}

/// Park a fresh TCP socket in `Listen` state on `port`. `None` if smoltcp refuses the port (only
/// port 0 does that), in which case the socket is removed rather than left in the set as a
/// `Closed` socket nothing owns.
fn arm_listener(sockets: &mut SocketSet, port: u16) -> Option<SocketHandle> {
    let s = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0u8; SOCK_BUF]),
        tcp::SocketBuffer::new(vec![0u8; SOCK_BUF]),
    );
    let handle = sockets.add(s);
    if sockets.get_mut::<tcp::Socket>(handle).listen(port).is_err() {
        sockets.remove(handle);
        return None;
    }
    Some(handle)
}

/// **`LISTEN`: claim a port, if this stack was granted it** (milestone 107).
///
/// Three refusals, and they are deliberately distinguishable (`socket_proto`): outside the grant is
/// a refusal of *authority* and no retry will fix it; already-listening is a collision on an
/// exclusive name and another port would work; `REP_ERR` is the client asking on a socket id it is
/// already using, which is its own bookkeeping bug.
///
/// A listener gets no shared frame. That is the contract's claim that a listener and a connection
/// are different objects, made concrete: there is nothing to map, because nothing is carried.
fn tcp_listen(
    sockets: &mut SocketSet,
    socks: &mut [Option<Sock>; MAX_SOCKETS],
    sid: usize,
    port_word: u64,
    grant: u64,
) -> u64 {
    if socks[sid].is_some() {
        return REP_ERR;
    }
    // Not truncated to 16 bits: a request for 65536 is a request for a port no grant can name, and
    // silently turning it into port 0 would answer a question nobody asked.
    if port_word > u16::MAX as u64 {
        return LISTEN_DENIED;
    }
    let port = port_word as u16;
    if !grant_allows(grant, port) {
        return LISTEN_DENIED;
    }
    if socks.iter().flatten().any(|s| s.listen_port == port) {
        return LISTEN_IN_USE;
    }
    let Some(handle) = arm_listener(sockets, port) else {
        return REP_ERR;
    };
    socks[sid] = Some(Sock {
        handle,
        is_tcp: true,
        va: 0,
        local_port: port,
        listen_port: port,
    });
    LISTEN_GRANTED
}

/// **`ACCEPT`: block until a peer connects, then hand the connection to its own socket id.**
///
/// The listener keeps its id and its port; what moves is the smoltcp socket that just completed a
/// handshake, which becomes the *connection* at `target_word`, and the listener is immediately
/// re-armed with a fresh socket parked on the same port. That re-arm is the difference between a
/// server that accepts a connection and a server that accepts **one** connection, and it happens
/// before this function returns, so the client can be busy serving the first exchange while the
/// second handshake completes underneath it in the poll loop.
///
/// Refusing `target_word == lsid` is the contract enforcing itself: POSIX would let a listening
/// descriptor become the connection in place, and that conflation is exactly what makes "give this
/// program port 80" and "give this program this connection" the same kind of grant there.
///
/// **BUGS.** The backlog is one connection deep, because smoltcp has no accept queue: a second peer
/// arriving in the window between a handshake completing and this call re-arming gets a RST rather
/// than a wait. Bounded by `service_until`'s 15 s, so an `ACCEPT` nobody ever connects to returns
/// `REP_ERR` instead of holding the server forever.
fn tcp_accept(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    socks: &mut [Option<Sock>; MAX_SOCKETS],
    lsid: usize,
    target_word: u64,
    target_va: u64,
) -> u64 {
    if target_word >= MAX_SOCKETS as u64 {
        return REP_ERR;
    }
    let target = target_word as usize;
    if target == lsid || socks[target].is_some() || target_va == 0 {
        return REP_ERR;
    }
    let Some(listener) = socks[lsid] else {
        return REP_ERR;
    };
    if listener.listen_port == 0 {
        return REP_ERR;
    }
    let handle = listener.handle;
    let port = listener.listen_port;

    // `Listen` means no SYN yet and `SynReceived` means the handshake is in flight; any other state
    // means it either landed or was aborted, and both of those end the wait.
    service_until(iface, dev, sockets, |s| {
        !matches!(
            s.get_mut::<tcp::Socket>(handle).state(),
            tcp::State::Listen | tcp::State::SynReceived
        )
    });
    let landed = !matches!(
        sockets.get_mut::<tcp::Socket>(handle).state(),
        tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed
    );

    // Re-arm whatever happened, including the aborted case: the listener's authority did not expire
    // because one peer sent a RST, and a client that gets `REP_ERR` should be able to accept again.
    match arm_listener(sockets, port) {
        Some(fresh) => {
            socks[lsid] = Some(Sock {
                handle: fresh,
                ..listener
            });
        }
        None => {
            socks[lsid] = None;
            return REP_ERR;
        }
    }
    if !landed {
        sockets.remove(handle);
        return REP_ERR;
    }
    socks[target] = Some(Sock {
        handle,
        is_tcp: true,
        va: target_va,
        local_port: port,
        listen_port: 0,
    });
    REP_OK
}

/// **`BIND_UDP`: claim a fixed UDP port, if this stack was granted it** (milestone 55's mDNS stack
/// half). `tcp_listen`'s three refusals, verbatim, because they are properties of claiming an
/// exclusive port and not of TCP: outside the grant is a refusal of *authority* (`LISTEN_DENIED`),
/// a port some live socket already holds is a collision (`LISTEN_IN_USE`, and the check spans
/// *all* sockets, so a fixed bind cannot silently shadow an ephemeral port a live socket was
/// allocated), and asking on an occupied socket id is the client's own bookkeeping bug
/// (`REP_ERR`). Unlike a listener the socket carries bytes, so it takes the frame attached at
/// `sid` exactly as `OP_OPEN_UDP` would.
fn udp_bind(
    sockets: &mut SocketSet,
    socks: &mut [Option<Sock>; MAX_SOCKETS],
    va: u64,
    sid: usize,
    port_word: u64,
    grant: u64,
) -> u64 {
    if socks[sid].is_some() {
        return REP_ERR;
    }
    // Not truncated to 16 bits, tcp_listen's reasoning: a request for 65536 is a request for a
    // port no grant can name, and silently taking it as port 0 answers a question nobody asked.
    if port_word > u16::MAX as u64 {
        return LISTEN_DENIED;
    }
    let port = port_word as u16;
    if !udp_grant_allows(grant, port) {
        return LISTEN_DENIED;
    }
    if socks.iter().flatten().any(|s| s.local_port == port) {
        return LISTEN_IN_USE;
    }
    let s = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; SOCK_BUF]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; SOCK_BUF]),
    );
    let handle = sockets.add(s);
    if sockets.get_mut::<udp::Socket>(handle).bind(port).is_err() {
        // Only port 0 is refused, and the grant already excludes it; kept so a surprise is a
        // clean error rather than a socket nothing owns parked in the set.
        sockets.remove(handle);
        return REP_ERR;
    }
    socks[sid] = Some(Sock {
        handle,
        is_tcp: false,
        va,
        local_port: port,
        listen_port: 0,
    });
    LISTEN_GRANTED
}

fn tcp_send(
    iface: &mut Interface,
    dev: &mut net_transport::VirtioNet,
    sockets: &mut SocketSet,
    socks: &[Option<Sock>; MAX_SOCKETS],
    sid: usize,
    len: usize,
) -> u64 {
    let Some(sk) = socks[sid] else { return REP_ERR };
    if !sk.is_tcp || sk.va == 0 || len > DATA_MAX {
        return REP_ERR;
    }
    let mut buf = vec![0u8; len];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = a_r8(sk.va + OFF_PAYLOAD + i as u64);
    }
    let sent = sockets
        .get_mut::<tcp::Socket>(sk.handle)
        .send_slice(&buf)
        .unwrap_or(0);
    iface.poll(instant(), dev, sockets);
    sent as u64
}

user_rt::panic_handler!();
