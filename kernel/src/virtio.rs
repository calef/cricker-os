//! virtio-mmio **enumeration**. Not a driver.
//!
//! This module reads the standardized identity registers of each virtio-mmio slot to find the
//! block device and route it to a userspace driver. It does not set up a queue, negotiate a
//! feature, or move a byte of data. That is all the driver's job, at EL0 (see the `virtio_blk`
//! role in user/src/hello.rs).
//!
//! **Why this much lives in the kernel.** Discovering which device is in which slot is bus
//! enumeration, the way firmware walks PCI: you read a device-independent ID register and hand
//! the device to whatever driver claims that ID. It is a bootstrap role, not device operation,
//! and it is the smallest amount of virtio knowledge that lets the kernel say "the block device
//! is in slot 3, its interrupt is INTID 51" without knowing the first thing about how a block
//! device works.

use crate::arch::mmu::{self, VIRTIO_IRQ_BASE, VIRTIO_MMIO_BASE};

/// One virtio-mmio slot is 0x200 bytes.
const SLOT_STRIDE: u64 = 0x200;
const SLOTS: u64 = 32;

/// "virt", little-endian, at offset 0x000 of every slot.
const MAGIC: u32 = 0x7472_6976;
/// DeviceID at offset 0x008. 0 means "empty slot"; 2 means "block device".
const DEVICE_ID_BLOCK: u32 = 2;

// Register offsets we read here. The driver knows many more; the kernel knows exactly three.
const REG_MAGIC: u64 = 0x000;
const REG_VERSION: u64 = 0x004;
const REG_DEVICE_ID: u64 = 0x008;

/// A block device found on the bus: where its registers are, and which interrupt it raises.
#[derive(Debug, Clone, Copy)]
pub struct BlockDevice {
    /// Physical address of this slot's registers. Handed to the driver as a device mapping.
    pub mmio_phys: u64,
    /// The interrupt this device raises. Handed to the driver as an `Irq` capability.
    pub intid: u32,
}

