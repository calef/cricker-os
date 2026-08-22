//! **The DMA-confinement validator, as pure logic** (milestone 35; DECISIONS §14, §18).
//!
//! A userspace virtio driver programs the descriptors a device does DMA against, and the device has
//! no MMU in front of it (or, where an IOMMU is present, this is defence in depth). So something
//! trusted must check that every address the device will touch lies inside the driver's own granted
//! DMA region, and refuse anything else. That check is [`validate_and_shadow`], and it is the last
//! isolation boundary in the system that was attacker-tested but never machine-checked. This crate
//! is that logic, lifted out of `kernel/src/virtio.rs` so Kani can prove it and the kernel then
//! *calls* the proved code rather than keeping a parallel copy (the discipline in
//! notes/verification.md, the same move `regions` and `ipc` made).
//!
//! The core property, proved for every input in the `#[cfg(kani)]` module: **no descriptor the
//! device can read out of the shadow ring references memory outside `[base, base + size)`**, in
//! either direction (the device reading a transmit buffer or writing a receive buffer), with
//! indirect descriptors refused, over any newly-published batch. The shadow copy is why the check
//! holds under time-of-check/time-of-use: the device reads a kernel-private copy the driver cannot
//! touch after validation, so mutating the driver's descriptor afterward changes only a copy nothing
//! reads.
//!
//! Everything here is address arithmetic over closures the caller supplies for memory access, so it
//! is `no_std`, allocation-free, and host-testable; the kernel passes closures that hit the direct
//! map, a test passes closures over ordinary arrays, and Kani passes closures over a symbolic memory
//! model. No hardware, no kernel globals.
//!
//! # Examples
//!
//! The confinement, one descriptor at a time. A driver's granted DMA region is a window, and every
//! address the device will touch has to be inside it, whichever direction the bytes move:
//!
//! ```
//! use dma_validator::{Desc, check_descriptor, in_region};
//!
//! // One 64 KiB region, granted to a userspace virtio driver.
//! let (base, size) = (0x4000_0000u64, 0x1_0000u64);
//! let qsize = 8u16;
//!
//! // A descriptor pointing at a 1500-byte frame inside the region. Fine in either direction: the
//! // device may read it (transmit) or write it (receive).
//! let ok = Desc { addr: base + 0x1000, word: 1500 };
//! assert!(check_descriptor(base, size, qsize, ok));
//!
//! // The same length, aimed at the kernel. Refused, and this is the whole point of the crate: a
//! // receive buffer here would let the device overwrite the kernel, and a transmit buffer would
//! // let it put the kernel on the wire.
//! let escape = Desc { addr: 0xffff_0000_0000_0000, word: 1500 };
//! assert!(!check_descriptor(base, size, qsize, escape));
//!
//! // A buffer that starts inside and runs off the end is refused too. One predicate closes both,
//! // which is why there is only one.
//! let overrun = Desc { addr: base + size - 8, word: 1500 };
//! assert!(!check_descriptor(base, size, qsize, overrun));
//! assert!(!in_region(base, size, base + size - 8, 1500));
//! ```
//!
//! Two refusals are less obvious and are the ones that make the check total rather than merely
//! correct on ordinary input:
//!
//! ```
//! use dma_validator::{Desc, F_INDIRECT, F_NEXT, check_descriptor, in_region};
//!
//! let (base, size, qsize) = (0x4000_0000u64, 0x1_0000u64, 8u16);
//!
//! // An **indirect** descriptor names a table of further descriptors. The table is never copied
//! // into the shadow, so the device would follow the driver's own memory unchecked. Refused
//! // outright rather than validated, whatever it points at.
//! let indirect = Desc { addr: base, word: 16 | ((F_INDIRECT as u64) << 32) };
//! assert!(indirect.is_indirect());
//! assert!(!check_descriptor(base, size, qsize, indirect));
//!
//! // A `next` link outside the ring would walk the chain off the end of the queue.
//! let bad_link = Desc { addr: base, word: 16 | ((F_NEXT as u64) << 32) | ((qsize as u64) << 48) };
//! assert!(bad_link.has_next() && bad_link.next() == qsize);
//! assert!(!check_descriptor(base, size, qsize, bad_link));
//!
//! // And arithmetic that would overflow answers "refuse" rather than panicking. A wrapping region
//! // can only ever over-reject, which is the safe direction.
//! assert!(!in_region(base, size, u64::MAX - 4, 1500));
//! assert!(!in_region(u64::MAX - 4, 1500, base, 16));
//! ```
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `dma_validate`. Refused
//! `dma_validate` (a verb, while the crate's own first line already called itself "the
//! DMA-confinement validator": it had named itself a noun and carried a verb).

#![no_std]
// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

/// Bit 0: this descriptor chains to another (`next` names it). The validator follows the chain,
/// bounded by the queue size, so a `next` cycle cannot spin forever.
pub const F_NEXT: u16 = 1;
/// Bit 2: this descriptor points at a **table of further descriptors** instead of a buffer. The
/// validator walks the flat chain and never follows that inner table, so a descriptor carrying this
/// flag would send the device to addresses that were never checked. It is refused here (and the
/// feature that enables it is negotiated off in the kernel), so the confinement fails closed.
pub const F_INDIRECT: u16 = 4;

