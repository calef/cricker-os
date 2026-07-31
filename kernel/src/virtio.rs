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

use crate::arch::mmu::{self, VIRTIO_IRQ_BASE, VIRTIO_MMIO_BASE, VIRTIO_SLOT_STRIDE, VIRTIO_SLOTS};

/// The virtio-mmio slot layout, from the arch: aarch64's `virt` has 32 slots 0x200 apart, RISC-V's
/// has 8 slots 0x1000 apart. The probe walks `SLOTS` of them at `SLOT_STRIDE`.
const SLOT_STRIDE: u64 = VIRTIO_SLOT_STRIDE;
const SLOTS: u64 = VIRTIO_SLOTS;

/// "virt", little-endian, at offset 0x000 of every slot.
const MAGIC: u32 = 0x7472_6976;
/// DeviceID at offset 0x008. 0 means "empty slot"; the virtio device-type numbers we route: 1 is a
/// network card, 2 is a block device, 4 is an entropy source.
const DEVICE_ID_NET: u32 = 1;
const DEVICE_ID_BLOCK: u32 = 2;
const DEVICE_ID_ENTROPY: u32 = 4;

// Register offsets we read here. The driver knows many more; the kernel knows exactly three.
const REG_MAGIC: u64 = 0x000;
const REG_VERSION: u64 = 0x004;
const REG_DEVICE_ID: u64 = 0x008;

/// A virtio device found on the mmio bus: where its registers are, and which interrupt it raises.
/// Device-neutral on purpose, because the fields the kernel hands a driver (a register base and an
/// interrupt) are the same whether the device is a disk or a NIC; only the driver differs.
#[derive(Debug, Clone, Copy)]
pub struct VirtioMmioDevice {
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

/// Scan the bus for the first virtio device of type `device_id`. The kernel's whole knowledge of a
/// device at this layer is its type number and its slot; which driver claims the type is a userspace
/// decision (see `virtio_service`).
fn find_by_device_id(device_id: u32) -> Option<VirtioMmioDevice> {
    for slot in 0..SLOTS {
        if read_reg(slot, REG_MAGIC) != MAGIC {
            continue; // not a virtio-mmio slot at all
        }
        if read_reg(slot, REG_DEVICE_ID) != device_id {
            continue; // empty, or some other kind of device
        }

        // We require modern virtio (version 2); the register is read for the debug assertion.
        debug_assert_eq!(
            read_reg(slot, REG_VERSION),
            2,
            "expected modern virtio-mmio"
        );

        return Some(VirtioMmioDevice {
            mmio_phys: VIRTIO_MMIO_BASE + slot * SLOT_STRIDE,
            intid: VIRTIO_IRQ_BASE + slot as u32,
        });
    }
    None
}

/// Scan the bus for the first virtio block device. `None` if there is no disk attached.
pub fn find_block_device() -> Option<VirtioMmioDevice> {
    find_by_device_id(DEVICE_ID_BLOCK)
}

/// Scan the bus for the first virtio network device. `None` if there is no NIC attached.
pub fn find_net_device() -> Option<VirtioMmioDevice> {
    find_by_device_id(DEVICE_ID_NET)
}

/// Scan the bus for the first virtio entropy source (milestone 56). `None` if there is no RNG
/// attached. The PCIe twin is [`crate::pci::find_rng_device`]; the entropy service is wired over
/// whichever one its caller names, and the milestone-56 tests run it over each in turn, because a
/// driver that works on one transport and silently not the other is the bug DECISIONS §18 exists
/// to prevent.
#[cfg_attr(not(test), allow(dead_code))] // entropy_service is the caller, and the m56 tests drive it
pub fn find_entropy_device() -> Option<VirtioMmioDevice> {
    find_by_device_id(DEVICE_ID_ENTROPY)
}

/// Scan the bus for the `n`-th (0-based) virtio block device, `None` if there is no such disk. The
/// FS server (milestone 32 phase 2) drives the SECOND mmio block disk (`n = 1`), a RedoxFS image,
/// leaving the first (the crickerfs disk) to the phase-1 driver tests. QEMU numbers the mmio slots
/// in device order, so the runner attaches the crickerfs disk first and the RedoxFS disk second.
#[cfg_attr(not(test), allow(dead_code))] // fs_service is the caller, and the phase-2 test drives it
pub fn find_block_device_n(n: usize) -> Option<VirtioMmioDevice> {
    let mut seen = 0;
    for slot in 0..SLOTS {
        if read_reg(slot, REG_MAGIC) != MAGIC || read_reg(slot, REG_DEVICE_ID) != DEVICE_ID_BLOCK {
            continue;
        }
        if seen == n {
            return Some(VirtioMmioDevice {
                mmio_phys: VIRTIO_MMIO_BASE + slot * SLOT_STRIDE,
                intid: VIRTIO_IRQ_BASE + slot as u32,
            });
        }
        seen += 1;
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
// Driver-visible reads the pci transport synthesizes (see `Transport::read_reg`).
const REG_MAGIC_RO: u64 = 0x000;
const REG_VERSION_RO: u64 = 0x004;
const REG_DEVICE_ID_RO: u64 = 0x008;
const REG_DEVICE_FEATURES: u64 = 0x010;
const REG_INTERRUPT_STATUS: u64 = 0x060;

/// **The transport seam** (the PCIe workstream's P4; notes/pcie-transport-scope.md). Everything
/// above this enum, the shadow ring, the validator, the queue layout contract, the userspace
/// driver, speaks one register vocabulary: virtio-mmio's. The seam keeps it that way. A
/// `Transport` answers that vocabulary against whichever bus the device actually sits on: the
/// mmio variant is a passthrough to the slot's registers; the pci variant translates each name
/// to the virtio-pci common-config layout (a different arrangement of the same registers), the
/// ISR byte, and the notify doorbell. The userspace driver is byte-identical over both, and the
/// DMA confinement neither knows nor cares which bus rang the device.
///
/// The mmio vocabulary is the seam's canonical language on purpose: it is the vocabulary the ABI
/// already exposes to drivers (`abi::virtio`), and it is flat (offset -> register), which makes
/// the translation table below legible. Nothing about the choice is load-bearing beyond that.
pub enum Transport {
    /// A virtio-mmio slot: the vocabulary IS this device's register block.
    Mmio { mmio_phys: u64 },
    /// A modern virtio-pci function, resolved by kernel/src/pci.rs: the common-config block, the
    /// ISR byte, and the notify doorbell parameters, all physical addresses in a mapped BAR.
    Pci {
        common: u64,
        notify_base: u64,
        notify_mult: u32,
        /// The virtio **device type** (2 = block, 1 = net, 16 = gpu), from the PCI device id
        /// (`0x1040 + type`). PCI config space has no `DeviceID` register in the mmio sense, so a
        /// driver's `DeviceID` read is answered from this. It used to be answered with a hardcoded
        /// 2, which was a manufactured fact for every device that is not a disk; milestone 29's GPU
        /// driver is the first to check it, and finding the lie is what got it fixed.
        device_type: u32,
        /// Each queue's resolved doorbell, computed at `setup_queue` (it needs `queue_notify_off`,
        /// which is only readable with that queue selected, and it differs per queue). Zero until
        /// the queue is set up. Indexed by queue number; see `MAX_QUEUES`.
        notify_addr: [u64; MAX_QUEUES],
        isr: u64,
    },
}

// virtio-pci common-config offsets (virtio spec 4.1.4.3), the pci side of the translation.
const PCI_DEVICE_FEATURE_SEL: u64 = 0x00;
const PCI_DEVICE_FEATURE: u64 = 0x04;
const PCI_DRIVER_FEATURE_SEL: u64 = 0x08;
const PCI_DRIVER_FEATURE: u64 = 0x0c;
const PCI_DEVICE_STATUS: u64 = 0x14;
const PCI_QUEUE_SEL: u64 = 0x16;
const PCI_QUEUE_SIZE: u64 = 0x18;
const PCI_QUEUE_ENABLE: u64 = 0x1c;
const PCI_QUEUE_NOTIFY_OFF: u64 = 0x1e;
const PCI_QUEUE_DESC: u64 = 0x20;
const PCI_QUEUE_DRIVER: u64 = 0x28;
const PCI_QUEUE_DEVICE: u64 = 0x30;

/// Volatile accessors at a physical address through the direct map, in the width the pci
/// common-config block prescribes per register (it is packed; mmio is uniformly u32).
fn pread<T: Copy>(phys: u64) -> T {
    // SAFETY: the caller passes addresses inside a device-mapped BAR or mmio window.
    unsafe { core::ptr::read_volatile(mmu::phys_to_virt(phys) as *const T) }
}
fn pwrite<T: Copy>(phys: u64, v: T) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut T, v) }
}

