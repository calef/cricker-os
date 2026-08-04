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
//!
//! Name: ratified 2026-07-30 (Chris, DECISIONS §39, landed by milestone 46), replacing `netd`, and
//! respelled from `netstack` on 2026-08-01 (milestone 63) because `net` is already this tree's word
//! and the two halves are separate concepts. Refused `netd`, the name §39 was written about: it
//! holds five explicit capabilities, cannot name its own callers, is supervised, and can be reaped
//! by something that lacks the authority to build it, which is about as far from a daemon as a
//! long-running process gets.

#![no_std]
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

/// The heap smoltcp allocates against, capped well under the granted budget.
const HEAP_MAX: u64 = 128 * 4096;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

/// Our MAC. Locally administered; slirp routes DHCP regardless.
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

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

/// One open socket: its smoltcp handle, whether it is TCP, where its shared frame is mapped, and the
/// ephemeral local port it was assigned.
#[derive(Clone, Copy)]
struct Sock {
    handle: SocketHandle,
    is_tcp: bool,
    va: u64,
    local_port: u16,
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
#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, _a2: u64) -> ! {
    if role == 0 {
        server(dma_phys)
    } else {
        socket_test_client::run(role)
    }
}

/// The net server: bring the NIC up, run DHCP, then serve the socket contract.
fn server(dma_phys: u64) -> ! {
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
        sockets
            .get_mut::<udp::Socket>(handle)
            .recv_slice(&mut buf)
            .map(|(n, _)| n)
            .unwrap_or(0)
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
