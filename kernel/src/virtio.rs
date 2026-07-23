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
/// Bit 2: this descriptor points at a **table of further descriptors** instead of a buffer. The
/// validator walks the flat chain and never follows that inner table, so a descriptor carrying
/// this flag would send the device to addresses we never checked. The kernel negotiates the
/// feature that enables it off (see `sanitize_driver_features`) *and* refuses the flag here, so the
/// confinement fails closed if that negotiation ever regresses.
const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// One block device the kernel operates the transport for.
struct Device {
    mmio_phys: u64,
    dma_base: u64,
    dma_size: u64,
    /// The last available-ring index we have already validated and forwarded. Descriptors are
    /// only ever *added* by the driver, so we validate the new ones each notify.
    last_avail: u16,
    /// Which 32-bit word of the feature bits the driver's next `DRIVER_FEATURES` write targets
    /// (`DRIVER_FEATURES_SEL`: 0 = features 0..31, 1 = 32..63). Tracked so a feature write can have
    /// the ring-layout features the validator cannot police stripped from whichever word carries
    /// them. See `sanitize_driver_features`.
    driver_features_sel: u32,
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
        driver_features_sel: 0,
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
    // At most QSIZE descriptors can be newly available since the last validation: the available
    // ring has only QSIZE slots, so a driver cannot have published more than that without the
    // device consuming some. A larger jump in avail.idx is malformed or hostile, and walking it
    // would spin this loop up to 65535 times under the caller's lock, with interrupts masked.
    // Refuse it before touching a single descriptor. (wrapping_sub because avail.idx wraps at u16.)
    if to_idx.wrapping_sub(from_idx) > QSIZE {
        return false;
    }

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
            // An indirect descriptor points at a table of descriptors this walk never validates, so
            // the device would follow it out of the region. Refuse it. The feature is negotiated off
            // as well; this is the belt to that suspenders, and it costs one branch.
            if flags & VIRTQ_DESC_F_INDIRECT != 0 {
                return false;
            }
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
    let mut devs = DEVICES.lock();
    let dev = devs.get_mut(id).ok_or(TransportError::NoDevice)?;

    // Feature negotiation is a two-step dance: the driver selects a 32-bit word with
    // `DRIVER_FEATURES_SEL`, then writes that word with `DRIVER_FEATURES`. We remember the selector
    // so the write can have the ring-layout features the validator cannot police stripped from
    // whichever word carries them, before the device ever sees the value.
    let val = match off {
        REG_DRIVER_FEATURES_SEL => {
            dev.driver_features_sel = val;
            val
        }
        REG_DRIVER_FEATURES => sanitize_driver_features(dev.driver_features_sel, val),
        _ => val,
    };

    reg_write(dev.mmio_phys, off, val);
    Ok(())
}

