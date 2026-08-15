//! `std::net` for nife (milestone 27 phase two): `TcpStream` and outbound `UdpSocket` bound
//! to the net_stack socket contract (DECISIONS §25, notes/net.md, `user/src/netproto.rs`).
//!
//! This is the client half of the contract std sees. A std program given the network holds a
//! `Stack` endpoint at slot 2 and an untyped budget at slot 3 (the slot convention in
//! `pal/nife/rt.rs`). Opening a socket mints a shared `Frame` from the untyped, maps it, and
//! delegates it to net_stack (`SEND_CAP`, `OP_ATTACH_FRAME`); every later operation is one `CALL`
//! carrying a **socket id** and control words, with the payload already sitting in the shared
//! frame. net_stack drives smoltcp over the confined NIC and replies. Bytes never cross a message.
//!
//! **Blocking, one exchange at a time.** The contract is synchronous: a `CALL` blocks in net_stack
//! while it services the network, so a std `read` blocks until data arrives, exactly the
//! semantics `std::net`'s default blocking API wants. The target is single-threaded, so a program
//! can hold several sockets (up to `MAX_SOCKETS`) but can only ever have one operation in flight.
//!
//! ## What is honestly Unsupported, and why
//! - **`TcpListener`.** The contract **does** have LISTEN and ACCEPT since milestone 107, and this
//!   is now a gap in the PAL rather than in the contract: `bind` still returns `Unsupported`. What
//!   it needs is small and mechanical (`bind` -> `OP_LISTEN` on a socket id this PAL allocates,
//!   `accept` -> `OP_ACCEPT` into a second id with a frame attached), plus a **listen grant** on the
//!   stack this program is spawned with, since a net server refuses every port it was not granted.
//!   See notes/net.md, "The inbound half".
//! - **Non-blocking mode and read/write timeouts.** The contract is blocking-only; there is no
//!   poll verb. `set_nonblocking(true)` and `set_*_timeout(Some(..))` return `Unsupported`;
//!   `set_nonblocking(false)` and the `None` timeouts (which mean "block") succeed.
//! - **DNS / `lookup_host`.** No resolver rides the contract; the demo uses literal addresses and
//!   does its own DNS as a plain UDP round trip. `lookup_host` returns `Unsupported`, so
//!   `ToSocketAddrs` resolves only already-numeric addresses.
//! - **IPv6.** net_stack is IPv4-only (smoltcp built with `proto-ipv4`). A V6 address is `Unsupported`.
//! - **`peek`, `duplicate`, multicast join/leave.** No contract verb backs them.
//!
//! Advisory knobs with no contract effect (`set_nodelay`, `set_ttl`, `set_linger`, broadcast,
//! multicast options) accept and return plausible values rather than fail, the way minimal
//! backends do; they change nothing on the wire.

#![allow(dead_code)]

use crate::cell::UnsafeCell;
use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::sync::atomic::{AtomicBool, Ordering};
use crate::sys::pal::nife::netproto::*;
use crate::sys::pal::nife::{abi, rt};
use crate::sys::unsupported;
use crate::time::Duration;

/// The slots the loader owes a networked std program (see `pal/nife/rt.rs`).
const STACK: u64 = rt::STACK_SLOT;
const NET_UNTYPED: u64 = rt::NET_UNTYPED_SLOT;

/// Where each socket's shared frame maps in this process. One page per id, well clear of the
/// program image (0x40_0000), its stack (below 0x50_0000), and the heap (0x4000_0000). net_stack maps
/// the same frame at its own address; the two are the one shared page the contract grants.
const FRAME_BASE: u64 = 0x0000_0000_1000_0000;

fn frame_va(id: u64) -> u64 {
    FRAME_BASE + id * 0x1000
}

