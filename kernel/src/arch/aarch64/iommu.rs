//! The SMMUv3 driver: aarch64's IOMMU, in front of the PCIe bus.
//!
//! Milestone 16b (DECISIONS §20, notes/iommu.md). The SMMU sits between PCIe devices and memory
//! and translates every address a device emits through per-stream tables the kernel programs. The
//! tables themselves are ordinary VMSAv8-64 page tables built by the portable seam
//! (`paging::domain` via `crate::iommu`); this module owns everything architecture-specific: the
//! register file, the stream table, the context descriptor, and the two in-memory queues the
//! SMMU is driven by (commands in, fault events out).
//!
//! # The shape of the hardware
//!
//! Software talks to an SMMUv3 almost entirely through **memory**, not registers:
//!
//! - The **stream table** maps a StreamID (for PCIe: the requester id, bus/dev/fn) to a Stream
//!   Table Entry. The STE says what happens to that device's transactions: abort, bypass, or
//!   translate via a **context descriptor**.
//! - The **context descriptor** (CD) is the SMMU's copy of what TTBR0_EL1 + TCR_EL1 + MAIR_EL1
//!   are to the CPU: the table root, the size/granule geometry, the ASID, the memory-attribute
//!   palette. Stage-1 translation through a CD walks the same descriptor format the CPU walks,
//!   which is why the `paging` crate builds the tables.
//! - The **command queue** is how configuration changes take effect: the SMMU caches STEs, CDs
//!   and TLB entries, and software pushes invalidation commands (then a sync) to make it re-read.
//! - The **event queue** is where the SMMU reports faults: a device that emitted an address its
//!   domain does not map lands here as an `F_TRANSLATION` event instead of touching memory.
//!
//! # Default deny
//!
//! `init` sets `GBPA.ABORT` (so traffic aborts even in the window before the SMMU is enabled),
//! points the SMMU at an all-invalid stream table, and only then sets `SMMUEN`. From that moment
//! every stream aborts until `attach` writes its STE. A device the kernel never attached cannot
//! DMA at all, which is the loud version of "a device without `iommu_platform=on` silently
//! bypasses": the bypass hole is closed on the QEMU side per device, and the abort default is
//! what the kernel can enforce from its side. See notes/iommu.md for the honest limits.

use crate::arch::mmu::phys_to_virt;
use crate::sync::{IrqSafeMutex, rank};

// --- Register file (offsets from the SMMU page; Arm IHI 0070, and QEMU's smmuv3-common.h) ---
const IDR1: u64 = 0x04;
const CR0: u64 = 0x20;
const CR0ACK: u64 = 0x24;
const CR1: u64 = 0x28;
const CR2: u64 = 0x2c;
const GBPA: u64 = 0x44;
const GERROR: u64 = 0x60;
const STRTAB_BASE: u64 = 0x80; // 64-bit
const STRTAB_BASE_CFG: u64 = 0x88;
const CMDQ_BASE: u64 = 0x90; // 64-bit
const CMDQ_PROD: u64 = 0x98;
const CMDQ_CONS: u64 = 0x9c;
const EVENTQ_BASE: u64 = 0xa0; // 64-bit
const EVENTQ_PROD: u64 = 0xa8;
const EVENTQ_CONS: u64 = 0xac;

// CR0 bits. Each write must be confirmed by the same bit appearing in CR0ACK.
const CR0_SMMUEN: u32 = 1 << 0;
const CR0_EVENTQEN: u32 = 1 << 2;
const CR0_CMDQEN: u32 = 1 << 3;

// GBPA: what happens to traffic while SMMUEN is 0. ABORT makes that window fail closed.
const GBPA_ABORT: u32 = 1 << 20;
const GBPA_UPDATE: u32 = 1 << 31;

// Queue geometry. A queue's base register carries LOG2SIZE in bits [4:0]; PROD/CONS carry an
// index plus a wrap bit at bit LOG2SIZE. 256 commands of 16 bytes and 128 events of 32 bytes are
// each exactly one frame.
const CMDQ_LOG2: u32 = 8;
const CMD_BYTES: u64 = 16;
const EVTQ_LOG2: u32 = 7;
#[cfg_attr(not(test), allow(dead_code))] // used by take_fault, which only the confinement test calls
const EVT_BYTES: u64 = 32;

