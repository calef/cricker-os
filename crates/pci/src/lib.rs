//! PCI configuration space: the decode logic, and none of the MMIO.
//!
//! This crate knows how ECAM addressing works, how to walk a config space for devices, how to
//! size a BAR, and how to find the virtio vendor capabilities. It deliberately does not know how
//! to *touch* any of it: every function takes read/write closures over `(bdf, offset)`, and the
//! kernel passes volatile accessors into the mapped ECAM window while the tests pass a fake
//! config space built in an array. The same inversion `validate_and_shadow` uses in
//! kernel/src/virtio.rs, and the reason all of this is host-testable.
//!
//! Vocabulary, because PCI is fifty years of acronyms: a function is addressed by **BDF**
//! (bus, device, function). Each function has 4 KB of **configuration space** (ECAM: a flat
//! memory window, 4 KB per function, `(bus << 20) | (dev << 15) | (fn << 12)`). The first 64
//! bytes are the standardized header: vendor/device id, command/status, class, the **BARs**
//! (Base Address Registers, where the function's register blocks live in memory), and the
//! capability list pointer. Everything virtio-modern needs beyond that lives in **vendor
//! capabilities** in that list. See notes/pcie.md.
//!
//! # Examples
//!
//! Enumerating a bus with no bus. Every function here takes read/write closures over
//! `(bdf, offset)`, so the kernel passes volatile accessors into the mapped ECAM window and a test
//! passes an array. **That inversion is why this crate is host-testable at all**, so it is what an
//! example should show:
//!
//! ```
//! use pci::{Bdf, VENDOR_ID, VIRTIO_VENDOR, enumerate};
//!
//! // A config space with exactly one device: modern virtio-blk at 00:01.0. Everything else reads
//! // 0xffff, which is the bus's way of saying nobody is home.
//! let mut read32 = |bdf: Bdf, off: u64| -> u32 {
//!     if bdf == (Bdf { bus: 0, dev: 1, func: 0 }) && off == VENDOR_ID {
//!         (0x1042 << 16) | VIRTIO_VENDOR as u32 // device 0x1042 in the high half
//!     } else if off == VENDOR_ID {
//!         0xffff_ffff
//!     } else {
//!         0 // header type 0: single function, so functions 1..8 are skipped
//!     }
//! };
//!
//! let mut found = Vec::new();
//! enumerate(1, &mut read32, &mut |bdf, vendor, device| {
//!     found.push((bdf, vendor, device));
//! });
//!
//! assert_eq!(found.len(), 1);
//! let (bdf, vendor, device) = found[0];
//! assert_eq!((vendor, device), (VIRTIO_VENDOR, 0x1042));
//!
//! // ECAM addressing: 4 KB per function, `bus:8 | dev:5 | fn:3 | offset:12`. This shift-and-or is
//! // the whole of the spec.
//! assert_eq!(bdf.ecam_offset(), 1 << 15);
//!
//! // And the requester id, which is the key an IOMMU looks the device up by. Both `virt` boards
//! // publish an identity `iommu-map`, so this number is exactly what the IOMMU sees.
//! assert_eq!(bdf.requester_id(), 0b0000_0000_0000_1000);
//! ```
//!
//! The INTx swizzle is four lines and worth pinning, because getting it wrong misroutes an interrupt
//! to a device that will never acknowledge it:
//!
//! ```
//! use pci::intx_irq;
//!
//! // Four devices, each on INTA, spread across four interrupt inputs rather than sharing one.
//! let base = 32;
//! let spread: Vec<u32> = (0..4).map(|dev| intx_irq(base, dev, 1)).collect();
//! assert_eq!(spread, vec![32, 33, 34, 35]);
//!
//! // The swizzle wraps, which is what makes it a swizzle rather than an offset.
//! assert_eq!(intx_irq(base, 4, 1), 32);
//! // And one device's four pins also spread, so a multi-function card does not self-collide.
//! assert_eq!(intx_irq(base, 0, 4), 35);
//! ```
//!
//! Name: ratified 2026-08-01 (calef, the naming tenet in CLAUDE.md). Named in the group of standard
//! terms that are already right and must not be touched, because a name a reader knows from outside
//! this project costs nothing to learn and renaming it would destroy the recognition the tenet
//! exists to buy.

#![cfg_attr(not(test), no_std)]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 24-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]

/// A function's address: (bus, device, function).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bdf {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Bdf {
    /// The function's byte offset inside the ECAM window: 4 KB per function, laid out
    /// `bus:8 | dev:5 | fn:3 | offset:12`. This shift-and-or IS the ECAM spec.
    pub fn ecam_offset(self) -> u64 {
        ((self.bus as u64) << 20) | ((self.dev as u64) << 15) | ((self.func as u64) << 12)
    }

    /// The **PCIe requester id**: the 16-bit `bus:8 | dev:5 | fn:3` a function stamps on every
    /// memory transaction it issues, and the key an IOMMU (SMMUv3 `StreamID`, RISC-V IOMMU `device_id`)
    /// looks a device up by. Both `virt` boards publish an identity `iommu-map` in the device tree,
    /// so this number is exactly the id the IOMMU sees. See kernel/src/iommu.rs (milestone 16b).
    pub fn requester_id(self) -> u32 {
        ((self.bus as u32) << 8) | ((self.dev as u32) << 3) | (self.func as u32)
    }
}