/// **Is `[addr, addr + len)` wholly inside `[base, base + size)`?** The whole confinement, as one
/// predicate. It is direction-agnostic on purpose: whether the device *reads* the buffer (transmit)
/// or *writes* it (receive), the address it touches must be in-region, so one check closes both
/// hazards (a receive buffer aimed at the kernel would let the device overwrite it; a transmit
/// buffer aimed there would let it exfiltrate the kernel to the wire).
///
/// Total: both additions are checked, so a length that overflows `addr`, or a region whose own
/// `base + size` wraps `u64`, returns `false` (refuse) rather than panicking. A wrapping region can
/// only ever over-reject (the run's end is a huge in-region address, above any wrapped limit), which
/// is the safe direction; real DMA regions are bounded by RAM and never wrap, so this hardening
/// changes nothing the kernel actually sees. See the `in_region_is_sound` proof.
pub fn in_region(base: u64, size: u64, addr: u64, len: u64) -> bool {
    match (addr.checked_add(len), base.checked_add(size)) {
        (Some(end), Some(limit)) => addr >= base && end <= limit,
        _ => false,
    }
}

/// One virtqueue split-ring descriptor as it sits in memory: `{ u64 addr @0; u32 len @8; u16 flags
/// @12; u16 next @14 }`. The kernel reads the low 8 bytes as `addr` and the high 8 as one `word`
/// carrying len/flags/next, and copies both verbatim into the shadow, so this models the same two
/// 64-bit words and decodes the fields the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desc {
    /// The buffer's guest-physical address, the first 64-bit word.
    pub addr: u64,
    /// Length, flags and next-index packed into the second 64-bit word; decode with
    /// [`buf_len`](Self::buf_len), [`flags`](Self::flags) and [`next`](Self::next).
    pub word: u64,
}

impl Desc {
    /// The buffer length, the low 32 bits of the second word.
    pub const fn buf_len(self) -> u64 {
        self.word & 0xffff_ffff
    }
    /// The descriptor flags, bits 32..48 of the second word.
    pub const fn flags(self) -> u16 {
        ((self.word >> 32) & 0xffff) as u16
    }
    /// The next-descriptor index, bits 48..64 of the second word.
    pub const fn next(self) -> u16 {
        ((self.word >> 48) & 0xffff) as u16
    }
    /// Whether `F_INDIRECT` is set: the buffer holds a table of further descriptors rather than
    /// data, which this validator refuses to chase (see the confinement decision above).
    pub const fn is_indirect(self) -> bool {
        self.flags() & F_INDIRECT != 0
    }
    /// Whether `F_NEXT` is set: [`next`](Self::next) names another descriptor in the chain.
    pub const fn has_next(self) -> bool {
        self.flags() & F_NEXT != 0
    }
}

/// **The per-descriptor decision: may the device be allowed to act on this descriptor?** True only
/// when it is not indirect, its buffer stays inside `[base, base + size)`, and any `next` link it
/// declares names a real slot (`< qsize`). Every descriptor copied into the shadow passes this
/// first, so it is the property the whole confinement rests on, isolated as a loopless function the
/// `an_accepted_descriptor_is_confined` proof can quantify over all inputs.
pub fn check_descriptor(base: u64, size: u64, qsize: u16, d: Desc) -> bool {
    if d.is_indirect() {
        return false; // an indirect table is never copied, so the device would follow it unchecked
    }
    if !in_region(base, size, d.addr, d.buf_len()) {
        return false; // the buffer escapes the driver's region
    }
    if d.has_next() && d.next() >= qsize {
        return false; // a chain link out of the descriptor table
    }
    true
}

/// **Validate the driver's newly-published descriptors AND copy the validated ones into the shadow
/// ring the device reads.** This is the security-critical function, moved verbatim out of the kernel.
///
/// For each newly-available head in `from_idx .. to_idx`, walk the chain in the *driver's* descriptor
/// table, check every descriptor with [`check_descriptor`], and copy the validated bytes into the
/// *shadow* table at the same index. Then mirror the head into the shadow available ring, and finally
/// publish the shadow's `avail.idx`. The device is programmed to read the shadow (which the driver
/// cannot write), so the bytes the device acts on are exactly the bytes validated here: mutating a
/// descriptor after this returns changes only the driver's own copy, which nothing reads. That is
/// what closes the time-of-check/time-of-use race an in-place check leaves open. Returns `false`,
/// leaving the shadow's published index untouched, if any descriptor escapes the region, is indirect,
/// or the batch claims more entries than the ring can hold.
///
/// Takes the driver and shadow ring *physical addresses* plus read/write word pairs, so the kernel
/// passes direct-map closures, a test passes array-backed closures, and Kani passes a symbolic memory
/// model. `qsize` is the negotiated queue size (the kernel's `QSIZE`); the walks are bounded by it.
#[allow(clippy::too_many_arguments)]
pub fn validate_and_shadow(
    dma_base: u64,
    dma_size: u64,
    driver_desc: u64,
    driver_avail: u64,
    shadow_desc: u64,
    shadow_avail: u64,
    from_idx: u16,
    to_idx: u16,
    qsize: u16,
    read16: &dyn Fn(u64) -> u16,
    read64: &dyn Fn(u64) -> u64,
    write16: &dyn Fn(u64, u16),
    write64: &dyn Fn(u64, u64),
) -> bool {
    // At most `qsize` descriptors can be newly available since the last validation: the available
    // ring has only `qsize` slots, so a driver cannot have published more than that without the
    // device consuming some. A larger jump in avail.idx is malformed or hostile, and walking it would
    // spin this loop up to 65535 times under the caller's lock with interrupts masked. Refuse it
    // before touching a single descriptor. (wrapping_sub because avail.idx wraps at u16.)
    if to_idx.wrapping_sub(from_idx) > qsize {
        return false;
    }

    // avail: { u16 flags; u16 idx; u16 ring[qsize]; }
    let mut idx = from_idx;
    while idx != to_idx {
        let slot = (idx % qsize) as u64;
        let head = read16(driver_avail + 4 + slot * 2);
        if head >= qsize {
            return false; // head index out of the descriptor table
        }

        // Walk this head's chain, validate every descriptor, and copy the validated ones into the
        // shadow. The confinement theorem is proved against `shadow_one_head` directly, so refuse the
        // whole batch if it refuses this head.
        if !shadow_one_head(
            dma_base,
            dma_size,
            driver_desc,
            shadow_desc,
            head,
            qsize,
            read64,
            write64,
        ) {
            return false;
        }

        // Mirror the head into the shadow available ring.
        write16(shadow_avail + 4 + slot * 2, head);
        idx = idx.wrapping_add(1);
    }

    // Publish LAST: the device reads the shadow's avail.idx to learn what is ready, so it must not
    // advance until every descriptor it points at is already in the shadow.
    write16(shadow_avail + 2, to_idx);
    true
}