// The linear stream table: 64 STEs of 64 bytes, one frame. StreamID on this board is the PCIe
// requester id (the DTB's iommu-map is identity), so 64 covers bus 0's 32 devices twice over.
const STRTAB_LOG2: u32 = 6;
const STE_BYTES: u64 = 64;

// Commands (opcode in byte 0 of word 0).
const CMD_CFGI_STE: u32 = 0x03;
const CMD_CFGI_CD: u32 = 0x05;
const CMD_TLBI_NSNH_ALL: u32 = 0x30;
const CMD_SYNC: u32 = 0x46;

/// One recorded fault, in portable terms. `code` is the SMMU event type (0x10 is F_TRANSLATION,
/// the "no mapping for that IOVA" event the confinement test expects). Read today only by the
/// confinement test; a production fault handler is future work, hence dead outside the test build.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub rid: u32,
    pub code: u32,
    pub addr: u64,
}

struct Smmu {
    base: u64,
    strtab: u64,
    /// One frame of CDs, one 64-byte CD per stream table entry index. Re-attaching a stream
    /// rewrites its CD in place and invalidates, so repeated test runs converge instead of
    /// leaking a frame per attach.
    cds: u64,
    cmdq: u64,
    cmdq_prod: u32,
    // Read by take_fault (the confinement test); no production fault handler yet.
    #[cfg_attr(not(test), allow(dead_code))]
    evtq: u64,
    #[cfg_attr(not(test), allow(dead_code))]
    evtq_cons: u32,
}

static SMMU: IrqSafeMutex<Option<Smmu>> = IrqSafeMutex::new(rank::IOMMU, None);

fn r32(base: u64, off: u64) -> u32 {
    // SAFETY: the SMMU register page is device-mapped (mmu::map_everything region 9, gated on the
    // DTB node); reads of these registers are side-effect-free.
    unsafe { core::ptr::read_volatile(phys_to_virt(base + off) as *const u32) }
}
fn w32(base: u64, off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + off) as *mut u32, v) }
}
fn w64(base: u64, off: u64, v: u64) {
    // SAFETY: as above; the 64-bit registers are 8-byte aligned and QEMU accepts 8-byte stores.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + off) as *mut u64, v) }
}

/// Allocate one zeroed frame and return its physical address. Queue memory, tables.
fn zeroed_frame(what: &str) -> u64 {
    let pa = crate::memory::alloc()
        .unwrap_or_else(|| panic!("no frame for the SMMU {what}"))
        .addr();
    // SAFETY: a fresh frame, reachable through the direct map, owned by this module from here on.
    unsafe {
        core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, frames::FRAME_SIZE as usize);
    }
    pa
}

/// Set bits in CR0 and wait for CR0ACK to confirm. The ack register exists because CR0 changes
/// have global effect and the SMMU may complete them asynchronously; on QEMU the ack is
/// immediate, but polling is the architectural contract.
fn cr0_set(base: u64, bits: u32) {
    let cur = r32(base, CR0);
    w32(base, CR0, cur | bits);
    let mut spins = 0u32;
    while r32(base, CR0ACK) & bits != bits {
        spins += 1;
        assert!(
            spins < 1_000_000,
            "SMMU never acknowledged CR0 |= {bits:#x}"
        );
    }
}

