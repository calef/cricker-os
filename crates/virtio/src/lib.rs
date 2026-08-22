#![no_std]
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
//!
//! # Examples
//!
//! **Nothing here can run off a machine, and the reason is the whole point of the crate.** Every
//! entry point below writes device registers through a mapping the kernel handed this process and
//! returns `!`: there is no virtio device on a host, `script/test`'s host pass excludes this crate
//! (it depends on `user_rt`'s EL0 `asm!`, an exclusion `script/lint` derives and checks), and a
//! function that never returns has no assertion to make. So the example is `no_run`: type-checked
//! against the real signatures, executed by the QEMU boot and by nothing else. The parts of this
//! subsystem that *are* checkable are in `dma_validator` and `fs_proto`, which is where their
//! examples live.
//!
//! What a driver's `_start` looks like, and it is the shortest interesting program in the tree,
//! because the interesting part is what it is holding rather than what it does:
//!
//! ```no_run
//! # use user_rt::exit;
//! /// The kernel starts this thread with the DMA page's **physical** address in the first argument
//! /// register. It has to: descriptors speak physical addresses and a process knows only virtual
//! /// ones, and there is no syscall that would translate one, deliberately.
//! ///
//! /// Everything else this process holds arrived at spawn: a device-typed mapping of the registers,
//! /// the DMA page, and an `Irq` capability in slot 1 so the interrupt reaches it as a message. It
//! /// holds no filesystem, no console, and no way to name a physical address other than this one.
//! fn start(dma_phys: u64) -> ! {
//!     virtio::run(dma_phys)
//! }
//! ```
//!
//! If any of the register programming below is wrong, **this process faults and the kernel does
//! not**, which is milestone 9's headline and the thing a reader should take from the crate.
//!
//! Name: unrecorded, and the tree comes one clause short of recording it. notes/naming.md's BUGS
//! says "the crate keeps its name, which is right, but the sentence under it is wrong", which
//! asserts the conclusion and never gives the reason, so a reader learns that somebody agreed
//! rather than why. Milestone 63 treats the name as claimed territory from the other side, refusing
//! `virtio_net` for the transport adapter because "`crates/virtio` also drives net". It is the
//! device family's own name from the specification and would sit in the tenet's protected group if
//! anyone had filed it there; nobody has. Introduced 2026-07-14 with milestone 9's block driver.

use abi::irq;
use fs_proto::blk;
use user_rt::{exit, invoke, send};

// The kernel maps the DMA page at this fixed VA (must match kernel/src/user/virtio_service.rs).
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
// Flush: no data buffer at all, just the header and the status byte. Legal only once
// `VIRTIO_BLK_F_FLUSH` is negotiated, which is why the block server probes for it rather than
// assuming it (virtio 1.2 §5.2.6; see `init_with_features` and `blk_flush`).
const VIRTIO_BLK_T_FLUSH: u32 = 4;

// Feature bit 9 (low word): VIRTIO_BLK_F_FLUSH, "the device supports a cache flush command".
//
// **The whole durability claim rests on this bit being offered**, so it is read from the device
// rather than assumed. A driver that sent `VIRTIO_BLK_T_FLUSH` without negotiating it would get
// `VIRTIO_BLK_S_UNSUPP` from a well-behaved device and undefined behaviour from a careless one,
// and either way a server that took the resulting silence for success would be telling a backup
// client its data was safe. The kernel's `sanitize_driver_features` strips only bits 28 and 34, so
// this bit reaches the device unchanged; `feature_negotiation_leaves_the_blk_flush_bit_alone` in
// `kernel/src/virtio.rs` is the gate that keeps that true.
const F_FLUSH_LO: u32 = 1 << 9;

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
/// queue-address and notify registers are NOT writable this way: the kernel owns them.
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
        core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags));
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

/// Read block 0 of the disk into the DMA data buffer, then verify the nifefs magic.
/// The virtio handshake and queue setup, shared by the real driver and the attack test. The queue
/// is set up THROUGH THE KERNEL, which places the rings at fixed offsets in our DMA region and
/// programs the device with those addresses: we never choose them.
fn init() {
    init_with_features(0, false);
}