// Standardized config-space header offsets (type 0).
pub const VENDOR_ID: u64 = 0x00;
pub const COMMAND: u64 = 0x04;
/// Class code (24 bits) over revision id (8): `cfg_read32(bdf, CLASS_REVISION) >> 8` is the class.
pub const CLASS_REVISION: u64 = 0x08;
pub const HEADER_TYPE: u64 = 0x0e;
pub const BAR0: u64 = 0x10;
pub const CAP_PTR: u64 = 0x34;
pub const INTERRUPT_PIN: u64 = 0x3d;

/// The NVMe class code: mass storage (0x01) / non-volatile memory (0x08) / NVMe I/O (0x02).
/// Matched by class rather than by vendor/device id on purpose: QEMU's controller is Red Hat
/// 1b36:0010, real drives are anything at all, and the class triple is the one identity the spec
/// requires of every one of them (NVMe 1.4 §3.1's PCI header requirements).
pub const CLASS_NVME: u32 = 0x01_08_02;

/// Command register bits we set to bring a device up.
pub const CMD_MEMORY_SPACE: u16 = 1 << 1;
/// Bus mastering is DMA permission at the PCI level: without it the device cannot issue a single
/// memory transaction. The kernel-side confinement (the shadow ring) polices *where* the DMA
/// goes; this bit is what allows DMA to exist at all, so it is set last, after the transport is
/// registered.
pub const CMD_BUS_MASTER: u16 = 1 << 2;

/// Status register bit 4: this function has a capability list.
const STATUS_CAP_LIST: u16 = 1 << 4;

/// virtio's PCI vendor id, and the modern virtio-blk device id (0x1040 + device type 2).
pub const VIRTIO_VENDOR: u16 = 0x1af4;
pub const VIRTIO_BLK_MODERN: u16 = 0x1042;
/// The transitional (legacy-capable) virtio-blk id. We do not drive legacy, but we recognize it
/// so enumeration can say "found a disk, but it is transitional" instead of "no disk".
pub const VIRTIO_BLK_TRANSITIONAL: u16 = 0x1001;

/// The modern virtio-net device id (0x1040 + device type 1), and its transitional twin. Same
/// recognition role as the blk pair: the net stack (milestone 30) drives modern only, and a
/// transitional NIC is reported as such rather than counted as absent.
pub const VIRTIO_NET_MODERN: u16 = 0x1041;
pub const VIRTIO_NET_TRANSITIONAL: u16 = 0x1000;

/// The modern virtio-gpu device id (0x1040 + device type 16), milestone 29. There is **no
/// transitional twin**: the legacy id space (0x1000..0x103f) was allocated before virtio-gpu
/// existed, so a GPU is modern-or-nothing and enumeration passes `None` for the legacy id rather
/// than inventing one. QEMU's `virtio-gpu-pci` presents exactly this id.
pub const VIRTIO_GPU_MODERN: u16 = 0x1050;

/// The modern virtio-input device id (0x1040 + device type 18), milestone 29's keyboard. **No
/// transitional twin**, for the same reason virtio-gpu has none: the device type was allocated long
/// after the legacy id space. QEMU's `virtio-keyboard-pci` presents exactly this id, and so does
/// `virtio-tablet-pci`, which is worth knowing rather than discovering: **the id names the device
/// class, not the keyboard**, so a machine with both would have to read the device's configuration
/// space to tell them apart. We attach only a keyboard.
pub const VIRTIO_INPUT_MODERN: u16 = 0x1052;

/// The modern virtio-rng device id (0x1040 + device type 4), milestone 56's entropy source, and its
/// transitional twin. Unlike the GPU and the keyboard this one **does** have a legacy id: virtio-rng
/// is device type 4, early enough to have been allocated a slot in the 0x1000..0x103f space. We
/// drive modern only, so the legacy id is here for the same reason blk's and net's are, to let
/// enumeration say "found an RNG, but it is transitional" rather than "no RNG".
pub const VIRTIO_RNG_MODERN: u16 = 0x1044;
pub const VIRTIO_RNG_TRANSITIONAL: u16 = 0x1005;

/// The virtio **device type** numbers, as the virtio-mmio `DeviceID` register reports them and as
/// the PCI ids above encode them (`0x1040 + type`). The kernel carries the type through the PCI
/// transport so a driver's `DeviceID` read answers truthfully on either bus; see
/// `kernel/src/virtio.rs`.
pub const VIRTIO_TYPE_NET: u32 = 1;
pub const VIRTIO_TYPE_BLOCK: u32 = 2;
pub const VIRTIO_TYPE_ENTROPY: u32 = 4;
pub const VIRTIO_TYPE_GPU: u32 = 16;
pub const VIRTIO_TYPE_INPUT: u32 = 18;

/// Walk every function on `buses` buses and call `f` with (bdf, vendor, device). Empty slots
/// read vendor 0xffff (the bus's way of saying "nobody home") and are skipped; a single-function
/// device (header type bit 7 clear) skips functions 1..8. QEMU `virt` is flat on bus 0, but the
/// walk covers every bus in range so a bridge topology enumerates too; the caller picks how many
/// buses its `bus-range` covers.
pub fn enumerate(
    buses: u16,
    read32: &mut dyn FnMut(Bdf, u64) -> u32,
    f: &mut dyn FnMut(Bdf, u16, u16),
) {
    for bus in 0..buses.min(256) {
        for dev in 0..32 {
            let bdf0 = Bdf {
                bus: bus as u8,
                dev,
                func: 0,
            };
            let id = read32(bdf0, VENDOR_ID);
            if id & 0xffff == 0xffff {
                continue; // empty slot
            }
            let multifunction =
                (read32(bdf0, HEADER_TYPE & !3) >> ((HEADER_TYPE & 3) * 8)) & 0x80 != 0;
            let funcs = if multifunction { 8 } else { 1 };
            for func in 0..funcs {
                let bdf = Bdf {
                    bus: bus as u8,
                    dev,
                    func,
                };
                let id = read32(bdf, VENDOR_ID);
                if id & 0xffff == 0xffff {
                    continue;
                }
                f(bdf, (id & 0xffff) as u16, (id >> 16) as u16);
            }
        }
    }
}