/// Bring the SMMU up: queues live, stream table installed all-invalid, translation enabled.
/// `base` is the register page from the device tree. Everything aborts until `attach`.
pub fn init(base: u64) {
    let mut g = SMMU.lock();
    assert!(g.is_none(), "SMMU initialized twice");

    // Fail closed even before SMMUEN: without this, the reset state of GBPA lets every
    // transaction bypass translation while SMMUEN is 0. UPDATE latches the new value.
    w32(base, GBPA, GBPA_UPDATE | GBPA_ABORT);
    let mut spins = 0u32;
    while r32(base, GBPA) & GBPA_UPDATE != 0 {
        spins += 1;
        assert!(spins < 1_000_000, "SMMU GBPA update never completed");
    }

    let idr1 = r32(base, IDR1);
    let cmdqs_max = (idr1 >> 21) & 0x1f;
    let evtqs_max = (idr1 >> 16) & 0x1f;
    assert!(
        cmdqs_max >= CMDQ_LOG2 && evtqs_max >= EVTQ_LOG2,
        "SMMU queues smaller than expected (IDR1 {idr1:#x})"
    );

    let strtab = zeroed_frame("stream table");
    let cds = zeroed_frame("context descriptors");
    let cmdq = zeroed_frame("command queue");
    let evtq = zeroed_frame("event queue");

    // Linear stream table (FMT 0), 2^6 entries. An all-zero STE is invalid, and an invalid STE
    // aborts the transaction and records C_BAD_STE: the default-deny posture.
    w64(base, STRTAB_BASE, strtab);
    w32(base, STRTAB_BASE_CFG, STRTAB_LOG2);

    w64(base, CMDQ_BASE, cmdq | CMDQ_LOG2 as u64);
    w32(base, CMDQ_PROD, 0);
    w32(base, CMDQ_CONS, 0);
    w64(base, EVENTQ_BASE, evtq | EVTQ_LOG2 as u64);
    w32(base, EVENTQ_PROD, 0);
    w32(base, EVENTQ_CONS, 0);

    // Table/queue access attributes; zero (non-cacheable) is always legal and QEMU ignores it.
    w32(base, CR1, 0);
    w32(base, CR2, 0);

    cr0_set(base, CR0_CMDQEN);
    cr0_set(base, CR0_EVENTQEN);
    cr0_set(base, CR0_SMMUEN);

    *g = Some(Smmu {
        base,
        strtab,
        cds,
        cmdq,
        cmdq_prod: 0,
        evtq,
        evtq_cons: 0,
    });
}

/// Is the SMMU up? The portable seam asks this to decide whether attaching is possible.
pub fn active() -> bool {
    SMMU.lock().is_some()
}

/// Push one 16-byte command and (for our uses) wait for the SMMU to consume it. The queue is far
/// larger than any burst we issue, so treating every push as synchronous keeps the driver simple;
/// QEMU consumes on the PROD write.
fn cmd_push(s: &mut Smmu, cmd: [u32; 4]) {
    let idx = (s.cmdq_prod & ((1 << CMDQ_LOG2) - 1)) as u64;
    let slot = phys_to_virt(s.cmdq + idx * CMD_BYTES) as *mut u32;
    // SAFETY: the command queue frame is kernel-owned; idx is masked to the queue.
    unsafe {
        for (i, w) in cmd.iter().enumerate() {
            core::ptr::write_volatile(slot.add(i), *w);
        }
    }
    // The SMMU reads the command by DMA from its own viewpoint: publish the words before PROD.
    crate::arch::dma_wmb();
    s.cmdq_prod = (s.cmdq_prod + 1) & ((1 << (CMDQ_LOG2 + 1)) - 1);
    w32(s.base, CMDQ_PROD, s.cmdq_prod);

    let mut spins = 0u32;
    while r32(s.base, CMDQ_CONS) & ((1 << (CMDQ_LOG2 + 1)) - 1) != s.cmdq_prod {
        spins += 1;
        assert!(
            spins < 1_000_000,
            "SMMU stopped consuming commands (GERROR {:#x}, CONS {:#x})",
            r32(s.base, GERROR),
            r32(s.base, CMDQ_CONS),
        );
    }
}