/// The handshake, with `driver_features_lo` OR'd into the low feature word. The honest driver
/// passes 0; the indirect attacker passes `F_INDIRECT_DESC_LO` to try to negotiate indirect
/// descriptors (the kernel strips it, proving negotiation is filtered).
///
/// `want_flush` additionally asks for `VIRTIO_BLK_F_FLUSH`, **but only if the device offered it**,
/// which is `F_ACCESS_PLATFORM_HI`'s discipline applied to the low word: the ack stays a subset of
/// the offer, so the same code negotiates correctly against a device that does caching and one that
/// does not. Returns whether that bit ended up negotiated, which the block server needs as a fact
/// rather than a hope: it is the difference between answering a sync and refusing one out loud.
///
/// The two words are asked for differently on purpose. `driver_features_lo` is passed **unmasked**,
/// because the indirect attacker's whole point is to ask for a bit the device really does offer and
/// be stripped by the *kernel*; masking it against the offer would quietly change what that test
/// proves.
fn init_with_features(driver_features_lo: u32, want_flush: bool) -> bool {
    assert!(mr(MAGIC) == 0x7472_6976); // "virt": we really are talking to a virtio device

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    // Accept VIRTIO_F_VERSION_1 (high word bit 32), plus whatever the caller asked for in the low
    // word. The kernel filters the low word, so a request for a ring-layout feature it cannot
    // validate does not survive.
    mw(DEVICE_FEATURES_SEL, 0);
    let dev_lo = mr(DEVICE_FEATURES);
    let flush = want_flush && (dev_lo & F_FLUSH_LO) != 0;
    mw(DRIVER_FEATURES_SEL, 0);
    mw(
        DRIVER_FEATURES,
        driver_features_lo | if flush { F_FLUSH_LO } else { 0 },
    );

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
    assert!(mr(STATUS) & S_FEATURES_OK != 0);

    // SAFETY: `svc`; the layout constants (OFF_DESC/AVAIL/USED) are the contract the kernel uses.
    assert!(unsafe { invoke(VIRTIO, abi::virtio::SETUP_QUEUE, QSIZE as u64, 0, 0) } == 0);

    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );
    // FEATURES_OK stuck above, so the device accepted the set we wrote, and the only way this bit
    // is in that set is that the device offered it and we asked for it. virtio-mmio's
    // `DriverFeatures` is write-only, so there is nothing to read back; this is the negotiated
    // truth, not an assumption about it.
    flush
}

pub fn run(dma_phys: u64) -> ! {
    init();

    // Read block 0: the nifefs superblock.
    read_block(dma_phys, 0);

    // It must be a nifefs image.
    let mut magic = [0u8; 8];
    for (i, b) in magic.iter_mut().enumerate() {
        *b = dma_read::<u8>(OFF_DATA + i as u64);
    }
    assert!(magic == nifefs::MAGIC);

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

    // Done: exit so the kernel reaps this one-shot driver thread rather than leaving it spinning
    // on a run queue forever. A leaked spinner is not just untidy: enough of them starve a later
    // heavy test (the RedoxFS mount) past the hang watchdog. See notes/supervision.md / the test
    // starvation finding.
    exit();
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
    assert!(magic == nifefs::MAGIC);
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
        assert!(dma_read::<u8>(OFF_DATA + i as u64) == pattern_byte(i));
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
    assert!(magic == nifefs::MAGIC);
    assert!(find_file(b"motd").is_some());

    send(REPORT, u64::from_le_bytes(head), 0, 0);

    // Done: exit so the kernel reaps this one-shot driver thread rather than leaving it spinning
    // on a run queue forever. A leaked spinner is not just untidy: enough of them starve a later
    // heavy test (the RedoxFS mount) past the hang watchdog. See notes/supervision.md / the test
    // starvation finding.
    exit();
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
    assert!(magic == nifefs::MAGIC);
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
    // Done: exit so the kernel reaps this one-shot driver thread rather than leaving it spinning
    // on a run queue forever. A leaked spinner is not just untidy: enough of them starve a later
    // heavy test (the RedoxFS mount) past the hang watchdog. See notes/supervision.md / the test
    // starvation finding.
    exit();
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
    init_with_features(F_INDIRECT_DESC_LO, false);

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
    // Done: exit so the kernel reaps this one-shot driver thread rather than leaving it spinning
    // on a run queue forever. A leaked spinner is not just untidy: enough of them starve a later
    // heavy test (the RedoxFS mount) past the hang watchdog. See notes/supervision.md / the test
    // starvation finding.
    exit();
}

