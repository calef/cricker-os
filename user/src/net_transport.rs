//! A virtio-net transport for smoltcp, at EL0, behind the multi-queue DMA confinement.
//!
//! This is the driver half of the net server (milestone 30, piece 3): it brings the NIC up through
//! the confined `Virtio` capability, manages the receive and transmit rings, and presents smoltcp's
//! `phy::Device` interface. smoltcp does the TCP/IP; this file only moves Ethernet frames across the
//! virtqueue, the same confinement the disk and the by-hand net driver use.
//!
//! Everything the device DMAs stays inside the one DMA page the spawn service maps, so the kernel's
//! validator confines it exactly as it does the disk. The page is small, so the MTU is small
//! (`MTU`); a demonstrator with a single-page DMA region cannot post full 1514-byte buffers, and
//! that is a recorded caveat, not a bug (see notes/net.md).

use alloc::vec;
use alloc::vec::Vec;

use abi::{irq, virtio};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use user_rt::invoke;

/// The DMA page's virtual address, matching the kernel's `net_server` mapping.
const DMA_VA: u64 = 0x0000_0000_0090_0000;

/// Capability slots the kernel granted, by convention.
const IRQ: u64 = 1;
const VIRTIO: u64 = 2;

// virtio-mmio register offsets (the transport seam speaks this vocabulary on both buses).
const MAGIC: u64 = 0x000;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;

const S_ACKNOWLEDGE: u32 = 1;
const S_DRIVER: u32 = 2;
const S_DRIVER_OK: u32 = 4;
const S_FEATURES_OK: u32 = 8;
const F_VERSION_1_HI: u32 = 1; // feature bit 32
const F_ACCESS_PLATFORM_HI: u32 = 1 << 1; // feature bit 33 (behind an IOMMU)

const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Queue size and the two queues, matching the kernel's contract (receive = 0, transmit = 1).
const QSIZE: u16 = 8;
const RX_Q: u64 = 0;
const TX_Q: u64 = 1;

/// The 12-byte modern virtio-net header in front of every frame (`VIRTIO_F_VERSION_1` negotiated).
const NET_HDR_LEN: u64 = 12;

// The DMA-page layout: each queue's rings at the kernel's per-queue stride (RING_BLOCK = 0x200,
// desc/avail/used at +0x000/+0x080/+0x100), then the frame buffers. All within the one 4 KiB page.
const RX_DESC: u64 = 0x000;
const RX_AVAIL: u64 = 0x080;
const RX_USED: u64 = 0x100;
const TX_DESC: u64 = 0x200;
const TX_AVAIL: u64 = 0x280;
const TX_USED: u64 = 0x300;

/// How many receive and transmit buffers fit, and how big each is. Two of each keeps a spare in
/// flight; 0x2C0 bytes holds a small frame plus the virtio header.
const RX_BUFS: usize = 2;
const TX_BUFS: usize = 2;
const BUF: u64 = 0x2C0;
const BUF_BASE: u64 = 0x400;

/// The interface MTU. Small because the buffers are small (single-page DMA region); slirp's DHCP,
/// DNS, and small TCP segments fit. A recorded demonstrator caveat, not a protocol limit.
pub const MTU: usize = 576;

fn rx_buf(i: usize) -> u64 {
    BUF_BASE + i as u64 * BUF
}
fn tx_buf(i: usize) -> u64 {
    BUF_BASE + (RX_BUFS as u64 + i as u64) * BUF
}

