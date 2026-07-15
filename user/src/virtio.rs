//! A virtio-blk driver. **At EL0.**
//!
//! This is milestone 9's headline: a real block device, driven by an unprivileged process. The
//! kernel handed us three things and knows nothing else about this device:
//!
//! - a mapping of the device's registers (`MMIO_VA`), as device memory,
//! - a DMA page (`DMA_VA`), whose *physical* address arrived in `x1`, because descriptors speak
//!   physical addresses and a process only knows virtual ones,
//! - an `Irq` capability (slot 1), so the device's interrupt can reach us as a message.
//!
//! Everything about how a virtio block device actually works lives here, in userspace. If any of
//! it is wrong, this process faults, and the kernel does not.
//!
//! The transport is **modern virtio-mmio (version 2)**: separate physical addresses for the
//! descriptor table and the two rings, negotiated through the registers below. See the virtio
//! 1.x spec, sections 4.2 (MMIO) and 5.2 (block).

use crate::{check, invoke, send};
use abi::irq;

// The kernel maps these at fixed user VAs (must match kernel/src/user.rs virtio_service). The
// device's registers sit at a sub-page offset within the mapped page; the kernel passes that
// offset in, and `run` folds it into the base once.
const MMIO_PAGE_VA: u64 = 0x0000_0000_0080_0000;
const DMA_VA: u64 = 0x0000_0000_0090_0000;

/// The slot base, set once at the top of `run`. A driver only ever talks to one device.
static mut MMIO_VA: u64 = 0;

/// Capability slots the kernel handed us, by convention.
const REPORT: u64 = 0; // SEND: report the result back to the kernel
const IRQ: u64 = 1; // WAIT/ACK the device interrupt

// --- virtio-mmio v2 register offsets (bytes from the slot base) ---
const MAGIC: u64 = 0x000;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_DRIVER_LOW: u64 = 0x090;
const QUEUE_DRIVER_HIGH: u64 = 0x094;
const QUEUE_DEVICE_LOW: u64 = 0x0a0;
const QUEUE_DEVICE_HIGH: u64 = 0x0a4;

// Status bits.
const S_ACKNOWLEDGE: u32 = 1;
const S_DRIVER: u32 = 2;
const S_DRIVER_OK: u32 = 4;
const S_FEATURES_OK: u32 = 8;

// Feature bit 32: VIRTIO_F_VERSION_1. Mandatory for a modern device.
const F_VERSION_1_HI: u32 = 1; // bit 32 lives in the high 32-bit word

// The virtqueue we build. Small: one request in flight is all a demo needs.
const QSIZE: usize = 8;

// Descriptor flags.
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2; // device writes (i.e. our read buffer)

// blk request types.
const VIRTIO_BLK_T_IN: u32 = 0; // read

// --- our DMA layout, offsets within the one DMA page ---
const OFF_DESC: u64 = 0x000; // 16 * QSIZE = 128 bytes
const OFF_AVAIL: u64 = 0x080; // 6 + 2*QSIZE
const OFF_USED: u64 = 0x100; // 6 + 8*QSIZE
const OFF_HEADER: u64 = 0x200; // 16-byte blk request header
const OFF_DATA: u64 = 0x400; // 512-byte block buffer
const OFF_STATUS: u64 = 0x600; // 1-byte status

const BLOCK: usize = 512;

fn base() -> u64 {
    // SAFETY: written once at the top of `run`, before any register access, single-threaded.
    unsafe { MMIO_VA }
}
fn mr(off: u64) -> u32 {
    // SAFETY: base() is our device mapping; every offset here is a valid v2 register.
    unsafe { core::ptr::read_volatile((base() + off) as *const u32) }
}
fn mw(off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile((base() + off) as *mut u32, v) }
}

/// A `dmb ish`: order our normal-memory accesses to the queue against the device's, and against
/// the MMIO notify. On QEMU DMA is coherent, but the barrier is still needed so neither the
/// compiler nor the CPU reorders "publish the descriptor" past "tell the device."
fn barrier() {
    // SAFETY: a barrier has no operands and cannot be unsound.
    unsafe { core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags)) };
}

fn dma_write<T>(off: u64, val: T) {
    // SAFETY: off is within the DMA page and T fits; the page is mapped read/write.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut T, val) };
}
fn dma_read<T: Copy>(off: u64) -> T {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const T) }
}