/// Find a file in the nifefs directory sitting in the block-0 buffer, returning its start
/// block. The format is `nifefs`'s and the offsets come from that crate rather than being
/// restated here: header (magic 8, count u32), then entries of
/// { name\[`NAME_LEN`\], `start_block` u32, len u32 }.
///
/// **Only the entries inside block 0 are visible**, because block 0 is all this driver has
/// buffered. That is `nifefs::ENTRIES_IN_FIRST_BLOCK`, not the archive's whole `count`, and
/// reading further would read past the DMA data buffer. The disk this walks holds three files.
fn find_file(name: &[u8]) -> Option<u32> {
    let count = dma_read::<u32>(OFF_DATA + 8) as usize;
    for i in 0..count.min(nifefs::ENTRIES_IN_FIRST_BLOCK) as u64 {
        let entry = OFF_DATA + nifefs::HEADER_LEN as u64 + i * nifefs::ENTRY_LEN as u64;
        let mut matches = true;
        for (j, &want) in name.iter().enumerate() {
            if dma_read::<u8>(entry + j as u64) != want {
                matches = false;
                break;
            }
        }
        // The name must end here, so "motd" does not match "motdx": either the next byte is NUL
        // padding, or the name used every byte of the field and there is no padding to check.
        let ends = name.len() == nifefs::NAME_LEN || dma_read::<u8>(entry + name.len() as u64) == 0;
        if matches && ends {
            return Some(dma_read::<u32>(entry + nifefs::NAME_LEN as u64));
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
        // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
        // before acting (user_rt's contract). A caller cannot break an invariant by passing a
        // bad slot or method; it gets an error back.
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
/// "not the nifefs magic" branch prints it. Only reached on a bring-up failure.
fn report_code(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    // Done: exit so the kernel reaps this one-shot driver thread rather than leaving it spinning
    // on a run queue forever. A leaked spinner is not just untidy: enough of them starve a later
    // heavy test (the RedoxFS mount) past the hang watchdog. See notes/supervision.md / the test
    // starvation finding.
    exit();
}

/// Write one 16-byte descriptor: { u64 addr; u32 len; u16 flags; u16 next; }.
fn write_desc(i: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let base = OFF_DESC + i * 16;
    dma_write::<u64>(base, addr);
    dma_write::<u32>(base + 8, len);
    dma_write::<u16>(base + 12, flags);
    dma_write::<u16>(base + 14, next);
}

// =================================================================================================
// A virtio-net driver. **At EL0, behind the multi-queue DMA confinement (milestone 30).**
//
// The same shape as the disk driver above: the kernel owns the registers and the two DMA-critical
// powers (the ring addresses and the "go"), and confines the device to this one DMA page. What is
// new is that a NIC needs TWO virtqueues, receive (queue 0, where the device WRITES a frame into our
// memory) and transmit (queue 1, where it READS one out), and milestone 30's confinement grew a
// proved second queue and second direction to carry exactly that. The driver drives both queues
// through the one `Virtio` capability, passing the queue number to SETUP_QUEUE and NOTIFY.
//
// The proof it runs, end to end, is a DHCP exchange against QEMU's user-mode (slirp) network: build
// a DHCP DISCOVER by hand, transmit it (TX), and receive the OFFER slirp sends back (RX). That
// exercises both directions and both queues with no TCP/IP stack in the loop, so it is a clean test
// of the driver and the confinement rather than of smoltcp.
// =================================================================================================

/// The two virtqueues, matching the kernel's contract (receive = 0, transmit = 1).
const NET_RX_Q: u64 = 0;
const NET_TX_Q: u64 = 1;

/// The modern virtio-net header, prepended to every frame in both directions. 12 bytes because we
/// negotiate `VIRTIO_F_VERSION_1` (the `num_buffers` field is then always present).
const NET_HDR_LEN: u64 = 12;

// Our DMA-page layout for the net driver. The queue rings follow the kernel's per-queue stride
// (RING_BLOCK = 0x200, with desc/avail/used at +0x000/+0x080/+0x100 inside each queue's block, see
// kernel/src/virtio.rs), and the packet buffers live above both queues' blocks. Everything fits in
// the one 4 KiB DMA page the spawn service hands us.
const NET_RX_DESC: u64 = 0x000;
const NET_RX_AVAIL: u64 = 0x080;
const NET_RX_USED: u64 = 0x100;
const NET_TX_DESC: u64 = 0x200;
const NET_TX_AVAIL: u64 = 0x280;
// The transmit used ring lives at 0x300 (queue 1's block + USED_OFF), but the driver never reads it:
// it does not wait on transmit completions, only on the receive side, so there is no constant for it.
/// One receive buffer: the 12-byte virtio header plus room for a full 1514-byte Ethernet frame.
const NET_RX_BUF: u64 = 0x400;
const NET_RX_BUF_LEN: u64 = 0x600;
/// One transmit buffer: the header plus the DHCP frame we build.
const NET_TX_BUF: u64 = 0xA00;

/// Our source MAC. Locally administered; slirp does not care what it is, only that DHCP names it.
const NET_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
/// A recognizable DHCP transaction id, so the OFFER we accept is unambiguously the reply to our
/// DISCOVER and not some unrelated broadcast.
const NET_XID: u32 = 0x3903_F326;

fn wr_be16(off: u64, v: u16) {
    dma_write::<u8>(off, (v >> 8) as u8);
    dma_write::<u8>(off + 1, v as u8);
}
fn wr_be32(off: u64, v: u32) {
    dma_write::<u8>(off, (v >> 24) as u8);
    dma_write::<u8>(off + 1, (v >> 16) as u8);
    dma_write::<u8>(off + 2, (v >> 8) as u8);
    dma_write::<u8>(off + 3, v as u8);
}
fn rd_be16(off: u64) -> u16 {
    ((dma_read::<u8>(off) as u16) << 8) | dma_read::<u8>(off + 1) as u16
}
fn rd_be32(off: u64) -> u32 {
    ((dma_read::<u8>(off) as u32) << 24)
        | ((dma_read::<u8>(off + 1) as u32) << 16)
        | ((dma_read::<u8>(off + 2) as u32) << 8)
        | (dma_read::<u8>(off + 3) as u32)
}

/// The IPv4 header checksum over `len` bytes at `off`: sum the 16-bit words, fold the carries, and
/// complement. The checksum field itself must be zero when this runs.
fn ip_checksum(off: u64, len: u64) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < len {
        sum += rd_be16(off + i) as u32;
        i += 2;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The virtio-net handshake and both-queue setup. Unlike the disk's `init`, this sets up receive AND
/// transmit, through the kernel, which places each queue's rings at its own block.
fn init_net() {
    assert!(mr(MAGIC) == 0x7472_6976); // "virt"

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    // Negotiate no low-word net features: no checksum offload, no MRG_RXBUF, so one frame lands in
    // one buffer behind one 12-byte header, which is the simplest correct thing.
    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, 0);

    // The high word: VERSION_1 always, ACCESS_PLATFORM only if offered (behind an IOMMU), the same
    // conditional ack the disk driver makes so one binary works on the bare NIC and the confined one.
    mw(DEVICE_FEATURES_SEL, 1);
    let dev_hi = mr(DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI;
    }
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, ack_hi);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    assert!(mr(STATUS) & S_FEATURES_OK != 0);

    // Both queues, through the kernel: it programs each queue's ring addresses to that queue's block
    // in our DMA region, so we never choose them. SAFETY: `svc`.
    assert!(unsafe { invoke(VIRTIO, abi::virtio::SETUP_QUEUE, QSIZE as u64, NET_RX_Q, 0) } == 0);
    // SAFETY: as above: the kernel validates the capability and the method.
    assert!(unsafe { invoke(VIRTIO, abi::virtio::SETUP_QUEUE, QSIZE as u64, NET_TX_Q, 0) } == 0);

    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );
}

/// Write one descriptor into an explicit descriptor table (the disk's `write_desc` is hardwired to
/// queue 0's; the NIC has two tables).
fn write_desc_at(desc_base: u64, i: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let b = desc_base + i * 16;
    dma_write::<u64>(b, addr);
    dma_write::<u32>(b + 8, len);
    dma_write::<u16>(b + 12, flags);
    dma_write::<u16>(b + 14, next);
}

/// Post the single receive buffer as a device-writable descriptor and ring queue 0. Re-callable: it
/// re-uses descriptor head 0 with a fresh available-ring slot each time, so the device fills the
/// same buffer again. `dma_phys` is the DMA page's physical base (descriptors speak physical).
fn post_rx(dma_phys: u64, avail_idx: u16) {
    write_desc_at(
        NET_RX_DESC,
        0,
        dma_phys + NET_RX_BUF,
        NET_RX_BUF_LEN as u32,
        VIRTQ_DESC_F_WRITE, // the device WRITES the received frame here
        0,
    );
    dma_write::<u16>(NET_RX_AVAIL + 4 + (avail_idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(NET_RX_AVAIL + 2, avail_idx.wrapping_add(1));
    barrier();
    // SAFETY: `svc`. The kernel validates the receive descriptor (device-writable, in our region).
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, NET_RX_Q, 0, 0) } < 0 {
        report_code(0xE7);
    }
}

/// Transmit the frame already built at `NET_TX_BUF + NET_HDR_LEN`, of `frame_len` bytes: zero the
/// virtio header in front of it, post one device-readable descriptor, and ring queue 1.
fn send_frame(dma_phys: u64, frame_len: u64, avail_idx: u16) {
    for i in 0..NET_HDR_LEN {
        dma_write::<u8>(NET_TX_BUF + i, 0);
    }
    write_desc_at(
        NET_TX_DESC,
        0,
        dma_phys + NET_TX_BUF,
        (NET_HDR_LEN + frame_len) as u32,
        0, // device READS the buffer (transmit)
        0,
    );
    dma_write::<u16>(NET_TX_AVAIL + 4 + (avail_idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(NET_TX_AVAIL + 2, avail_idx.wrapping_add(1));
    barrier();
    // SAFETY: `svc`.
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, NET_TX_Q, 0, 0) } < 0 {
        report_code(0xE8);
    }
}

/// Build a DHCP DISCOVER (Ethernet + IPv4 + UDP + BOOTP/DHCP) at `NET_TX_BUF + NET_HDR_LEN`, and
/// return the Ethernet frame length. The broadcast flag is set so slirp's OFFER comes back to the
/// broadcast address, where our single RX buffer will catch it.
fn build_dhcp_discover() -> u64 {
    let f = NET_TX_BUF + NET_HDR_LEN;

    // --- Ethernet: to broadcast, from us, IPv4 ---
    for i in 0..6u64 {
        dma_write::<u8>(f + i, 0xff);
        dma_write::<u8>(f + 6 + i, NET_MAC[i as usize]);
    }
    wr_be16(f + 12, 0x0800);

    let ip = f + 14;
    let udp = ip + 20;
    let dhcp = udp + 8;

    // --- BOOTP/DHCP payload ---
    dma_write::<u8>(dhcp, 1); // op = BOOTREQUEST
    dma_write::<u8>(dhcp + 1, 1); // htype = Ethernet
    dma_write::<u8>(dhcp + 2, 6); // hlen
    dma_write::<u8>(dhcp + 3, 0); // hops
    wr_be32(dhcp + 4, NET_XID);
    wr_be16(dhcp + 8, 0); // secs
    wr_be16(dhcp + 10, 0x8000); // flags = broadcast
    for i in 12..28u64 {
        dma_write::<u8>(dhcp + i, 0); // ciaddr/yiaddr/siaddr/giaddr
    }
    for i in 0..6u64 {
        dma_write::<u8>(dhcp + 28 + i, NET_MAC[i as usize]); // chaddr
    }
    for i in 34..236u64 {
        dma_write::<u8>(dhcp + i, 0); // rest of chaddr, sname, file
    }
    wr_be32(dhcp + 236, 0x6382_5363); // DHCP magic cookie

    // Options: message type DISCOVER, a parameter request list, end.
    let mut o = dhcp + 240;
    dma_write::<u8>(o, 53); // option 53: DHCP message type
    dma_write::<u8>(o + 1, 1);
    dma_write::<u8>(o + 2, 1); // DISCOVER
    o += 3;
    dma_write::<u8>(o, 55); // option 55: parameter request list
    dma_write::<u8>(o + 1, 3);
    dma_write::<u8>(o + 2, 1); // subnet mask
    dma_write::<u8>(o + 3, 3); // router
    dma_write::<u8>(o + 4, 6); // DNS
    o += 5;
    dma_write::<u8>(o, 255); // end
    o += 1;

    let dhcp_len = o - dhcp;
    let udp_len = 8 + dhcp_len;
    let ip_total = 20 + udp_len;

    // --- UDP: bootpc(68) -> bootps(67), checksum 0 (optional for IPv4) ---
    wr_be16(udp, 68);
    wr_be16(udp + 2, 67);
    wr_be16(udp + 4, udp_len as u16);
    wr_be16(udp + 6, 0);

    // --- IPv4 header ---
    dma_write::<u8>(ip, 0x45); // version 4, IHL 5
    dma_write::<u8>(ip + 1, 0x00);
    wr_be16(ip + 2, ip_total as u16);
    wr_be16(ip + 4, 0); // identification
    wr_be16(ip + 6, 0); // flags/fragment
    dma_write::<u8>(ip + 8, 64); // TTL
    dma_write::<u8>(ip + 9, 17); // protocol = UDP
    wr_be16(ip + 10, 0); // checksum placeholder
    wr_be32(ip + 12, 0x0000_0000); // src 0.0.0.0
    wr_be32(ip + 16, 0xffff_ffff); // dst 255.255.255.255
    let csum = ip_checksum(ip, 20);
    wr_be16(ip + 10, csum);

    14 + ip_total
}

/// Wait for the receive queue's used ring to advance, discarding wakeups that were really the
/// transmit completion (the device's interrupt line is shared across both queues). Returns false if
/// no receive completion arrives within a bounded number of wakeups. Mirrors the disk's
/// `complete_block`: the used ring, not the wakeup, is the completion.
fn wait_rx(rx_used_before: u16) -> bool {
    for _ in 0..64 {
        // SAFETY: `svc`; blocks until the device raises its line.
        unsafe { invoke(IRQ, abi::irq::WAIT, 0, 0, 0) };
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: `svc`; re-enable the line the kernel masked when it fired.
        unsafe { invoke(IRQ, abi::irq::ACK, 0, 0, 0) };
        barrier();
        if dma_read::<u16>(NET_RX_USED + 2) != rx_used_before {
            return true;
        }
    }
    false
}

/// Parse the frame in the receive buffer as a DHCP OFFER for our transaction. Returns the offered
/// address (`yiaddr`) on a match, `None` otherwise, so an unrelated broadcast is skipped rather than
/// mistaken for the reply.
fn parse_dhcp_offer() -> Option<u32> {
    let f = NET_RX_BUF + NET_HDR_LEN;
    if rd_be16(f + 12) != 0x0800 {
        return None; // not IPv4
    }
    let ip = f + 14;
    let ihl = (dma_read::<u8>(ip) & 0x0f) as u64 * 4;
    if ihl < 20 || dma_read::<u8>(ip + 9) != 17 {
        return None; // not a sane IPv4/UDP header
    }
    let udp = ip + ihl;
    if rd_be16(udp + 2) != 68 {
        return None; // not addressed to the DHCP client port
    }
    let dhcp = udp + 8;
    if dma_read::<u8>(dhcp) != 2 || rd_be32(dhcp + 4) != NET_XID {
        return None; // not a BOOTREPLY for our transaction
    }
    if rd_be32(dhcp + 236) != 0x6382_5363 {
        return None; // no DHCP magic cookie
    }
    let yiaddr = rd_be32(dhcp + 16);

    // Scan the options for message type 53 == 2 (OFFER).
    let mut o = dhcp + 240;
    let end = NET_RX_BUF + NET_RX_BUF_LEN;
    let mut is_offer = false;
    while o + 1 < end {
        let tag = dma_read::<u8>(o);
        if tag == 255 {
            break; // end
        }
        if tag == 0 {
            o += 1; // pad
            continue;
        }
        let len = dma_read::<u8>(o + 1) as u64;
        if tag == 53 && len >= 1 && dma_read::<u8>(o + 2) == 2 {
            is_offer = true;
        }
        o += 2 + len;
    }

    (is_offer && yiaddr != 0).then_some(yiaddr)
}

/// **The virtio-net headline: a DHCP round trip from an EL0 driver.** Bring the NIC up, post a
/// receive buffer, transmit a DHCP DISCOVER, and wait for slirp's OFFER. Reports the offered IPv4
/// address (`yiaddr`) as a little-endian word, which the kernel test checks lands in QEMU user-mode
/// networking's 10.0.2.0/24. TX and RX, both queues, both directions of the confinement, proven end
/// to end with no TCP/IP stack in the loop.
pub fn run_net(dma_phys: u64) -> ! {
    init_net();

    let mut rx_avail: u16 = 0;
    post_rx(dma_phys, rx_avail);
    rx_avail = rx_avail.wrapping_add(1);

    let frame_len = build_dhcp_discover();
    send_frame(dma_phys, frame_len, 0);

    let mut rx_used: u16 = 0;
    for _ in 0..8 {
        if !wait_rx(rx_used) {
            report_code(0xE9); // no receive completion arrived
        }
        rx_used = dma_read::<u16>(NET_RX_USED + 2);

        if let Some(yiaddr) = parse_dhcp_offer() {
            send(REPORT, yiaddr as u64, 0, 0);
            exit(); // one-shot: reported the DHCP offer, so exit and be reaped, do not spin
        }

        // Not our OFFER (some other broadcast): re-post the buffer and wait for the next frame.
        post_rx(dma_phys, rx_avail);
        rx_avail = rx_avail.wrapping_add(1);
    }
    report_code(0xEA); // frames arrived, but none was our DHCP OFFER
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

/// The block server's request endpoint (slot 0): the FS server CALLs here; this role `RECV_CAPs` the
/// request plus the one-shot Reply that names the caller. IRQ (1) and VIRTIO (2) are as every role.
const BLK_REQ: u64 = 0;

/// Where the kernel maps the data region shared with the FS server (the block buffer). Distinct
/// from the DMA region, which the FS server must never see, and from [`DMA_VA`].
///
/// The block server's DMA region is **`1 + blk::TRANSFER_BLOCKS` contiguous pages**
/// (`kernel/src/user/fs_service.rs`): page 0 holds the rings, request header, and status
/// (block-server-private), and pages `1..=blk::TRANSFER_BLOCKS` are the data buffer, which IS the
/// region shared with the FS server. So one virtio request moves up to `blk::TRANSFER_BLOCKS`
/// contiguous filesystem blocks and the device DMAs them straight into the FS server's region as a
/// single descriptor, with no per-block loop and no copy (milestone 138 step 4; it was one page and
/// one block until then). `BLK_OFF_DATA` is that page-1 offset.
const BLK_OFF_DATA: u64 = 0x1000;

/// The block server's readiness endpoint (slot 3): SEND once after the device is up, so the test
/// can tell a device-bring-up hang from a first-read hang. See `kernel/src/user/fs_service.rs`.
const BLK_READY: u64 = 3;

/// virtio-mmio device-config window; virtio-blk's capacity (u64, in 512-byte sectors) is at its
/// start. Reads are DMA-safe, so the kernel's `READ_REG` permits this offset.
const CONFIG: u64 = 0x100;

/// **The block server.** Bring the device up, then serve blk IPC forever. Every request is a CALL
/// answered through the kernel-minted Reply, so this server can answer a client it was never wired
/// to, exactly once each. The transfer unit is one filesystem block; the bulk rides in the shared
/// page, the control (opcode, block index, result) rides in the message, the §10 split.
pub fn run_blk_server(dma_phys: u64) -> ! {
    // Ask for VIRTIO_BLK_F_FLUSH, and remember whether we got it. Everything about
    // `fs_proto::blk::FLUSH` hangs off this one bool: with it, a sync is a device round trip; without
    // it, a sync is a loud refusal. There is no third behaviour, and in particular there is no
    // "answer yes and do nothing", which is the state this server was in before milestone 55.
    let can_flush = init_with_features(0, true);
    // The device is up: signal readiness, so a hang here (bring-up) is distinct from a hang in the
    // first read completion. SEND rendezvous, so this also paces the test past bring-up.
    send(BLK_READY, fs_proto::fixture::READY, 0, 0);
    // How many device flushes have completed. Answered back on every successful `blk::FLUSH`, so a
    // client can tell a real round trip from a constant yes: see `fs_proto::blk::FLUSH`.
    let mut flushes: i64 = 0;
    loop {
        // RECV_CAP: (first word, the Reply cap's slot, second word = the starting block index).
        let (w0, reply, block) = user_rt::recv_cap(BLK_REQ);
        // **Clamp, the same defence the file channel's server-side clamp is** (milestone 138 step
        // 4, mirroring step 3's `fs_service.rs` clamp): every caller today sends at most
        // `blk::TRANSFER_BLOCKS` (the field cannot encode more), but a request is never trusted to
        // stay inside the region it shares just because the packing allows a larger number to be
        // spelled at all.
        let count = blk::req_blocks(w0).min(blk::TRANSFER_BLOCKS) as u64;
        let r0: i64 = match fs_proto::op(w0) {
            blk::READ => {
                blk_read(dma_phys, block, count);
                0
            }
            blk::WRITE => {
                blk_write(dma_phys, block, count);
                0
            }
            blk::SIZE => disk_size_bytes(),
            // The one verb that can refuse for a reason outside this program. A device that never
            // offered the feature gets EOPNOTSUPP rather than a zero, because a zero here is a lie
            // about somebody's data; see `fs_proto::blk::FLUSH`.
            blk::FLUSH => {
                if !can_flush {
                    -95 // EOPNOTSUPP: this device has no flush, and pretending otherwise is the bug
                } else if blk_flush(dma_phys) {
                    flushes += 1;
                    flushes
                } else {
                    -5 // EIO: the device took the request and completed it with a non-OK status
                }
            }
            _ => -22, // EINVAL: an opcode this server does not implement
        };
        // Answer through the one-shot Reply. SAFETY: `svc`; the kernel validated and consumes it.
        unsafe { invoke(reply, abi::reply::REPLY, r0 as u64, 0, 0) };
    }
}

/// Complete a block-server transfer by **waiting on the completion interrupt**, the milestone-9
/// discipline, not by polling the used ring.
///
/// The FS server does hundreds of reads at open (RedoxFS scans a 256-entry header ring). An earlier
/// version polled the used ring here, on the belief that a WAIT-per-read overran the test's watchdog;
/// measured (fix/irq-delivery, 2026-07-29), it does not. Because each request moves a whole 4096-byte
/// filesystem block ([`submit_blk`]), the mount's read count stays in the low hundreds, and a WAIT
/// per read fits well inside the watchdog on both ISAs at the 4-core boot. So this server uses the
/// same interrupt-as-message completion every other driver does, which is what a real asynchronous
/// device needs and what the confinement was built to carry. QEMU still completes synchronously
/// inside `NOTIFY`, so the interrupt is already pending and the WAIT returns at once (the kernel's
/// pending-signal count, DECISIONS §9a).
fn complete_blk(used_before: u16) {
    // **Wait on the completion interrupt** (the milestone-9 discipline), not a poll of the used ring.
    // The kernel turns the device's completion IRQ into a message on this server's `Irq` endpoint;
    // WAIT for it, quiet the device, ACK the line, and let `used.idx` decide when the completion is
    // really ours (a wakeup can be stale, coalesced, or a prior operator's). This is the same loop
    // `complete_block` uses. An earlier note (notes/fs-server.md) claimed this "overran the watchdog"
    // and forced a poll; it does not, because the whole-block reads above keep the mount's read count
    // low enough that a WAIT per read fits comfortably (proven: fs_server test green on both ISAs at
    // the 4-core boot). QEMU still completes synchronously inside `NOTIFY`, so the interrupt is
    // already pending and the kernel's pending-signal count (DECISIONS §9a) returns this WAIT at once
    // rather than blocking on an event already over. See notes/dma.md.
    loop {
        // SAFETY: as above: the kernel validates the capability and the method.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: as above: the kernel validates the capability and the method.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };
        barrier();
        if dma_read::<u16>(OFF_USED + 2) != used_before {
            break; // our request is on the used ring; this wakeup was really ours
        }
    }
    let st = dma_read::<u8>(OFF_STATUS);
    if st != 0 {
        panic!(); // the device reported a non-OK status
    }
}

/// Read `count` contiguous filesystem blocks starting at `block` in a SINGLE virtio request. The
/// device DMAs straight into the data pages of the DMA region, which is the FS server's shared
/// region, so there is no per-block loop and no copy. Contrast the read path above, which the
/// small-buffer driver roles use one 512-byte sector at a time; the FS server reads hundreds of
/// blocks at open, and moves up to `blk::TRANSFER_BLOCKS` of them per request since milestone 138
/// step 4, both for the same reason: a device round trip costs far more than the transfer itself.
fn blk_read(dma_phys: u64, block: u64, count: u64) {
    let used_before = submit_blk(
        dma_phys,
        block * blk::SECTORS_PER_BLOCK,
        VIRTIO_BLK_T_IN,
        count,
    );
    complete_blk(used_before);
}

/// Write `count` contiguous filesystem blocks from the data pages (the FS server filled them before
/// this request) in a single virtio request. The one direction flag differs from the read; the
/// addresses are identical.
fn blk_write(dma_phys: u64, block: u64, count: u64) {
    let used_before = submit_blk(
        dma_phys,
        block * blk::SECTORS_PER_BLOCK,
        VIRTIO_BLK_T_OUT,
        count,
    );
    complete_blk(used_before);
}

/// **Ask the device to make everything it has acknowledged durable**, and wait for it to say it
/// did. `true` if the device completed with `VIRTIO_BLK_S_OK`.
///
/// This is the operation the whole `VOLUME_FULL_SYNC` claim rests on, so it is worth being exact
/// about what it proves. The device dequeues the request, performs whatever its backing calls a
/// flush, and posts a completion on the used ring with a status byte. When that byte is 0 the
/// device has told us the flush is done. It cannot prove the *hardware* underneath the device is
/// honest, and nothing at this layer could; what it removes is the layer that was previously
/// missing entirely, where nobody asked at all.
///
/// Unlike [`complete_blk`] this does **not** panic on a bad status. The block server is long-lived
/// and its death blocks every client of the filesystem forever, so a device refusing a flush has to
/// come back as an error a caller can read rather than as a corpse. `VIRTIO_BLK_S_UNSUPP` (2) is
/// the status a device returns for a command it will not do, and a device that offered
/// `VIRTIO_BLK_F_FLUSH` and then answers that way is exactly the case worth reporting rather than
/// crashing on.
fn blk_flush(dma_phys: u64) -> bool {
    let used_before = submit_flush(dma_phys);
    complete_flush(used_before)
}

/// A flush's **two**-descriptor chain: the 16-byte request header the device reads, and the status
/// byte it writes. No data descriptor, because a flush moves no data (virtio 1.2 §5.2.6.1); a chain
/// with one would be describing a transfer the device was never asked to make.
fn submit_flush(dma_phys: u64) -> u16 {
    dma_write::<u32>(OFF_HEADER, VIRTIO_BLK_T_FLUSH);
    dma_write::<u32>(OFF_HEADER + 4, 0);
    // The sector field is unused for a flush and the spec says to set it to 0. Writing it anyway
    // rather than leaving the previous request's value there, because a stale sector in a header
    // the device is entitled to ignore is the kind of thing that reads as deliberate to whoever
    // debugs the next device.
    dma_write::<u64>(OFF_HEADER + 8, 0);
    dma_write::<u8>(OFF_STATUS, 0xff);

    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(1, dma_phys + OFF_STATUS, 1, VIRTQ_DESC_F_WRITE, 0);

    let used_before: u16 = dma_read::<u16>(OFF_USED + 2);
    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0);
    barrier();
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier();

    // SAFETY: `svc`. As `submit_blk`: the kernel validates every descriptor against the region.
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, 0, 0, 0) } < 0 {
        panic!();
    }
    used_before
}

