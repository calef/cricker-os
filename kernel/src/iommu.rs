//! **The portable IOMMU seam** (milestone 16b; DECISIONS §20, notes/iommu.md).
//!
//! One IOMMU confines a device by translating every address it emits through page tables the
//! kernel programs, in the CPU's own page-table format. aarch64 has an SMMUv3 that walks
//! VMSAv8-64; riscv has the ratified RISC-V IOMMU (v1.0.1) that walks Sv39. Those are the two
//! formats the `paging` crate already builds for process address spaces, so a device's DMA domain
//! is not a new kind of table (`paging::domain::build_identity_domain`). This module is the thin,
//! architecture-neutral layer that ties that portable domain builder to whichever arch IOMMU
//! driver is compiled in (`crate::arch::iommu`): pick the frame allocator and the format, build
//! the domain, hand its root to the driver's `attach`. Everything ISA-specific (the register file,
//! the stream/device table, the command and fault queues) lives under `arch/`, per rule #1.
//!
//! # What "confine" does, and why it fails closed
//!
//! `confine(rid, regions)` builds an identity map (IOVA == PA) over exactly `regions` and nothing
//! else, then attaches the device whose requester id is `rid` to it. From that point the device
//! reaches precisely the frames in `regions`; any other address it emits has no mapping, so the
//! IOMMU faults instead of touching memory. Before `confine` runs for a device, the driver's
//! `init` has already installed an all-invalid stream table / device directory, so an unattached
//! device is denied by default. The window between "the bus grants Bus-Master Enable" and "the
//! kernel confines the device" is therefore closed by the hardware, not by careful ordering.

use crate::arch::mmu::{DmaFormat, phys_to_virt};
use paging::domain::{DmaRegion, build_identity_domain};

// The fault-reporting surface, re-exported as the portable interface. `take_fault` is drained by
// the confinement test (kernel/src/virtio.rs); `Fault` is its public return type. Neither is used
// by a production fault handler yet (that routing is future work), and the test reads the fault's
// fields without ever naming the type, so both re-exports are allowed to be locally unused.
#[allow(unused_imports)]
pub use crate::arch::iommu::{Fault, take_fault};

/// Is an IOMMU present and initialized on this machine? False on a `virt` boot without
/// `iommu=smmuv3` (aarch64) or without a `riscv-iommu-pci` function (riscv), where the kernel
/// runs exactly as it did before this milestone. The confinement path checks this before
/// attaching a device, so a machine with no IOMMU keeps working (with only the software shadow
/// ring for DMA defence, now demoted to defence in depth; see notes/dma.md).
pub fn active() -> bool {
    crate::arch::iommu::active()
}

/// Allocate one zeroed frame and return its physical address. The domain's root table and every
/// intermediate table come from here. These frames are owned by the IOMMU from now on; a re-attach
/// of the same device leaks the previous domain's tables, which is acceptable because `confine`
/// runs once per device per boot (see notes/iommu.md).
fn zeroed_frame() -> u64 {
    let pa = crate::memory::alloc()
        .expect("no frame for an IOMMU DMA domain")
        .addr();
    // SAFETY: a fresh frame, reachable through the direct map, owned by this module from here on.
    unsafe {
        core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, frames::FRAME_SIZE as usize);
    }
    pa
}

/// **Confine PCI device `rid` to exactly `regions`.** Builds a DMA domain in this architecture's
/// page-table format (the seam's whole point: one call, `DmaFormat` picks VMSAv8-64 on aarch64 and
/// Sv39 on riscv) and attaches the device to it. After this returns, the device faults on any
/// address outside `regions`.
///
/// `rid` is the PCIe requester id (bus/dev/fn), which both IOMMUs key their tables on (each `virt`
/// board's device tree gives an identity `iommu-map`). The caller must have checked [`active`];
/// attaching before the driver's `init` panics.
pub fn confine(rid: u32, regions: &[DmaRegion]) {
    let root = zeroed_frame();
    // Build the identity domain over `regions`. The frame allocator and the pointer projection are
    // the same the kernel's own `Mapper` uses; `DmaFormat` selects the format for this ISA.
    // SAFETY: `root` is a freshly zeroed, page-aligned frame; `zeroed_frame` returns the same for
    // every intermediate table; `phys_to_virt` yields a pointer the Mapper contract accepts. Nobody
    // installs `root` until `attach` below, which happens only after this returns Ok.
    unsafe {
        build_identity_domain::<_, _, DmaFormat>(
            root,
            || Some(zeroed_frame()),
            |pa| phys_to_virt(pa) as *mut paging::PageTable,
            regions,
        )
        .expect("IOMMU DMA domain build failed");
    }
    // The tables are read by the IOMMU, a separate observer: publish them before it is pointed at
    // the root. The driver's `attach` issues its own invalidation + sync after installing the STE /
    // device context, so a stale cached entry cannot survive either.
    crate::arch::dma_wmb();

    // The domain's cache tag (ASID on aarch64, PSCID on riscv). One per device; the requester id is
    // unique per device and never zero for a real PCI function (dev >= 1), so it is a fine tag.
    crate::arch::iommu::attach(rid, root, rid as u16);
}

/// The physical regions a virtio device's domain must cover: the driver's DMA region (its rings'
/// used half and its data buffers) and the kernel-private shadow page (the descriptor table and
/// available ring the device actually reads). Both are frame-granular. See notes/dma.md for why the
/// device reads a shadow the driver cannot write.
pub fn virtio_regions(dma_base: u64, dma_size: u64, shadow_base: u64) -> [DmaRegion; 2] {
    [
        DmaRegion {
            base: dma_base,
            size: dma_size,
        },
        DmaRegion {
            base: shadow_base,
            size: frames::FRAME_SIZE,
        },
    ]
}
