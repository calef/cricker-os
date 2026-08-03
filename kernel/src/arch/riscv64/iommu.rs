//! The RISC-V IOMMU (v1.0.1) driver: riscv's IOMMU, in front of the PCIe bus.
//!
//! Milestone 16b (DECISIONS §20, notes/iommu.md). The ratified RISC-V IOMMU is the architectural
//! twin of the SMMUv3 next door (arch/aarch64/iommu.rs), and the structural rhyme is the point of
//! building both: a **device directory table** instead of a stream table (keyed by the PCIe
//! requester id, called `device_id`), a **device context** instead of an STE+CD (the IOMMU's copy
//! of `satp`: a first-stage `iosatp` naming an Sv39 root the portable seam built), a **command
//! queue** for invalidations, and a **fault queue** where blocked transactions are recorded.
//!
//! On QEMU's `virt` board the IOMMU is itself a PCI function (`riscv-iommu-pci`, Red Hat
//! 1b36:0014): its register file lives in BAR 0, which the kernel places like any other BAR (the
//! kernel is the firmware here; see notes/pcie.md). The portable seam does that PCI legwork and
//! hands this module the register base; everything after that is this file.
//!
//! # Default deny, and one honest asymmetry
//!
//! `init` installs an all-invalid device directory before enabling anything, so a device the
//! kernel never attached faults (`DDT_ENTRY_INVALID`) instead of reaching memory. Before `init`
//! runs, though, the reset state of `ddtp` is mode `Off`, which *blocks* all transactions on this
//! IOMMU, so the pre-init window fails closed by architecture here; the SMMU needed an explicit
//! `GBPA.ABORT` for the same posture. Sv39 single-stage translation requires the U bit on every
//! leaf (a device does not "request supervisor privilege"), which is why the seam builds domains
//! with `user_data` flags; see `paging::domain`.

use crate::arch::mmu::phys_to_virt;
use crate::sync::{IrqSafeMutex, rank};

// --- Register file (offsets into BAR 0; RISC-V IOMMU spec ch. 5, QEMU's riscv-iommu-bits.h) ---
const CAPS: u64 = 0x00; // 64-bit
const FCTL: u64 = 0x08;
const DDTP: u64 = 0x10; // 64-bit
const CQB: u64 = 0x18; // 64-bit
const CQH: u64 = 0x20;
const CQT: u64 = 0x24;
const FQB: u64 = 0x28; // 64-bit
const FQH: u64 = 0x30;
const FQT: u64 = 0x34;
const CQCSR: u64 = 0x48;
const FQCSR: u64 = 0x4c;

const CAP_SV39: u64 = 1 << 9;
const CAP_MSI_FLAT: u64 = 1 << 22;

// ddtp: mode in bits [3:0], busy at 4, PPN at [53:10]. Mode 2 = one-level device directory.
const DDTP_MODE_1LVL: u64 = 2;
const DDTP_BUSY: u64 = 1 << 4;

// cqcsr/fqcsr: enable at 0, active ("on") at 16, busy at 17; error bits 8..11 for diagnostics.
const QUEUE_EN: u32 = 1 << 0;
const QUEUE_ON: u32 = 1 << 16;
const QUEUE_BUSY: u32 = 1 << 17;

// Queue geometry: the base register's bits [4:0] hold log2(entries) - 1, PPN at [53:10]. One
// frame each: 256 commands of 16 bytes; 128 fault records of 32 bytes.
const CQ_LOG2: u32 = 8;
const CMD_BYTES: u64 = 16;
const FQ_LOG2: u32 = 7;
const FQ_BYTES: u64 = 32;

// Commands: opcode bits [6:0], function bits [9:7] of dword 0.
const CMD_IOTINVAL_VMA: u64 = 1; // func 0
const CMD_IOFENCE_C: u64 = 2; // func 0
const CMD_IODIR_INVAL_DDT: u64 = 3; // func 0
const CMD_DV: u64 = 1 << 33; // IODIR: the DID field is valid
const CMD_DID_SHIFT: u32 = 40;

// Device context fields. tc.V validates the entry; fsc = iosatp: mode Sv39 (8) in [63:60], PPN
// in [43:0]. ta.PSCID in [31:12] tags this domain's entries in the IOMMU's address translation
// cache the way an ASID tags the CPU TLB.
const DC_TC_V: u64 = 1 << 0;
const FSC_MODE_SV39: u64 = 8 << 60;
const TA_PSCID_SHIFT: u32 = 12;

/// One recorded fault, in portable terms. `code` is the spec's CAUSE (e.g. 15 = store/AMO page
/// fault, the "no mapping for that IOVA" case; 258 = invalid device-directory entry). Read today
/// only by the confinement test; a production fault handler is future work.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub rid: u32,
    pub code: u32,
    pub addr: u64,
}