/// [`complete_blk`]'s wait, returning the device's verdict instead of panicking on it. See
/// [`blk_flush`] for why the difference is deliberate.
fn complete_flush(used_before: u16) -> bool {
    loop {
        // SAFETY: `svc`; the kernel validates the capability and the method.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: as above.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };
        barrier();
        if dma_read::<u16>(OFF_USED + 2) != used_before {
            break; // our request is on the used ring; this wakeup was really ours
        }
    }
    dma_read::<u8>(OFF_STATUS) == 0
}

/// Build and publish the three-descriptor chain for a `count`-block transfer and ring the device
/// through the kernel. Mirrors [`submit_block`] but with the data descriptor spanning `count`
/// contiguous blocks at [`BLK_OFF_DATA`] (the shared data pages) instead of one sector. **One
/// descriptor for the whole transfer, whatever `count` is** (milestone 138 step 4): virtio-blk
/// places no requirement on a data descriptor's length beyond a whole number of sectors, so
/// widening it is the entire batching mechanism, the same shape step 3 used for the file channel's
/// length field. Returns the used-ring index from before the submit, which [`complete_block`]
/// compares against.
fn submit_blk(dma_phys: u64, sector: u64, req_type: u32, count: u64) -> u16 {
    dma_write::<u32>(OFF_HEADER, req_type);
    dma_write::<u32>(OFF_HEADER + 4, 0);
    dma_write::<u64>(OFF_HEADER + 8, sector);
    dma_write::<u8>(OFF_STATUS, 0xff);

    let data_flags = if req_type == VIRTIO_BLK_T_IN {
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE // read: the device fills the data pages
    } else {
        VIRTQ_DESC_F_NEXT // write: the device consumes the data pages
    };
    let data_len = count * blk::BLOCK_SIZE as u64;
    write_desc(0, dma_phys + OFF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(1, dma_phys + BLK_OFF_DATA, data_len as u32, data_flags, 2);
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