/// **Validate and shadow one head's descriptor chain: the core of the confinement.** Walk the chain
/// starting at `head` in the driver's descriptor table, check every descriptor with
/// [`check_descriptor`], and copy each validated descriptor into the shadow table at the same index.
/// Returns `false` (having possibly copied earlier, validated descriptors, which is harmless because
/// the caller does not publish the head on failure) if any descriptor escapes the region, is
/// indirect, or names a `next` link out of range.
///
/// This is the walk the [`validate_and_shadow_confines_every_chain`](self) proof quantifies over:
/// because every descriptor is checked before it is copied, no descriptor the device can read out of
/// the shadow is ever out-of-region or indirect. The chain is bounded by `qsize`, so a `next` cycle
/// cannot loop forever. `desc[d] = { u64 addr @0; u32 len @8; u16 flags @12; u16 next @14 }`; the
/// len/flags/next share one 64-bit word read and copied verbatim.
#[allow(clippy::too_many_arguments)]
fn shadow_one_head(
    dma_base: u64,
    dma_size: u64,
    driver_desc: u64,
    shadow_desc: u64,
    head: u16,
    qsize: u16,
    read64: &dyn Fn(u64) -> u64,
    write64: &dyn Fn(u64, u64),
) -> bool {
    let mut d = head;
    for _ in 0..qsize {
        let src = driver_desc + d as u64 * 16;
        let desc = Desc {
            addr: read64(src),
            word: read64(src + 8),
        };

        if !check_descriptor(dma_base, dma_size, qsize, desc) {
            return false;
        }

        // Copy the validated descriptor into the shadow, byte-for-byte. From here the device reads
        // this, not the driver's copy.
        let dst = shadow_desc + d as u64 * 16;
        write64(dst, desc.addr);
        write64(dst + 8, desc.word);

        if !desc.has_next() {
            break;
        }
        d = desc.next();
    }
    true
}

// ---------------------------------------------------------------------------------------------
// The queue ring layout. **Owned here, and used by the kernel** (kernel/src/virtio.rs defines its
// `QSIZE`, `RING_BLOCK`, `MAX_QUEUES`, and the three ring offsets as aliases of these), because a
// proof about a *copy* of the layout proves nothing about the layout that runs. The first cut of
// milestone 35 duplicated them and said so; that left `distinct_queues_occupy_disjoint_blocks`
// quantifying over constants the kernel could drift away from silently. The kernel keeps its own
// compile-time asserts as well, so drift would now be a compile error in two places rather than a
// proof that quietly stopped applying.
// ---------------------------------------------------------------------------------------------

/// The negotiated queue size the kernel drives (`QSIZE` in kernel/src/virtio.rs).
pub const LAYOUT_QSIZE: u16 = 8;
/// Offsets of a queue's three rings, relative to its ring block.
pub const DESC_OFF: u64 = 0x000;
/// Offset of the avail ring: past the descriptor table, `6 + 2*QSIZE` bytes in.
pub const AVAIL_OFF: u64 = 0x080; // 6 + 2*QSIZE
/// Offset of the used ring: past the avail ring, `6 + 8*QSIZE` bytes in.
pub const USED_OFF: u64 = 0x100; // 6 + 8*QSIZE
/// The whole ring area of one queue must fit under this; a queue's data buffers live above it.
pub const RING_END: u64 = USED_OFF + 6 + 8 * LAYOUT_QSIZE as u64;
/// The stride between successive queues' ring areas, in both the driver's region and the shadow.
pub const RING_BLOCK: u64 = 0x200;
/// The most virtqueues the confinement drives per device (RX = queue 0, TX = queue 1 on a NIC).
pub const MAX_QUEUES: u64 = 2;

/// Queue `q`'s ring block base, relative to a region (driver DMA or shadow) base. Queue `q`'s
/// descriptor table, available ring, and used ring sit at `queue_block(q) + {DESC,AVAIL,USED}_OFF`,
/// so distinct queues validate on disjoint blocks and never read or write each other's rings.
pub const fn queue_block(q: u16) -> u64 {
    q as u64 * RING_BLOCK
}

// =============================================================================================
// Machine-checked proofs (Kani). Behind `#[cfg(kani)]`, so an ordinary build/test never sees them;
// only `cargo kani` (script/verify) sets that cfg. See notes/verification.md.
// =============================================================================================
#[cfg(kani)]
mod verification {
    use core::cell::RefCell;

    use super::*;

    /// The queue size the proofs fix. Concrete so the bounded loops unroll to a known depth; it is
    /// the kernel's real `QSIZE`, so the proof is over the configuration that ships.
    const QS: u16 = 8;
    const Q: usize = 8;

