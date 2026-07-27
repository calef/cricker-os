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

use crate::arch::mmu::{
    self, PCI_BAR_BASE, PCI_BAR_MAPPED, PCI_ECAM_BASE, PCI_ECAM_BUSES, PCI_IRQ_BASE,
};
use pci::{Bar, Bdf, VirtioCap};

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

/// A modern virtio block device on the PCI bus, brought up: every register block resolved to a
/// physical address, memory decoding and bus mastering enabled, its INTx line known.
#[derive(Debug, Clone, Copy)]
pub struct PciBlockDevice {
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
}

/// Find the first modern virtio-blk function on the bus and bring it up. `None` if there is no
/// PCI disk (an empty bus reads all-ones and enumerates nothing).
///
/// Bring-up order matters and is deliberate:
/// 1. enumerate and size BARs while memory decoding is off (the sizing dance writes the BARs);
/// 2. assign addresses to unassigned BARs (with `-bios default`, OpenSBI has done no PCI setup,
///    so every BAR arrives zero and the kernel is the firmware here);
/// 3. parse the virtio vendor capabilities and resolve them against the assigned BARs;
/// 4. only then set Memory-Space Enable, and Bus-Master last: DMA permission is granted at the
///    final moment, after the transport the confinement layer owns is fully described.
pub fn find_block_device() -> Option<PciBlockDevice> {
    let mut found: Option<Bdf> = None;
    pci::enumerate(
        PCI_ECAM_BUSES,
        &mut |b, o| cfg_read32(b, o),
        &mut |bdf, vendor, device| {
            if found.is_none() && vendor == pci::VIRTIO_VENDOR {
                match device {
                    pci::VIRTIO_BLK_MODERN => found = Some(bdf),
                    pci::VIRTIO_BLK_TRANSITIONAL => {
                        crate::println!(
                            "  pci: virtio-blk at {:02x}:{:02x}.{} is transitional (legacy); \
                             we drive modern only",
                            bdf.bus,
                            bdf.dev,
                            bdf.func,
                        );
                    }
                    _ => {}
                }
            }
        },
    );
    let bdf = found?;

    // Size every BAR, then place the unassigned ones. Bump allocation from the window base,
    // aligned to each BAR's size (a BAR's address must be size-aligned; that is what the
    // writable-bits mask encodes).
    let mut bars = pci::read_bars(bdf, &mut |b, o| cfg_read32(b, o), &mut |b, o, v| {
        cfg_write32(b, o, v)
    });
    let mut next = PCI_BAR_BASE;
    for (i, bar) in bars.iter_mut().enumerate() {
        let Some(bar) = bar.as_mut() else { continue };
        if bar.base != 0 {
            continue; // firmware (or a previous boot stage) already placed it
        }
        let base = next.next_multiple_of(bar.size.max(0x10));
        if base + bar.size > PCI_BAR_BASE + PCI_BAR_MAPPED {
            crate::println!("  pci: BAR window exhausted; cannot place BAR{i}");
            return None;
        }
        let off = pci::BAR0 + i as u64 * 4;
        cfg_write32(bdf, off, base as u32 | if bar.is_64 { 0b100 } else { 0 });
        if bar.is_64 {
            cfg_write32(bdf, off + 4, (base >> 32) as u32);
        }
        bar.base = base;
        next = base + bar.size;
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
        crate::println!("  pci: the virtio-blk function declares no INTx pin");
        return None;
    }
    let intid = pci::intx_irq(PCI_IRQ_BASE, bdf.dev, pin);

    Some(PciBlockDevice {
        bdf,
        common,
        notify_base,
        notify_mult,
        isr,
        intid,
    })
}