// --- The socket registry ---------------------------------------------------------------------
//
// A small fixed table, one entry per possible socket id, guarded by a spinlock (uncontended on
// this single-threaded target, correct if threads ever arrive, the same discipline as the heap in
// `sys/alloc/nife`). It tracks which ids are in use, which have had their shared frame attached
// to net_stack (a once-per-id, sticky fact so open/close cycles reuse the frame without re-mapping an
// already-mapped VA), a per-TCP-socket residual buffer (bytes net_stack delivered into the frame that a
// short `read` could not take), and each UDP socket's peer/last destination.

struct Slot {
    in_use: bool,
    attached: bool,
    // TCP: bytes received but not yet handed to the caller (a `read` whose buffer was smaller than
    // the segment net_stack delivered). Served before the next `RECV`, so a stream never drops bytes.
    res_off: usize,
    res_len: usize,
    res: [u8; DATA_MAX],
    // UDP: the connected peer (set by `connect`) and the most recent send destination, used as the
    // reported source of `recv_from` since the contract's RECV does not carry the datagram source.
    peer: Option<(Ipv4Addr, u16)>,
    last_dst: Option<(Ipv4Addr, u16)>,
}

impl Slot {
    const fn new() -> Slot {
        Slot {
            in_use: false,
            attached: false,
            res_off: 0,
            res_len: 0,
            res: [0; DATA_MAX],
            peer: None,
            last_dst: None,
        }
    }
}

struct Registry {
    locked: AtomicBool,
    slots: UnsafeCell<[Slot; MAX_SOCKETS]>,
    // A round-robin cursor for id allocation. net_stack derives each socket's local port from its id
    // (`LOCAL_PORT_BASE + sid`), so handing out ids in rotation rather than always the lowest free
    // one keeps a fresh open from immediately reusing the port a just-closed socket held, which
    // slirp can still be holding. See the reuse note in notes/std.md.
    next: UnsafeCell<usize>,
}

// SAFETY: all access to `slots` goes through the `locked` spinlock.
unsafe impl Sync for Registry {}

static REG: Registry = Registry {
    locked: AtomicBool::new(false),
    slots: UnsafeCell::new([const { Slot::new() }; MAX_SOCKETS]),
    next: UnsafeCell::new(0),
};

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        REG.locked.store(false, Ordering::Release);
    }
}

fn lock() -> Guard {
    while REG
        .locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        crate::hint::spin_loop();
    }
    Guard
}

impl Guard {
    #[allow(clippy::mut_from_ref)]
    fn slots(&mut self) -> &mut [Slot; MAX_SOCKETS] {
        // SAFETY: holding the guard means holding the spinlock; access is exclusive.
        unsafe { &mut *REG.slots.get() }
    }
}

/// Claim a free socket id in round-robin order, or `None` if all `MAX_SOCKETS` are in use.
fn alloc_id() -> Option<u64> {
    let mut g = lock();
    // SAFETY: the cursor is only touched under the spinlock, like the slots.
    let start = unsafe { *REG.next.get() };
    let slots = g.slots();
    for step in 0..MAX_SOCKETS {
        let i = (start + step) % MAX_SOCKETS;
        if !slots[i].in_use {
            slots[i].in_use = true;
            slots[i].res_off = 0;
            slots[i].res_len = 0;
            slots[i].peer = None;
            slots[i].last_dst = None;
            // SAFETY: as above; advance so the next open prefers a different id (and port).
            unsafe { *REG.next.get() = (i + 1) % MAX_SOCKETS };
            return Some(i as u64);
        }
    }
    None
}

/// Release a socket id. `attached` stays set so the id's shared frame is reused, not re-minted.
fn free_id(id: u64) {
    let mut g = lock();
    g.slots()[id as usize].in_use = false;
}

// --- Shared-frame access ---------------------------------------------------------------------
//
// Single-threaded, one exchange at a time, so the frame needs no locking: only the running
// operation touches it between its own `CALL`s.

fn fw8(va: u64, v: u8) {
    // SAFETY: `va` is inside this socket's mapped, writable shared frame.
    unsafe { core::ptr::write_volatile(va as *mut u8, v) }
}