impl Transport {
    /// The PCI transport for an enumerated function (kernel/src/pci.rs). Five fields copied out of
    /// one struct, in one place, so a new field (milestone 29's `device_type` was the second) is not
    /// a five-call-site edit and cannot be filled in differently at one of them.
    pub fn pci(d: &crate::pci::PciVirtioDevice) -> Transport {
        Transport::Pci {
            common: d.common,
            notify_base: d.notify_base,
            notify_mult: d.notify_mult,
            device_type: d.device_type,
            notify_addr: [0; MAX_QUEUES],
            isr: d.isr,
        }
    }

    /// Answer a read in the mmio vocabulary.
    fn read_reg(&self, off: u64) -> u32 {
        match self {
            Transport::Mmio { mmio_phys } => reg_read(*mmio_phys, off),
            Transport::Pci {
                common,
                isr,
                device_type,
                ..
            } => match off {
                // pci has no magic/version/device-id registers: identity was proved from config
                // space (vendor/device id) before this transport existed. Synthesized so the
                // driver's sanity checks mean the same thing on both buses. The device id is the
                // virtio device TYPE recovered from the PCI id, not a constant: see `device_type`.
                REG_MAGIC_RO => 0x7472_6976,
                REG_VERSION_RO => 2,
                REG_DEVICE_ID_RO => *device_type,
                REG_DEVICE_FEATURES => pread::<u32>(common + PCI_DEVICE_FEATURE),
                // The ISR byte is read-to-ack at the device: the read itself deasserts INTx.
                // Same bit 0 (queue interrupt) the mmio register reports.
                REG_INTERRUPT_STATUS => pread::<u8>(*isr) as u32,
                REG_STATUS => pread::<u8>(common + PCI_DEVICE_STATUS) as u32,
                REG_QUEUE_NUM_MAX => {
                    pwrite::<u16>(common + PCI_QUEUE_SEL, 0);
                    pread::<u16>(common + PCI_QUEUE_SIZE) as u32
                }
                _ => 0,
            },
        }
    }

    /// Answer a write in the mmio vocabulary. Only the offsets the kernel itself writes and the
    /// driver-safe set (`write_register`'s SAFE list) reach here.
    fn write_reg(&mut self, off: u64, v: u32) {
        match self {
            Transport::Mmio { mmio_phys } => reg_write(*mmio_phys, off, v),
            Transport::Pci { common, .. } => match off {
                REG_DEVICE_FEATURES_SEL => pwrite::<u32>(*common + PCI_DEVICE_FEATURE_SEL, v),
                REG_DRIVER_FEATURES_SEL => pwrite::<u32>(*common + PCI_DRIVER_FEATURE_SEL, v),
                REG_DRIVER_FEATURES => pwrite::<u32>(*common + PCI_DRIVER_FEATURE, v),
                REG_STATUS => pwrite::<u8>(*common + PCI_DEVICE_STATUS, v as u8),
                // The ISR read already acknowledged; the mmio-style ack has no pci counterpart.
                REG_INTERRUPT_ACK => {}
                _ => {}
            },
        }
    }

    /// Queue `q`'s maximum size, as the device reports it. Each queue is selected and read on its
    /// own: a device may size its queues differently (virtio-net rarely does, but the kernel does
    /// not assume it).
    fn queue_num_max(&self, q: u16) -> u16 {
        match self {
            Transport::Mmio { mmio_phys } => {
                reg_write(*mmio_phys, REG_QUEUE_SEL, q as u32);
                reg_read(*mmio_phys, REG_QUEUE_NUM_MAX) as u16
            }
            Transport::Pci { common, .. } => {
                pwrite::<u16>(*common + PCI_QUEUE_SEL, q);
                pread::<u16>(*common + PCI_QUEUE_SIZE)
            }
        }
    }

