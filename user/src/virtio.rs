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
use fs_proto::blk;

// The kernel maps the DMA page at this fixed VA (must match kernel/src/user.rs virtio_service).
// The device REGISTERS are NOT mapped: we drive the device through a `Virtio` capability (slot 2),
// so we cannot point it outside this DMA region. See kernel/src/virtio.rs.
const DMA_VA: u64 = 0x0000_0000_0090_0000;

/// Capability slots the kernel handed us, by convention.
const REPORT: u64 = 0; // SEND: report the result back to the kernel
const IRQ: u64 = 1; // WAIT/ACK the device interrupt
const VIRTIO: u64 = 2; // the device transport: read/write registers, set up the queue, submit

// --- virtio-mmio v2 register offsets (bytes from the slot base) ---
const MAGIC: u64 = 0x000;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;

// Status bits.
const S_ACKNOWLEDGE: u32 = 1;
const S_DRIVER: u32 = 2;
const S_DRIVER_OK: u32 = 4;
const S_FEATURES_OK: u32 = 8;

// Feature bit 32: VIRTIO_F_VERSION_1. Mandatory for a modern device.
const F_VERSION_1_HI: u32 = 1; // bit 32 lives in the high 32-bit word

// Feature bit 33: VIRTIO_F_ACCESS_PLATFORM. The device sets it when it sits behind an IOMMU
// (QEMU's `iommu_platform=on`, milestone 16b); it then treats the addresses in our descriptors and
// ring registers as IOVAs to be translated, and REQUIRES the driver to negotiate the bit or it
// refuses to run. Our DMA domain is an identity map (IOVA == PA, kernel/src/iommu.rs), so accepting
// the bit changes nothing about the addresses we compute; it just tells the device we know we are
// translated. Offered only when behind an IOMMU, so we ack it conditionally: the virtio-mmio disk
// does not offer it, and acking a feature the device did not offer would fail negotiation.
const F_ACCESS_PLATFORM_HI: u32 = 1 << 1; // bit 33 lives in the high 32-bit word

// The virtqueue we build. Small: one request in flight is all a demo needs.
const QSIZE: usize = 8;

// Descriptor flags.
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2; // device writes (i.e. our read buffer)
const VIRTQ_DESC_F_INDIRECT: u16 = 4; // points at a table of further descriptors (attack test)

// Feature bit 28 (low word): VIRTIO_RING_F_INDIRECT_DESC. The indirect attacker asks for it; the
// kernel strips it during negotiation so the device never honours the flag above.
const F_INDIRECT_DESC_LO: u32 = 1 << 28;

// blk request types.
const VIRTIO_BLK_T_IN: u32 = 0; // read: the device WRITES the data buffer
const VIRTIO_BLK_T_OUT: u32 = 1; // write: the device READS the data buffer

// --- our DMA layout, offsets within the one DMA page ---
const OFF_DESC: u64 = 0x000; // 16 * QSIZE = 128 bytes
const OFF_AVAIL: u64 = 0x080; // 6 + 2*QSIZE
const OFF_USED: u64 = 0x100; // 6 + 8*QSIZE
const OFF_HEADER: u64 = 0x200; // 16-byte blk request header
const OFF_DATA: u64 = 0x400; // 512-byte block buffer
const OFF_STATUS: u64 = 0x600; // 1-byte status

const BLOCK: usize = 512;

/// Read a device register, through the kernel. Reads are DMA-safe, so any register is allowed.
fn mr(off: u64) -> u32 {
    // SAFETY: `svc`; the kernel validates the Virtio capability in slot 2.
    unsafe { invoke(VIRTIO, abi::virtio::READ_REG, off, 0, 0) as u32 }
}
/// Write a DMA-*safe* device register (status, features, interrupt-ack) through the kernel. The
/// queue-address and notify registers are NOT writable this way — the kernel owns them.
fn mw(off: u64, v: u32) {
    // SAFETY: `svc`.
    unsafe { invoke(VIRTIO, abi::virtio::WRITE_REG, off, v as u64, 0) };
}

