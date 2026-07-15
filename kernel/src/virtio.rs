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