    /// **The confinement predicate is sound: an accepted range is genuinely inside the region.** For
    /// every base, size, addr, and len, if [`in_region`] says yes then the addition did not overflow
    /// and `base <= addr` and `addr + len <= base + size`. This is the arithmetic the whole boundary
    /// rests on, quantified over all 2^256 input combinations at once, and it also proves totality
    /// (the hardened checked adds never panic on any input).
    #[kani::proof]
    fn in_region_is_sound() {
        let base: u64 = kani::any();
        let size: u64 = kani::any();
        let addr: u64 = kani::any();
        let len: u64 = kani::any();

        if in_region(base, size, addr, len) {
            let end = addr
                .checked_add(len)
                .expect("acceptance implies no addr+len overflow");
            let limit = base
                .checked_add(size)
                .expect("acceptance implies no base+size overflow");
            assert!(addr >= base);
            assert!(end <= limit);
            // Stated as containment: no byte the device would touch lies outside the granted region.
            assert!(addr >= base && end <= limit);
        }
    }

    /// **An accepted descriptor is confined and not indirect, in either direction.** For every
    /// descriptor bit pattern (the `flags` field is fully symbolic, so the device-writable RX bit and
    /// the device-readable TX shape are both covered) and every region, if [`check_descriptor`]
    /// accepts it then it carries no indirect flag and its whole buffer is in-region. This is the
    /// per-descriptor core of the walk, isolated so it is proved without any loop.
    #[kani::proof]
    fn an_accepted_descriptor_is_confined() {
        let base: u64 = kani::any();
        let size: u64 = kani::any();
        let d = Desc {
            addr: kani::any(),
            word: kani::any(),
        };
        if check_descriptor(base, size, QS, d) {
            assert!(!d.is_indirect(), "an indirect descriptor was accepted");
            assert!(
                in_region(base, size, d.addr, d.buf_len()),
                "an accepted descriptor escapes the region",
            );
        }
    }

    // Concrete, widely-separated descriptor-table bases so that decoding an address the walk
    // generates is concrete arithmetic over symbolic *contents*. A descriptor's own `addr` field is
    // symbolic and checked against the region; it is never decoded as a table address, so the
    // separation is clean.
    const DRIVER_DESC: u64 = 0x1_0000;
    const DRIVER_AVAIL: u64 = 0x2_0000;
    const SHADOW_DESC: u64 = 0x3_0000;
    const SHADOW_AVAIL: u64 = 0x4_0000;

    /// The symbolic memory the chain walk runs against: the driver descriptor table it reads (all
    /// symbolic input) and the shadow table it writes (the device's view). The write closure checks
    /// the confinement invariant *as each descriptor lands*, so a harness that runs the walk and trips
    /// no assertion has proved: nothing out-of-region or indirect can ever reach the shadow, for any
    /// driver input. The avail rings are not modeled here because [`shadow_one_head`] does not touch
    /// them (the outer loop does; its bound is proved by `an_oversized_batch_is_refused`).
    struct ChainMem {
        base: u64,
        size: u64,
        d_desc: RefCell<[Desc; Q]>, // driver descriptors (symbolic; RefCell so a proof can mutate them post-hoc)
        s_desc: RefCell<[Desc; Q]>, // shadow descriptors (written by the walk = device's view)
        s_written: RefCell<[bool; Q]>,
    }

    impl ChainMem {
        fn symbolic() -> ChainMem {
            let mut d_desc = [Desc { addr: 0, word: 0 }; Q];
            let mut i = 0;
            while i < Q {
                d_desc[i] = Desc {
                    addr: kani::any(),
                    word: kani::any(),
                };
                i += 1;
            }
            ChainMem {
                base: kani::any(),
                size: kani::any(),
                d_desc: RefCell::new(d_desc),
                s_desc: RefCell::new([Desc { addr: 0, word: 0 }; Q]),
                s_written: RefCell::new([false; Q]),
            }
        }
        fn read64(&self, p: u64) -> u64 {
            // The two words of driver descriptor `d`: desc + d*16 (+8 for the word half).
            let off = p - DRIVER_DESC;
            let d = (off / 16) as usize;
            let entry = self.d_desc.borrow()[d];
            if off.is_multiple_of(16) {
                entry.addr
            } else {
                entry.word
            }
        }
        fn write64(&self, p: u64, v: u64) {
            // A write to any address the subtraction cannot resolve (below the shadow table) would
            // trip Kani's overflow check here, which itself proves the walk only ever writes the
            // shadow, never back into the driver's region.
            let off = p - SHADOW_DESC;
            // And it never writes *past* its own queue's descriptor table. Stated as an assertion
            // rather than left implicit in the array bound below, because this is the multi-queue
            // half of the confinement: a write bounded by `16 * qsize` stays inside `DESC_OFF ..
            // AVAIL_OFF` of one queue's ring block, and `distinct_queues_occupy_disjoint_blocks`
            // proves the blocks do not overlap. Together: validating a NIC's receive queue cannot
            // touch its transmit queue's rings.
            assert!(
                off < 16 * QS as u64,
                "a shadow write ran past this queue's descriptor table",
            );
            let d = (off / 16) as usize;
            if off.is_multiple_of(16) {
                self.s_desc.borrow_mut()[d].addr = v; // addr half, written first
            } else {
                self.s_desc.borrow_mut()[d].word = v; // word half: the pair is now complete
                self.s_written.borrow_mut()[d] = true;
                // THE INVARIANT, checked the instant a descriptor becomes readable by the device:
                // every descriptor in the shadow is in-region and not indirect.
                let landed = self.s_desc.borrow()[d];
                assert!(
                    !landed.is_indirect(),
                    "an indirect descriptor reached the shadow the device reads",
                );
                assert!(
                    in_region(self.base, self.size, landed.addr, landed.buf_len()),
                    "a descriptor that escapes the DMA region reached the shadow the device reads",
                );
            }
        }
    }

    fn walk(mem: &ChainMem, head: u16) -> bool {
        shadow_one_head(
            mem.base,
            mem.size,
            DRIVER_DESC,
            SHADOW_DESC,
            head,
            QS,
            &|p| mem.read64(p),
            &|p, v| mem.write64(p, v),
        )
    }