/// Strip the ring-layout features the descriptor validator cannot police from a `DRIVER_FEATURES`
/// word. `sel` is the word the driver selected: 0 = features 0..31, 1 = features 32..63.
///
/// Two features change **what the device reads descriptors from**, which is exactly the thing
/// `validate_avail` assumes it controls:
///
/// - **`INDIRECT_DESC`** (bit 28, low word): a descriptor may point at a table of further
///   descriptors. The validator walks the flat chain and never follows that table, so the inner
///   descriptors reach the device unchecked.
/// - **`RING_PACKED`** (bit 34, high word): the entire ring format changes. The validator
///   understands only the split ring, so a packed ring would be read by the device and validated by
///   nobody.
///
/// Forcing both off keeps every descriptor the device ever sees on the split-ring path the
/// validator actually covers. The honest driver negotiates neither, so nothing legitimate breaks.
/// The real cure (a shadow descriptor ring the driver cannot write) is the shape validation should
/// take; until then this closes the reachable bypass. See notes/virtio.md.
fn sanitize_driver_features(sel: u32, val: u32) -> u32 {
    const F_INDIRECT_DESC_LO: u32 = 1 << 28; // feature bit 28
    const F_RING_PACKED_HI: u32 = 1 << (34 - 32); // feature bit 34
    match sel {
        0 => val & !F_INDIRECT_DESC_LO,
        1 => val & !F_RING_PACKED_HI,
        _ => val,
    }
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

    /// **An indirect descriptor is refused even when it points inside the region.**
    ///
    /// This is the bypass the direct-descriptor attack test does not cover: a descriptor flagged
    /// `INDIRECT` points at a *table* of further descriptors, and the validator never walks that
    /// table. So the inner descriptors, which the device WILL follow, are unchecked. A wholly
    /// in-region indirect descriptor still has to be refused, because it is not the descriptor the
    /// device ultimately acts on.
    #[test_case]
    fn the_validator_refuses_an_indirect_descriptor() {
        let frame = crate::memory::alloc().expect("no frame").addr();
        let base = frame;
        let size = frames::FRAME_SIZE;

        let desc = base + DESC_OFF;
        let avail = base + AVAIL_OFF;
        let w16 =
            |phys: u64, v: u16| unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u16, v) };
        let w64 =
            |phys: u64, v: u64| unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u64, v) };
        let read16 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u16) };
        let read64 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u64) };

        // desc[0]: a legal in-region address and length, but flagged INDIRECT. The device would
        // read the "table" at that address and follow ITS descriptors anywhere; we do not walk it,
        // so the only safe answer is no.
        w64(desc, base + 0x200); // addr: inside the region
        w64(desc + 8, 128); // len: 128 bytes (8 inner descriptors), inside the region
        w16(desc + 12, VIRTQ_DESC_F_INDIRECT);
        w16(desc + 14, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1

        assert!(
            !validate_avail(base, size, desc, avail, 0, 1, &read16, &read64),
            "an indirect descriptor was accepted: the device could follow its unvalidated table \
             out of the DMA region",
        );

        crate::memory::free(frames::Frame::from_addr(frame));
    }

    /// **Feature negotiation strips the ring-layout features the validator cannot police.**
    ///
    /// A driver that asks for `INDIRECT_DESC` (bit 28) or `RING_PACKED` (bit 34) gets that bit
    /// cleared before the device sees it, so the device never honours a descriptor format the
    /// validator does not understand. Every other bit passes through untouched, so real device
    /// features still negotiate.
    #[test_case]
    fn feature_negotiation_strips_indirect_and_packed() {
        // Low word (sel 0): INDIRECT_DESC at bit 28 is cleared; unrelated bits survive.
        let asked_lo = (1 << 28) | (1 << 5) | 1; // indirect + two blk feature bits
        let got_lo = sanitize_driver_features(0, asked_lo);
        assert_eq!(got_lo & (1 << 28), 0, "INDIRECT_DESC was not stripped");
        assert_eq!(got_lo, (1 << 5) | 1, "a non-ring feature bit was disturbed");

        // High word (sel 1): RING_PACKED is feature bit 34, i.e. bit 2 of the high word.
        let asked_hi = (1 << 2) | 1; // packed + VERSION_1 (bit 32 = high-word bit 0)
        let got_hi = sanitize_driver_features(1, asked_hi);
        assert_eq!(got_hi & (1 << 2), 0, "RING_PACKED was not stripped");
        assert_eq!(got_hi & 1, 1, "VERSION_1 must survive negotiation");
    }

    /// **A jump in `avail.idx` larger than the ring is refused, not walked.**
    ///
    /// The available ring holds only `QSIZE` slots, so at most `QSIZE` descriptors can be newly
    /// available between notifies. A hostile driver that advances `avail.idx` by tens of thousands
    /// would otherwise make the validator loop that many times, under the `DEVICES` lock with
    /// interrupts masked. Every descriptor and ring slot here is individually valid, so the ONLY
    /// thing that can refuse the oversized batch is the jump-size guard.
    #[test_case]
    fn the_validator_refuses_more_new_entries_than_the_ring_can_hold() {
        let frame = crate::memory::alloc().expect("no frame").addr();
        let base = frame;
        let size = frames::FRAME_SIZE;

        let desc = base + DESC_OFF;
        let avail = base + AVAIL_OFF;
        let w16 =
            |phys: u64, v: u16| unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u16, v) };
        let w64 =
            |phys: u64, v: u64| unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u64, v) };
        let read16 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u16) };
        let read64 = |p: u64| unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u64) };

        // Fill the whole ring with individually valid single-descriptor entries: desc[i] is an
        // in-region 8-byte buffer, ring[i] = head i. With no size guard the walk would accept them.
        for i in 0..QSIZE as u64 {
            let d = desc + i * 16;
            w64(d, base + 0x200 + i * 8); // addr: in-region
            w64(d + 8, 8); // len: 8 bytes, in-region
            w16(d + 12, 0); // flags: no NEXT, no INDIRECT
            w16(d + 14, 0);
            w16(avail + 4 + i * 2, i as u16); // ring[i] = head i
        }

        // Exactly QSIZE new entries is legal: the ring holds that many.
        assert!(
            validate_avail(base, size, desc, avail, 0, QSIZE, &read16, &read64),
            "a batch of exactly QSIZE valid entries was refused",
        );

        // One more than the ring can hold is refused, though every descriptor is valid.
        assert!(
            !validate_avail(base, size, desc, avail, 0, QSIZE + 1, &read16, &read64),
            "a jump of QSIZE+1 was walked instead of refused: a hostile avail.idx could spin the \
             validator up to 65535 times with interrupts masked",
        );

        crate::memory::free(frames::Frame::from_addr(frame));
    }
}