fn read_reg(slot: u64, offset: u64) -> u32 {
    let va = mmu::phys_to_virt(VIRTIO_MMIO_BASE + slot * SLOT_STRIDE + offset);
    // SAFETY: the virtio-mmio window is mapped device memory (mmu::map_everything), and these
    // offsets are within a slot. Reading an ID register has no side effect.
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

/// Scan the bus for the first virtio block device. `None` if there is no disk attached.
pub fn find_block_device() -> Option<BlockDevice> {
    for slot in 0..SLOTS {
        if read_reg(slot, REG_MAGIC) != MAGIC {
            continue; // not a virtio-mmio slot at all
        }
        if read_reg(slot, REG_DEVICE_ID) != DEVICE_ID_BLOCK {
            continue; // empty, or some other kind of device
        }

        // We require modern virtio (version 2); the register is read for the debug assertion.
        debug_assert_eq!(
            read_reg(slot, REG_VERSION),
            2,
            "expected modern virtio-mmio"
        );

        return Some(BlockDevice {
            mmio_phys: VIRTIO_MMIO_BASE + slot * SLOT_STRIDE,
            intid: VIRTIO_IRQ_BASE + slot as u32,
        });
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Milestone: DMA confinement. The kernel owns the block device's transport.
//
// The device does DMA against raw physical addresses with no IOMMU in front of it, so page-table
// permissions do not apply to it. If a userspace driver could program the queue and ring the
// device itself, it could point the device at any physical address (the kernel, another process)
// and the device would read or write it. So the kernel keeps the two DMA-critical powers — the
// queue's ring addresses and the "go" signal — and **validates that every address the device
// will touch lies within the driver's own DMA region** before letting it proceed. The driver
// still builds its own requests in its own region and reads its own results; it simply cannot
// aim the device anywhere else.
//
// This is the software stand-in for an IOMMU. It is not generic (it understands the virtqueue
// *transport*: the descriptor table and the available ring), but it knows nothing about block
// devices — the request format, sectors, and results stay in the userspace driver.
// ---------------------------------------------------------------------------------------------

use crate::sync::{IrqSafeMutex, rank};
use alloc::vec::Vec;

// virtio-mmio v2 registers the kernel drives.
const REG_DEVICE_FEATURES_SEL: u64 = 0x014;
const REG_DRIVER_FEATURES: u64 = 0x020;
const REG_DRIVER_FEATURES_SEL: u64 = 0x024;
const REG_QUEUE_SEL: u64 = 0x030;
const REG_QUEUE_NUM_MAX: u64 = 0x034;
const REG_QUEUE_NUM: u64 = 0x038;
const REG_QUEUE_READY: u64 = 0x044;
const REG_QUEUE_NOTIFY: u64 = 0x050;
const REG_INTERRUPT_ACK: u64 = 0x064;
const REG_STATUS: u64 = 0x070;
const REG_QUEUE_DESC_LOW: u64 = 0x080;
const REG_QUEUE_DESC_HIGH: u64 = 0x084;
const REG_QUEUE_DRIVER_LOW: u64 = 0x090;
const REG_QUEUE_DRIVER_HIGH: u64 = 0x094;
const REG_QUEUE_DEVICE_LOW: u64 = 0x0a0;
const REG_QUEUE_DEVICE_HIGH: u64 = 0x0a4;

/// The fixed queue layout, a contract shared with the userspace driver (user/src/virtio.rs). The
/// kernel places the rings at these offsets in the DMA region, so it always knows where they are.
pub const QSIZE: u16 = 8;
const DESC_OFF: u64 = 0x000; // 16 * QSIZE
const AVAIL_OFF: u64 = 0x080; // 6 + 2*QSIZE
const USED_OFF: u64 = 0x100; // 6 + 8*QSIZE
/// The whole ring area must fit under this; the data buffers the driver adds live above it.
const RING_END: u64 = USED_OFF + 6 + 8 * QSIZE as u64;

const VIRTQ_DESC_F_NEXT: u16 = 1;

/// One block device the kernel operates the transport for.
struct Device {
    mmio_phys: u64,
    dma_base: u64,
    dma_size: u64,
    /// The last available-ring index we have already validated and forwarded. Descriptors are
    /// only ever *added* by the driver, so we validate the new ones each notify.
    last_avail: u16,
}

static DEVICES: IrqSafeMutex<Vec<Device>> = IrqSafeMutex::new(rank::VIRTIO, Vec::new());

/// Register the block device and its DMA region with the transport. Returns its id, which is what
/// goes inside an `Object::Virtio` capability. The driver never sees the MMIO; it drives the
/// device through that capability.
pub fn register(mmio_phys: u64, dma_base: u64, dma_size: u64) -> usize {
    let mut devs = DEVICES.lock();
    devs.push(Device {
        mmio_phys,
        dma_base,
        dma_size,
        last_avail: 0,
    });
    devs.len() - 1
}

fn reg_read(mmio_phys: u64, off: u64) -> u32 {
    // SAFETY: the virtio-mmio window is mapped device memory (mmu::map_everything); off is a v2
    // register within a slot.
    unsafe { core::ptr::read_volatile(mmu::phys_to_virt(mmio_phys + off) as *const u32) }
}
fn reg_write(mmio_phys: u64, off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(mmu::phys_to_virt(mmio_phys + off) as *mut u32, v) }
}

/// Read `n` bytes from a physical address in the DMA region, through the direct map. Used by the
/// validator to walk the driver's descriptor table and available ring.
fn dma_read16(phys: u64) -> u16 {
    unsafe { core::ptr::read_volatile(mmu::phys_to_virt(phys) as *const u16) }
}
fn dma_read64(phys: u64) -> u64 {
    unsafe { core::ptr::read_volatile(mmu::phys_to_virt(phys) as *const u64) }
}

/// **The security-critical check.** Given a descriptor-table base and an available-ring base in
/// the DMA region, validate that every descriptor reachable from the newly-available heads
/// (`from_idx .. to_idx`) points entirely within `[dma_base, dma_base + dma_size)`. Returns the
/// new `last_avail` on success, or `None` if any descriptor escapes the region (or the chain is
/// malformed).
///
/// Written to take the ring/desc *physical addresses* and a `read16`/`read64` pair, so the same
/// logic can be exercised by a test that builds a fake region in ordinary memory.
fn validate_avail(
    dma_base: u64,
    dma_size: u64,
    desc_phys: u64,
    avail_phys: u64,
    from_idx: u16,
    to_idx: u16,
    read16: &dyn Fn(u64) -> u16,
    read64: &dyn Fn(u64) -> u64,
) -> bool {
    let in_region = |addr: u64, len: u64| -> bool {
        // No overflow, and both ends inside the region.
        match addr.checked_add(len) {
            Some(end) => addr >= dma_base && end <= dma_base + dma_size,
            None => false,
        }
    };

    // avail: { u16 flags; u16 idx; u16 ring[QSIZE]; }
    let mut idx = from_idx;
    while idx != to_idx {
        let slot = (idx % QSIZE) as u64;
        let head = read16(avail_phys + 4 + slot * 2);
        if head >= QSIZE {
            return false; // head index out of the descriptor table
        }

        // Walk the chain. Bounded by QSIZE, so a malicious `next` cycle cannot loop forever.
        // desc[d] = { u64 addr @0; u32 len @8; u16 flags @12; u16 next @14 }.
        let mut d = head;
        for _ in 0..QSIZE {
            let base = desc_phys + d as u64 * 16;
            let addr = read64(base);
            let len = (read64(base + 8) & 0xffff_ffff) as u64; // the u32 len, low half of a word
            if !in_region(addr, len) {
                return false; // a descriptor points outside the driver's region
            }
            let flags = read16(base + 12);
            if flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            d = read16(base + 14);
            if d >= QSIZE {
                return false;
            }
        }
        idx = idx.wrapping_add(1);
    }
    true
}

/// Errors the transport can return to the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// No such device id.
    NoDevice,
    /// The queue does not fit in the DMA region, or QUEUE_NUM_MAX is too small.
    BadQueue,
    /// **A descriptor pointed outside the driver's DMA region.** The device was NOT told to go.
    DmaEscape,
}

/// Read a device register the driver is allowed to see (status, features, interrupt status).
pub fn read_register(id: usize, off: u64) -> Option<u32> {
    let devs = DEVICES.lock();
    let dev = devs.get(id)?;
    Some(reg_read(dev.mmio_phys, off))
}

/// Write one of the DMA-*safe* registers (status, features selection, interrupt ack). Refuses the
/// DMA-critical ones (queue addresses, notify), which have their own validated paths.
pub fn write_register(id: usize, off: u64, val: u32) -> Result<(), TransportError> {
    // Only these offsets are safe to pass straight through. Everything to do with queue setup or
    // notification goes through setup_queue / notify, which validate.
    const SAFE: &[u64] = &[
        REG_STATUS,
        REG_DEVICE_FEATURES_SEL,
        REG_DRIVER_FEATURES_SEL,
        REG_DRIVER_FEATURES,
        REG_INTERRUPT_ACK,
    ];
    if !SAFE.contains(&off) {
        return Err(TransportError::BadQueue);
    }
    let devs = DEVICES.lock();
    let dev = devs.get(id).ok_or(TransportError::NoDevice)?;
    reg_write(dev.mmio_phys, off, val);
    Ok(())
}

/// Set up queue 0 with `num` entries, placing the rings at the fixed offsets in the DMA region.
/// The kernel programs the ring addresses, so the device's ring bases are always inside the
/// region — the driver never gets to choose them.
pub fn setup_queue(id: usize, num: u16) -> Result<(), TransportError> {
    let devs = DEVICES.lock();
    let dev = devs.get(id).ok_or(TransportError::NoDevice)?;

    if num == 0 || num > QSIZE || dev.dma_size < RING_END {
        return Err(TransportError::BadQueue);
    }
    reg_write(dev.mmio_phys, REG_QUEUE_SEL, 0);
    if (reg_read(dev.mmio_phys, REG_QUEUE_NUM_MAX) as u16) < num {
        return Err(TransportError::BadQueue);
    }
    reg_write(dev.mmio_phys, REG_QUEUE_NUM, num as u32);

    let desc = dev.dma_base + DESC_OFF;
    let avail = dev.dma_base + AVAIL_OFF;
    let used = dev.dma_base + USED_OFF;
    reg_write(dev.mmio_phys, REG_QUEUE_DESC_LOW, desc as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_DESC_HIGH, (desc >> 32) as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_DRIVER_LOW, avail as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_DEVICE_LOW, used as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
    reg_write(dev.mmio_phys, REG_QUEUE_READY, 1);
    Ok(())
}

/// **The validated "go".** Validate the descriptor chains the driver has newly published, and
/// only if all of them stay within the driver's DMA region, ring the device. If any escapes, the
/// device is NOT notified and the driver gets `DmaEscape`.
pub fn notify(id: usize) -> Result<(), TransportError> {
    let mut devs = DEVICES.lock();
    let dev = devs.get_mut(id).ok_or(TransportError::NoDevice)?;

    let desc = dev.dma_base + DESC_OFF;
    let avail = dev.dma_base + AVAIL_OFF;
    let to_idx = dma_read16(avail + 2); // avail.idx

    let ok = validate_avail(
        dev.dma_base,
        dev.dma_size,
        desc,
        avail,
        dev.last_avail,
        to_idx,
        &|p| dma_read16(p),
        &|p| dma_read64(p),
    );
    if !ok {
        return Err(TransportError::DmaEscape);
    }
    dev.last_avail = to_idx;

    reg_write(dev.mmio_phys, REG_QUEUE_NOTIFY, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a one-page fake DMA region in kernel memory and exercise `validate_avail` directly.
    /// Proves the security-critical check: a descriptor pointing OUTSIDE the region is refused,
    /// one inside is accepted, and a `next`-cycle cannot hang the validator.
    #[test_case]
    fn the_validator_refuses_a_descriptor_that_escapes_the_dma_region() {
        let frame = crate::memory::alloc().expect("no frame").addr();
        let base = frame; // treat the frame as the "DMA region" (physical address)
        let size = frames::FRAME_SIZE;

        let desc = base + DESC_OFF;
        let avail = base + AVAIL_OFF;
        let w16 = |phys: u64, v: u16| unsafe {
            core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u16, v)
        };
        let w64 = |phys: u64, v: u64| unsafe {
            core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u64, v)
        };
        let write_desc = |i: u64, addr: u64, len: u32, flags: u16, next: u16| {
            let d = desc + i * 16;
            w64(d, addr);
            w64(d + 8, len as u64); // len (u32) in the low half; high half unused here
            w16(d + 12, flags);
            w16(d + 14, next);
        };

        let read16 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u16) };
        let read64 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u64) };

        // --- a GOOD chain: header + data + status, all inside the region ---
        write_desc(0, base + 0x200, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(1, base + 0x400, 512, VIRTQ_DESC_F_NEXT, 2);
        write_desc(2, base + 0x600, 1, 0, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1
        assert!(
            validate_avail(base, size, desc, avail, 0, 1, &read16, &read64),
            "a chain wholly inside the region was rejected",
        );

        // --- the ATTACK: descriptor 1 points at kernel memory (the kernel image) ---
        write_desc(1, 0xffff_0000_4008_0000, 512, VIRTQ_DESC_F_NEXT | 2, 2);
        assert!(
            !validate_avail(base, size, desc, avail, 0, 1, &read16, &read64),
            "a descriptor pointing at kernel memory was NOT refused",
        );

        // --- a length that overflows the region by one byte ---
        write_desc(1, base + size - 256, 512, VIRTQ_DESC_F_NEXT | 2, 2);
        assert!(
            !validate_avail(base, size, desc, avail, 0, 1, &read16, &read64),
            "a descriptor running past the end of the region was NOT refused",
        );

        // --- a next-pointer cycle must terminate, not hang ---
        write_desc(0, base + 0x200, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(1, base + 0x400, 16, VIRTQ_DESC_F_NEXT, 0); // 1 -> 0 -> 1 -> ...
        // (This is a valid-address cycle; the QSIZE bound is what stops the walk.)
        assert!(
            validate_avail(base, size, desc, avail, 0, 1, &read16, &read64),
            "a bounded cyclic chain with valid addresses should still validate (and terminate)",
        );

        crate::memory::free(frames::Frame::from_addr(frame));
    }
}