    /// **The main theorem: no descriptor chain can put out-of-region or indirect memory in front of
    /// the device.** Over a fully symbolic driver descriptor table (every descriptor's
    /// addr/len/flags/next unconstrained) and a symbolic region, walk a chain from any valid head with
    /// the real [`shadow_one_head`], the confinement core of [`validate_and_shadow`]. The shadow write
    /// closure asserts the confinement invariant on every descriptor *as it lands*, so a run that
    /// trips nothing proves the invariant for **every** input: the device only ever reads in-region,
    /// non-indirect descriptors. This covers both directions (symbolic flags include the
    /// device-writable RX bit), indirect descriptors, and any chain the queue can hold. The array
    /// indexing is symbolic-index-bounded, so it also proves the walk never reads or writes past the
    /// table.
    ///
    /// One head, not a whole symbolic batch, and that loses no coverage: the write closure checks the
    /// invariant on *every* shadow write, and a single head's chain already writes up to `qsize` fully
    /// symbolic descriptors, so the per-write property is quantified over arbitrary descriptor content
    /// and position. The outer loop of [`validate_and_shadow`] only repeats this validated single-head
    /// processing for each further head (its bound proved by [`an_oversized_batch_is_refused`]), so it
    /// reaches no new descriptor state.
    #[kani::proof]
    #[kani::unwind(10)]
    fn validate_and_shadow_confines_every_chain() {
        let mem = ChainMem::symbolic();
        let head: u16 = kani::any();
        // The outer loop refuses `head >= qsize` before ever calling `shadow_one_head`, so the walk
        // runs only under this precondition; a head naming a real descriptor slot.
        kani::assume(head < QS);
        let _ = walk(&mem, head);
    }

    /// **An oversized batch is refused before a single descriptor is touched.** The available ring
    /// holds only `qsize` slots, so a claim of more than `qsize` newly-available entries is malformed
    /// or hostile, and walking it would spin the loop up to 65535 times under the caller's lock with
    /// interrupts masked. For every `from, to` whose gap exceeds `qsize`, the full
    /// [`validate_and_shadow`] returns false having read and written nothing: the memory closures
    /// panic if called, so reaching one is a proof failure. This is the outer-loop bound the
    /// single-head main theorem factors out.
    #[kani::proof]
    fn an_oversized_batch_is_refused() {
        let from: u16 = kani::any();
        let to: u16 = kani::any();
        kani::assume(to.wrapping_sub(from) > QS);
        let ok = validate_and_shadow(
            kani::any(),
            kani::any(),
            DRIVER_DESC,
            DRIVER_AVAIL,
            SHADOW_DESC,
            SHADOW_AVAIL,
            from,
            to,
            QS,
            &|_| panic!("a descriptor was read before the batch-size guard refused the batch"),
            &|_| panic!("a descriptor was read before the batch-size guard refused the batch"),
            &|_, _| panic!("the shadow was written before the batch-size guard refused the batch"),
            &|_, _| panic!("the shadow was written before the batch-size guard refused the batch"),
        );
        assert!(!ok, "an oversized batch was accepted instead of refused");
    }

    /// **A descriptor mutated after validation cannot reach the device.** The shadow ring's whole
    /// point. Walk a symbolic head; if it succeeds, snapshot what the device will read from the
    /// shadow, then let the driver aim its *own* descriptor copies at arbitrary (possibly escaping)
    /// addresses, exactly the time-of-check/time-of-use move real async-DMA hardware would allow. The
    /// device reads the shadow, which is unchanged and still confined: the mutation touched only the
    /// driver's copy, which nothing reads.
    #[kani::proof]
    #[kani::unwind(10)]
    fn a_descriptor_mutated_after_validation_cannot_reach_the_device() {
        let mem = ChainMem::symbolic();
        let head: u16 = kani::any();
        kani::assume(head < QS);

        if walk(&mem, head) {
            // A successful walk validated and copied the head descriptor, so the device will read it
            // from the shadow. Capture it; the write invariant already proved it is confined.
            let slot = head as usize;
            assert!(
                mem.s_written.borrow()[slot],
                "a successful walk did not shadow the head descriptor"
            );
            let device_sees = mem.s_desc.borrow()[slot];
            assert!(
                in_region(mem.base, mem.size, device_sees.addr, device_sees.buf_len()),
                "the device would read an out-of-region descriptor from the shadow",
            );

            // The driver now aims its OWN copy of that descriptor at an arbitrary address, after the
            // check. On async-DMA hardware this is the time-of-check/time-of-use race.
            mem.d_desc.borrow_mut()[slot] = Desc {
                addr: kani::any(),
                word: kani::any(),
            };

            // The device reads the shadow, which is untouched by that write: the mutation landed only
            // in the driver's own copy, which nothing reads.
            assert_eq!(
                mem.s_desc.borrow()[slot],
                device_sees,
                "a post-validation driver write reached the shadow the device reads",
            );
        }
    }

    // A region and a descriptor that always validate, for the harness below. Concrete on purpose:
    // that harness isolates the *outer* loop's index arithmetic, and descriptor content is
    // `validate_and_shadow_confines_every_chain`'s job. Making both symbolic at once would put
    // `qsize` heads times `qsize` descriptors of symbolic reads in one formula, which is the batching
    // blowup the main theorem factors out, for no coverage the two harnesses do not already give.
    const OUTER_BASE: u64 = 0x8000_0000;
    const OUTER_SIZE: u64 = 0x1000;