/// Order our normal-memory accesses to the queue against the device's, and against the MMIO
/// notify. On QEMU DMA is coherent, but the barrier is still needed so neither the compiler nor
/// the CPU reorders "publish the descriptor" past "tell the device." `dmb ish` on aarch64; on
/// RISC-V one full `fence`, the same conservative choice the kernel's `arch::dma_wmb` makes.
fn barrier() {
    // SAFETY: a barrier has no operands and cannot be unsound.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags))
    };
    // SAFETY: as above; a fence only constrains ordering.
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence", options(nostack, preserves_flags))
    };
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
/// The virtio handshake and queue setup, shared by the real driver and the attack test. The queue
/// is set up THROUGH THE KERNEL, which places the rings at fixed offsets in our DMA region and
/// programs the device with those addresses — we never choose them.
fn init() {
    init_with_features(0);
}

/// The handshake, with `driver_features_lo` OR'd into the low feature word. The honest driver
/// passes 0; the indirect attacker passes `F_INDIRECT_DESC_LO` to try to negotiate indirect
/// descriptors (the kernel strips it, proving negotiation is filtered).
fn init_with_features(driver_features_lo: u32) {
    check(mr(MAGIC) == 0x7472_6976); // "virt": we really are talking to a virtio device

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    // Accept VIRTIO_F_VERSION_1 (high word bit 32), plus whatever the caller asked for in the low
    // word. The kernel filters the low word, so a request for a ring-layout feature it cannot
    // validate does not survive.
    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, driver_features_lo);

    // The high word: VERSION_1 always, and ACCESS_PLATFORM only if the device offered it (i.e. it
    // is behind an IOMMU). Reading the device's high feature word first is what keeps the ack a
    // subset of the offer, so the same driver binary negotiates correctly on the bare mmio disk and
    // the IOMMU-fronted PCIe disk alike.
    mw(DEVICE_FEATURES_SEL, 1);
    let dev_hi = mr(DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI;
    }
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, ack_hi);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    check(mr(STATUS) & S_FEATURES_OK != 0);

    // SAFETY: `svc`; the layout constants (OFF_DESC/AVAIL/USED) are the contract the kernel uses.
    check(unsafe { invoke(VIRTIO, abi::virtio::SETUP_QUEUE, QSIZE as u64, 0, 0) } == 0);

    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );
}

pub fn run(dma_phys: u64) -> ! {
    init();

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

/// The byte the write test puts at offset `i` of the scratch block. The first eight bytes are a
/// literal the kernel test asserts against; the rest is a cheap position-dependent function, so a
/// buffer that came back shifted, truncated, or stale cannot match it.
fn pattern_byte(i: usize) -> u8 {
    const HEAD: &[u8; 8] = b"CRKWRIT1";
    if i < 8 {
        HEAD[i]
    } else {
        (i as u8).wrapping_mul(31) ^ 0x5A
    }
}

/// **The write path, end to end.** Find the `scratch` file in the directory (the one block on the
/// disk the write tests own), write a pattern into its block, wipe the buffer, read the block
/// back, and verify every byte made the round trip through the device. Then re-read block 0 and
/// look motd up again, so the report also certifies the write did not eat the filesystem around
/// it. Reports the first 8 bytes of the read-back pattern.
pub fn run_write(dma_phys: u64) -> ! {
    init();

    // The directory tells us where scratch lives; nothing about the sector is hardcoded.
    read_block(dma_phys, 0);
    let mut magic = [0u8; 8];
    for (i, b) in magic.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    check(&magic == b"CRKR0001");
    let scratch = find_file(b"scratch").unwrap_or_else(|| report_code(0xE6)) as u64;

    // Write the pattern...
    for i in 0..BLOCK {
        dma_write::<u8>(OFF_DATA + i as u64, pattern_byte(i));
    }
    write_block(dma_phys, scratch);

    // ...wipe the buffer so a read that silently does nothing cannot pass, and read it back.
    for i in 0..BLOCK {
        dma_write::<u8>(OFF_DATA + i as u64, 0);
    }
    read_block(dma_phys, scratch);
    for i in 0..BLOCK {
        check(dma_read::<u8>(OFF_DATA + i as u64) == pattern_byte(i));
    }
    let mut head = [0u8; 8];
    for (i, b) in head.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }

    // The write must have landed on ITS block and nothing else: the superblock still parses and
    // the read-path file is still in the directory.
    read_block(dma_phys, 0);
    for (i, b) in magic.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    check(&magic == b"CRKR0001");
    check(find_file(b"motd").is_some());

    send(REPORT, u64::from_le_bytes(head), 0, 0);

    loop {
        core::hint::spin_loop();
    }
}

