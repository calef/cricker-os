//! PCIe **enumeration and virtio-pci bring-up**. Not a driver.
//!
//! The PCIe analog of virtio.rs's discovery half, and the same division of labor: the kernel
//! walks the bus (a bootstrap role), finds the block device, brings its register blocks up, and
//! hands a userspace driver a confined transport capability. The decode logic (ECAM addressing,
//! BAR sizing, capability parsing) lives in the host-tested `pci` crate; this module supplies
//! only the volatile accessors into the mapped ECAM window and the policy: which device to pick,
//! where BARs go, which command bits to set. See notes/pcie.md and notes/pcie-transport-scope.md.
//!
//! Portable: everything here except the `arch::mmu` window/irq constants is architecture-
//! neutral, and both `virt` boards expose the same `pci-host-ecam-generic` bridge. On riscv the
//! INTx lines route to the PLIC (32..35), on aarch64 to GIC SPIs (INTIDs 35..38); each arch's
//! constants say so, and host-run witnesses hold them against the machine's own device tree.

use core::sync::atomic::{AtomicU64, Ordering};

use pci::{Bar, Bdf, VirtioCap};

use crate::arch::mmu::{
    self, PCI_BAR_BASE, PCI_BAR_MAPPED, PCI_ECAM_BASE, PCI_ECAM_BUSES, PCI_IRQ_BASE,
};

/// The shared bump cursor for kernel-assigned BARs. With `-bios default` the kernel is the PCI
/// firmware (OpenSBI does no PCI setup), so every BAR arrives zero and the kernel places it. More
/// than one function now needs placing: the virtio disk, and on riscv the IOMMU (itself a PCI
/// function, milestone 16b). Two independent bump allocators starting from `PCI_BAR_BASE` would
/// hand out the same address twice, so the cursor is one shared value that only ever advances.
/// Enumeration runs on the boot hart alone; the atomic is for correctness of the shared state, not
/// contention.
static BAR_NEXT: AtomicU64 = AtomicU64::new(PCI_BAR_BASE);

/// Place every unassigned BAR of `bdf` at a size-aligned address drawn from the shared cursor,
/// writing the config-space BAR registers. Returns false (after saying so) if the window is
/// exhausted. A BAR that already carries an address (nonzero) is left alone.
fn place_bars(bdf: Bdf, bars: &mut [Option<Bar>; 6]) -> bool {
    for (i, bar) in bars.iter_mut().enumerate() {
        let Some(bar) = bar.as_mut() else { continue };
        if bar.base != 0 {
            continue; // firmware (or a previous boot stage) already placed it
        }
        // A BAR's address must be aligned to its size (the writable-bits mask encodes that). Reserve
        // size-aligned space from the shared cursor.
        let align = bar.size.max(0x10);
        let base = loop {
            let cur = BAR_NEXT.load(Ordering::Relaxed);
            let base = cur.next_multiple_of(align);
            if base + bar.size > PCI_BAR_BASE + PCI_BAR_MAPPED {
                crate::println!("  pci: BAR window exhausted; cannot place BAR{i}");
                return false;
            }
            if BAR_NEXT
                .compare_exchange(cur, base + bar.size, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break base;
            }
        };
        let off = pci::BAR0 + i as u64 * 4;
        cfg_write32(bdf, off, base as u32 | if bar.is_64 { 0b100 } else { 0 });
        if bar.is_64 {
            cfg_write32(bdf, off + 4, (base >> 32) as u32);
        }
        bar.base = base;
    }
    true
}