    /// **The outer walk stays inside both rings and terminates, for every index pair, wraparound
    /// included.** The confinement theorem above proves one head's chain; this proves the loop that
    /// feeds it. `avail.idx` is a `u16` that wraps, and the walk runs `from_idx .. to_idx` through
    /// that wrap (`from = 0xffff, to = 0x0002` is an ordinary three-entry batch), computing a ring
    /// slot as `idx % qsize` and `wrapping_add`ing. Both `from` and `to` are unconstrained here, so
    /// the wrapped case is covered rather than assumed away, and every closure asserts the address it
    /// is handed:
    ///
    /// - a 16-bit read is exactly a `ring[slot]` load with `slot < qsize` (a read below the ring
    ///   underflows the offset subtraction, which Kani flags, so "never reads outside the available
    ///   ring" is proved in both directions);
    /// - a 16-bit write is either the `avail.idx` publish or a `ring[slot]` mirror with `slot <
    ///   qsize`;
    /// - a 64-bit descriptor read or shadow write stays inside a `qsize`-entry table.
    ///
    /// **Termination is proved, not assumed.** `#[kani::unwind(11)]` is one more than the walk can
    /// possibly need (the batch guard caps the outer loop at `qsize = 8` iterations, and each chain
    /// here is one descriptor), and Kani's unwinding assertion fails if a loop could run longer. So
    /// no index pair, wrapped or not, makes this loop spin: the property that keeps a hostile
    /// `avail.idx` from holding the `DEVICES` lock with interrupts masked.
    #[kani::proof]
    #[kani::unwind(11)]
    fn the_outer_walk_stays_inside_the_rings_and_terminates() {
        let from: u16 = kani::any();
        let to: u16 = kani::any();

        // A symbolic available ring: each slot names an arbitrary head, in range or not.
        let mut ring = [0u16; Q];
        let mut i = 0;
        while i < Q {
            ring[i] = kani::any();
            i += 1;
        }

        let read16 = |p: u64| -> u16 {
            let off = p - DRIVER_AVAIL; // underflow here = a read below the ring: Kani catches it
            assert!(
                off >= 4,
                "a 16-bit read below the available ring's ring[] array"
            );
            assert!(
                (off - 4).is_multiple_of(2),
                "an unaligned available-ring read"
            );
            let slot = (off - 4) / 2;
            assert!(
                slot < QS as u64,
                "a read past the end of the available ring"
            );
            ring[slot as usize]
        };
        let read64 = |p: u64| -> u64 {
            let off = p - DRIVER_DESC;
            assert!(
                off < 16 * QS as u64,
                "a descriptor read past the end of the table"
            );
            // An in-region 8-byte buffer, no flags, so every chain is exactly one descriptor.
            if off.is_multiple_of(16) {
                OUTER_BASE
            } else {
                8
            }
        };
        let write16 = |p: u64, _v: u16| {
            let off = p - SHADOW_AVAIL;
            if off == 2 {
                return; // publishing the shadow's avail.idx, the last write
            }
            assert!(
                off >= 4,
                "a 16-bit write below the shadow ring's ring[] array"
            );
            assert!(
                (off - 4).is_multiple_of(2),
                "an unaligned shadow available-ring write"
            );
            assert!(
                (off - 4) / 2 < QS as u64,
                "a write past the end of the shadow available ring"
            );
        };
        let write64 = |p: u64, _v: u64| {
            let off = p - SHADOW_DESC;
            assert!(
                off < 16 * QS as u64,
                "a shadow descriptor write past the end of the table"
            );
        };

        let ok = validate_and_shadow(
            OUTER_BASE,
            OUTER_SIZE,
            DRIVER_DESC,
            DRIVER_AVAIL,
            SHADOW_DESC,
            SHADOW_AVAIL,
            from,
            to,
            QS,
            &read16,
            &read64,
            &write16,
            &write64,
        );

        // **Non-vacuity, machine-checked rather than argued.** An assertion-only harness passes
        // trivially if its inputs cannot reach the interesting states, and notes/verification.md names
        // that ("it proves what you asserted, not what you meant") as the main failure mode, not solver
        // bugs. `kani::cover!` inverts the question: it fails if the state is *un*reachable. So these
        // are the claim "this harness really does exercise wraparound and really does walk descriptors",
        // checked. Without them, "from and to are unconstrained, so the wrapped pairs are in there
        // somewhere" would be a reading of the code rather than a result.
        kani::cover!(to < from, "a wrapped batch (to < from) is reachable");
        kani::cover!(
            to < from && ok,
            "a wrapped batch is walked to completion, not merely refused"
        );
        kani::cover!(
            to != from && ok,
            "some batch is accepted, so the walk is not vacuously refusing everything"
        );
        kani::cover!(
            !ok,
            "some batch is refused, so the guards are reachable too"
        );
    }

    /// **Distinct queues occupy disjoint ring blocks** (milestone 30 multi-queue isolation). A NIC
    /// drives receive on queue 0 and transmit on queue 1; each queue's rings live at `queue_block(q)`
    /// in both the driver region and the shadow, so validating one queue never reads or writes
    /// another's rings. For any two distinct in-range queues, the lower queue's whole ring area ends
    /// before the higher queue's block begins, and a queue's ring area fits inside its own block.
    #[kani::proof]
    fn distinct_queues_occupy_disjoint_blocks() {
        let q1: u16 = kani::any();
        let q2: u16 = kani::any();
        kani::assume((q1 as u64) < MAX_QUEUES);
        kani::assume((q2 as u64) < MAX_QUEUES);
        kani::assume(q1 < q2);
        // The lower queue's ring area ends at or before the higher queue's block begins: disjoint.
        assert!(queue_block(q1) + RING_END <= queue_block(q2));
        // And each queue's ring area fits within its own block, so the blocks fully contain them.
        // A `const` assertion, because both operands are constants: this is a fact about the layout
        // and not about `q1`/`q2`, so it is worth failing the build rather than a proof run.
        const { assert!(RING_END <= RING_BLOCK) };
        // The descriptor table a walk writes (`16 * qsize` bytes from `DESC_OFF`) ends at or before
        // the available ring begins. This is what makes the walk's own `off < 16 * qsize` write bound
        // (asserted in `ChainMem::write64`) add up to block isolation: a shadow write stays inside
        // `DESC_OFF .. AVAIL_OFF` of one block, and the blocks do not overlap, so no shadow write can
        // land in another queue's rings or in this queue's own available ring.
        assert!(DESC_OFF + 16 * LAYOUT_QSIZE as u64 <= AVAIL_OFF);
    }
}