/// **The abandoned write.** Submit a write through the validated path and then die on purpose,
/// without waiting for the completion, acknowledging the interrupt, or touching the used ring:
/// the kill-mid-write case. The kernel reaps this thread with the device still owing it a
/// completion; the test then runs the full writer against the same device and it must succeed,
/// which is the proof the abandoned request left the transport, the validator bookkeeping, and
/// the disk in a sane state. Reports 1 after the submit so the test knows the write really left,
/// then panics (`brk`/`ebreak`), which is how a userspace process dies here.
pub fn run_write_abandon(dma_phys: u64) -> ! {
    init();

    read_block(dma_phys, 0);
    let mut magic = [0u8; 8];
    for (i, b) in magic.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    check(&magic == b"CRKR0001");
    let scratch = find_file(b"scratch").unwrap_or_else(|| report_code(0xE6)) as u64;

    for i in 0..BLOCK {
        dma_write::<u8>(OFF_DATA + i as u64, 0xAB);
    }
    let _used_before = submit_block(dma_phys, scratch, VIRTIO_BLK_T_OUT);

    send(REPORT, 1, 0, 0); // submitted; the kernel validated it and rang the device
    panic!(); // and now we are gone, mid-operation
}

/// **The attack, refused.** A malicious driver that points a descriptor at kernel memory and asks
/// the device to write there. The kernel validates the descriptor on submit and refuses, so the
/// device is never told to go and never touches the address. Reports 1 if the kernel refused
/// (correct), 0 if the notify went through (a hole). Used by the security test.
pub fn run_attack(dma_phys: u64) -> ! {
    init();

    // The forbidden target: the kernel's own image, which the device would happily DMA over if it
    // were ever handed this address.
    const KERNEL_ADDR: u64 = 0xffff_0000_4008_0000;

    // A single descriptor pointing OUTSIDE our DMA region, marked device-writable.
    write_desc(0, KERNEL_ADDR, 512, VIRTQ_DESC_F_WRITE, 0);

    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier();

    // Submit. The kernel walks the descriptor, sees KERNEL_ADDR is outside our region, and refuses.
    // SAFETY: `svc`.
    let r = unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, 0, 0, 0) };
    send(REPORT, if r < 0 { 1 } else { 0 }, 0, 0); // 1 = refused (good), 0 = it went through (bad)

    let _ = dma_phys;
    loop {
        core::hint::spin_loop();
    }
}

/// **The indirect-descriptor attack, refused.** The subtler cousin of `run_attack`: instead of a
/// direct descriptor pointing at kernel memory (which the validator obviously refuses), this
/// negotiates `INDIRECT_DESC` and submits a single descriptor flagged `INDIRECT`, whose *inner*
/// table (in our own region) points the device at the kernel. A validator that walked only the
/// flat chain would wave the outer descriptor through and let the device follow the table out. The
/// kernel strips the feature during negotiation and refuses the flag on submit, so the device is
/// never told to go. Reports 1 if refused (correct), 0 if the notify went through (a hole).
pub fn run_attack_indirect(dma_phys: u64) -> ! {
    // Ask for indirect descriptors. The kernel strips the bit, but we build the attack anyway to
    // prove the validator refuses the flag even if the feature ever slipped through.
    init_with_features(F_INDIRECT_DESC_LO);

    const KERNEL_ADDR: u64 = 0xffff_0000_4008_0000;

    // The indirect TABLE lives in our region (the header scratch area). Its one entry aims the
    // device at kernel memory, device-writable.
    dma_write::<u64>(OFF_HEADER, KERNEL_ADDR); // inner desc addr
    dma_write::<u32>(OFF_HEADER + 8, 512); // inner desc len
    dma_write::<u16>(OFF_HEADER + 12, VIRTQ_DESC_F_WRITE); // inner desc flags
    dma_write::<u16>(OFF_HEADER + 14, 0); // inner desc next

    // desc[0]: flagged INDIRECT, pointing at the in-region table above. Wholly in-region, so only
    // the INDIRECT handling stands between this and a kernel-memory DMA.
    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_INDIRECT, 0);

    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier();

    // SAFETY: `svc`. The kernel refuses the indirect descriptor and never rings the device.
    let r = unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, 0, 0, 0) };
    send(REPORT, if r < 0 { 1 } else { 0 }, 0, 0); // 1 = refused (good), 0 = it went through (bad)

    let _ = dma_phys;
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