/// One decoded Base Address Register: where the register block is, how big, and how wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// The assigned base address (0 if firmware assigned nothing; the caller must then place it).
    pub base: u64,
    /// The region size in bytes, from the write-ones probe.
    pub size: u64,
    /// A 64-bit BAR consumes two BAR slots; the caller's index bookkeeping needs to know.
    pub is_64: bool,
}

/// Read and size the six BAR slots of a type-0 header. Returns one entry per **slot** (six),
/// `None` for the upper half of a 64-bit BAR and for I/O-space BARs (we drive memory BARs only).
///
/// Sizing is the standard dance the spec prescribes: write all-ones, read back the mask of
/// writable bits (the low bits that stay zero encode the alignment/size), restore the original.
/// The device is quiescent during this (memory decoding is off until the command register is
/// set), which is why enumeration runs before enable.
pub fn read_bars(
    bdf: Bdf,
    read32: &mut dyn FnMut(Bdf, u64) -> u32,
    write32: &mut dyn FnMut(Bdf, u64, u32),
) -> [Option<Bar>; 6] {
    let mut out = [None; 6];
    let mut i = 0;
    while i < 6 {
        let off = BAR0 + i as u64 * 4;
        let orig = read32(bdf, off);
        if orig & 1 != 0 {
            // An I/O-space BAR. x86 legacy; nothing we drive uses one.
            i += 1;
            continue;
        }
        let is_64 = orig & 0b110 == 0b100;

        write32(bdf, off, u32::MAX);
        let mask_lo = read32(bdf, off);
        write32(bdf, off, orig);

        if is_64 {
            let off_hi = off + 4;
            let orig_hi = read32(bdf, off_hi);
            write32(bdf, off_hi, u32::MAX);
            let mask_hi = read32(bdf, off_hi);
            write32(bdf, off_hi, orig_hi);

            let mask = ((mask_hi as u64) << 32) | (mask_lo as u64 & 0xffff_fff0);
            if mask != 0 {
                out[i] = Some(Bar {
                    base: ((orig_hi as u64) << 32) | (orig as u64 & 0xffff_fff0),
                    size: !mask + 1,
                    is_64: true,
                });
            }
            i += 2; // the upper half consumed a slot
        } else {
            let mask = mask_lo as u64 & 0xffff_fff0;
            if mask != 0 {
                out[i] = Some(Bar {
                    base: orig as u64 & 0xffff_fff0,
                    // Bits above a 32-bit BAR's reach can never be set; extend the mask so the
                    // `!mask + 1` size math is done at u64 width.
                    size: !(mask | 0xffff_ffff_0000_0000) + 1,
                    is_64: false,
                });
            }
            i += 1;
        }
    }
    out
}

/// What each virtio vendor capability describes (virtio spec 4.1.4: `cfg_type`).
pub const VIRTIO_CAP_COMMON: u8 = 1;
pub const VIRTIO_CAP_NOTIFY: u8 = 2;
pub const VIRTIO_CAP_ISR: u8 = 3;
pub const VIRTIO_CAP_DEVICE: u8 = 4;

/// A virtio vendor capability, decoded: which register block it names, and where that block
/// lives as (BAR index, offset into the BAR, length). `notify_off_multiplier` is meaningful only
/// for the notify capability (the doorbell for queue N is at
/// `offset + queue_notify_off * multiplier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioCap {
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    pub notify_off_multiplier: u32,
}

/// Walk the capability list and call `f` for each **virtio vendor capability** (cap id 0x09).
/// The list is a linked list in config space: status bit 4 says one exists, 0x34 points at the
/// first entry, each entry is `{cap_id: u8, next: u8, ...}`. Bounded at 64 hops so a cycle in a
/// hostile or broken device terminates instead of hanging the walk.
pub fn virtio_caps(
    bdf: Bdf,
    read32: &mut dyn FnMut(Bdf, u64) -> u32,
    f: &mut dyn FnMut(VirtioCap),
) {
    const CAP_ID_VENDOR: u8 = 0x09;

    let status = (read32(bdf, COMMAND) >> 16) as u16;
    if status & STATUS_CAP_LIST == 0 {
        return;
    }
    let mut at = (read32(bdf, CAP_PTR) & 0xfc) as u64;
    for _ in 0..64 {
        if at == 0 {
            return;
        }
        let head = read32(bdf, at);
        let cap_id = (head & 0xff) as u8;
        let next = ((head >> 8) & 0xfc) as u64;
        if cap_id == CAP_ID_VENDOR {
            // struct virtio_pci_cap: u8 id, next, cap_len, cfg_type; u8 bar; 3 pad; u32 offset,
            // length; the notify capability carries one extra u32 after that.
            let cfg_type = ((head >> 24) & 0xff) as u8;
            let bar = (read32(bdf, at + 4) & 0xff) as u8;
            f(VirtioCap {
                cfg_type,
                bar,
                offset: read32(bdf, at + 8),
                length: read32(bdf, at + 12),
                notify_off_multiplier: if cfg_type == VIRTIO_CAP_NOTIFY {
                    read32(bdf, at + 16)
                } else {
                    0
                },
            });
        }
        at = next;
    }
}