// Raw DMA-page and device-register access. The DMA page is one contiguous physical frame mapped at
// DMA_VA, so a physical address the device needs is `dma_phys + offset`.
fn r8(off: u64) -> u8 {
    // SAFETY: `DMA_VA` is the base of the single contiguous DMA frame this process mapped at startup, and callers pass offsets inside it. Volatile because the device reads and writes the same memory.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u8) }
}
fn r16(off: u64) -> u16 {
    // SAFETY: `DMA_VA` is the base of the single contiguous DMA frame this process mapped at startup, and callers pass offsets inside it. Volatile because the device reads and writes the same memory.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u16) }
}
fn r32(off: u64) -> u32 {
    // SAFETY: `DMA_VA` is the base of the single contiguous DMA frame this process mapped at startup, and callers pass offsets inside it. Volatile because the device reads and writes the same memory.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u32) }
}
fn w8(off: u64, v: u8) {
    // SAFETY: `DMA_VA` is the base of the single contiguous DMA frame this process mapped at startup, and callers pass offsets inside it. Volatile because the device reads and writes the same memory.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u8, v) }
}
fn w16(off: u64, v: u16) {
    // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
    // before acting (user_rt's contract). A caller cannot break an invariant by passing a
    // bad slot or method; it gets an error back.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u16, v) }
}

fn mr(off: u64) -> u32 {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(VIRTIO, virtio::READ_REG, off, 0, 0) as u32 }
}
fn mw(off: u64, v: u32) {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe {
        invoke(VIRTIO, virtio::WRITE_REG, off, v as u64, 0);
    }
}