    /// Program queue `q`: its size and its three ring addresses, then mark it live. The addresses
    /// are the kernel's choice (the shadow page and the driver's used ring); that this method is
    /// the only way they reach the device is the confinement. On PCI the queue's doorbell is
    /// resolved here (it is only readable with the queue selected) and remembered per queue.
    fn setup_queue(&mut self, q: u16, num: u16, desc: u64, avail: u64, used: u64) {
        match self {
            Transport::Mmio { mmio_phys } => {
                let m = *mmio_phys;
                reg_write(m, REG_QUEUE_SEL, q as u32);
                reg_write(m, REG_QUEUE_NUM, num as u32);
                reg_write(m, REG_QUEUE_DESC_LOW, desc as u32);
                reg_write(m, REG_QUEUE_DESC_HIGH, (desc >> 32) as u32);
                reg_write(m, REG_QUEUE_DRIVER_LOW, avail as u32);
                reg_write(m, REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
                reg_write(m, REG_QUEUE_DEVICE_LOW, used as u32);
                reg_write(m, REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
                reg_write(m, REG_QUEUE_READY, 1);
            }
            Transport::Pci {
                common,
                notify_base,
                notify_mult,
                notify_addr,
                ..
            } => {
                pwrite::<u16>(*common + PCI_QUEUE_SEL, q);
                pwrite::<u16>(*common + PCI_QUEUE_SIZE, num);
                pwrite::<u64>(*common + PCI_QUEUE_DESC, desc);
                pwrite::<u64>(*common + PCI_QUEUE_DRIVER, avail);
                pwrite::<u64>(*common + PCI_QUEUE_DEVICE, used);
                // The doorbell: notify_base + queue_notify_off * multiplier, readable only with
                // this queue selected, so it is resolved here and remembered for this queue.
                let off = pread::<u16>(*common + PCI_QUEUE_NOTIFY_OFF) as u64;
                notify_addr[q as usize] = *notify_base + off * (*notify_mult as u64);
                pwrite::<u16>(*common + PCI_QUEUE_ENABLE, 1);
            }
        }
    }

    /// Ring the doorbell for queue `q`. Only [`notify`] calls this, after validation. On mmio the
    /// notify register carries the queue number as its value; on PCI each queue has its own
    /// doorbell address, resolved at [`setup_queue`].
    fn notify_queue(&self, q: u16) {
        match self {
            Transport::Mmio { mmio_phys } => reg_write(*mmio_phys, REG_QUEUE_NOTIFY, q as u32),
            Transport::Pci { notify_addr, .. } => pwrite::<u16>(notify_addr[q as usize], 0),
        }
    }
}

/// The fixed queue layout, a contract shared with the userspace driver (user/src/virtio.rs). The
/// kernel places the rings at these offsets in the DMA region, so it always knows where they are.
/// These offsets are **relative to a queue's ring block**; a device's queue `q` puts its rings at
/// `q * RING_BLOCK + {DESC,AVAIL,USED}_OFF` (see `RING_BLOCK`).
///
/// **These are aliases, not copies.** The layout is defined in `crates/dma_validate`, where
/// `distinct_queues_occupy_disjoint_blocks` proves the isolation that follows from it (a queue's ring
/// area fits inside its own block, blocks do not overlap, and the descriptor table a validation walk
/// writes ends before the available ring begins). Milestone 35 first duplicated them here, which left
/// the proof quantifying over constants this file could drift away from silently; aliasing makes the
/// proved layout the layout that runs, the same discipline that has the kernel *call*
/// `dma_validate::validate_and_shadow` rather than keep a parallel copy of it.
pub const QSIZE: u16 = dma_validate::LAYOUT_QSIZE;
const DESC_OFF: u64 = dma_validate::DESC_OFF; // 16 * QSIZE
const AVAIL_OFF: u64 = dma_validate::AVAIL_OFF; // 6 + 2*QSIZE
const USED_OFF: u64 = dma_validate::USED_OFF; // 6 + 8*QSIZE
/// The whole ring area of one queue must fit under this; a queue's data buffers live above it.
const RING_END: u64 = dma_validate::RING_END;

/// The most virtqueues the confinement drives per device. A virtio-net device needs two (receive =
/// queue 0, transmit = queue 1); the disk uses only queue 0. Fixed and small on purpose: one
/// kernel-private shadow frame per device holds every queue's shadow rings, so the ceiling is what
/// fits there (`MAX_QUEUES * RING_BLOCK <= FRAME_SIZE`), asserted below, not a policy dial.
pub const MAX_QUEUES: usize = dma_validate::MAX_QUEUES as usize;

/// The stride between successive queues' ring areas, in **both** the driver's DMA region and the
/// kernel-private shadow. Queue `q`'s descriptor table, available ring, and used ring sit at
/// `q * RING_BLOCK + {DESC,AVAIL,USED}_OFF`. 0x200 leaves room for a whole queue's ring area
/// (`RING_END`) and keeps queue 0 byte-identical to the single-queue layout the disk driver already
/// uses: the disk's data buffers begin at 0x200 (= queue 1's block), which is free because a disk
/// has no queue 1, so the disk needs no change at all.
const RING_BLOCK: u64 = dma_validate::RING_BLOCK;

// The shadow frame must hold every queue's ring block, and a queue's ring area must fit in its
// block. Both are compile-time facts, so break the build if a future edit violates either.
const _: () = assert!(
    RING_END <= RING_BLOCK,
    "a queue's rings overflow its ring block"
);
const _: () = assert!(
    MAX_QUEUES as u64 * RING_BLOCK <= frames::FRAME_SIZE,
    "the shadow frame cannot hold every queue's ring block",
);

/// Queue `q`'s ring block base, relative to a region (driver DMA or shadow) base. The proved
/// function, for the same reason the constants above are aliases.
const fn queue_block(q: u16) -> u64 {
    dma_validate::queue_block(q)
}

// The descriptor flags the validator acts on. The validation logic that reads them now lives in
// `crates/dma_validate` (as `dma_validate::F_NEXT` / `F_INDIRECT`); these copies remain for the
// attacker tests below, which build descriptor words by hand. Bit 2 (`INDIRECT`) points a
// descriptor at a table of further descriptors the validator never copies into the shadow, so it is
// refused (and the feature that enables it is negotiated off in `sanitize_driver_features`), which
// is the confinement failing closed if that negotiation ever regresses.
#[cfg(test)]
const VIRTQ_DESC_F_NEXT: u16 = 1;
#[cfg(test)]
const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// One block device the kernel operates the transport for.
struct Device {
    transport: Transport,
    dma_base: u64,
    dma_size: u64,
    /// The last available-ring index we have already validated and forwarded, **per queue**.
    /// Descriptors are only ever *added* by the driver, so we validate the new ones each notify;
    /// RX and TX advance independently, so each queue keeps its own high-water mark.
    last_avail: [u16; MAX_QUEUES],
    /// Which 32-bit word of the feature bits the driver's next `DRIVER_FEATURES` write targets
    /// (`DRIVER_FEATURES_SEL`: 0 = features 0..31, 1 = 32..63). Tracked so a feature write can have
    /// the ring-layout features the validator cannot police stripped from whichever word carries
    /// them. See `sanitize_driver_features`.
    driver_features_sel: u32,
    /// Physical base of the **kernel-private shadow page**: the descriptor table and available ring
    /// the *device* actually reads. The driver builds its own copies in its DMA region; on `notify`
    /// the kernel validates those and copies them here, so the device only ever reads descriptors
    /// the driver cannot touch. This is what closes the time-of-check/time-of-use race: the bytes
    /// the device reads are the bytes the kernel validated. See notes/dma.md.
    shadow_base: u64,
}

/// The most virtio transports we will drive. Each `register` call takes a slot for the life of
/// the boot (there is no unregister), and the test suite registers one per driver it spawns: the
/// reader, two attackers, the PCIe reader, two writers, the abandoner, and its post-kill reader
/// already make eight, and milestone 30 adds the net driver and net server over both buses (four
/// more), so the ceiling grew again. Fixed-size (milestone 14 phase B.1): probing never allocates.
///
/// **Bumped to 26 for milestone 29's display terminal**, which spawns a third `display` over the same
/// physical GPU (the confinement test, the terminal test, and the pattern test each program the
/// device from scratch). Recorded rather than quietly widened, with the standing suggestion this
/// number's history keeps earning: the honest fix is releasing a transport when its driver dies,
/// which is its own piece of work because nothing here unregisters. The same suggestion DECISIONS
/// §33 made about `KERNEL_EP_PAGES`, for the same reason.
///
/// **27 for milestone 37**, whose crash test brings up a block server on a disk of its own so that
/// deliberately leaving a filesystem half-written cannot touch the image every other FS test reads.
/// One more transport for the boot, and the same standing suggestion: this is the fourth bump, and
/// each one is a receipt for the missing unregister.
///
/// **29 for milestone 56's entropy service**, one transport per bus. This bump is the cheapest of
/// the five and the most annoying, because the entropy service is wired **once per device per boot**
/// (`entropy_service::ensure`) precisely so that a second one cannot reset the device under the
/// first, and it still costs two slots that are never released. The fifth receipt.
const MAX_DEVICES: usize = 29;

/// The device table, fixed. `get`/`get_mut` mirror the slice API the call sites already used.
struct Devices {
    entries: [Option<Device>; MAX_DEVICES],
    count: usize,
}

impl Devices {
    fn get(&self, i: usize) -> Option<&Device> {
        self.entries.get(i)?.as_ref()
    }

    fn get_mut(&mut self, i: usize) -> Option<&mut Device> {
        self.entries.get_mut(i)?.as_mut()
    }
}

static DEVICES: IrqSafeMutex<Devices> = IrqSafeMutex::new(
    rank::VIRTIO,
    Devices {
        entries: [const { None }; MAX_DEVICES],
        count: 0,
    },
);

/// Register the block device and its DMA region with the transport layer. Returns its id, which
/// is what goes inside an `Object::Virtio` capability. The driver never sees the device's
/// registers on either bus; it drives the device through that capability.
///
/// `rid` is the PCIe requester id when the device sits behind an IOMMU (the PCI transport), `None`
/// for a virtio-mmio device (no IOMMU fronts the mmio bus on either board). When an IOMMU is active
/// and a requester id is given, the device is confined in hardware to exactly the frames it may
/// reach (its DMA region plus the shadow page) before it is entered in the table: from that moment
/// any other address it emits faults at the IOMMU. The software shadow ring stays either way, now
/// as defence in depth (notes/dma.md).
pub fn register(transport: Transport, dma_base: u64, dma_size: u64, rid: Option<u32>) -> usize {
    // The shadow page the device reads its rings from. One frame per device, kernel-owned and never
    // mapped into the driver, so the driver cannot touch what the device sees.
    let shadow_base = crate::memory::alloc()
        .expect("no frame for the virtio shadow ring")
        .addr();
    // SAFETY: a fresh frame, reachable through the direct map, owned by nobody yet. Zero it so a
    // stale word can never look like a valid descriptor before the first copy fills it.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(shadow_base) as *mut u8,
            0,
            frames::FRAME_SIZE as usize,
        );
    }