fn fr8(va: u64) -> u8 {
    // SAFETY: `va` is inside this socket's mapped shared frame.
    unsafe { core::ptr::read_volatile(va as *const u8) }
}

fn set_dst(id: u64, ip: Ipv4Addr, port: u16) {
    let base = frame_va(id);
    for (i, b) in ip.octets().iter().enumerate() {
        fw8(base + OFF_DST_IP + i as u64, *b);
    }
    // Port, little-endian, matching the frame header (netproto).
    fw8(base + OFF_DST_PORT, port as u8);
    fw8(base + OFF_DST_PORT + 1, (port >> 8) as u8);
}

fn write_payload(id: u64, buf: &[u8]) {
    let base = frame_va(id) + OFF_PAYLOAD;
    for (i, b) in buf.iter().enumerate() {
        fw8(base + i as u64, *b);
    }
}

fn read_payload(id: u64, off: usize, out: &mut [u8]) {
    let base = frame_va(id) + OFF_PAYLOAD + off as u64;
    for (i, b) in out.iter_mut().enumerate() {
        *b = fr8(base + i as u64);
    }
}

// --- Errors ----------------------------------------------------------------------------------

fn err_ipv6() -> io::Error {
    io::Error::UNSUPPORTED_PLATFORM
}

/// A CALL whose first reply word, read as `i64`, is negative failed at the syscall layer: an empty
/// `Stack` slot (this program was not given the network) or wrong rights. The honest answer is
/// `Unsupported`, the same as a program with no net grants at all.
fn is_syscall_err(r0: u64) -> bool {
    (r0 as i64) < 0
}

// --- Attaching a shared frame ----------------------------------------------------------------

/// Ensure socket `id` has a shared frame delegated to net_stack. Once per id and sticky: the frame and
/// its mapping outlive an open/close cycle, so a reused id does not re-map an already-mapped VA.
fn ensure_attached(id: u64) -> io::Result<()> {
    {
        let mut g = lock();
        if g.slots()[id as usize].attached {
            return Ok(());
        }
    }

    let va = frame_va(id);
    // Mint a fresh frame from the net untyped. A negative result means no untyped in slot 3, i.e.
    // this program was not endowed with the network: honestly Unsupported.
    // SAFETY: plain syscall; the kernel validates the slot and the budget.
    let frame = unsafe { rt::invoke(NET_UNTYPED, abi::untyped::RETYPE, 0, 0, 0) };
    if frame < 0 {
        return Err(io::Error::UNSUPPORTED_PLATFORM);
    }
    let frame = frame as u64;

    // Map it writable at this socket's VA; its page table comes from the same untyped.
    // SAFETY: plain syscall; the frame was just minted and is ours to map.
    if unsafe { rt::invoke(frame, abi::frame::MAP, va, 1, NET_UNTYPED) } < 0 {
        return Err(io::const_error!(io::ErrorKind::Other, "mapping the socket frame failed"));
    }

    // Delegate it (narrowed to read/write) to net_stack with the ATTACH request. A negative result
    // means no `Stack` endpoint in slot 2: Unsupported.
    // SAFETY: plain syscall; the frame carries GRANT (minted by RETYPE), narrowed here.
    if unsafe {
        rt::invoke(
            STACK,
            abi::endpoint::SEND_CAP,
            frame,
            abi::rights::READ | abi::rights::WRITE,
            req(OP_ATTACH_FRAME, id),
        )
    } < 0
    {
        return Err(io::Error::UNSUPPORTED_PLATFORM);
    }

    lock().slots()[id as usize].attached = true;
    Ok(())
}