struct Iommu {
    base: u64,
    ddt: u64,
    /// Extended (64-byte) device contexts, because the QEMU device reports `MSI_FLAT`. The base
    /// format is 32 bytes; both are handled, decided once at init from the capabilities register.
    dc_bytes: u64,
    cq: u64,
    cq_tail: u32,
    // Read by take_fault (the confinement test); no production fault handler yet.
    #[cfg_attr(not(test), allow(dead_code))]
    fq: u64,
    #[cfg_attr(not(test), allow(dead_code))]
    fq_head: u32,
}

static IOMMU: IrqSafeMutex<Option<Iommu>> = IrqSafeMutex::new(rank::IOMMU, None);

fn r32(base: u64, off: u64) -> u32 {
    // SAFETY: the IOMMU's BAR lies inside the PCI window mapped device-typed by
    // mmu::map_everything; these register reads are side-effect-free.
    unsafe { core::ptr::read_volatile(phys_to_virt(base + off) as *const u32) }
}
fn w32(base: u64, off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + off) as *mut u32, v) }
}
fn r64(base: u64, off: u64) -> u64 {
    // SAFETY: as above; the 64-bit registers are 8-byte aligned.
    unsafe { core::ptr::read_volatile(phys_to_virt(base + off) as *const u64) }
}
fn w64(base: u64, off: u64, v: u64) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + off) as *mut u64, v) }
}

fn zeroed_frame(what: &str) -> u64 {
    let pa = crate::memory::alloc()
        .unwrap_or_else(|| panic!("no frame for the IOMMU {what}"))
        .addr();
    // SAFETY: a fresh frame, reachable through the direct map, owned by this module from here on.
    unsafe {
        core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, frames::FRAME_SIZE as usize);
    }
    pa
}

/// A queue base register value: PPN in [53:10], log2(entries)-1 in [4:0].
fn queue_base(pa: u64, log2: u32) -> u64 {
    ((pa >> 12) << 10) | (log2 as u64 - 1)
}

/// Enable a queue through its control/status register and wait for it to come on.
fn queue_enable(base: u64, csr: u64, what: &str) {
    w32(base, csr, QUEUE_EN);
    let mut spins = 0u32;
    loop {
        let v = r32(base, csr);
        if v & QUEUE_ON != 0 && v & QUEUE_BUSY == 0 {
            break;
        }
        spins += 1;
        assert!(
            spins < 1_000_000,
            "IOMMU {what} queue never came on ({v:#x})"
        );
    }
}

/// Bring the IOMMU up: queues live, an all-invalid device directory installed. From here every
/// device faults until `attach` writes its context.
pub fn init(base: u64) {
    let mut g = IOMMU.lock();
    assert!(g.is_none(), "IOMMU initialized twice");

    let caps = r64(base, CAPS);
    assert!(
        caps & CAP_SV39 != 0,
        "the IOMMU does not support Sv39 first-stage (caps {caps:#x}); the seam depends on it"
    );
    // MSI_FLAT widens the device context from 32 to 64 bytes (the MSI page-table words). QEMU's
    // riscv-iommu-pci reports it, so the extended format is what this driver actually runs; the
    // base format is kept because real silicon without MSI support will report otherwise.
    let dc_bytes = if caps & CAP_MSI_FLAT != 0 { 64 } else { 32 };

    // Little-endian, wire-signaled interrupts off (we poll both queues).
    w32(base, FCTL, 0);

    let cq = zeroed_frame("command queue");
    w64(base, CQB, queue_base(cq, CQ_LOG2));
    queue_enable(base, CQCSR, "command");

    let fq = zeroed_frame("fault queue");
    w64(base, FQB, queue_base(fq, FQ_LOG2));
    queue_enable(base, FQCSR, "fault");

    // The device directory: one page of device contexts, all invalid. Keyed by device_id, which
    // for PCIe is the requester id; one level covers ids 0..63 in the extended format, which
    // spans all of bus 0. Installing it (mode 1LVL) is what arms translation: from this write on,
    // a transaction from an unattached device is blocked with DDT_ENTRY_INVALID in the fault
    // queue rather than reaching memory.
    let ddt = zeroed_frame("device directory");
    w64(base, DDTP, DDTP_MODE_1LVL | ((ddt >> 12) << 10));
    let mut spins = 0u32;
    while r64(base, DDTP) & DDTP_BUSY != 0 {
        spins += 1;
        assert!(spins < 1_000_000, "IOMMU ddtp write never completed");
    }

    *g = Some(Iommu {
        base,
        ddt,
        dc_bytes,
        cq,
        cq_tail: 0,
        fq,
        fq_head: 0,
    });
}

/// Is the IOMMU up? The portable seam asks this to decide whether attaching is possible.
pub fn active() -> bool {
    IOMMU.lock().is_some()
}