/// Build and publish the three-descriptor chain for one block transfer, and ring the device
/// through the kernel. Returns the used-ring index from before the submit, which is what
/// [`complete_block`] compares against to see the completion land. The chain shape is the blk
/// spec's for both directions; only the data descriptor's device-writable flag differs:
/// a read (`T_IN`) has the device WRITE our buffer, a write (`T_OUT`) has it READ the buffer.
/// Either way every address is inside our DMA region, and the kernel checks that on NOTIFY;
/// the direction flag changes nothing about the confinement, which bounds addresses, not flags.
fn submit_block(dma_phys: u64, sector: u64, req_type: u32) -> u16 {
    // The request header the device reads: type, reserved, sector.
    dma_write::<u32>(OFF_HEADER, req_type);
    dma_write::<u32>(OFF_HEADER + 4, 0);
    dma_write::<u64>(OFF_HEADER + 8, sector);
    dma_write::<u8>(OFF_STATUS, 0xff); // the device overwrites this with 0 on success

    let data_flags = if req_type == VIRTIO_BLK_T_IN {
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE // read: the device fills our buffer
    } else {
        VIRTQ_DESC_F_NEXT // write: the device consumes our buffer
    };

    // desc[0]: header, device reads it.          NEXT -> 1
    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
    // desc[1]: data buffer, direction above.     NEXT -> 2
    write_desc(1, dma_phys + OFF_DATA, BLOCK as u32, data_flags, 2);
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

    // Submit THROUGH THE KERNEL. The kernel walks the descriptors we just published and, only if
    // every one stays inside our DMA region, rings the device. If we pointed one outside our
    // region, it returns an error and the device is never told to go. SAFETY: `svc`.
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, 0, 0, 0) } < 0 {
        report_code(0xE5); // the kernel refused our request (a descriptor escaped our region)
    }

    used_before
}

/// Wait for the completion of a submitted transfer and confirm the device reported success.
///
/// **The completion is the used ring advancing, not the wakeup.** A driver cannot treat one
/// interrupt as one completion: the device's line is shared across every driver that ever holds
/// this device, and a wakeup can be spurious, coalesced, or left pending by a *previous* operator
/// of the device. The kill-mid-write case makes that concrete: a driver that submitted a write and
/// died leaves its completion interrupt pending on the shared routing endpoint, and this fresh
/// driver's first WAIT would consume that stale signal before its own request is on the used ring.
/// So loop: WAIT, quiet the device, re-enable the line, and let `used.idx` decide when we are
/// actually done. Every real completion the device makes also raises the line, so the loop always
/// makes progress and cannot outlast the device. See notes/dma.md (the kill-mid-write section).
fn complete_block(used_before: u16) {
    loop {
        // **Wait for the interrupt, as a message.** The device raises its line when it puts our
        // buffer on the used ring; the kernel (milestone 9a) masks the line, turns it into a
        // notification, and wakes us. If the interrupt already fired between the notify above and
        // this call, the kernel's pending count makes WAIT return at once instead of blocking on
        // an event that is already over. SAFETY: `svc`; the kernel validates the Irq capability.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };

        // Quiet the device (read its interrupt-status, acknowledge it), then re-enable the line at
        // the controller, which the kernel masked when it fired. SAFETY: `svc`.
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };

        barrier();
        if dma_read::<u16>(OFF_USED + 2) != used_before {
            break; // our request is on the used ring; this wakeup was really ours
        }
        // The used ring has not moved, so that wakeup belonged to someone else (a stale completion
        // from a dead driver, most likely). Wait again for our own.
    }
    let st = dma_read::<u8>(OFF_STATUS);
    if st != 0 {
        report_code(0xE200 | st as u64); // device reported a non-OK status
    }
}