/// Open a socket of the given kind, returning its id. Fails `Unsupported` if the network was not
/// granted, or `Other` if net_stack refused.
fn open(is_tcp: bool) -> io::Result<u64> {
    let id = alloc_id().ok_or_else(|| {
        io::const_error!(io::ErrorKind::Other, "too many open sockets (contract limit)")
    })?;
    if let Err(e) = ensure_attached(id) {
        free_id(id);
        return Err(e);
    }
    let op = if is_tcp { OP_OPEN_TCP } else { OP_OPEN_UDP };
    let (r0, _) = rt::call(STACK, req(op, id), 0);
    if is_syscall_err(r0) {
        free_id(id);
        return Err(io::Error::UNSUPPORTED_PLATFORM);
    }
    if r0 != REP_OK {
        free_id(id);
        return Err(io::const_error!(io::ErrorKind::Other, "the net server refused the open"));
    }
    Ok(id)
}

/// Close socket `id`: tell net_stack to drop it, then release the id (keeping its attached frame).
fn abandon(id: u64) {
    let _ = rt::call(STACK, req(OP_CLOSE, id), 0);
    free_id(id);
}

fn v4(addr: &SocketAddr) -> io::Result<(Ipv4Addr, u16)> {
    match addr {
        SocketAddr::V4(a) => Ok((*a.ip(), a.port())),
        SocketAddr::V6(_) => Err(err_ipv6()),
    }
}

// --- TcpStream -------------------------------------------------------------------------------