    // Confine the device in hardware before it is entered in the table. Done outside the DEVICES
    // lock: building the domain allocates page-table frames, and the IOMMU attach takes its own
    // lock, so keeping both off the VIRTIO rank keeps the lock order a plain leaf (sync::rank).
    if let Some(rid) = rid
        && crate::iommu::active()
    {
        let regions = crate::iommu::virtio_regions(dma_base, dma_size, shadow_base);
        crate::iommu::confine(rid, &regions);
    }

    let mut devs = DEVICES.lock();
    assert!(
        devs.count < MAX_DEVICES,
        "more virtio devices than MAX_DEVICES"
    );
    let id = devs.count;
    devs.entries[id] = Some(Device {
        transport,
        dma_base,
        dma_size,
        last_avail: [0; MAX_QUEUES],
        driver_features_sel: 0,
        shadow_base,
    });
    devs.count += 1;
    id
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

/// Write into the kernel-private shadow ring, through the direct map. The shadow page is a
/// kernel-owned frame, so this is a plain store to memory only the kernel names.
fn dma_write16(phys: u64, v: u16) {
    unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u16, v) }
}
fn dma_write64(phys: u64, v: u64) {
    unsafe { core::ptr::write_volatile(mmu::phys_to_virt(phys) as *mut u64, v) }
}

/// **The security-critical step: validate the driver's descriptors AND copy them into the shadow
/// ring the device reads.**
///
/// The logic lives in `crates/dma_validate` (milestone 35), lifted out so Kani can prove it for
/// every input: no descriptor the device reads out of the shadow references memory outside
/// `[dma_base, dma_base + dma_size)`, in either direction, with indirect descriptors refused, over
/// any batch. This is the thin kernel adapter: it passes the driver and shadow ring *physical
/// addresses* and the direct-map read/write closures, plus `QSIZE`, straight through to the proved
/// walk. The device reads the shadow (see [`setup_queue`]), which the driver cannot write, so the
/// bytes the device acts on are exactly the bytes validated here; mutating a descriptor after this
/// returns changes only the driver's own copy, which nothing reads. See notes/dma.md and
/// notes/verification.md.
#[allow(clippy::too_many_arguments)]
fn validate_and_shadow(
    dma_base: u64,
    dma_size: u64,
    driver_desc: u64,
    driver_avail: u64,
    shadow_desc: u64,
    shadow_avail: u64,
    from_idx: u16,
    to_idx: u16,
    read16: &dyn Fn(u64) -> u16,
    read64: &dyn Fn(u64) -> u64,
    write16: &dyn Fn(u64, u16),
    write64: &dyn Fn(u64, u64),
) -> bool {
    dma_validate::validate_and_shadow(
        dma_base,
        dma_size,
        driver_desc,
        driver_avail,
        shadow_desc,
        shadow_avail,
        from_idx,
        to_idx,
        QSIZE,
        read16,
        read64,
        write16,
        write64,
    )
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
    Some(dev.transport.read_reg(off))
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

    dev.transport.write_reg(off, val);
    Ok(())
}