/// The INTx swizzle on QEMU's `virt` boards: the legacy interrupt pin of the function at device
/// `d` using pin `p` (1=INTA..4=INTD) lands on PLIC/GIC input `base + ((d + p - 1) % 4)`. This is
/// the standard bridge swizzle the PCI spec prescribes for a flat bus, and QEMU's generic ECAM
/// bridge implements exactly it; the dtb crate's fixture test cross-checks this formula against
/// the machine's own `interrupt-map`, so if a future board routes differently the host tests say
/// so before the kernel misroutes an interrupt.
pub fn intx_irq(base: u32, dev: u8, pin: u8) -> u32 {
    // Total for every input, proved in the verification module. Pins are 1-based (1=INTA); the
    // saturating_sub means a (contract-violating) pin of 0 behaves as INTA instead of
    // underflowing, and the saturating_add means a nonsense base cannot wrap. The callers all
    // check pin == 0 before calling; this is defence in depth, not an invitation.
    base.saturating_add((dev as u32 + (pin as u32).saturating_sub(1)) % 4)
}

/// The 32-bit non-prefetchable memory window from a PCI host bridge's `ranges` property, as
/// `(cpu_base, size)`. `None` when no entry qualifies, or the property is not the standard shape.
///
/// `ranges` is the property's raw bytes: big-endian cells in the standard PCI layout (child
/// `#address-cells = 3`, parent `#address-cells = 2`, `#size-cells = 2`, so seven cells per
/// entry). The first child cell (`phys.hi`) carries the space code at bits 25:24 (0b01 IO,
/// 0b10 32-bit memory, 0b11 64-bit memory) and prefetchability at bit 30; the rest are the
/// 64-bit PCI address, CPU address, and size. This takes the first 32-bit non-prefetchable
/// memory entry, which is the window the kernel assigns BARs from (kernel/src/pci.rs).
///
/// An entry whose PCI and CPU addresses differ is skipped rather than translated, on purpose:
/// the kernel's BAR assigner writes one number into both the BAR register and its own page
/// tables, so a translated window is one it cannot yet honor, and refusing it here is honest
/// where handing back a CPU address the bridge would not decode from a BAR is not. Both QEMU
/// `virt` boards state identity windows. A ragged length (not a multiple of seven cells) means
/// the node is not the shape this parser knows, and the answer is `None` rather than a guess.
pub fn mem32_window(ranges: &[u8]) -> Option<(u64, u64)> {
    const ENTRY: usize = 7 * 4;
    if ranges.is_empty() || !ranges.len().is_multiple_of(ENTRY) {
        return None;
    }
    for entry in ranges.as_chunks::<ENTRY>().0 {
        let cell = |i: usize| -> u64 {
            u64::from(u32::from_be_bytes(
                entry[i * 4..i * 4 + 4].try_into().unwrap(),
            ))
        };
        let hi = cell(0);
        let space = (hi >> 24) & 0b11;
        let prefetchable = hi & (1 << 30) != 0;
        if space != 0b10 || prefetchable {
            continue;
        }
        let pci_addr = (cell(1) << 32) | cell(2);
        let cpu_addr = (cell(3) << 32) | cell(4);
        let size = (cell(5) << 32) | cell(6);
        if pci_addr != cpu_addr || size == 0 {
            continue;
        }
        return Some((cpu_addr, size));
    }
    None
}

/// Machine-checked proofs (`script/verify`; notes/verification.md).
///
/// This crate's input comes from a DEVICE: a hostile or broken PCI function can return any
/// bytes at all through the config-space closures, and the decode runs in the kernel. So the
/// properties proved are the hostile-input ones: the walks are total (no device response can
/// panic them) and structurally bounded (a cycle in a capability list terminates). `enumerate`
/// has no proof because it has nothing to prove: it owns no arrays and does no fallible
/// arithmetic; its loops are bounded by literals.
#[cfg(kani)]
mod verification {
    use super::*;

    /// **ECAM addressing stays inside the window.** Any BDF's config page lies below the
    /// 256-bus window size (`0x1000_0000`), so `ecam_base + ecam_offset() + off` for off < 4096
    /// cannot escape a correctly-sized mapping. This is the arithmetic the kernel's volatile
    /// accessors trust.
    #[kani::proof]
    fn ecam_offset_stays_inside_the_window() {
        let bdf = Bdf {
            bus: kani::any(),
            dev: kani::any(),
            func: kani::any(),
        };
        // dev and func are 5- and 3-bit fields by construction of every caller (enumerate
        // produces them from bounded loops); the offset must hold for all such values.
        kani::assume(bdf.dev < 32 && bdf.func < 8);
        assert!(bdf.ecam_offset() + 0xfff < 0x1000_0000);
    }