fn cfg_read32(bdf: Bdf, off: u64) -> u32 {
    let va = mmu::phys_to_virt(PCI_ECAM_BASE + bdf.ecam_offset() + (off & !3));
    // SAFETY: the ECAM window for the buses we enumerate is device-mapped (mmu::map_everything
    // region 9), and ECAM config reads are side-effect-free.
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

fn cfg_write32(bdf: Bdf, off: u64, v: u32) {
    let va = mmu::phys_to_virt(PCI_ECAM_BASE + bdf.ecam_offset() + (off & !3));
    // SAFETY: as above; config writes go to the one function this bdf names.
    unsafe { core::ptr::write_volatile(va as *mut u32, v) }
}

/// A modern virtio device on the PCI bus, brought up: every register block resolved to a
/// physical address, memory decoding and bus mastering enabled, its INTx line known. Device-neutral
/// (a disk and a NIC differ only in the driver above this), so both `find_block_device` and
/// `find_net_device` return it.
#[derive(Debug, Clone, Copy)]
pub struct PciVirtioDevice {
    /// Which function this is, for the boot tours' prints; every other build (tests, the shell
    /// and bench boots) drives the device without ever naming it, so the lint is quieted rather
    /// than chased through four cfg combinations.
    #[allow(dead_code)]
    pub bdf: Bdf,
    /// The virtio common-config block (queue setup, status, features), physical.
    pub common: u64,
    /// The notify region base and the per-queue multiplier; queue N's doorbell is at
    /// `notify_base + queue_notify_off(N) * notify_mult`.
    pub notify_base: u64,
    pub notify_mult: u32,
    /// The ISR byte (read-to-ack), physical.
    pub isr: u64,
    /// The PLIC input its INTx pin routes to (the standard swizzle; see `pci::intx_irq`).
    pub intid: u32,
    /// The PCIe requester id (`bus:8 | dev:5 | fn:3`), the id the IOMMU keys its per-device tables
    /// on (milestone 16b). Carried through to `virtio::register` so the device is confined to its
    /// DMA region in hardware.
    pub rid: u32,
    /// The **virtio device type** (2 = block, 1 = net, 16 = gpu), recovered from the PCI device id
    /// (`0x1040 + type`). PCI config space has no `DeviceID` register in the virtio-mmio sense, so
    /// the transport answers a driver's `DeviceID` read from this instead of a hardcoded value; a
    /// GPU driver that sanity-checks what it is talking to then gets the truth on either bus.
    pub device_type: u32,
}

/// Find the first modern virtio-blk function on the bus and bring it up. `None` if there is no
/// PCI disk (an empty bus reads all-ones and enumerates nothing).
pub fn find_block_device() -> Option<PciVirtioDevice> {
    let bdf = find_virtio_bdf(
        pci::VIRTIO_BLK_MODERN,
        Some(pci::VIRTIO_BLK_TRANSITIONAL),
        "virtio-blk",
    )?;
    bring_up(bdf, pci::VIRTIO_TYPE_BLOCK)
}

/// Find the first modern virtio-net function on the bus and bring it up. `None` if there is no
/// PCI NIC. Same bring-up as the disk; the transport seam and the DMA confinement do not know or
/// care that a NIC sits behind them (milestone 30).
pub fn find_net_device() -> Option<PciVirtioDevice> {
    let bdf = find_virtio_bdf(
        pci::VIRTIO_NET_MODERN,
        Some(pci::VIRTIO_NET_TRANSITIONAL),
        "virtio-net",
    )?;
    bring_up(bdf, pci::VIRTIO_TYPE_NET)
}

/// Find the first modern virtio-gpu function on the bus and bring it up (milestone 29, the display
/// ladder's first rung). `None` if there is no PCI GPU.
///
/// **PCIe only, on purpose and on both boards.** A virtio-gpu is not on either `virt` machine's
/// virtio-mmio bus in any configuration we attach, so unlike the disk and the NIC there is no mmio
/// twin to prove parity over; the parity that matters here is aarch64 `virt` and riscv `virt`, both
/// of which carry `virtio-gpu-pci` over the §18 transport. Same bring-up as the disk, and the same
/// hardware confinement: the device is entered behind the IOMMU at `virtio::register`.
pub fn find_gpu_device() -> Option<PciVirtioDevice> {
    // No transitional twin exists for virtio-gpu (the legacy id range predates the device), so this
    // asks for the modern id and nothing else rather than warning about a legacy device that cannot
    // be on the bus.
    let bdf = find_virtio_bdf(pci::VIRTIO_GPU_MODERN, None, "virtio-gpu")?;
    bring_up(bdf, pci::VIRTIO_TYPE_GPU)
}

/// Find the first modern virtio-input function on the bus and bring it up (milestone 29's keyboard).
/// `None` if there is no PCI input device.
///
/// PCIe only, and here that **is** a choice rather than a constraint: unlike the GPU, both `virt`
/// machines do offer a `virtio-keyboard-device` on the virtio-mmio bus. The keyboard rides PCIe
/// anyway so it sits behind the same IOMMU domain the GPU does and the display's two devices are
/// confined the same way. A keyboard is the one device whose DMA a user would least like
/// unconfined: its buffers are where every keystroke lands.
pub fn find_input_device() -> Option<PciVirtioDevice> {
    let bdf = find_virtio_bdf(pci::VIRTIO_INPUT_MODERN, None, "virtio-input")?;
    bring_up(bdf, pci::VIRTIO_TYPE_INPUT)
}

/// Find the first modern virtio-rng function on the bus and bring it up (milestone 56's entropy
/// source). `None` if there is no PCI RNG.
///
/// The mmio twin is [`crate::virtio::find_entropy_device`], and both exist because both machines
/// offer both: the entropy service is wired over whichever one the wiring asked for, and the
/// milestone-56 test runs it over each in turn. An RNG behind the IOMMU is worth having for the
/// same reason a keyboard is: its buffer is the one place in memory whose contents must not be
/// guessable, and an unconfined device could write it anywhere and read the rest.
#[cfg_attr(not(test), allow(dead_code))] // entropy_service is the caller, and the m56 tests drive it
pub fn find_rng_device() -> Option<PciVirtioDevice> {
    let bdf = find_virtio_bdf(
        pci::VIRTIO_RNG_MODERN,
        Some(pci::VIRTIO_RNG_TRANSITIONAL),
        "virtio-rng",
    )?;
    bring_up(bdf, pci::VIRTIO_TYPE_ENTROPY)
}

/// How many modern virtio-blk functions are on the bus (milestone 57's roster).
///
/// **Counting is not bringing up.** [`find_block_device`] resolves a transport, which means sizing
/// and assigning BARs and enabling memory decoding: side effects a *listing* has no business
/// causing, and ones that would disturb whichever driver already owns the function. This reads
/// config space and nothing else, which is all a roster is entitled to know
/// (`crates/block_roster`).
pub fn count_block_devices() -> usize {
    let mut n = 0;
    pci::enumerate(
        PCI_ECAM_BUSES,
        &mut |b, o| cfg_read32(b, o),
        &mut |_, vendor, device| {
            if vendor == pci::VIRTIO_VENDOR && device == pci::VIRTIO_BLK_MODERN {
                n += 1;
            }
        },
    );
    n
}

/// Enumerate the bus for the first function matching `modern`, warning (once) if only a
/// `transitional` (legacy) twin is present, since we drive modern only. `kind` names the device for
/// that warning. `transitional` is `None` for a device type that has no legacy id at all
/// (virtio-gpu): there is then no such warning to give, and inventing an id to compare against
/// would be a fact nobody checked.
fn find_virtio_bdf(modern: u16, transitional: Option<u16>, kind: &str) -> Option<Bdf> {
    let mut found: Option<Bdf> = None;
    pci::enumerate(
        PCI_ECAM_BUSES,
        &mut |b, o| cfg_read32(b, o),
        &mut |bdf, vendor, device| {
            if found.is_none() && vendor == pci::VIRTIO_VENDOR {
                if device == modern {
                    found = Some(bdf);
                } else if Some(device) == transitional {
                    crate::println!(
                        "  pci: {kind} at {:02x}:{:02x}.{} is transitional (legacy); \
                         we drive modern only",
                        bdf.bus,
                        bdf.dev,
                        bdf.func,
                    );
                }
            }
        },
    );
    found
}

/// Bring an enumerated virtio function up and resolve its transport. Bring-up order matters and is
/// deliberate:
/// 1. size BARs while memory decoding is off (the sizing dance writes the BARs);
/// 2. assign addresses to unassigned BARs (with `-bios default`, OpenSBI has done no PCI setup,
///    so every BAR arrives zero and the kernel is the firmware here);
/// 3. parse the virtio vendor capabilities and resolve them against the assigned BARs;
/// 4. only then set Memory-Space Enable, and Bus-Master last: DMA permission is granted at the
///    final moment, after the transport the confinement layer owns is fully described.
fn bring_up(bdf: Bdf, device_type: u32) -> Option<PciVirtioDevice> {
    // Size every BAR, then place the unassigned ones from the shared cursor (the IOMMU function
    // draws from the same cursor on riscv, so the two cannot overlap).
    let mut bars = pci::read_bars(bdf, &mut |b, o| cfg_read32(b, o), &mut |b, o, v| {
        cfg_write32(b, o, v);
    });
    if !place_bars(bdf, &mut bars) {
        return None;
    }

    // The virtio vendor capabilities name (bar, offset) pairs; resolve them to physical
    // addresses against the now-assigned BARs.
    let resolve = |bars: &[Option<Bar>; 6], cap: &VirtioCap| -> Option<u64> {
        let bar = bars.get(cap.bar as usize)?.as_ref()?;
        (u64::from(cap.offset) + u64::from(cap.length) <= bar.size)
            .then(|| bar.base + u64::from(cap.offset))
    };
    let (mut common, mut notify_base, mut notify_mult, mut isr) = (None, None, 0u32, None);
    pci::virtio_caps(
        bdf,
        &mut |b, o| cfg_read32(b, o),
        &mut |cap| match cap.cfg_type {
            pci::VIRTIO_CAP_COMMON if common.is_none() => common = resolve(&bars, &cap),
            pci::VIRTIO_CAP_NOTIFY if notify_base.is_none() => {
                notify_base = resolve(&bars, &cap);
                notify_mult = cap.notify_off_multiplier;
            }
            pci::VIRTIO_CAP_ISR if isr.is_none() => isr = resolve(&bars, &cap),
            _ => {}
        },
    );
    let (common, notify_base, isr) = (common?, notify_base?, isr?);

    // Memory-Space Enable so the BARs decode; Bus-Master Enable so the device may DMA at all.
    // The upper half of this dword is the STATUS register, whose error bits are write-1-to-clear,
    // so it is written as zero: clearing nothing, changing nothing.
    let cmd = cfg_read32(bdf, pci::COMMAND) as u16;
    cfg_write32(
        bdf,
        pci::COMMAND,
        (cmd | pci::CMD_MEMORY_SPACE | pci::CMD_BUS_MASTER) as u32,
    );

    // The INTx pin (config 0x3d; 1=INTA..4=INTD, 0=none), through the board's swizzle to a PLIC
    // input. The dtb fixture test holds the swizzle against the machine's own interrupt-map.
    let pin = ((cfg_read32(bdf, 0x3c) >> 8) & 0xff) as u8;
    if pin == 0 {
        crate::println!(
            "  pci: the virtio function at {:02x} declares no INTx pin",
            bdf.dev
        );
        return None;
    }
    let intid = pci::intx_irq(PCI_IRQ_BASE, bdf.dev, pin);

    Some(PciVirtioDevice {
        bdf,
        common,
        notify_base,
        notify_mult,
        isr,
        intid,
        rid: bdf.requester_id(),
        device_type,
    })
}

/// The QEMU `riscv-iommu-pci` function's PCI identity (Red Hat vendor, RISC-V IOMMU device id).
/// riscv-only: aarch64's SMMUv3 is a device-tree platform node, not a PCI function, and x86's
/// IOMMUs are discovered through ACPI, so a PCI-function IOMMU is a RISC-V shape today.
#[cfg(target_arch = "riscv64")]
const IOMMU_VENDOR: u16 = 0x1b36;
#[cfg(target_arch = "riscv64")]
const IOMMU_DEVICE: u16 = 0x0014;

/// **Bring up the RISC-V IOMMU, if the machine has one** (milestone 16b). Unlike the aarch64
/// SMMUv3 (a platform device the kernel finds in the device tree), the ratified RISC-V IOMMU is
/// itself a PCI function: the kernel enumerates it, places its BAR0 (the register file) from the
/// same shared cursor as every other BAR, enables memory-space decoding, and hands the base to the
/// arch driver's `init`. No such function on the bus (a plain `virt` boot, or aarch64), no-op.
///
/// Its own registers are reached by the CPU as MMIO, so it needs Memory-Space Enable but not
/// Bus-Master: the IOMMU is not a DMA initiator, it is the thing that polices them.
#[cfg(target_arch = "riscv64")]
pub fn init_iommu() {
    let mut found: Option<Bdf> = None;
    pci::enumerate(
        PCI_ECAM_BUSES,
        &mut |b, o| cfg_read32(b, o),
        &mut |bdf, vendor, device| {
            if found.is_none() && vendor == IOMMU_VENDOR && device == IOMMU_DEVICE {
                found = Some(bdf);
            }
        },
    );
    let Some(bdf) = found else { return };

    let mut bars = pci::read_bars(bdf, &mut |b, o| cfg_read32(b, o), &mut |b, o, v| {
        cfg_write32(b, o, v);
    });
    if !place_bars(bdf, &mut bars) {
        crate::println!("  pci: could not place the IOMMU's BAR; leaving the IOMMU off");
        return;
    }
    let Some(base) = bars[0].as_ref().map(|b| b.base) else {
        crate::println!("  pci: the IOMMU function has no BAR0; leaving the IOMMU off");
        return;
    };

    // Memory-Space Enable so BAR0 decodes and the register reads below land. No Bus-Master: the
    // IOMMU does not itself DMA.
    let cmd = cfg_read32(bdf, pci::COMMAND) as u16;
    cfg_write32(bdf, pci::COMMAND, (cmd | pci::CMD_MEMORY_SPACE) as u32);

    crate::arch::iommu::init(base);
}