/// Point stream `rid` at the domain rooted at `ttb` (a VMSAv8-64 table the portable seam built),
/// tagged `asid`, then invalidate so the SMMU drops any cached STE/CD/TLB entry for the previous
/// domain. After this returns, the device can reach exactly what the domain maps.
pub fn attach(rid: u32, ttb: u64, asid: u16) {
    let mut g = SMMU.lock();
    let s = g.as_mut().expect("SMMU attach before init");
    assert!(
        (rid as u64) < (1 << STRTAB_LOG2),
        "StreamID {rid} beyond the linear stream table"
    );

    // The CD: the SMMU's TTBR0+TCR+MAIR for this stream. Geometry mirrors the CPU's TCR_EL1
    // exactly (T0SZ=16 so IOVAs are 48-bit, 4 KiB granule, TTB1 walks disabled): the domain is
    // built by the same paging code as a process address space, so it must be described the same.
    let cd = phys_to_virt(s.cds + rid as u64 * STE_BYTES) as *mut u32;
    let cd_w0: u32 = 16              // T0SZ: 48-bit IOVA space, matching Aarch64::SPLIT_SHIFT
        | (0b01 << 8)                // IR0: walks are write-back cacheable (ignored by QEMU)
        | (0b01 << 10)               // OR0
        | (0b11 << 12)               // SH0: inner shareable
        | (1 << 30)                  // EPD1: no TTB1, the high half does not exist for devices
        | (1 << 31); //                 V
    let cd_w1: u32 = 0b101           // IPS: 48-bit output
        | (1 << 9)                   // AA64
        | (1 << 13)                  // R: record faults from this stream to the event queue
        | (1 << 14)                  // A: and abort the faulting transaction
        | ((asid as u32) << 16);
    // SAFETY: cds is a kernel-owned frame; rid is bounds-checked above.
    unsafe {
        core::ptr::write_volatile(cd, cd_w0);
        core::ptr::write_volatile(cd.add(1), cd_w1);
        core::ptr::write_volatile(cd.add(2), (ttb as u32 & 0xffff_fff0) | 0);
        core::ptr::write_volatile(cd.add(3), (ttb >> 32) as u32 & 0xf_ffff);
        core::ptr::write_volatile(cd.add(4), 0); // TTB1 does not exist
        core::ptr::write_volatile(cd.add(5), 0);
        // MAIR0/MAIR1: the same palette mmu::init programs into MAIR_EL1 (slot 0 device, slot 1
        // normal write-back). QEMU does not model attributes; real silicon reads these.
        core::ptr::write_volatile(cd.add(6), paging::aarch64::mair::VALUE as u32);
        core::ptr::write_volatile(cd.add(7), (paging::aarch64::mair::VALUE >> 32) as u32);
    }

    // The STE: valid, stage-1 translate (config 0b101), one linear CD.
    let ste = phys_to_virt(s.strtab + rid as u64 * STE_BYTES) as *mut u32;
    let ctxptr = s.cds + rid as u64 * STE_BYTES;
    // SAFETY: strtab is a kernel-owned frame; rid is bounds-checked above.
    unsafe {
        // Word 0 low half: V | CONFIG=0b101 | S1FMT=0 | CTXPTR[31:6]; word 1: CTXPTR[51:32],
        // S1CDMAX=0 (exactly one CD, no substreams).
        core::ptr::write_volatile(ste, 1 | (0b101 << 1) | (ctxptr as u32 & 0xffff_ffc0));
        core::ptr::write_volatile(ste.add(1), (ctxptr >> 32) as u32 & 0xf_ffff);
        for i in 2..16 {
            core::ptr::write_volatile(ste.add(i), 0);
        }
    }

    // The tables are read by the SMMU, a separate observer: publish before telling it to look.
    crate::arch::dma_wmb();

    // Drop whatever the SMMU cached for this stream: its STE, its CD, and every TLB entry (a
    // re-attach reuses the ASID with a brand-new table, so the broad TLBI is the correct one).
    cmd_push(s, [CMD_CFGI_STE, rid, 1, 0]); // leaf=1: just this STE
    cmd_push(s, [CMD_CFGI_CD, rid, 1, 0]);
    cmd_push(s, [CMD_TLBI_NSNH_ALL, 0, 0, 0]);
    cmd_push(s, [CMD_SYNC, 0, 0, 0]);
}

/// Pop one fault from the event queue, if any. The confinement test drains this to prove a DMA
/// escape was stopped by the hardware, not merely absent.
#[cfg_attr(not(test), allow(dead_code))]
pub fn take_fault() -> Option<Fault> {
    let mut g = SMMU.lock();
    let s = g.as_mut()?;
    let wrap_mask = (1u32 << (EVTQ_LOG2 + 1)) - 1;
    let prod = r32(s.base, EVENTQ_PROD) & wrap_mask;
    if prod == s.evtq_cons {
        return None;
    }
    let idx = (s.evtq_cons & ((1 << EVTQ_LOG2) - 1)) as u64;
    let evt = phys_to_virt(s.evtq + idx * EVT_BYTES) as *const u32;
    // SAFETY: the event queue frame is kernel-owned; the SMMU wrote this entry before advancing
    // PROD, and idx is masked to the queue.
    let (code, rid, lo, hi) = unsafe {
        (
            core::ptr::read_volatile(evt) & 0xff,
            core::ptr::read_volatile(evt.add(1)),
            core::ptr::read_volatile(evt.add(4)),
            core::ptr::read_volatile(evt.add(5)),
        )
    };
    s.evtq_cons = (s.evtq_cons + 1) & wrap_mask;
    w32(s.base, EVENTQ_CONS, s.evtq_cons);
    Some(Fault {
        rid,
        code,
        addr: (hi as u64) << 32 | lo as u64,
    })
}