/// Strip the ring-layout features the descriptor validator cannot police from a `DRIVER_FEATURES`
/// word. `sel` is the word the driver selected: 0 = features 0..31, 1 = features 32..63.
///
/// Two features change **what the device reads descriptors from**, which is exactly the thing
/// `validate_and_shadow` assumes it controls:
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
/// The shadow descriptor ring ([`validate_and_shadow`]) is the structural fix that removes the
/// underlying race; this stripping stays as defence in depth, so the transport refuses a format it
/// cannot police even before a descriptor is built. See notes/dma.md.
fn sanitize_driver_features(sel: u32, val: u32) -> u32 {
    const F_INDIRECT_DESC_LO: u32 = 1 << 28; // feature bit 28
    const F_RING_PACKED_HI: u32 = 1 << (34 - 32); // feature bit 34
    match sel {
        0 => val & !F_INDIRECT_DESC_LO,
        1 => val & !F_RING_PACKED_HI,
        _ => val,
    }
}

/// Set up queue `queue` with `num` entries. The kernel programs the ring addresses, so the driver
/// never gets to choose them:
///
/// - **Descriptor table and available ring** point at this queue's block in the kernel-private
///   **shadow** page. The device reads its descriptors from memory the driver cannot write; the
///   driver builds its own copies in its region and the kernel validates and copies them across on
///   `notify`.
/// - **Used ring** stays in this queue's block in the driver's region, so the driver reads
///   completions directly. The device only ever *writes* indices and lengths there, never
///   addresses, so nothing to confine.
///
/// A device with several queues (virtio-net: receive on 0, transmit on 1) calls this once per
/// queue; each queue's rings sit at `queue * RING_BLOCK` in both regions, so they never overlap.
pub fn setup_queue(id: usize, num: u16, queue: u16) -> Result<(), TransportError> {
    if queue as usize >= MAX_QUEUES {
        return Err(TransportError::BadQueue);
    }
    let mut devs = DEVICES.lock();
    let dev = devs.get_mut(id).ok_or(TransportError::NoDevice)?;

    // Every queue's ring block must lie inside the driver's DMA region. The last block ends at
    // `MAX_QUEUES * RING_BLOCK`, but this queue only needs its own block plus a ring area.
    let block = queue_block(queue);
    if num == 0 || num > QSIZE || dev.dma_size < block + RING_END {
        return Err(TransportError::BadQueue);
    }
    if dev.transport.queue_num_max(queue) < num {
        return Err(TransportError::BadQueue);
    }

    let desc = dev.shadow_base + block + DESC_OFF; // the SHADOW descriptor table (device-read)
    let avail = dev.shadow_base + block + AVAIL_OFF; // the SHADOW available ring
    let used = dev.dma_base + block + USED_OFF; // the used ring stays in the driver's region
    dev.transport.setup_queue(queue, num, desc, avail, used);
    Ok(())
}

/// **The validated "go" for queue `queue`.** Validate the descriptor chains the driver has newly
/// published on that queue, copy the validated ones into the queue's shadow ring the device reads,
/// and only then ring the device. If any descriptor escapes the driver's DMA region, the shadow is
/// not published, the device is NOT notified, and the driver gets `DmaEscape`. Each queue keeps its
/// own last-validated index, so a receive submit and a transmit submit never interfere.
pub fn notify(id: usize, queue: u16) -> Result<(), TransportError> {
    if queue as usize >= MAX_QUEUES {
        return Err(TransportError::BadQueue);
    }
    let mut devs = DEVICES.lock();
    let dev = devs.get_mut(id).ok_or(TransportError::NoDevice)?;

    let block = queue_block(queue);
    let driver_desc = dev.dma_base + block + DESC_OFF;
    let driver_avail = dev.dma_base + block + AVAIL_OFF;
    let shadow_desc = dev.shadow_base + block + DESC_OFF;
    let shadow_avail = dev.shadow_base + block + AVAIL_OFF;
    let to_idx = dma_read16(driver_avail + 2); // the driver's avail.idx for this queue

    let ok = validate_and_shadow(
        dev.dma_base,
        dev.dma_size,
        driver_desc,
        driver_avail,
        shadow_desc,
        shadow_avail,
        dev.last_avail[queue as usize],
        to_idx,
        &|p| dma_read16(p),
        &|p| dma_read64(p),
        &|p, v| dma_write16(p, v),
        &|p, v| dma_write64(p, v),
    );
    if !ok {
        return Err(TransportError::DmaEscape);
    }
    dev.last_avail[queue as usize] = to_idx;

    // The shadow writes above must be globally visible before the device is rung: the device is a
    // separate observer that will read the shadow by DMA. See arch::dma_wmb.
    crate::arch::dma_wmb();
    dev.transport.notify_queue(queue);
    Ok(())
}