// =============================================================================================
// Host tests. Fast (`cargo test`), and a redundant check that the extracted logic matches the
// attacker cases the kernel's QEMU suite exercises against the same function.
// =============================================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const QSIZE: u16 = 8;
    const F_WRITE: u16 = 2; // device-writable (an RX buffer): the validator bounds it the same way

    /// A pair of ring regions in ordinary host memory, addressed like the kernel's physical layout.
    struct Rings {
        driver_desc: [Desc; 8],
        driver_ring: [u16; 8],
        driver_idx: u16,
        shadow_desc: core::cell::RefCell<[Desc; 8]>,
        shadow_ring: core::cell::RefCell<[u16; 8]>,
        shadow_idx: core::cell::RefCell<u16>,
    }

    const DD: u64 = 0x1000;
    const DA: u64 = 0x2000;
    const SD: u64 = 0x3000;
    const SA: u64 = 0x4000;

    impl Rings {
        fn new() -> Self {
            Rings {
                driver_desc: [Desc { addr: 0, word: 0 }; 8],
                driver_ring: [0; 8],
                driver_idx: 0,
                shadow_desc: core::cell::RefCell::new([Desc { addr: 0, word: 0 }; 8]),
                shadow_ring: core::cell::RefCell::new([0; 8]),
                shadow_idx: core::cell::RefCell::new(0),
            }
        }
        fn set_desc(&mut self, i: usize, addr: u64, len: u32, flags: u16, next: u16) {
            self.driver_desc[i] = Desc {
                addr,
                word: (len as u64) | ((flags as u64) << 32) | ((next as u64) << 48),
            };
        }
        fn run(&self) -> bool {
            self.run_range(0, self.driver_idx)
        }
        /// The same run with explicit batch indices, for the tests that need a slot other than
        /// 0..2: `avail.idx` is a free-running u16, so real batches start at arbitrary indices and
        /// the slot arithmetic (`idx % qsize`, `4 + slot * 2`) only shows its shape there.
        fn run_range(&self, from: u16, to: u16) -> bool {
            validate_and_shadow(
                0x1_0000_0000, // dma_base: an arbitrary in-test region
                0x1000,        // dma_size
                DD,
                DA,
                SD,
                SA,
                from,
                to,
                QSIZE,
                &|p| {
                    if p == DA + 2 {
                        self.driver_idx
                    } else {
                        self.driver_ring[((p - DA - 4) / 2) as usize]
                    }
                },
                &|p| {
                    let off = p - DD;
                    let d = self.driver_desc[(off / 16) as usize];
                    if off.is_multiple_of(16) {
                        d.addr
                    } else {
                        d.word
                    }
                },
                &|p, v| {
                    let off = p - SA;
                    if off == 2 {
                        *self.shadow_idx.borrow_mut() = v;
                    } else {
                        self.shadow_ring.borrow_mut()[((off - 4) / 2) as usize] = v;
                    }
                },
                &|p, v| {
                    let off = p - SD;
                    let d = (off / 16) as usize;
                    if off.is_multiple_of(16) {
                        self.shadow_desc.borrow_mut()[d].addr = v;
                    } else {
                        self.shadow_desc.borrow_mut()[d].word = v;
                    }
                },
            )
        }
    }

    const BASE: u64 = 0x1_0000_0000;

    #[test]
    fn a_chain_wholly_inside_the_region_validates() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 16, F_NEXT, 1);
        r.set_desc(1, BASE + 0x400, 512, F_NEXT, 2);
        r.set_desc(2, BASE + 0x600, 1, 0, 0);
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(r.run(), "an in-region chain was refused");
        assert_eq!(r.shadow_desc.borrow()[0].addr, BASE + 0x200);
        assert_eq!(
            *r.shadow_idx.borrow(),
            1,
            "the shadow avail.idx was not published"
        );
    }

    #[test]
    fn a_descriptor_pointing_outside_the_region_is_refused() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 16, F_NEXT, 1);
        r.set_desc(1, 0xffff_0000_4008_0000, 512, F_NEXT | F_WRITE, 2); // kernel memory
        r.set_desc(2, BASE + 0x600, 1, 0, 0);
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(
            !r.run(),
            "a descriptor aimed at kernel memory was not refused"
        );
    }

    #[test]
    fn a_length_that_overflows_the_region_is_refused() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x1000 - 256, 512, 0, 0); // runs 256 bytes past the region end
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(
            !r.run(),
            "a descriptor running past the region end was not refused"
        );
    }

    #[test]
    fn an_indirect_descriptor_is_refused_even_in_region() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 128, F_INDIRECT, 0); // legal address, but indirect
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(!r.run(), "an indirect descriptor was accepted");
    }

    #[test]
    fn a_device_writable_rx_buffer_is_bounded_the_same_as_a_tx_buffer() {
        // An in-region receive buffer validates; the same buffer aimed at the kernel is refused.
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x400, 1514, F_WRITE, 0);
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(r.run(), "an in-region device-writable buffer was refused");

        r.set_desc(0, 0xffff_0000_4008_0000, 1514, F_WRITE, 0);
        assert!(
            !r.run(),
            "a device-writable buffer aimed at the kernel was not refused"
        );
    }

    #[test]
    fn a_batch_larger_than_the_ring_is_refused() {
        let mut r = Rings::new();
        for i in 0..8u64 {
            r.set_desc(i as usize, BASE + 0x200 + i * 8, 8, 0, 0);
            r.driver_ring[i as usize] = i as u16;
        }
        r.driver_idx = QSIZE; // exactly a full ring is legal
        assert!(r.run(), "a full ring of valid entries was refused");
        r.driver_idx = QSIZE + 1; // one more than the ring can hold
        assert!(!r.run(), "an oversized batch was walked instead of refused");
    }

    #[test]
    fn a_next_cycle_terminates() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 16, F_NEXT, 1);
        r.set_desc(1, BASE + 0x400, 16, F_NEXT, 0); // 1 -> 0 -> 1 -> ...
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(
            r.run(),
            "a bounded cyclic chain with valid addresses should validate and terminate"
        );
    }

    /// **The walk follows the descriptor's own `next` field, not a fixed successor.** Chain 0 -> 2
    /// with descriptor 1 poisoned (aimed at kernel memory): a walk that misdecodes `next` and steps
    /// to slot 1 refuses the batch, and one that never steps lands nothing at slot 2. The existing
    /// chain test used 0 -> 1 -> 2, where "always step to 1" still validates.
    #[test]
    fn the_chain_walk_lands_on_the_declared_next_slot() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 16, F_NEXT, 2); // chains to 2, skipping 1
        r.set_desc(1, 0xffff_0000_4008_0000, 16, 0, 0); // poison: reached only by a wrong step
        r.set_desc(2, BASE + 0x400, 32, 0, 0);
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(r.run(), "a 0 -> 2 chain was refused");
        assert_eq!(
            r.shadow_desc.borrow()[2].addr,
            BASE + 0x400,
            "the chain did not shadow the slot its next field names"
        );
    }

    /// **A clear `F_NEXT` means the `next` field is dead, whatever else the flags say.** The
    /// descriptor is device-writable (bit 1 set, so the flag word is nonzero) with `F_NEXT` (bit 0)
    /// clear and a garbage `next` of 15 in an 8-slot queue. A correct validator ignores both; one
    /// that reads the chain bit with `|` instead of `&`, or hardcodes "has next", trips the
    /// `next >= qsize` refusal on a valid descriptor.
    #[test]
    fn a_clear_next_flag_makes_the_next_field_dead() {
        let mut r = Rings::new();
        r.set_desc(0, BASE + 0x200, 64, F_WRITE, 15);
        r.driver_ring[0] = 0;
        r.driver_idx = 1;
        assert!(
            r.run(),
            "a valid unchained descriptor was refused for its dead next field"
        );
    }

    /// **The available-ring slot arithmetic, pinned at an index where the operators diverge.**
    /// `avail.idx` free-runs, so a batch starting at 11 uses slot 11 % 8 = 3, and the head is read
    /// at `avail + 4 + 3*2` = avail + 10 (mirrored at the same offset in the shadow). The
    /// wrong-operator addresses all differ there: 11 / 8 = 1 lands on ring[1], 4 + (3+2) = 9 maps
    /// to ring[2], 4 + 3/2 = 5 maps to ring[0]. Indices 0..2, which every other test uses, cannot
    /// tell these apart (0*2 = 0+2 is off by the same slot, 2*2 = 2+2 exactly). Every slot except 3
    /// holds head 9, out of the 8-entry table, so any misdirected read refuses the batch; the
    /// mirror is checked by where head 5 lands in the shadow ring.
    #[test]
    fn the_slot_arithmetic_holds_at_a_free_running_index() {
        let mut r = Rings::new();
        r.set_desc(5, BASE + 0x200, 16, 0, 0);
        r.driver_ring = [9; 8]; // every slot but 3 is an out-of-table head
        r.driver_ring[3] = 5;
        assert!(
            r.run_range(11, 12),
            "the head at slot 3 was not read from slot 3"
        );
        assert_eq!(
            r.shadow_ring.borrow()[3],
            5,
            "the head was not mirrored at slot 3 of the shadow ring"
        );
        assert_eq!(r.shadow_ring.borrow()[2], 0, "the mirror strayed to slot 2");
        assert_eq!(r.shadow_ring.borrow()[0], 0, "the mirror strayed to slot 0");
        assert_eq!(
            *r.shadow_idx.borrow(),
            12,
            "the shadow avail.idx was not published"
        );
    }

    /// **`RING_END` is the value its formula documents: 0x100 + 6 + 8*8 = 0x146.** The used ring is
    /// 2 (flags) + 2 (idx) + 8 bytes per element times `LAYOUT_QSIZE` = 8 elements + 2 (avail event).
    /// Nothing else pins the value: every relation the proofs and the compile-time asserts state
    /// (`RING_END <= RING_BLOCK`, disjoint blocks) still holds for the smaller wrong values an
    /// operator slip produces, and the kernel aliases this constant, so a silent shrink would
    /// under-reserve the ring area a real device writes into.
    #[test]
    fn ring_end_is_the_documented_value() {
        assert_eq!(RING_END, 0x146);
    }

    #[test]
    fn in_region_rejects_overflow() {
        assert!(!in_region(0, 0x1000, u64::MAX, 1)); // addr + len wraps
        assert!(in_region(0x1000, 0x1000, 0x1000, 0x1000)); // exact fit
        assert!(!in_region(0x1000, 0x1000, 0x1000, 0x1001)); // one past
    }

    #[test]
    fn ring_blocks_are_disjoint() {
        // Compile-time, because the relations are over constants: a queue's ring area fits inside its
        // block, and queue 0's area ends before queue 1's block begins (so the two never overlap).
        const { assert!(RING_END <= RING_BLOCK) };
        const { assert!(queue_block(0) + RING_END <= queue_block(1)) };
    }
}