/// Read block 0 of the disk into the DMA data buffer, then verify the crickerfs magic.
pub fn run(dma_phys: u64, mmio_offset: u64) -> ! {
    // SAFETY: single-threaded, before any register access.
    unsafe { MMIO_VA = MMIO_PAGE_VA + mmio_offset };
    check(mr(MAGIC) == 0x7472_6976); // "virt": we really are talking to a virtio device

    // 1-4: reset, then walk the handshake the spec requires, in order.
    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    // Accept exactly VIRTIO_F_VERSION_1 and nothing else. We do not use any optional block
    // feature, so the minimal negotiation is: low word 0, high word bit 32.
    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, 0);
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, F_VERSION_1_HI);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    check(mr(STATUS) & S_FEATURES_OK != 0); // the device agreed to our feature set

    // Set up virtqueue 0.
    mw(QUEUE_SEL, 0);
    check(mr(QUEUE_NUM_MAX) as usize >= QSIZE);
    mw(QUEUE_NUM, QSIZE as u32);

    let desc = dma_phys + OFF_DESC;
    let avail = dma_phys + OFF_AVAIL;
    let used = dma_phys + OFF_USED;
    mw(QUEUE_DESC_LOW, desc as u32);
    mw(QUEUE_DESC_HIGH, (desc >> 32) as u32);
    mw(QUEUE_DRIVER_LOW, avail as u32);
    mw(QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
    mw(QUEUE_DEVICE_LOW, used as u32);
    mw(QUEUE_DEVICE_HIGH, (used >> 32) as u32);
    mw(QUEUE_READY, 1);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

    // Read block 0: the crickerfs superblock.
    read_block(dma_phys, 0);

    // It must be a crickerfs image.
    let mut magic = [0u8; 8];
    for (i, b) in magic.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    check(&magic == b"CRKR0001");

    // Walk the directory (still in the block-0 buffer) to find the file named "motd", then read
    // its first data block. This is a **read from a read-only filesystem, off a real disk, by a
    // driver at EL0**: superblock -> directory -> file, all in userspace.
    let start_block = find_file(b"motd").unwrap_or_else(|| report_code(0xE4));
    read_block(dma_phys, start_block as u64);

    // Report the file's first 8 bytes. The kernel checks them against the known contents, which
    // proves the actual file data came off the disk and across the EL0 boundary.
    let mut head = [0u8; 8];
    for (i, b) in head.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    send(REPORT, u64::from_le_bytes(head), 0, 0);

    loop {
        core::hint::spin_loop();
    }
}

/// Find a file in the crickerfs directory sitting in the block-0 buffer, returning its start
/// block. The format: magic(8), count u32, then entries of { name[24], start_block u32, len u32 }.
fn find_file(name: &[u8]) -> Option<u32> {
    let count = dma_read::<u32>(OFF_DATA + 8);
    for i in 0..count.min(15) as u64 {
        let entry = OFF_DATA + 12 + i * 32;
        let mut matches = true;
        for (j, &want) in name.iter().enumerate() {
            if dma_read::<u8>(entry + j as u64) != want {
                matches = false;
                break;
            }
        }
        // The name must end here (next byte is the NUL padding), so "motd" does not match "motdx".
        if matches && dma_read::<u8>(entry + name.len() as u64) == 0 {
            return Some(dma_read::<u32>(entry + 24));
        }
    }
    None
}

/// Build the three-descriptor chain for a block read, publish it, wait for the interrupt, and
/// confirm the device reported success.
fn read_block(dma_phys: u64, sector: u64) {
    // The request header the device reads: type=IN (read), reserved, sector.
    dma_write::<u32>(OFF_HEADER, VIRTIO_BLK_T_IN);
    dma_write::<u32>(OFF_HEADER + 4, 0);
    dma_write::<u64>(OFF_HEADER + 8, sector);
    dma_write::<u8>(OFF_STATUS, 0xff); // the device overwrites this with 0 on success

    // desc[0]: header, device reads it.        NEXT -> 1
    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
    // desc[1]: data buffer, device WRITES it.  NEXT -> 2
    write_desc(1, dma_phys + OFF_DATA, BLOCK as u32, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 2);
    // desc[2]: status byte, device WRITES it.  (end of chain)
    write_desc(2, dma_phys + OFF_STATUS, 1, VIRTQ_DESC_F_WRITE, 0);

    // Publish the head descriptor (index 0) in the available ring.
    // avail: { u16 flags; u16 idx; u16 ring[QSIZE]; }
    let used_before: u16 = dma_read::<u16>(OFF_USED + 2);
    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0); // ring[idx] = head 0
    barrier(); // the descriptor and ring entry must be visible before we bump idx
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier(); // idx must be visible before we notify

    // Tell the device queue 0 has work.
    mw(QUEUE_NOTIFY, 0);

    // **Wait for the interrupt, as a message.** The device raises its line when it puts our
    // buffer on the used ring; the kernel (milestone 9a) masks the line, turns it into a
    // notification, and wakes us. If the interrupt already fired between the notify above and
    // this call, the kernel's pending count makes WAIT return at once instead of blocking on an
    // event that is already over. SAFETY: `svc`; the kernel validates the Irq capability.
    unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };

    // Quiet the device (read its interrupt-status, acknowledge it), then re-enable the line at
    // the GIC, which the kernel masked when it fired. SAFETY: `svc`.
    let istatus = mr(INTERRUPT_STATUS);
    mw(INTERRUPT_ACK, istatus);
    unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };

    barrier();
    if dma_read::<u16>(OFF_USED + 2) == used_before {
        report_code(0xE3); // woke, but the device did not complete the request
    }
    let st = dma_read::<u8>(OFF_STATUS);
    if st != 0 {
        report_code(0xE200 | st as u64); // device reported a non-OK status
    }
}

/// Report a diagnostic code to the kernel and stop. Distinct from the magic, so the kernel's
/// "not the crickerfs magic" branch prints it. Only reached on a bring-up failure.
fn report_code(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Write one 16-byte descriptor: { u64 addr; u32 len; u16 flags; u16 next; }.
fn write_desc(i: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let base = OFF_DESC + i * 16;
    dma_write::<u64>(base, addr);
    dma_write::<u32>(base + 8, len);
    dma_write::<u16>(base + 12, flags);
    dma_write::<u16>(base + 14, next);
}