pub struct TcpStream {
    id: u64,
    peer: SocketAddr,
}

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        let mut last = None;
        for addr in addr.to_socket_addrs()? {
            match TcpStream::connect_one(&addr) {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::const_error!(io::ErrorKind::InvalidInput, "no addresses to connect to")
        }))
    }

    fn connect_one(addr: &SocketAddr) -> io::Result<TcpStream> {
        let (ip, port) = v4(addr)?;
        let id = open(true)?;
        set_dst(id, ip, port);
        let (r0, _) = rt::call(STACK, req(OP_CONNECT, id), 0);
        if is_syscall_err(r0) {
            abandon(id);
            return Err(io::Error::UNSUPPORTED_PLATFORM);
        }
        match r0 {
            CONNECT_ESTABLISHED => Ok(TcpStream { id, peer: *addr }),
            CONNECT_REFUSED => {
                abandon(id);
                Err(io::const_error!(io::ErrorKind::ConnectionRefused, "connection refused"))
            }
            _ => {
                abandon(id);
                Err(io::const_error!(io::ErrorKind::Other, "the net server could not connect"))
            }
        }
    }

    pub fn connect_timeout(_: &SocketAddr, _: Duration) -> io::Result<TcpStream> {
        // The contract is blocking-only; a bounded connect is not expressible. Honest refusal.
        unsupported()
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match t {
            None => Ok(()),
            Some(_) => unsupported(),
        }
    }

    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match t {
            None => Ok(()),
            Some(_) => unsupported(),
        }
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn peek(&self, _: &mut [u8]) -> io::Result<usize> {
        unsupported()
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // Serve any residual from a previous short read first, so a stream never drops bytes.
        {
            let mut g = lock();
            let s = &mut g.slots()[self.id as usize];
            if s.res_len > 0 {
                let n = s.res_len.min(buf.len());
                buf[..n].copy_from_slice(&s.res[s.res_off..s.res_off + n]);
                s.res_off += n;
                s.res_len -= n;
                return Ok(n);
            }
        }

        let (r0, _) = rt::call(STACK, req(OP_RECV, self.id), 0);
        if is_syscall_err(r0) {
            return Err(io::Error::UNSUPPORTED_PLATFORM);
        }
        if r0 == REP_ERR {
            return Err(io::const_error!(
                io::ErrorKind::TimedOut,
                "the net server timed out waiting for data"
            ));
        }
        let total = (r0 as usize).min(DATA_MAX);
        if total == 0 {
            return Ok(0); // peer closed: end of stream
        }
        let take = total.min(buf.len());
        read_payload(self.id, 0, &mut buf[..take]);
        if total > take {
            // Stash the rest of the segment for the next read.
            let mut g = lock();
            let s = &mut g.slots()[self.id as usize];
            let extra = total - take;
            read_payload(self.id, take, &mut s.res[..extra]);
            s.res_off = 0;
            s.res_len = extra;
        }
        Ok(take)
    }

    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let mut tmp = [0u8; 512];
        let want = cursor.capacity().min(tmp.len());
        if want == 0 {
            return Ok(());
        }
        let n = self.read(&mut tmp[..want])?;
        cursor.append(&tmp[..n]);
        Ok(())
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        for b in bufs {
            if !b.is_empty() {
                return self.read(b);
            }
        }
        Ok(0)
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let chunk = buf.len().min(DATA_MAX);
        write_payload(self.id, &buf[..chunk]);
        let (r0, _) = rt::call(STACK, req(OP_SEND, self.id), chunk as u64);
        if is_syscall_err(r0) {
            return Err(io::Error::UNSUPPORTED_PLATFORM);
        }
        if r0 == REP_ERR {
            return Err(io::const_error!(io::ErrorKind::Other, "the net server rejected the send"));
        }
        Ok((r0 as usize).min(chunk))
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        for b in bufs {
            if !b.is_empty() {
                return self.write(b);
            }
        }
        Ok(0)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer)
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        // The local address (a DHCP-assigned IP, an ephemeral port net_stack picks) is not reported
        // back across the contract. Honestly unsupported rather than fabricated.
        unsupported()
    }

    pub fn shutdown(&self, _: Shutdown) -> io::Result<()> {
        // Teardown happens once, on Drop (`OP_CLOSE`); a half-shutdown verb is not in the contract.
        Ok(())
    }

    pub fn duplicate(&self) -> io::Result<TcpStream> {
        unsupported()
    }

    pub fn set_linger(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_nodelay(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_keepalive(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn keepalive(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn ttl(&self) -> io::Result<u32> {
        Ok(64)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        if nonblocking { unsupported() } else { Ok(()) }
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        abandon(self.id);
    }
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStream").field("socket", &self.id).field("peer", &self.peer).finish()
    }
}

// --- TcpListener: the contract has LISTEN/ACCEPT (milestone 107); this PAL does not bind them yet.
// --- See the Unsupported list at the top for what binding them takes. (notes/net.md) ------------

pub struct TcpListener(!);

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(_: A) -> io::Result<TcpListener> {
        unsupported()
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.0
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        self.0
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        self.0
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        self.0
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.0
    }

    pub fn set_only_v6(&self, _: bool) -> io::Result<()> {
        self.0
    }

    pub fn only_v6(&self) -> io::Result<bool> {
        self.0
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0
    }

    pub fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        self.0
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
    }
}

// --- UdpSocket (outbound) --------------------------------------------------------------------