    /// **`intx_irq` is total and lands on one of the four lines.** For any base, device, and
    /// pin, no underflow (the pin-0 case that used to panic in debug builds) and no overflow,
    /// and the result is within `base..=base+3` whenever that range exists.
    #[kani::proof]
    fn intx_irq_is_total_and_bounded() {
        let base: u32 = kani::any();
        let dev: u8 = kani::any();
        let pin: u8 = kani::any();
        let irq = intx_irq(base, dev, pin);
        assert!(irq >= base || irq == u32::MAX);
        assert!(irq.saturating_sub(base) <= 3 || irq == u32::MAX);
    }

    /// **`read_bars` is total for any device responses.** The closures return arbitrary values
    /// on every call, standing in for a device that answers the size probe with garbage; the
    /// decode must never panic (the size arithmetic `!mask + 1` cannot overflow because the
    /// type bits are masked out of `mask` first, so it is never all-ones).
    #[kani::proof]
    #[kani::unwind(8)]
    fn read_bars_is_total_for_any_device() {
        let bdf = Bdf {
            bus: kani::any(),
            dev: kani::any(),
            func: kani::any(),
        };
        let _ = read_bars(bdf, &mut |_, _| kani::any(), &mut |_, _, _| {});
    }

    /// **The capability walk terminates and never panics, even on a cyclic list.** The
    /// closures return arbitrary values, standing in for a device whose capability pointers
    /// form any graph at all; the walk visits at most 64 entries and the callback fires at
    /// most that often. This is the bounded-walk discipline (the virtqueue chain walk's twin)
    /// proved rather than argued.
    #[kani::proof]
    #[kani::unwind(66)]
    fn the_capability_walk_terminates_on_any_device() {
        let bdf = Bdf {
            bus: kani::any(),
            dev: kani::any(),
            func: kani::any(),
        };
        let mut calls = 0u32;
        virtio_caps(bdf, &mut |_, _| kani::any(), &mut |_| calls += 1);
        assert!(calls <= 64);
    }