/// Push one 16-byte command and wait for the IOMMU to consume it (QEMU processes the queue on
/// the tail write; polling the head is the architectural contract).
fn cmd_push(s: &mut Iommu, dword0: u64, dword1: u64) {
    let entries = 1u32 << CQ_LOG2;
    let idx = (s.cq_tail % entries) as u64;
    let slot = phys_to_virt(s.cq + idx * CMD_BYTES) as *mut u64;
    // SAFETY: the command queue frame is kernel-owned; idx is masked to the queue.
    unsafe {
        core::ptr::write_volatile(slot, dword0);
        core::ptr::write_volatile(slot.add(1), dword1);
    }
    // The IOMMU reads the command from memory: publish the words before moving the tail.
    crate::arch::dma_wmb();
    s.cq_tail = (s.cq_tail + 1) % (2 * entries);
    w32(s.base, CQT, s.cq_tail);

    let mut spins = 0u32;
    while r32(s.base, CQH) != s.cq_tail {
        spins += 1;
        assert!(
            spins < 1_000_000,
            "IOMMU stopped consuming commands (cqcsr {:#x})",
            r32(s.base, CQCSR),
        );
    }
}

/// Point device `rid` at the domain rooted at `root` (an Sv39 table the portable seam built),
/// tagged `pscid`, then invalidate so the IOMMU drops anything cached for the previous domain.
pub fn attach(rid: u32, root: u64, pscid: u16) {
    let mut g = IOMMU.lock();
    let s = g.as_mut().expect("IOMMU attach before init");
    let per_page = frames::FRAME_SIZE / s.dc_bytes;
    assert!(
        (rid as u64) < per_page,
        "device_id {rid} beyond the one-level device directory"
    );

    // The device context, written back to front: address and tag words first, the valid bit
    // last, with a barrier between, so the IOMMU can never observe a valid entry with a stale
    // root. iohgatp stays Bare (no second stage; there is no guest here), and the MSI words stay
    // zero (MSI mode Off).
    let dc = phys_to_virt(s.ddt + rid as u64 * s.dc_bytes) as *mut u64;
    // SAFETY: ddt is a kernel-owned frame; rid is bounds-checked above.
    unsafe {
        core::ptr::write_volatile(dc.add(1), 0); // iohgatp: Bare
        core::ptr::write_volatile(dc.add(2), (pscid as u64) << TA_PSCID_SHIFT); // ta
        core::ptr::write_volatile(dc.add(3), FSC_MODE_SV39 | (root >> 12)); // fsc = iosatp
        if s.dc_bytes == 64 {
            for i in 4..8 {
                core::ptr::write_volatile(dc.add(i), 0); // msiptp, mask, pattern, reserved
            }
        }
    }
    crate::arch::dma_wmb();
    // SAFETY: as above.
    unsafe {
        core::ptr::write_volatile(dc, DC_TC_V); // tc: valid, faults reported (DTF clear)
    }
    crate::arch::dma_wmb();

    // Invalidate the cached context and every translation, then fence: the IOMMU's IODIR covers
    // its device-context cache, IOTINVAL.VMA (no address, no PSCID: everything) its address
    // translation cache, IOFENCE.C orders both before any later transaction.
    cmd_push(
        s,
        CMD_IODIR_INVAL_DDT | CMD_DV | ((rid as u64) << CMD_DID_SHIFT),
        0,
    );
    cmd_push(s, CMD_IOTINVAL_VMA, 0);
    cmd_push(s, CMD_IOFENCE_C, 0);
}

/// Pop one fault from the fault queue, if any. The confinement test drains this to prove a DMA
/// escape was stopped by the hardware, not merely absent.
#[cfg_attr(not(test), allow(dead_code))]
pub fn take_fault() -> Option<Fault> {
    let mut g = IOMMU.lock();
    let s = g.as_mut()?;
    let entries = 1u32 << FQ_LOG2;
    let tail = r32(s.base, FQT);
    if tail == s.fq_head {
        return None;
    }
    let idx = (s.fq_head % entries) as u64;
    let rec = phys_to_virt(s.fq + idx * FQ_BYTES) as *const u64;
    // SAFETY: the fault queue frame is kernel-owned; the IOMMU wrote this record before moving
    // the tail, and idx is masked to the queue.
    let (hdr, iotval) = unsafe {
        (
            core::ptr::read_volatile(rec),
            core::ptr::read_volatile(rec.add(2)),
        )
    };
    s.fq_head = (s.fq_head + 1) % (2 * entries);
    w32(s.base, FQH, s.fq_head);
    Some(Fault {
        rid: (hdr >> 40) as u32 & 0xff_ffff, // DID, bits [63:40]
        code: (hdr & 0xfff) as u32,          // CAUSE, bits [11:0]
        addr: iotval,
    })
}