fn barrier() {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a barrier: no operands, and the options say it touches neither memory nor the
    // stack, so it cannot break an invariant. It orders the descriptor writes against the
    // device's reads, which is the whole reason this function exists.
    unsafe {
        core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: the RISC-V twin of the barrier above. `fence` carries no `nomem`, deliberately:
    // ordering memory is precisely its job.
    unsafe {
        core::arch::asm!("fence", options(nostack, preserves_flags))
    };
}

fn write_desc(desc_base: u64, i: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let b = desc_base + i * 16;
    // SAFETY: `DMA_VA` is the base of the single contiguous DMA frame this process mapped at startup, and callers pass offsets inside it. Volatile because the device reads and writes the same memory.
    unsafe {
        core::ptr::write_volatile((DMA_VA + b) as *mut u64, addr);
        core::ptr::write_volatile((DMA_VA + b + 8) as *mut u32, len);
        core::ptr::write_volatile((DMA_VA + b + 12) as *mut u16, flags);
        core::ptr::write_volatile((DMA_VA + b + 14) as *mut u16, next);
    }
}

/// The virtio-net device, presenting smoltcp's `Device`. Holds the ring bookkeeping; the buffers
/// and descriptor tables live in the DMA page at fixed offsets.
pub struct VirtioNet {
    dma_phys: u64,
    rx_avail: u16,
    rx_seen: u16, // receive used-ring index already drained
    tx_avail: u16,
    tx_next: usize,
}

impl VirtioNet {
    /// Bring the NIC up: the modern handshake, both queues through the kernel, and every receive
    /// buffer posted so the device has somewhere to put inbound frames from the first instant.
    pub fn bring_up(dma_phys: u64) -> VirtioNet {
        assert_eq!(mr(MAGIC), 0x7472_6976, "not a virtio device");

        mw(STATUS, 0);
        mw(STATUS, S_ACKNOWLEDGE);
        mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

        mw(DRIVER_FEATURES_SEL, 0);
        mw(DRIVER_FEATURES, 0); // no low-word net features (no MRG_RXBUF, no offloads)

        mw(DEVICE_FEATURES_SEL, 1);
        let dev_hi = mr(DEVICE_FEATURES);
        let mut ack_hi = F_VERSION_1_HI;
        if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
            ack_hi |= F_ACCESS_PLATFORM_HI; // behind an IOMMU
        }
        mw(DRIVER_FEATURES_SEL, 1);
        mw(DRIVER_FEATURES, ack_hi);

        mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
        assert!(mr(STATUS) & S_FEATURES_OK != 0, "FEATURES_OK stuck off");

        // Set up both queues through the kernel; it programs each queue's ring addresses.
        assert_eq!(
            // SAFETY: as above: the kernel validates the capability and the method.
            unsafe { invoke(VIRTIO, virtio::SETUP_QUEUE, QSIZE as u64, RX_Q, 0) },
            0,
        );
        assert_eq!(
            // SAFETY: as above: the kernel validates the capability and the method.
            unsafe { invoke(VIRTIO, virtio::SETUP_QUEUE, QSIZE as u64, TX_Q, 0) },
            0,
        );

        mw(
            STATUS,
            S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
        );

        let mut dev = VirtioNet {
            dma_phys,
            rx_avail: 0,
            rx_seen: 0,
            tx_avail: 0,
            tx_next: 0,
        };

        // A permanent descriptor per receive buffer, then publish them all as available.
        for i in 0..RX_BUFS {
            write_desc(
                RX_DESC,
                i as u64,
                dev.dma_phys + rx_buf(i),
                BUF as u32,
                VIRTQ_DESC_F_WRITE,
                0,
            );
        }
        for i in 0..RX_BUFS {
            let slot = (dev.rx_avail % QSIZE) as u64;
            w16(RX_AVAIL + 4 + slot * 2, i as u16);
            dev.rx_avail = dev.rx_avail.wrapping_add(1);
        }
        barrier();
        w16(RX_AVAIL + 2, dev.rx_avail);
        barrier();
        dev.notify(RX_Q);
        dev
    }

    fn notify(&self, q: u64) {
        // SAFETY: `svc`; the kernel validates the queue's newly-published descriptors.
        unsafe {
            invoke(VIRTIO, virtio::NOTIFY, q, 0, 0);
        }
    }

    /// Quiet the device's interrupt and re-enable the line at the controller, after a `WAIT`
    /// returned. The server calls this each wakeup, the disk driver's discipline.
    pub fn ack_irq(&self) {
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: `svc`; re-enable the interrupt the kernel masked when it fired.
        unsafe {
            invoke(IRQ, irq::ACK, 0, 0, 0);
        }
    }

    /// Take one received frame, if the receive used ring has advanced. Copies the frame out (minus
    /// the virtio header) and re-posts the buffer so the device can fill it again.
    fn rx_take(&mut self) -> Option<Vec<u8>> {
        let used_idx = r16(RX_USED + 2);
        if used_idx == self.rx_seen {
            return None;
        }
        // used ring element: { u32 id; u32 len } at RX_USED + 4 + slot*8.
        let slot = (self.rx_seen % QSIZE) as u64;
        let id = r32(RX_USED + 4 + slot * 8) as usize; // descriptor head = buffer index
        let total = r32(RX_USED + 4 + slot * 8 + 4) as u64; // bytes written incl the virtio header

        // **Both of those are 32-bit values the DEVICE wrote, and neither is ours to trust**
        // (notes/shared-page-audit.md, finding 6). The IOMMU and `crates/dma_validator` confine
        // where the device may *touch*; they say nothing about what it may *say*, and the used
        // ring is inside this driver's own DMA page, which the device is entitled to write.
        //
        // An unchecked `id` walks `rx_buf(id) = 0x400 + id * 0x2C0` clean out of the one-page DMA
        // region: `id = 4` is already past it, and `id` near 1.5 million lands on this process's
        // heap, so a lying device makes us copy our own memory into a frame and hand it to
        // smoltcp, which may put it on the wire. An unchecked `total` reads past the buffer and
        // asks the allocator for up to 4 GiB. Neither needs a race; the device simply lies once.
        //
        // Fail closed: a completion naming a buffer we never posted is consumed and dropped, and
        // one claiming more bytes than a buffer holds is truncated to the buffer. `entropy.rs`
        // already clamps its length this way; this driver and `kbd.rs` were the two that did not.
        //
        // The dropped completion's buffer is NOT re-posted, because a bogus `id` does not say
        // which buffer it was. That costs a receive buffer per lie, which is the right trade: a
        // device that lies has already stopped being a network card.
        if id >= RX_BUFS {
            self.rx_seen = self.rx_seen.wrapping_add(1);
            return None;
        }
        let frame_len = total.saturating_sub(NET_HDR_LEN).min(BUF - NET_HDR_LEN);

        let base = rx_buf(id) + NET_HDR_LEN;
        let mut v = vec![0u8; frame_len as usize];
        for (k, byte) in v.iter_mut().enumerate() {
            *byte = r8(base + k as u64);
        }

        self.rx_seen = self.rx_seen.wrapping_add(1);
        // Re-post this buffer (its descriptor is permanent) so the device reuses it.
        let aslot = (self.rx_avail % QSIZE) as u64;
        w16(RX_AVAIL + 4 + aslot * 2, id as u16);
        barrier();
        self.rx_avail = self.rx_avail.wrapping_add(1);
        w16(RX_AVAIL + 2, self.rx_avail);
        barrier();
        self.notify(RX_Q);
        Some(v)
    }

    /// Transmit one frame: prepend the zero virtio header, post the next transmit buffer's
    /// descriptor, and ring the device. Waits briefly for a buffer to free if all are in flight.
    fn tx_send(&mut self, frame: &[u8]) {
        let i = self.tx_next;
        // Do not overwrite a buffer the device has not finished sending. Under slirp this never
        // actually spins, but a bounded wait keeps a slow completion from corrupting an in-flight
        // frame. The transmit used ring advances once per consumed descriptor.
        if self.tx_avail.wrapping_sub(r16(TX_USED + 2)) >= TX_BUFS as u16 {
            for _ in 0..100_000 {
                if self.tx_avail.wrapping_sub(r16(TX_USED + 2)) < TX_BUFS as u16 {
                    break;
                }
                core::hint::spin_loop();
            }
        }

        let buf = tx_buf(i);
        for k in 0..NET_HDR_LEN {
            w8(buf + k, 0);
        }
        for (k, &byte) in frame.iter().enumerate() {
            w8(buf + NET_HDR_LEN + k as u64, byte);
        }
        write_desc(
            TX_DESC,
            i as u64,
            self.dma_phys + buf,
            (NET_HDR_LEN + frame.len() as u64) as u32,
            0, // device reads the buffer (transmit)
            0,
        );
        let slot = (self.tx_avail % QSIZE) as u64;
        w16(TX_AVAIL + 4 + slot * 2, i as u16);
        barrier();
        self.tx_avail = self.tx_avail.wrapping_add(1);
        w16(TX_AVAIL + 2, self.tx_avail);
        barrier();
        self.notify(TX_Q);
        self.tx_next = (self.tx_next + 1) % TX_BUFS;
    }
}

impl Device for VirtioNet {
    type RxToken<'a> = VnetRxToken;
    type TxToken<'a> = VnetTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(VnetRxToken, VnetTxToken)> {
        let frame = self.rx_take()?;
        Some((VnetRxToken { frame }, VnetTxToken { dev: self }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<VnetTxToken> {
        Some(VnetTxToken { dev: self })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps
    }
}

/// Holds the received frame by value, so it never borrows the device.
pub struct VnetRxToken {
    frame: Vec<u8>,
}
impl phy::RxToken for VnetRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame)
    }
}

/// Carries a raw pointer to the device rather than a borrow, so a receive token and a transmit
/// token can coexist (both are returned from one `&mut self` call). `net_stack` is single-threaded, and
/// the device outlives any token within a poll, so the deref in `consume` is sound.
pub struct VnetTxToken {
    dev: *mut VirtioNet,
}
impl phy::TxToken for VnetTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        // SAFETY: single-threaded; the device outlives this token, and the `&mut self` borrow that
        // produced the token has ended (the token is an owned value).
        unsafe {
            (*self.dev).tx_send(&buf);
        }
        r
    }
}