/// **Test hook: make device `id` emit an address its IOMMU domain does not map, so the IOMMU must
/// fault** (milestone 16b confinement proof, kernel/src/iommu.rs).
///
/// The software shadow ring refuses an out-of-region descriptor *before* the device is ever rung,
/// so a normal path can never reach the hardware. To prove the IOMMU itself confines the device we
/// deliberately go around that: bring the device up at the transport level and point its **available
/// ring** at `avail_out_of_domain`, a frame the domain does not include. On the kick, the device's
/// first act is to read the available ring; that read is an out-of-domain IOVA, so the IOMMU faults
/// it and records the event. The CPU can still fill the frame through the direct map (CPU accesses
/// do not go through the IOMMU); only the *device's* view of it is unmapped, which is the whole
/// point. No block-device knowledge is needed: the fault happens before any descriptor is read.
#[cfg(test)]
pub(crate) fn provoke_iommu_escape(id: usize, avail_out_of_domain: u64) {
    // Status bits and the two feature words, in the mmio vocabulary the transport speaks.
    const S_ACK: u32 = 1;
    const S_DRIVER: u32 = 2;
    const S_DRIVER_OK: u32 = 4;
    const S_FEATURES_OK: u32 = 8;
    const F_VERSION_1_HI: u32 = 1; // feature bit 32
    const F_ACCESS_PLATFORM_HI: u32 = 1 << 1; // feature bit 33 (set when behind an IOMMU)

    let mut devs = DEVICES.lock();
    let dev = devs
        .get_mut(id)
        .expect("provoke_iommu_escape: no such device");

    // Reset, then the modern handshake up to FEATURES_OK.
    dev.transport.write_reg(REG_STATUS, 0);
    dev.transport.write_reg(REG_STATUS, S_ACK);
    dev.transport.write_reg(REG_STATUS, S_ACK | S_DRIVER);
    dev.transport.write_reg(REG_DRIVER_FEATURES_SEL, 0);
    dev.transport.write_reg(REG_DRIVER_FEATURES, 0);
    dev.transport.write_reg(REG_DEVICE_FEATURES_SEL, 1);
    let dev_hi = dev.transport.read_reg(REG_DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI; // required when iommu_platform=on, else FEATURES_OK sticks off
    }
    dev.transport.write_reg(REG_DRIVER_FEATURES_SEL, 1);
    dev.transport.write_reg(REG_DRIVER_FEATURES, ack_hi);
    dev.transport
        .write_reg(REG_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK);

    // Queue 0: descriptor table on the (in-domain) shadow, used ring in the (in-domain) driver
    // region, available ring pointed OUT of the domain. Then DRIVER_OK to make the queue live.
    let num = dev.transport.queue_num_max(0).min(QSIZE);
    let desc = dev.shadow_base + DESC_OFF;
    let used = dev.dma_base + USED_OFF;
    dev.transport
        .setup_queue(0, num, desc, avail_out_of_domain, used);
    dev.transport
        .write_reg(REG_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

    // Publish an available head from the CPU side (bypassing the IOMMU) so the device has a reason to
    // read the available ring. avail = { u16 flags; u16 idx; u16 ring[] }.
    dma_write16(avail_out_of_domain, 0); // flags
    dma_write16(avail_out_of_domain + 2, 1); // idx = 1: one entry available
    dma_write16(avail_out_of_domain + 4, 0); // ring[0] = head 0
    crate::arch::dma_wmb();

    // Kick. The device now reads the available ring at an unmapped IOVA and the IOMMU faults. Under
    // TCG QEMU processes the kick synchronously in this vCPU thread, so the fault is already in the
    // IOMMU's queue by the time this returns.
    dev.transport.notify_queue(0);

    // Reset the device so a later test that re-registers it does not inherit this deliberately
    // broken queue. The fault event lives in the IOMMU's own queue, which a device reset does not
    // touch, so this does not erase what the caller is about to drain.
    dev.transport.write_reg(REG_STATUS, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device-writable descriptor flag, bit 1. The kernel validator does not name it (it bounds
    /// addresses whichever way the device moves the bytes), but the receive-direction tests set it
    /// to build a descriptor the *device writes into*, which is what a virtio-net RX buffer is.
    const VIRTQ_DESC_F_WRITE: u16 = 2;

    // Read/write the direct map at a physical address. One set serves both the fake driver region
    // and the fake shadow region below, since they take absolute addresses. Passed to
    // `validate_and_shadow` as `&dyn Fn` (function items coerce).
    fn r16(p: u64) -> u16 {
        unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u16) }
    }
    fn r64(p: u64) -> u64 {
        unsafe { core::ptr::read_volatile(mmu::phys_to_virt(p) as *const u64) }
    }
    fn w16(p: u64, v: u16) {
        unsafe { core::ptr::write_volatile(mmu::phys_to_virt(p) as *mut u16, v) }
    }
    fn w64(p: u64, v: u64) {
        unsafe { core::ptr::write_volatile(mmu::phys_to_virt(p) as *mut u64, v) }
    }

    /// Write descriptor `i` of the table at `desc`: { u64 addr; u32 len; u16 flags; u16 next }.
    fn write_desc(desc: u64, i: u64, addr: u64, len: u32, flags: u16, next: u16) {
        let d = desc + i * 16;
        w64(d, addr);
        w64(d + 8, len as u64); // len in the low half; flags/next written over the high half next
        w16(d + 12, flags);
        w16(d + 14, next);
    }

    /// A fake driver DMA region and a fake shadow region, each a real frame reached through the
    /// direct map. Returns their physical bases and the region size. Free both with [`free_regions`].
    fn two_regions() -> (u64, u64, u64) {
        let driver = crate::memory::alloc().expect("no driver frame").addr();
        let shadow = crate::memory::alloc().expect("no shadow frame").addr();
        (driver, shadow, frames::FRAME_SIZE)
    }
    fn free_regions(driver: u64, shadow: u64) {
        crate::memory::free(frames::Frame::from_addr(driver));
        crate::memory::free(frames::Frame::from_addr(shadow));
    }

    /// Drive `validate_and_shadow` against the fake regions with the standard closures. Queue 0,
    /// the single-queue shape the disk uses.
    fn run(driver: u64, size: u64, shadow: u64, from: u16, to: u16) -> bool {
        run_q(driver, size, shadow, 0, from, to)
    }

    /// Drive `validate_and_shadow` for a specific queue, applying that queue's ring-block offset in
    /// both the driver region and the shadow, exactly as `notify(id, queue)` does. Lets a test
    /// exercise queue 1 (virtio-net's transmit queue) on its own block.
    fn run_q(driver: u64, size: u64, shadow: u64, q: u16, from: u16, to: u16) -> bool {
        let block = queue_block(q);
        validate_and_shadow(
            driver,
            size,
            driver + block + DESC_OFF,
            driver + block + AVAIL_OFF,
            shadow + block + DESC_OFF,
            shadow + block + AVAIL_OFF,
            from,
            to,
            &r16,
            &r64,
            &w16,
            &w64,
        )
    }

    /// Build a fake DMA region and exercise the security-critical check: a descriptor pointing
    /// OUTSIDE the region is refused, one inside is accepted, and a `next`-cycle cannot hang it.
    #[test_case]
    fn the_validator_refuses_a_descriptor_that_escapes_the_dma_region() {
        let (driver, shadow, size) = two_regions();
        let desc = driver + DESC_OFF;
        let avail = driver + AVAIL_OFF;

        // --- a GOOD chain: header + data + status, all inside the region ---
        write_desc(desc, 0, driver + 0x200, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(desc, 1, driver + 0x400, 512, VIRTQ_DESC_F_NEXT, 2);
        write_desc(desc, 2, driver + 0x600, 1, 0, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1
        assert!(
            run(driver, size, shadow, 0, 1),
            "a chain wholly inside the region was rejected",
        );

        // --- the ATTACK: descriptor 1 points at kernel memory (the kernel image) ---
        write_desc(
            desc,
            1,
            0xffff_0000_4008_0000,
            512,
            VIRTQ_DESC_F_NEXT | 2,
            2,
        );
        assert!(
            !run(driver, size, shadow, 0, 1),
            "a descriptor pointing at kernel memory was NOT refused",
        );

        // --- a length that overflows the region by one byte ---
        write_desc(desc, 1, driver + size - 256, 512, VIRTQ_DESC_F_NEXT | 2, 2);
        assert!(
            !run(driver, size, shadow, 0, 1),
            "a descriptor running past the end of the region was NOT refused",
        );

        // --- a next-pointer cycle must terminate, not hang ---
        write_desc(desc, 0, driver + 0x200, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(desc, 1, driver + 0x400, 16, VIRTQ_DESC_F_NEXT, 0); // 1 -> 0 -> 1 -> ...
        assert!(
            run(driver, size, shadow, 0, 1),
            "a bounded cyclic chain with valid addresses should still validate (and terminate)",
        );

        free_regions(driver, shadow);
    }

    /// **The shadow ring closes the time-of-check/time-of-use race.**
    ///
    /// Validate a good chain (which copies it into the shadow), then mutate the DRIVER's descriptor
    /// to point at kernel memory, exactly as a device fetching descriptors asynchronously would let
    /// a driver do after the check. The device reads the SHADOW, so the shadow must still hold the
    /// validated, in-region address: the mutation touched only the driver's own copy, which nothing
    /// reads. This is the whole reason the shadow ring exists.
    #[test_case]
    fn the_shadow_ring_is_immune_to_a_descriptor_mutated_after_validation() {
        let (driver, shadow, size) = two_regions();
        let desc = driver + DESC_OFF;
        let avail = driver + AVAIL_OFF;
        let shadow_desc = shadow + DESC_OFF;
        let shadow_avail = shadow + AVAIL_OFF;

        // One valid in-region descriptor, published as head 0.
        let good_addr = driver + 0x200;
        write_desc(desc, 0, good_addr, 512, 0, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1

        assert!(
            run(driver, size, shadow, 0, 1),
            "a valid descriptor was rejected"
        );
        assert_eq!(
            r64(shadow_desc),
            good_addr,
            "the shadow did not receive the validated descriptor",
        );
        assert_eq!(
            r16(shadow_avail + 2),
            1,
            "the shadow avail.idx was not published"
        );

        // The driver now aims its descriptor at kernel memory, AFTER the check. On async-DMA
        // hardware this is the race. The device reads the shadow, which must be untouched.
        w64(desc, 0xffff_0000_4008_0000);
        assert_eq!(
            r64(shadow_desc),
            good_addr,
            "a post-validation write to the driver's descriptor reached the shadow the device \
             reads: the TOCTOU race is open",
        );

        free_regions(driver, shadow);
    }

    /// **An indirect descriptor is refused even when it points inside the region.**
    ///
    /// A descriptor flagged `INDIRECT` points at a *table* of further descriptors we do not copy
    /// into the shadow, so the device would follow it out of the region. A wholly in-region
    /// indirect descriptor still has to be refused, because it is not the descriptor the device
    /// ultimately acts on.
    #[test_case]
    fn the_validator_refuses_an_indirect_descriptor() {
        let (driver, shadow, size) = two_regions();
        let desc = driver + DESC_OFF;
        let avail = driver + AVAIL_OFF;

        // desc[0]: a legal in-region address, but flagged INDIRECT.
        write_desc(desc, 0, driver + 0x200, 128, VIRTQ_DESC_F_INDIRECT, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1

        assert!(
            !run(driver, size, shadow, 0, 1),
            "an indirect descriptor was accepted: the device could follow its unvalidated table \
             out of the DMA region",
        );

        free_regions(driver, shadow);
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
    /// would otherwise make the walk loop that many times, under the `DEVICES` lock with interrupts
    /// masked. Every descriptor and ring slot here is individually valid, so the ONLY thing that can
    /// refuse the oversized batch is the jump-size guard.
    #[test_case]
    fn the_validator_refuses_more_new_entries_than_the_ring_can_hold() {
        let (driver, shadow, size) = two_regions();
        let desc = driver + DESC_OFF;
        let avail = driver + AVAIL_OFF;

        // Fill the whole ring with individually valid single-descriptor entries.
        for i in 0..QSIZE as u64 {
            write_desc(desc, i, driver + 0x200 + i * 8, 8, 0, 0);
            w16(avail + 4 + i * 2, i as u16); // ring[i] = head i
        }

        // Exactly QSIZE new entries is legal: the ring holds that many.
        assert!(
            run(driver, size, shadow, 0, QSIZE),
            "a batch of exactly QSIZE valid entries was refused",
        );

        // One more than the ring can hold is refused, though every descriptor is valid.
        assert!(
            !run(driver, size, shadow, 0, QSIZE + 1),
            "a jump of QSIZE+1 was walked instead of refused: a hostile avail.idx could spin the \
             validator up to 65535 times with interrupts masked",
        );

        free_regions(driver, shadow);
    }

    /// **The receive direction is confined: a device-WRITABLE descriptor aimed outside the region is
    /// refused** (milestone 30, the multi-queue confinement's second, proved direction). virtio-net's
    /// receive queue is the direction where the *device writes into* the driver's memory: the driver
    /// posts an empty buffer and the device fills it with a packet. A hostile driver posts a receive
    /// buffer pointing at kernel memory and lets the device write a packet over the kernel image. The
    /// validator bounds the address whether the device reads or writes it, so this is refused before
    /// the device is rung; the in-region receive buffer that validates first proves the refusal is
    /// about the address, not the direction flag. This is the same property milestone 32's write path
    /// relied on, now asserted for the direction where the device is the writer.
    #[test_case]
    fn the_validator_refuses_an_rx_descriptor_that_escapes_the_region() {
        let (driver, shadow, size) = two_regions();
        let desc = driver + DESC_OFF;
        let avail = driver + AVAIL_OFF;

        // A legitimate receive posting: one device-writable buffer, wholly in-region.
        write_desc(desc, 0, driver + 0x400, 1514, VIRTQ_DESC_F_WRITE, 0);
        w16(avail + 4, 0); // ring[0] = head 0
        w16(avail + 2, 1); // avail.idx = 1
        assert!(
            run(driver, size, shadow, 0, 1),
            "an in-region device-writable receive buffer was refused",
        );

        // The attack: the same device-writable buffer, now aimed at kernel memory, so the device
        // would DMA a received packet straight over the kernel image.
        write_desc(desc, 0, 0xffff_0000_4008_0000, 1514, VIRTQ_DESC_F_WRITE, 0);
        assert!(
            !run(driver, size, shadow, 0, 1),
            "a device-writable receive descriptor pointing at kernel memory was NOT refused: the \
             device could overwrite the kernel with a received packet",
        );

        free_regions(driver, shadow);
    }

    /// **A second queue validates on its own ring block, independent of queue 0** (milestone 30). A
    /// virtio-net device drives receive on queue 0 and transmit on queue 1; each queue's rings live
    /// at `queue * RING_BLOCK` in both the driver region and the shadow, so a submit on one queue
    /// never reads or writes the other's shadow. Validate a good chain on queue 1, confirm it landed
    /// in queue 1's shadow block while a sentinel in queue 0's shadow block stayed put, then confirm
    /// an escape on queue 1 is refused the same as on queue 0.
    #[test_case]
    fn a_second_queue_validates_on_its_own_block() {
        let (driver, shadow, size) = two_regions();
        let q1 = queue_block(1);
        let desc1 = driver + q1 + DESC_OFF;
        let avail1 = driver + q1 + AVAIL_OFF;

        // A sentinel in queue 0's shadow descriptor slot. Frames from `alloc` are not zeroed, so we
        // plant a known value and assert queue-1 validation leaves it untouched.
        const SENTINEL: u64 = 0xC0FF_EE00_C0FF_EE00;
        w64(shadow + DESC_OFF, SENTINEL);

        // A good transmit chain on queue 1: header + data, wholly in-region, above every ring block.
        let data = MAX_QUEUES as u64 * RING_BLOCK; // first byte past all queues' rings
        write_desc(desc1, 0, driver + data, 60, VIRTQ_DESC_F_NEXT, 1);
        write_desc(desc1, 1, driver + data + 64, 8, 0, 0);
        w16(avail1 + 4, 0); // ring[0] = head 0
        w16(avail1 + 2, 1); // avail.idx = 1
        assert!(
            run_q(driver, size, shadow, 1, 0, 1),
            "a valid chain on queue 1 was refused",
        );

        // It landed in queue 1's shadow block, and queue 0's shadow block is untouched.
        assert_eq!(
            r64(shadow + q1 + DESC_OFF),
            driver + data,
            "queue 1's validated descriptor did not reach queue 1's shadow block",
        );
        assert_eq!(
            r64(shadow + DESC_OFF),
            SENTINEL,
            "validating queue 1 disturbed queue 0's shadow block",
        );

        // An escape on queue 1 is refused, the same confinement as queue 0.
        write_desc(desc1, 0, 0xffff_0000_4008_0000, 60, VIRTQ_DESC_F_NEXT, 1);
        assert!(
            !run_q(driver, size, shadow, 1, 0, 1),
            "a queue-1 descriptor escaping the region was NOT refused",
        );

        free_regions(driver, shadow);
    }

    /// **The IOMMU faults a DMA that escapes the domain, in hardware** (milestone 16b, both ISAs).
    ///
    /// The shadow ring proves the *software* refuses an out-of-region descriptor before the device
    /// is rung; this proves the *hardware* would stop the device even if that software were bypassed.
    /// Confine the PCIe disk to its domain, then point its available ring at a frame the domain does
    /// not map and kick it. The device reads that ring at an out-of-domain IOVA, the IOMMU faults,
    /// and the fault turns up in the fault/event queue. A pass here also means the IOMMU is really in
    /// force: if translation were absent (a missing `iommu=smmuv3` / `riscv-iommu-pci`, or the
    /// `iommu_platform=on` that puts the device behind it), the escaping read would succeed silently
    /// and no fault would appear, so the assertion fails loudly rather than passing on a fiction.
    ///
    /// Skipped only when there is no PCIe disk on the bus (nothing to confine). An IOMMU that is
    /// absent while a disk *is* present is the failure this test exists to catch, so that path
    /// asserts rather than skips.
    #[test_case]
    fn the_iommu_faults_a_dma_that_escapes_the_domain() {
        let Some(d) = crate::pci::find_block_device() else {
            // No PCIe disk attached: nothing to confine, nothing to prove. (The test runners always
            // attach one, so this branch is for a bare boot, not the parity gate.)
            return;
        };

        // The IOMMU must be up. If it is not while a PCIe disk is present, every PCIe DMA is
        // bypassing translation: fail loudly, do not skip.
        assert!(
            crate::iommu::active(),
            "a PCIe disk is present but the IOMMU is not active: DMA is bypassing translation \
             (is iommu=smmuv3 / -device riscv-iommu-pci missing from the runner?)",
        );

        // Register (and thereby confine) the device to its DMA region + shadow page.
        let dma = crate::memory::alloc().expect("no DMA frame").addr();
        // SAFETY: fresh frame via the direct map.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(dma) as *mut u8,
                0,
                frames::FRAME_SIZE as usize,
            );
        }
        let id = register(Transport::pci(&d), dma, frames::FRAME_SIZE, Some(d.rid));

        // A frame the domain does not map: the device's escape target.
        let victim = crate::memory::alloc().expect("no victim frame").addr();
        let victim_page = victim & !0xfff;

        // Drain any stale fault first, so what we observe is ours.
        while crate::iommu::take_fault().is_some() {}

        provoke_iommu_escape(id, victim);

        // Poll the fault/event queue. QEMU records the fault as it processes the kick under TCG, so a
        // bounded spin is plenty; the loop bound turns "no fault ever" into a failure, not a hang.
        let mut fault = None;
        for _ in 0..2_000_000 {
            if let Some(f) = crate::iommu::take_fault() {
                fault = Some(f);
                break;
            }
            core::hint::spin_loop();
        }

        let f = fault.expect(
            "the device read an out-of-domain address and the IOMMU recorded no fault: it is not \
             confining the device in hardware",
        );
        assert_eq!(
            f.addr & !0xfff,
            victim_page,
            "the IOMMU faulted, but on {:#x} (code {:#x}, rid {:#x}), not the escape frame {:#x}",
            f.addr,
            f.code,
            f.rid,
            victim_page,
        );
    }
}