    /// **The ranges parser is total.** The property comes from firmware's device tree, which is
    /// the same hostile-input class as a device's config space: any byte string at all must
    /// come back as an answer or a `None`, never a panic. Three entries covers every branch
    /// (the loop is per-entry with no state across iterations).
    #[kani::proof]
    #[kani::unwind(4)]
    fn the_ranges_parse_is_total() {
        let bytes: [u8; 3 * 28] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        let _ = mem32_window(&bytes[..len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake config space: a virtio-blk at 00:01.0 with two BARs and a capability list, a
    /// multifunction device at 00:02, a device whose first BAR is 64-bit at 00:03, and empty
    /// slots everywhere else. Offsets are byte addresses into per-function 4 KB pages.
    struct FakeCfg {
        space: std::collections::HashMap<(u8, u8, u8, u64), u32>,
    }

    impl FakeCfg {
        fn new() -> Self {
            let mut s = std::collections::HashMap::new();
            let f = (0u8, 1u8, 0u8);
            // vendor 0x1af4, device 0x1042 (modern virtio-blk)
            s.insert((f.0, f.1, f.2, 0x00), 0x1042_1af4u32);
            // command 0, status: capability list present
            s.insert((f.0, f.1, f.2, 0x04), (0x0010u32) << 16);
            // header type 0, single-function
            s.insert((f.0, f.1, f.2, 0x0c), 0);
            // BAR0: 32-bit memory at 0x4000_0000 (assigned), size 0x1000
            s.insert((f.0, f.1, f.2, 0x10), 0x4000_0000);
            // BAR4: 64-bit memory, unassigned (base 0), size 0x4000
            s.insert((f.0, f.1, f.2, 0x20), 0b100);
            s.insert((f.0, f.1, f.2, 0x24), 0);
            // capability list: 0x40 -> vendor cap (common, bar 4, off 0x0, len 0x1000)
            //                  0x50 -> vendor cap (notify, bar 4, off 0x3000, len 0x1000, mult 4;
            //                          the notify cap is 20 bytes, so its multiplier sits at 0x60)
            //                  0x70 -> MSI-X cap (id 0x11), to prove non-vendor caps are skipped
            s.insert((f.0, f.1, f.2, 0x34), 0x40);
            s.insert(
                (f.0, f.1, f.2, 0x40),
                u32::from_le_bytes([0x09, 0x50, 16, VIRTIO_CAP_COMMON]),
            );
            s.insert((f.0, f.1, f.2, 0x44), 4); // bar 4
            s.insert((f.0, f.1, f.2, 0x48), 0x0); // offset
            s.insert((f.0, f.1, f.2, 0x4c), 0x1000); // length
            s.insert(
                (f.0, f.1, f.2, 0x50),
                u32::from_le_bytes([0x09, 0x70, 20, VIRTIO_CAP_NOTIFY]),
            );
            s.insert((f.0, f.1, f.2, 0x54), 4);
            s.insert((f.0, f.1, f.2, 0x58), 0x3000);
            s.insert((f.0, f.1, f.2, 0x5c), 0x1000);
            s.insert((f.0, f.1, f.2, 0x60), 4); // notify_off_multiplier
            s.insert(
                (f.0, f.1, f.2, 0x70),
                u32::from_le_bytes([0x11, 0x00, 0, 0]),
            ); // MSI-X, end of list

            // 00:01.1 answers, and must never be enumerated: the header at 00:01.0 says
            // single-function. A device that aliases its config space across all eight functions
            // is why that bit is checked instead of probing all eight and believing the answers.
            s.insert((0, 1, 1, 0x00), 0x1042_1af4);

            // 00:02.0, multifunction (header type bit 7 set): virtio-net at function 0 and
            // virtio-rng at function 1, functions 2..8 empty so the per-function "nobody home"
            // check has something to reject.
            s.insert((0, 2, 0, 0x00), 0x1041_1af4);
            s.insert((0, 2, 0, 0x0c), 0x0080_0000); // header type is byte 0x0e of this dword
            s.insert((0, 2, 1, 0x00), 0x1044_1af4);

            // 00:03.0, a virtio-gpu whose FIRST BAR is 64-bit and assigned. The blk device's
            // 64-bit BAR is the last pair and unassigned, so nothing there shows the walk
            // resuming two slots on, or a base whose high half is nonzero.
            s.insert((0, 3, 0, 0x00), 0x1050_1af4);
            s.insert((0, 3, 0, 0x0c), 0);
            s.insert((0, 3, 0, 0x10), 0xc000_0004); // 64-bit memory BAR, base 0x1_c000_0000
            s.insert((0, 3, 0, 0x14), 0x0000_0001);

            FakeCfg { space: s }
        }

        fn read32(&mut self, bdf: Bdf, off: u64) -> u32 {
            // Empty slots float high: all-ones, the hardware's "nobody home".
            *self
                .space
                .get(&(bdf.bus, bdf.dev, bdf.func, off & !3))
                .unwrap_or(&u32::MAX)
        }

        /// BAR writes emulate the size probe: all-ones writes read back as the size mask.
        fn write32(&mut self, bdf: Bdf, off: u64, v: u32) {
            let key = (bdf.bus, bdf.dev, bdf.func, off & !3);
            if !self.space.contains_key(&key) {
                return;
            }
            // Keyed on the device too: 00:01 and 00:03 both have a BAR at 0x10 and they are
            // different sizes.
            let v = match (bdf.dev, off & !3) {
                (1, 0x10) if v == u32::MAX => !(0x1000u32 - 1), // BAR0: size 0x1000
                (1, 0x20) if v == u32::MAX => (!(0x4000u32 - 1)) | 0b100, // BAR4 low: size 0x4000, keep type bits
                (1, 0x24) if v == u32::MAX => u32::MAX, // BAR4 high: all bits writable
                (3, 0x10) if v == u32::MAX => (!(0x2000u32 - 1)) | 0b100, // BAR0 low: size 0x2000
                (3, 0x14) if v == u32::MAX => u32::MAX, // BAR0 high
                _ => v,
            };
            self.space.insert(key, v);
        }
    }

    /// ECAM addressing is a pure shift-and-or, and these three points pin the layout: function
    /// stride 4 KB, device stride 32 KB, bus stride 1 MB.
    #[test]
    fn ecam_offsets_have_the_specified_strides() {
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 0,
                func: 1
            }
            .ecam_offset(),
            0x1000
        );
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 1,
                func: 0
            }
            .ecam_offset(),
            0x8000
        );
        assert_eq!(
            Bdf {
                bus: 1,
                dev: 0,
                func: 0
            }
            .ecam_offset(),
            0x10_0000
        );
    }

    /// The requester id is `bus:8 | dev:5 | fn:3`, the id the IOMMU keys per-device tables on
    /// (milestone 16b). A different packing than `ecam_offset`, so it gets its own witness.
    #[test]
    fn requester_id_packs_bus_dev_fn() {
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 0,
                func: 0
            }
            .requester_id(),
            0
        );
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 1,
                func: 0
            }
            .requester_id(),
            0x08
        );
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 0,
                func: 1
            }
            .requester_id(),
            0x01
        );
        assert_eq!(
            Bdf {
                bus: 0,
                dev: 2,
                func: 3
            }
            .requester_id(),
            0x13
        );
        assert_eq!(
            Bdf {
                bus: 1,
                dev: 0,
                func: 0
            }
            .requester_id(),
            0x100
        );
    }

    /// Enumeration reports every function that answers and nothing else. The three properties
    /// come as one walk because they only exist together: empty slots (vendor 0xffff) are
    /// skipped, the multifunction device's function 1 is found, and the single-function device's
    /// function 1 is never probed even though this fixture would answer for it.
    #[test]
    fn enumeration_follows_the_multifunction_bit_and_skips_what_does_not_answer() {
        let mut cfg = FakeCfg::new();
        let mut found = Vec::new();
        enumerate(
            256,
            &mut |b, o| cfg.read32(b, o),
            &mut |bdf, vendor, device| found.push((bdf, vendor, device)),
        );
        assert_eq!(
            found,
            vec![
                (
                    Bdf {
                        bus: 0,
                        dev: 1,
                        func: 0
                    },
                    VIRTIO_VENDOR,
                    VIRTIO_BLK_MODERN
                ),
                (
                    Bdf {
                        bus: 0,
                        dev: 2,
                        func: 0
                    },
                    VIRTIO_VENDOR,
                    VIRTIO_NET_MODERN
                ),
                (
                    Bdf {
                        bus: 0,
                        dev: 2,
                        func: 1
                    },
                    VIRTIO_VENDOR,
                    VIRTIO_RNG_MODERN
                ),
                (
                    Bdf {
                        bus: 0,
                        dev: 3,
                        func: 0
                    },
                    VIRTIO_VENDOR,
                    VIRTIO_GPU_MODERN
                ),
            ],
        );
    }

    /// An empty slot costs exactly one config read, which is the entire job of the vendor-id
    /// check on function 0. Without it every empty slot on the bus also pays a header-type read
    /// and eight per-function reads, and the walk still returns the right answer, so read count
    /// is the only thing that can see the guard.
    #[test]
    fn an_empty_slot_costs_one_config_read() {
        let mut cfg = FakeCfg::new();
        let mut reads = 0usize;
        enumerate(
            1,
            &mut |b, o| {
                reads += 1;
                cfg.read32(b, o)
            },
            &mut |_, _, _| {},
        );
        // 29 empty slots at one read each, plus the three that answer: blk 3 (id, header type,
        // one function), the multifunction device 10 (id, header type, eight functions), gpu 3.
        assert_eq!(reads, 29 + 3 + 10 + 3);
    }

    /// The BAR probe decodes an assigned 32-bit BAR and an UNassigned 64-bit BAR (base 0), sizes
    /// both by the write-ones dance, marks widths, and restores the original values (the probe
    /// must not leave the device reprogrammed).
    #[test]
    fn bars_are_sized_and_restored() {
        let cfg = std::cell::RefCell::new(FakeCfg::new());
        let bdf = Bdf {
            bus: 0,
            dev: 1,
            func: 0,
        };
        let bars = read_bars(
            bdf,
            &mut |b, o| cfg.borrow_mut().read32(b, o),
            &mut |b, o, v| cfg.borrow_mut().write32(b, o, v),
        );
        let mut cfg = cfg.into_inner();

        assert_eq!(
            bars[0],
            Some(Bar {
                base: 0x4000_0000,
                size: 0x1000,
                is_64: false
            })
        );
        assert_eq!(
            bars[4],
            Some(Bar {
                base: 0,
                size: 0x4000,
                is_64: true
            })
        );
        assert_eq!(bars[1], None);
        assert_eq!(
            bars[5], None,
            "the upper half of a 64-bit BAR is not its own BAR"
        );

        // Restored: originals read back, not the probe's all-ones.
        assert_eq!(cfg.read32(bdf, 0x10), 0x4000_0000);
        assert_eq!(cfg.read32(bdf, 0x20), 0b100);
    }

    /// A 64-bit BAR in the *first* slot: the base is assembled from both halves, the upper half
    /// is not a BAR of its own, and the walk stops after six slots. None of the three is visible
    /// on the blk device, whose 64-bit BAR is unassigned and sits in the last pair, so a base
    /// that ignored its high half and a walk that ran one slot long both read as correct there.
    #[test]
    fn a_64_bit_bar_in_the_first_slot_keeps_its_high_half() {
        let cfg = std::cell::RefCell::new(FakeCfg::new());
        let bdf = Bdf {
            bus: 0,
            dev: 3,
            func: 0,
        };
        // A high-water mark rather than a log of every access: the mutants that make this walk
        // run forever are caught by a timeout, and a growing Vec would turn that into an OOM.
        let highest = std::cell::Cell::new(0u64);
        let bars = read_bars(
            bdf,
            &mut |b, o| {
                highest.set(highest.get().max(o));
                cfg.borrow_mut().read32(b, o)
            },
            &mut |b, o, v| {
                highest.set(highest.get().max(o));
                cfg.borrow_mut().write32(b, o, v);
            },
        );

        assert_eq!(
            bars[0],
            Some(Bar {
                base: 0x1_c000_0000,
                size: 0x2000,
                is_64: true
            })
        );
        assert_eq!(
            bars[1], None,
            "the upper half of a 64-bit BAR is not its own BAR"
        );

        // A type-0 header has six BAR slots, 0x10 through 0x24. 0x28 is the Cardbus CIS pointer,
        // and a walk that ran one slot long would size that pointer as if it were a BAR.
        assert_eq!(
            highest.get(),
            BAR0 + 20,
            "the probe touched a register outside the six BAR slots"
        );
    }

    /// The capability walk yields the two virtio vendor capabilities with their (bar, offset,
    /// length) and the notify multiplier, and skips the MSI-X capability in the same list.
    #[test]
    fn the_capability_walk_finds_virtio_vendor_caps_only() {
        let mut cfg = FakeCfg::new();
        let bdf = Bdf {
            bus: 0,
            dev: 1,
            func: 0,
        };
        let mut caps = Vec::new();
        virtio_caps(bdf, &mut |b, o| cfg.read32(b, o), &mut |c| caps.push(c));

        assert_eq!(caps.len(), 2, "exactly the two vendor caps, MSI-X skipped");
        assert_eq!(caps[0].cfg_type, VIRTIO_CAP_COMMON);
        assert_eq!(
            (caps[0].bar, caps[0].offset, caps[0].length),
            (4, 0, 0x1000)
        );
        assert_eq!(caps[1].cfg_type, VIRTIO_CAP_NOTIFY);
        assert_eq!((caps[1].bar, caps[1].offset), (4, 0x3000));
        assert_eq!(caps[1].notify_off_multiplier, 4);
    }

    /// A function whose status register does not advertise a capability list is not walked. The
    /// capability pointer at 0x34 is undefined on such a function, so following it decodes
    /// whatever bytes happen to sit there as a capability chain. This fixture keeps its real
    /// chain in place and only clears the status bit, which is the case that tells the two
    /// apart.
    #[test]
    fn a_function_without_a_capability_list_is_not_walked() {
        let mut cfg = FakeCfg::new();
        let bdf = Bdf {
            bus: 0,
            dev: 1,
            func: 0,
        };
        cfg.space.insert((0, 1, 0, COMMAND), 0);
        let mut caps = Vec::new();
        virtio_caps(bdf, &mut |b, o| cfg.read32(b, o), &mut |c| caps.push(c));
        assert!(caps.is_empty(), "walked a list the device does not claim");
    }

    /// A capability list that loops must terminate, not hang: hostile or broken hardware gets a
    /// bounded walk, the same discipline as the virtqueue chain walk.
    #[test]
    fn a_cyclic_capability_list_terminates() {
        let mut cfg = FakeCfg::new();
        let bdf = Bdf {
            bus: 0,
            dev: 1,
            func: 0,
        };
        // Point the second cap's `next` back at the first.
        cfg.space.insert(
            (0, 1, 0, 0x50),
            u32::from_le_bytes([0x09, 0x40, 20, VIRTIO_CAP_NOTIFY]),
        );
        let mut n = 0;
        virtio_caps(bdf, &mut |b, o| cfg.read32(b, o), &mut |_| n += 1);
        assert!(n <= 64, "the walk did not terminate promptly on a cycle");
    }

    /// The INTx swizzle, pinned: on QEMU virt the PCI irq base is 32 (riscv) and the four lines
    /// rotate by device. Device 1 pin INTA is base+1; device 4 wraps back to base+0.
    #[test]
    fn the_intx_swizzle_rotates_by_device() {
        assert_eq!(intx_irq(32, 1, 1), 33);
        assert_eq!(intx_irq(32, 2, 1), 34);
        assert_eq!(intx_irq(32, 4, 1), 32);
        assert_eq!(intx_irq(32, 1, 2), 34, "pin B advances the rotation too");
    }

    /// The two command-register bits, pinned to their spec positions. This crate never reads
    /// them back (the kernel writes them into the register), so a wrong value here surfaces as a
    /// device that silently never decodes memory or never gets to DMA, with nothing in the decode
    /// path to catch it.
    #[test]
    fn the_command_bits_are_the_specified_positions() {
        assert_eq!(CMD_MEMORY_SPACE, 0x0002);
        assert_eq!(CMD_BUS_MASTER, 0x0004);
    }

    /// **A modern virtio PCI device id is `0x1040 + the virtio device type`**, which is what lets
    /// the kernel hand a driver a truthful `DeviceID` over the PCI transport instead of a
    /// hardcoded one. Every id we drive is pinned against that derivation, so a typo in one
    /// of them is a build-time-cheap test failure rather than a device we quietly never find.
    /// Build a `ranges` blob from 7-cell entries, the way a device tree stores it.
    fn ranges(entries: &[[u32; 7]]) -> Vec<u8> {
        entries
            .iter()
            .flatten()
            .flat_map(|c| c.to_be_bytes())
            .collect()
    }

    /// The QEMU riscv `virt` ranges verbatim (IO, 32-bit memory, 64-bit memory): the parser must
    /// step past the IO entry, take the mem32 one, and never reach the mem64 one. The fixture
    /// test in `tests/qemu_virt_dtb.rs` holds the same claim against the real tree.
    #[test]
    fn the_mem32_entry_is_found_among_its_neighbours() {
        let r = ranges(&[
            [0x0100_0000, 0, 0, 0, 0x0300_0000, 0, 0x1_0000],
            [0x0200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000],
            [0x0300_0000, 4, 0, 4, 0, 4, 0],
        ]);
        assert_eq!(mem32_window(&r), Some((0x4000_0000, 0x4000_0000)));
    }

    /// A prefetchable 32-bit entry is not the window BARs are placed in; a tree that states only
    /// that answers `None` rather than a window the non-prefetchable BARs should not land in.
    #[test]
    fn a_prefetchable_window_is_not_taken() {
        let r = ranges(&[[0x4200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000]]);
        assert_eq!(mem32_window(&r), None);
    }

    /// A translated window (PCI address != CPU address) is refused, per the doc: the kernel
    /// writes one number into both the BAR and its page tables, so it cannot honor one yet.
    #[test]
    fn a_translated_window_is_refused() {
        let r = ranges(&[[0x0200_0000, 0, 0x0000_0000, 0, 0x4000_0000, 0, 0x4000_0000]]);
        assert_eq!(mem32_window(&r), None);
    }

    /// A ragged length is not the shape this parser knows; `None`, not a partial read. Empty is
    /// the same answer for the same reason.
    #[test]
    fn a_ragged_or_empty_ranges_is_refused() {
        let r = ranges(&[[0x0200_0000, 0, 0x4000_0000, 0, 0x4000_0000, 0, 0x4000_0000]]);
        assert_eq!(mem32_window(&r[..r.len() - 4]), None);
        assert_eq!(mem32_window(&[]), None);
    }

    #[test]
    fn a_modern_virtio_pci_id_is_0x1040_plus_the_device_type() {
        assert_eq!(VIRTIO_NET_MODERN as u32, 0x1040 + VIRTIO_TYPE_NET);
        assert_eq!(VIRTIO_BLK_MODERN as u32, 0x1040 + VIRTIO_TYPE_BLOCK);
        assert_eq!(VIRTIO_RNG_MODERN as u32, 0x1040 + VIRTIO_TYPE_ENTROPY);
        assert_eq!(VIRTIO_GPU_MODERN as u32, 0x1040 + VIRTIO_TYPE_GPU);
        assert_eq!(VIRTIO_INPUT_MODERN as u32, 0x1040 + VIRTIO_TYPE_INPUT);
    }
}