pub struct UdpSocket {
    id: u64,
}

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        // The requested local address is validated (must be a numeric V4) but not honored: net_stack
        // binds an ephemeral local port per socket. A program that wants a specific local port is
        // not served by this phase of the contract.
        let mut chosen = None;
        for a in addr.to_socket_addrs()? {
            if v4(&a).is_ok() {
                chosen = Some(a);
                break;
            }
        }
        if chosen.is_none() {
            return Err(err_ipv6());
        }
        let id = open(false)?;
        Ok(UdpSocket { id })
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        let mut g = lock();
        match g.slots()[self.id as usize].peer {
            Some((ip, port)) => Ok(SocketAddr::V4(SocketAddrV4::new(ip, port))),
            None => Err(io::const_error!(io::ErrorKind::NotConnected, "the socket is not connected")),
        }
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        unsupported()
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let n = self.recv(buf)?;
        // The contract's RECV does not report the datagram source, so recv_from names the peer set
        // by `connect`, else the most recent send destination (correct for request/response, the
        // pattern the resolver-less demo uses). Recorded in notes/std.md.
        let src = {
            let mut g = lock();
            let s = &g.slots()[self.id as usize];
            s.peer.or(s.last_dst)
        };
        let addr = match src {
            Some((ip, port)) => SocketAddr::V4(SocketAddrV4::new(ip, port)),
            None => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
        };
        Ok((n, addr))
    }

    pub fn peek_from(&self, _: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        unsupported()
    }

    pub fn send_to(&self, buf: &[u8], addr: &SocketAddr) -> io::Result<usize> {
        let (ip, port) = v4(addr)?;
        if buf.len() > DATA_MAX {
            return Err(io::const_error!(
                io::ErrorKind::InvalidInput,
                "datagram larger than the shared frame"
            ));
        }
        set_dst(self.id, ip, port);
        write_payload(self.id, buf);
        {
            let mut g = lock();
            g.slots()[self.id as usize].last_dst = Some((ip, port));
        }
        let (r0, _) = rt::call(STACK, req(OP_SENDTO, self.id), buf.len() as u64);
        if is_syscall_err(r0) {
            return Err(io::Error::UNSUPPORTED_PLATFORM);
        }
        if r0 != REP_OK {
            return Err(io::const_error!(io::ErrorKind::Other, "the net server rejected the send"));
        }
        Ok(buf.len())
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let peer = {
            let mut g = lock();
            g.slots()[self.id as usize].peer
        };
        match peer {
            Some((ip, port)) => self.send_to(buf, &SocketAddr::V4(SocketAddrV4::new(ip, port))),
            None => {
                Err(io::const_error!(io::ErrorKind::NotConnected, "the socket is not connected"))
            }
        }
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let (r0, _) = rt::call(STACK, req(OP_RECV, self.id), 0);
        if is_syscall_err(r0) {
            return Err(io::Error::UNSUPPORTED_PLATFORM);
        }
        if r0 == REP_ERR {
            return Err(io::const_error!(
                io::ErrorKind::TimedOut,
                "the net server timed out waiting for a datagram"
            ));
        }
        let total = (r0 as usize).min(DATA_MAX);
        // A datagram is message-oriented: a short buffer truncates it, standard UDP semantics.
        let n = total.min(buf.len());
        read_payload(self.id, 0, &mut buf[..n]);
        Ok(n)
    }

    pub fn peek(&self, _: &mut [u8]) -> io::Result<usize> {
        unsupported()
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        // A UDP "connect" only fixes a default peer; there is no handshake and no contract call.
        for a in addr.to_socket_addrs()? {
            if let Ok((ip, port)) = v4(&a) {
                lock().slots()[self.id as usize].peer = Some((ip, port));
                return Ok(());
            }
        }
        Err(err_ipv6())
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        unsupported()
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match t {
            None => Ok(()),
            Some(_) => unsupported(),
        }
    }

    pub fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match t {
            None => Ok(()),
            Some(_) => unsupported(),
        }
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_broadcast(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_multicast_loop_v4(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_multicast_ttl_v4(&self, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        Ok(1)
    }

    pub fn set_multicast_loop_v6(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn join_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn join_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn leave_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn leave_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn ttl(&self) -> io::Result<u32> {
        Ok(64)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        if nonblocking { unsupported() } else { Ok(()) }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        abandon(self.id);
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UdpSocket").field("socket", &self.id).finish()
    }
}

// --- DNS: no resolver rides the contract -----------------------------------------------------

pub struct LookupHost(!);

impl Iterator for LookupHost {
    type Item = SocketAddr;
    fn next(&mut self) -> Option<SocketAddr> {
        self.0
    }
}

pub fn lookup_host(_host: &str, _port: u16) -> io::Result<LookupHost> {
    // No name resolution over the socket contract: `ToSocketAddrs` resolves numeric addresses
    // only. A program that needs DNS does it as a plain UDP query (as the demo does). See
    // notes/std.md.
    unsupported()
}