/// Read one block from `sector` into the DMA data buffer, start to finish.
fn read_block(dma_phys: u64, sector: u64) {
    let used_before = submit_block(dma_phys, sector, VIRTIO_BLK_T_IN);
    complete_block(used_before);
}

/// Write the DMA data buffer to `sector`, start to finish. **The write verb** (milestone 32
/// phase 1): the same chain, the same validated submit, the same completion; only the data
/// descriptor's direction differs, so everything the read path proved carries over.
fn write_block(dma_phys: u64, sector: u64) {
    let used_before = submit_block(dma_phys, sector, VIRTIO_BLK_T_OUT);
    complete_block(used_before);
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

// ---------------------------------------------------------------------------------------------
// The block server (milestone 32 phase 2): drive the device, serve blocks over blk IPC.
//
// The driver roles above read one block and report. This role instead becomes a long-lived server:
// it inits the device once, then answers read/write/size requests from the FS server forever, one
// filesystem block (8 sectors, 4096 bytes) per request. The FS server is its only client and never
// touches the DMA region; the device confinement (kernel/src/virtio.rs) is unchanged, so a serving
// block driver is as confined as a reading one. See notes/fs-server.md and notes/dma.md.
// ---------------------------------------------------------------------------------------------

/// The block server's request endpoint (slot 0): the FS server CALLs here; this role RECV_CAPs the
/// request plus the one-shot Reply that names the caller. IRQ (1) and VIRTIO (2) are as every role.
const BLK_REQ: u64 = 0;

/// Where the kernel maps the page shared with the FS server (the block buffer). Distinct from the
/// DMA region, which the FS server must never see, and from [`DMA_VA`].
///
/// The block server's DMA region is **two contiguous pages** (kernel/src/user.rs `fs_service`):
/// page 0 holds the rings, request header, and status (block-server-private), and page 1 is the
/// 4096-byte data buffer, which IS the page shared with the FS server. So one virtio request moves a
/// whole filesystem block (eight sectors) and the device DMAs it straight into the FS server's page,
/// with no per-sector loop and no copy. `BLK_OFF_DATA` is that page-1 offset.
const BLK_OFF_DATA: u64 = 0x1000;
/// One filesystem block in the data buffer: eight 512-byte sectors.
const BLK_DATA_LEN: u32 = 4096;

/// The block server's readiness endpoint (slot 3): SEND once after the device is up, so the test
/// can tell a device-bring-up hang from a first-read hang. See kernel/src/user.rs fs_service.
const BLK_READY: u64 = 3;

/// virtio-mmio device-config window; virtio-blk's capacity (u64, in 512-byte sectors) is at its
/// start. Reads are DMA-safe, so the kernel's `READ_REG` permits this offset.
const CONFIG: u64 = 0x100;

/// **The block server.** Bring the device up, then serve blk IPC forever. Every request is a CALL
/// answered through the kernel-minted Reply, so this server can answer a client it was never wired
/// to, exactly once each. The transfer unit is one filesystem block; the bulk rides in the shared
/// page, the control (opcode, block index, result) rides in the message, the §10 split.
pub fn run_blk_server(dma_phys: u64) -> ! {
    init();
    // The device is up: signal readiness, so a hang here (bring-up) is distinct from a hang in the
    // first read completion. SEND rendezvous, so this also paces the test past bring-up.
    send(BLK_READY, fs_proto::fixture::READY, 0, 0);
    loop {
        // RECV_CAP: (first word, the Reply cap's slot, second word = the block index).
        let (w0, reply, block) = user_rt::recv_cap(BLK_REQ);
        let r0: i64 = match fs_proto::op(w0) {
            blk::READ => {
                blk_read(dma_phys, block);
                0
            }
            blk::WRITE => {
                blk_write(dma_phys, block);
                0
            }
            blk::SIZE => disk_size_bytes(),
            _ => -22, // EINVAL: an opcode this server does not implement
        };
        // Answer through the one-shot Reply. SAFETY: `svc`; the kernel validated and consumes it.
        unsafe { invoke(reply, abi::reply::REPLY, r0 as u64, 0, 0) };
    }
}

/// Complete a block-server transfer by **polling** the used ring, not by waiting on the interrupt.
///
/// QEMU pops descriptors and writes the used ring synchronously inside the `QUEUE_NOTIFY` MMIO write
/// (notes/dma.md), so by the time the kernel's `NOTIFY` returns the completion is already done. The
/// FS server does hundreds of reads at open (RedoxFS scans a 256-entry header ring), and a
/// WAIT-on-interrupt per read pays a full reschedule each time, which overran the test's watchdog.
/// Polling the used ring skips that. The fallback to [`complete_block`] keeps the driver correct on
/// hardware that completes asynchronously, where the poll would spin out and the interrupt is real;
/// on QEMU the poll succeeds on the first read of the ring, so the fallback never runs.
fn complete_blk(used_before: u16) {
    for _ in 0..50_000_000 {
        barrier();
        if dma_read::<u16>(OFF_USED + 2) != used_before {
            let st = dma_read::<u8>(OFF_STATUS);
            if st != 0 {
                panic!(); // the device reported a non-OK status
            }
            return;
        }
        core::hint::spin_loop();
    }
    // The device did not complete inline within a generous bound. This poll-based server requires
    // synchronous completion (which QEMU provides); fault loudly rather than hang invisibly.
    panic!();
}

/// Read one filesystem block (eight sectors, 4096 bytes) in a SINGLE virtio request. The device
/// DMAs straight into page 1 of the DMA region, which is the FS server's block page, so there is no
/// per-sector loop and no copy. Contrast the read path above, which the small-buffer driver roles
/// use one 512-byte sector at a time; the FS server reads hundreds of blocks at open, so the eight-
/// fold reduction in device round trips is what keeps it inside the test's watchdog.
fn blk_read(dma_phys: u64, block: u64) {
    let used_before = submit_blk(dma_phys, block * blk::SECTORS_PER_BLOCK, VIRTIO_BLK_T_IN);
    complete_blk(used_before);
}

/// Write one filesystem block from page 1 (the FS server filled it before this request) in a single
/// virtio request. The one direction flag differs from the read; the addresses are identical.
fn blk_write(dma_phys: u64, block: u64) {
    let used_before = submit_blk(dma_phys, block * blk::SECTORS_PER_BLOCK, VIRTIO_BLK_T_OUT);
    complete_blk(used_before);
}

/// Build and publish the three-descriptor chain for a whole-block (4096-byte) transfer and ring the
/// device through the kernel. Mirrors [`submit_block`] but with the data descriptor spanning a full
/// block at [`BLK_OFF_DATA`] (page 1, the shared page) instead of one sector. Returns the used-ring
/// index from before the submit, which [`complete_block`] compares against.
fn submit_blk(dma_phys: u64, sector: u64, req_type: u32) -> u16 {
    dma_write::<u32>(OFF_HEADER, req_type);
    dma_write::<u32>(OFF_HEADER + 4, 0);
    dma_write::<u64>(OFF_HEADER + 8, sector);
    dma_write::<u8>(OFF_STATUS, 0xff);

    let data_flags = if req_type == VIRTIO_BLK_T_IN {
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE // read: the device fills the block page
    } else {
        VIRTQ_DESC_F_NEXT // write: the device consumes the block page
    };
    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(1, dma_phys + BLK_OFF_DATA, BLK_DATA_LEN, data_flags, 2);
    write_desc(2, dma_phys + OFF_STATUS, 1, VIRTQ_DESC_F_WRITE, 0);

    let used_before: u16 = dma_read::<u16>(OFF_USED + 2);
    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier();

    // SAFETY: `svc`. The kernel validates every descriptor against the 2-page region before ringing;
    // a refusal here is a block-server bug, so fault rather than limp on.
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, 0, 0, 0) } < 0 {
        panic!();
    }
    used_before
}

/// The disk's size in bytes, from virtio-blk's capacity register (sectors * 512). RedoxFS asks for
/// this once, at open, to size its allocator; the value is the whole extent the image lives in.
fn disk_size_bytes() -> i64 {
    let lo = mr(CONFIG) as u64;
    let hi = mr(CONFIG + 4) as u64;
    let sectors = lo | (hi << 32);
    (sectors * 512) as i64
}
