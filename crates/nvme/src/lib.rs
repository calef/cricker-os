#![no_std]
//! **NVMe queue mechanics, as pure logic** (milestone 53's storage half; notes/nvme.md).
//!
//! Everything an NVMe driver computes, with nothing an NVMe driver touches. The controller's
//! register file, the doorbells, and the DMA memory the queues live in are all the kernel's
//! (`kernel/src/nvme.rs`, which is a thin volatile layer over this crate); what lives here is the
//! arithmetic those accesses carry: which doorbell, what dword, whose completion. That split is
//! rule 7's, the same one `pci` and `virtio` already follow, and it is what makes the queue logic
//! host-testable and Kani-reachable when the device itself only exists inside an emulator.
//!
//! The shape of the protocol, in one paragraph (NVMe 1.4, chapters 4 and 5): the driver builds
//! 64-byte **commands** in a **submission queue** (a ring in driver-owned DMA memory), then writes
//! the ring's new tail to that queue's **doorbell register**; the controller reads the commands,
//! does the work, and writes 16-byte **completions** into the paired **completion queue**, each
//! carrying a **phase tag** that flips every time the controller laps the ring, which is how the
//! driver tells a fresh completion from a stale one without any shared index; the driver then
//! writes the completion queue's new head to its own doorbell. Queue pair 0 is the **admin**
//! queue, created by registers at reset; every other pair is created by admin commands. Data moves
//! through **PRPs**, physical page pointers carried inside the command.
//!
//! # Examples
//!
//! The phase tag is the mechanism worth demonstrating, because it is how a driver tells a fresh
//! completion from a stale one **with no shared index and no memory the controller and driver both
//! write**. A ring that starts zeroed reads as phase 0, the first lap writes 1s, and every lap
//! flips:
//!
//! ```
//! use nvme::{Completion, CqState};
//!
//! let mut cq = CqState::new(2); // a two-slot ring, so it laps quickly
//!
//! // The initial zeros. Phase 0 against an expected phase of 1: not ours, do not read the result.
//! let stale = Completion::from_dwords([0, 0, 0, 0]);
//! assert!(!cq.owned(&stale));
//!
//! // The controller's first completion, phase bit set, for command id 7.
//! let fresh = Completion::from_dwords([0, 0, 0, 7 | (1 << 16)]);
//! assert!(cq.owned(&fresh));
//! assert_eq!(fresh.cid, 7);
//! assert_eq!(fresh.status, 0); // 0 is success
//! assert_eq!(cq.pop(), 1); // the value the driver writes to the completion head doorbell
//!
//! // Slot 1, same lap, still phase 1.
//! assert!(cq.owned(&fresh));
//! assert_eq!(cq.pop(), 0); // wrapped, so the expected phase flipped
//!
//! // Now the ring has lapped, and last lap's entries are the stale ones. This is the whole trick:
//! // the bytes in slot 0 have not changed, and their meaning has.
//! assert!(!cq.owned(&fresh));
//! assert!(cq.owned(&stale));
//! ```
//!
//! Doorbells are pure arithmetic over a stride the controller reports, and getting the parity wrong
//! means ringing a completion head where a submission tail belongs:
//!
//! ```
//! use nvme::{Cap, Doorbell, doorbell};
//!
//! // A controller reporting DSTRD 0, so doorbells are 4 bytes apart.
//! let cap = Cap(0);
//! assert_eq!(cap.doorbell_stride(), 0);
//!
//! // Admin queue (qid 0), then the one I/O queue pair this driver creates.
//! assert_eq!(doorbell(0, Doorbell::SubmissionTail, 0), 0x1000);
//! assert_eq!(doorbell(0, Doorbell::CompletionHead, 0), 0x1004);
//! assert_eq!(doorbell(1, Doorbell::SubmissionTail, 0), 0x1008);
//! assert_eq!(doorbell(1, Doorbell::CompletionHead, 0), 0x100c);
//! ```
//!
//! And [`prp_pair`] answers `None` rather than guessing, which is the interesting half: a transfer
//! that would need a PRP *list* is a bug in this driver rather than a device error, because its unit
//! of transfer is one 4096-byte filesystem block.
//!
//! ```
//! use nvme::prp_pair;
//!
//! // A page-aligned block: one pointer is enough.
//! assert_eq!(prp_pair(0x4000_0000, 4096, 4096), Some((0x4000_0000, 0)));
//!
//! // The same block, offset half a page: it spills, so PRP2 carries the next page boundary, with
//! // no offset of its own. That "offset-free by construction" is the spec's rule for PRP2.
//! assert_eq!(prp_pair(0x4000_0800, 4096, 4096), Some((0x4000_0800, 0x4000_1000)));
//!
//! // Three pages would need a list. Not expressible, and said so.
//! assert_eq!(prp_pair(0x4000_0000, 3 * 4096, 4096), None);
//! ```
//!
//! Name: unrecorded. Introduced 2026-08-15 with milestone 53's NVMe block driver. Provisional
//! (this lane's proposal, awaiting ratification): the specification's own name for the device
//! family, the same claim `pci` and `virtio` make, and `crates/virtio` is precedent for naming the
//! driver-logic crate after the family it drives. The kernel's `nvme` module is this crate's
//! volatile half, the crate/module name-sharing convention AGENTS.md records for `compositor`.

// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

/// Register offsets in BAR0 (NVMe 1.4 §3.1). All are 4-byte registers or 8-byte registers the
/// kernel accesses as two 4-byte halves (the spec permits either for the 64-bit ones).
pub mod regs {
    /// Controller Capabilities, 64-bit read-only. See [`super::Cap`].
    pub const CAP: u64 = 0x00;
    /// Controller Configuration, 32-bit. See [`super::cc_enabled`].
    pub const CC: u64 = 0x14;
    /// Controller Status, 32-bit read-only. See [`super::CSTS_RDY`].
    pub const CSTS: u64 = 0x1c;
    /// Admin Queue Attributes: (ACQ entries - 1) << 16 | (ASQ entries - 1).
    pub const AQA: u64 = 0x24;
    /// Admin Submission Queue base, 64-bit, page-aligned physical.
    pub const ASQ: u64 = 0x28;
    /// Admin Completion Queue base, 64-bit, page-aligned physical.
    pub const ACQ: u64 = 0x30;
    /// The first doorbell. Doorbell N is at `DOORBELL_BASE + N * (4 << CAP.DSTRD)`; see
    /// [`super::doorbell`].
    pub const DOORBELL_BASE: u64 = 0x1000;
}

/// CSTS.RDY: the controller has come up (after CC.EN is set) or gone down (after it is cleared).
pub const CSTS_RDY: u32 = 1;
/// CSTS.CFS: Controller Fatal Status. The controller gave up; no doorbell will help.
pub const CSTS_CFS: u32 = 1 << 1;

/// The decoded fields of CAP this driver acts on. A newtype over the raw register value so the
/// bit positions live in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap(pub u64);

impl Cap {
    /// Maximum Queue Entries Supported, converted from the register's 0-based field to a count.
    /// Every queue this driver creates must be no deeper than this.
    pub fn max_queue_entries(self) -> u32 {
        (self.0 as u32 & 0xffff) + 1
    }
    /// Doorbell Stride: doorbell N is at `0x1000 + N * (4 << DSTRD)` (a 4-bit field, so the shift
    /// is at most 19 and the multiply cannot overflow a u64 for any real doorbell index).
    pub fn doorbell_stride(self) -> u32 {
        (self.0 >> 32) as u32 & 0xf
    }
    /// Timeout for CSTS.RDY to follow CC.EN, in 500 ms units.
    pub fn timeout_500ms(self) -> u32 {
        (self.0 >> 24) as u32 & 0xff
    }
    /// Memory Page Size Minimum, as a byte count (the field is `2^(12+MPSMIN)`). This driver runs
    /// with CC.MPS = 0, i.e. 4 KiB pages, so bring-up checks this is at most 4096.
    pub fn min_page_size(self) -> u64 {
        1u64 << (12 + ((self.0 >> 48) & 0xf))
    }
}

/// The CC value that enables the controller for this driver's configuration: EN=1, CSS=0 (the NVM
/// command set), MPS=0 (4 KiB memory pages, matching the kernel's frames), AMS=0 (round-robin),
/// IOSQES=6 (64-byte submission entries), IOCQES=4 (16-byte completion entries). The entry sizes
/// are the spec's only defined ones and the controller checks them before honoring a queue
/// creation, which is why they are set here rather than defaulted.
pub fn cc_enabled() -> u32 {
    1 | (6 << 16) | (4 << 20)
}

/// A queue's role, which picks its doorbell: submission tails are even doorbells, completion
/// heads odd (NVMe 1.4 §3.1.24-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doorbell {
    /// The submission queue's tail doorbell: even index.
    SubmissionTail,
    /// The completion queue's head doorbell: odd index.
    CompletionHead,
}

/// The BAR0 offset of queue `qid`'s doorbell. `dstrd` is [`Cap::doorbell_stride`].
pub fn doorbell(qid: u16, which: Doorbell, dstrd: u32) -> u64 {
    let index = 2 * qid as u64
        + match which {
            Doorbell::SubmissionTail => 0,
            Doorbell::CompletionHead => 1,
        };
    regs::DOORBELL_BASE + index * (4u64 << (dstrd & 0xf))
}

// --- commands -------------------------------------------------------------------------------

/// Admin opcodes (NVMe 1.4 §5).
pub const ADMIN_CREATE_IO_SQ: u8 = 0x01;
/// Create an I/O completion queue.
pub const ADMIN_CREATE_IO_CQ: u8 = 0x05;
/// Fetch a controller or namespace data structure ([`CNS_CONTROLLER`], [`CNS_NAMESPACE`]).
pub const ADMIN_IDENTIFY: u8 = 0x06;

/// NVM (I/O) opcodes (NVMe 1.4 §6).
pub const NVM_WRITE: u8 = 0x01;
/// Read logical blocks.
pub const NVM_READ: u8 = 0x02;

/// Identify CNS values: the namespace data structure, and the controller's.
pub const CNS_NAMESPACE: u32 = 0x00;
/// The controller's own identify data structure, rather than a namespace's.
pub const CNS_CONTROLLER: u32 = 0x01;

/// One 64-byte submission queue entry, as the sixteen dwords the driver writes. `#[repr(C)]` so
/// the kernel can copy it into the ring as-is; the wire format is little-endian dwords and both
/// target ISAs are little-endian (the same layout argument `fs_proto` records).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command(pub [u32; 16]);

impl Command {
    /// The common prefix every builder shares: opcode and command identifier in dword 0 (no fused
    /// operation, PRP data transfers), namespace id in dword 1, PRP1/PRP2 in dwords 6..10.
    fn new(opcode: u8, cid: u16, nsid: u32, prp1: u64, prp2: u64) -> Self {
        let mut dw = [0u32; 16];
        dw[0] = opcode as u32 | (cid as u32) << 16;
        dw[1] = nsid;
        dw[6] = prp1 as u32;
        dw[7] = (prp1 >> 32) as u32;
        dw[8] = prp2 as u32;
        dw[9] = (prp2 >> 32) as u32;
        Command(dw)
    }

    /// IDENTIFY (admin): the 4096-byte data structure named by `cns` lands at `prp1`.
    pub fn identify(cid: u16, cns: u32, nsid: u32, prp1: u64) -> Self {
        let mut c = Self::new(ADMIN_IDENTIFY, cid, nsid, prp1, 0);
        c.0[10] = cns;
        c
    }

    /// CREATE I/O COMPLETION QUEUE (admin): queue `qid`, `entries` deep, physically contiguous
    /// (PC=1) at `prp1`. Interrupts stay off (IEN=0): the kernel driver completes by polling the
    /// phase tag, so no vector is named and none is needed.
    pub fn create_io_cq(cid: u16, qid: u16, entries: u16, prp1: u64) -> Self {
        let mut c = Self::new(ADMIN_CREATE_IO_CQ, cid, 0, prp1, 0);
        c.0[10] = qid as u32 | ((entries as u32 - 1) << 16);
        c.0[11] = 1; // PC only; IEN=0
        c
    }

    /// CREATE I/O SUBMISSION QUEUE (admin): queue `qid`, `entries` deep, physically contiguous at
    /// `prp1`, completing into completion queue `cqid` (which must already exist; the kernel
    /// creates the pair CQ-first for exactly that reason).
    pub fn create_io_sq(cid: u16, qid: u16, entries: u16, cqid: u16, prp1: u64) -> Self {
        let mut c = Self::new(ADMIN_CREATE_IO_SQ, cid, 0, prp1, 0);
        c.0[10] = qid as u32 | ((entries as u32 - 1) << 16);
        c.0[11] = 1 | (cqid as u32) << 16; // PC, and the paired CQ
        c
    }

    /// READ (I/O): `blocks` logical blocks starting at `slba`, into the PRPs. `blocks` is the real
    /// count; the wire field is 0-based and this builder does the subtraction, so a caller cannot
    /// get the off-by-one wrong. `blocks` must be nonzero (asserted: zero would wrap to 65536).
    pub fn read(cid: u16, nsid: u32, slba: u64, blocks: u16, prp1: u64, prp2: u64) -> Self {
        assert!(blocks != 0, "a zero-block transfer has no wire encoding");
        let mut c = Self::new(NVM_READ, cid, nsid, prp1, prp2);
        c.0[10] = slba as u32;
        c.0[11] = (slba >> 32) as u32;
        c.0[12] = (blocks - 1) as u32;
        c
    }

    /// WRITE (I/O): the same addressing as [`Command::read`], the other direction.
    pub fn write(cid: u16, nsid: u32, slba: u64, blocks: u16, prp1: u64, prp2: u64) -> Self {
        assert!(blocks != 0, "a zero-block transfer has no wire encoding");
        let mut c = Self::new(NVM_WRITE, cid, nsid, prp1, prp2);
        c.0[10] = slba as u32;
        c.0[11] = (slba >> 32) as u32;
        c.0[12] = (blocks - 1) as u32;
        c
    }
}

/// The two PRP words for one data transfer of `len` bytes at physical `base`, against `page_size`
/// pages. PRP1 may carry an offset; every later page pointer must not. One page of data needs only
/// PRP1; a transfer spilling into a second page puts that page in PRP2; anything longer needs a
/// PRP *list*, which this driver does not build (its unit is one 4096-byte filesystem block), so
/// `None` says "not expressible", and the caller treats it as a bug rather than a device error.
pub fn prp_pair(base: u64, len: u64, page_size: u64) -> Option<(u64, u64)> {
    assert!(page_size.is_power_of_two() && page_size >= 512);
    if len == 0 {
        return None;
    }
    let offset = base & (page_size - 1);
    let end = offset.checked_add(len)?;
    if end <= page_size {
        Some((base, 0))
    } else if end <= 2 * page_size {
        // The second pointer is the next page boundary after base, offset-free by construction.
        // Checked: a base in the last page of the address space has no next boundary, and Kani
        // found exactly that overflow in this line's first draft (`+` for `checked_add`).
        Some((base, (base & !(page_size - 1)).checked_add(page_size)?))
    } else {
        None // would need a PRP list
    }
}

// --- completions ----------------------------------------------------------------------------

/// One 16-byte completion queue entry, decoded from the four dwords the controller wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    /// Command-specific result (dword 0).
    pub result: u32,
    /// The submission queue head as the controller last consumed it: how the driver learns its
    /// submission ring entries are reusable.
    pub sq_head: u16,
    /// Which submission queue the completed command came from.
    pub sq_id: u16,
    /// The command identifier the driver assigned, echoed back.
    pub cid: u16,
    /// The phase tag: dword 3, bit 16. See [`CqState::owned`].
    pub phase: bool,
    /// The 15-bit status field; 0 is success. Status code type in bits 8..11, code in 0..8.
    pub status: u16,
}

impl Completion {
    /// Decode the four dwords the controller wrote into a completion queue slot.
    pub fn from_dwords(dw: [u32; 4]) -> Self {
        Completion {
            result: dw[0],
            sq_head: dw[2] as u16,
            sq_id: (dw[2] >> 16) as u16,
            cid: dw[3] as u16,
            phase: dw[3] & (1 << 16) != 0,
            status: (dw[3] >> 17) as u16,
        }
    }
}

// --- ring arithmetic ------------------------------------------------------------------------

/// A submission ring's driver-side state: where the next command lands and what tail value to
/// ring. The controller's consumption (via [`Completion::sq_head`]) is deliberately not modeled:
/// this driver completes every command before submitting the next, so the ring can never fill,
/// and [`SqState::push`] asserts the invariant that guards that assumption instead of carrying a
/// free-slot count nothing would exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqState {
    entries: u16,
    tail: u16,
    /// The controller's head, updated from each completion's [`Completion::sq_head`].
    head: u16,
}

impl SqState {
    /// A fresh ring of `entries` slots (at least 2: a one-slot ring cannot distinguish full from
    /// empty in tail/head form, and the spec's minimum queue size is 2).
    pub fn new(entries: u16) -> Self {
        assert!(entries >= 2);
        SqState {
            entries,
            tail: 0,
            head: 0,
        }
    }

    /// Claim the slot for one command: returns the slot index to write, advancing the tail. The
    /// caller then rings [`doorbell`] with [`SqState::tail`]. Panics if the ring is full, which
    /// for this driver is a logic bug (one command in flight per queue), not a load condition.
    pub fn push(&mut self) -> u16 {
        let next = (self.tail + 1) % self.entries;
        assert!(next != self.head, "submission ring full");
        let slot = self.tail;
        self.tail = next;
        slot
    }

    /// The tail value to write to the doorbell after [`SqState::push`].
    pub fn tail(&self) -> u16 {
        self.tail
    }

    /// Record the controller's consumption, from a completion's `sq_head`.
    pub fn note_head(&mut self, head: u16) {
        assert!(
            head < self.entries,
            "controller reported an impossible head"
        );
        self.head = head;
    }
}

/// A completion ring's driver-side state: where the next completion will appear and which phase
/// value marks it as fresh. The phase starts true (the ring starts zeroed, so the controller's
/// first lap writes 1s) and flips every time the head wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CqState {
    entries: u16,
    head: u16,
    phase: bool,
}

impl CqState {
    /// A fresh ring of `entries` slots, phase starting true (see the struct docs).
    pub fn new(entries: u16) -> Self {
        assert!(entries >= 2);
        CqState {
            entries,
            head: 0,
            phase: true,
        }
    }

    /// The slot to poll for the next completion.
    pub fn head(&self) -> u16 {
        self.head
    }

    /// Does this completion belong to the driver yet? True when its phase tag matches the phase
    /// this lap expects; a stale entry (last lap's, or the initial zeros) fails the match.
    pub fn owned(&self, c: &Completion) -> bool {
        c.phase == self.phase
    }

    /// Consume the entry at the head: advance, flipping the expected phase on wrap. Returns the
    /// new head, which the caller writes to this queue's [`doorbell`].
    pub fn pop(&mut self) -> u16 {
        self.head += 1;
        if self.head == self.entries {
            self.head = 0;
            self.phase = !self.phase;
        }
        self.head
    }
}

// --- identify parsing -----------------------------------------------------------------------

/// The two facts the driver needs from the 4096-byte IDENTIFY namespace structure (NVMe 1.4
/// §5.15.2, figure 245): the namespace size in logical blocks (NSZE, bytes 0..8) and the data
/// shift of the LBA format in use (FLBAS names one of 16 formats; each format's LBADS is
/// `1 << shift` bytes per block). `None` if the structure is shorter than the fields it must
/// carry, or describes a block size outside what a driver of 4096-byte filesystem blocks can
/// serve (LBADS below 9 is the spec's "unsupported" marker; above 12 a block would outgrow the
/// transfer buffer).
pub fn parse_identify_namespace(data: &[u8]) -> Option<IdentifyNamespace> {
    if data.len() < 384 {
        return None; // NSZE..the LBA format table must be present
    }
    let nsze = u64::from_le_bytes(data[0..8].try_into().ok()?);
    let flbas = data[26] & 0xf;
    // LBA format `flbas` is 4 bytes at 128 + 4*flbas; LBADS is its third byte.
    let lbads = data[128 + 4 * flbas as usize + 2];
    if !(9..=12).contains(&lbads) {
        return None;
    }
    Some(IdentifyNamespace {
        blocks: nsze,
        lba_shift: lbads as u32,
    })
}

/// See [`parse_identify_namespace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentifyNamespace {
    /// The namespace's size in logical blocks.
    pub blocks: u64,
    /// `1 << lba_shift` bytes per logical block; between 9 and 12 by construction.
    pub lba_shift: u32,
}

impl IdentifyNamespace {
    /// The namespace's capacity in bytes.
    pub fn bytes(&self) -> u64 {
        self.blocks << self.lba_shift
    }
    /// How many logical blocks one `unit`-byte transfer covers, or `None` if `unit` is not a
    /// whole number of logical blocks (a namespace this driver cannot serve 4096-byte filesystem
    /// blocks on; `parse_identify_namespace`'s shift bounds make it impossible for unit 4096).
    pub fn blocks_per(&self, unit: u64) -> Option<u16> {
        if unit == 0 || unit & ((1 << self.lba_shift) - 1) != 0 {
            return None;
        }
        Some((unit >> self.lba_shift) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_fields_decode_from_their_spec_positions() {
        // QEMU's real CAP value for a default nvme device is in this family: MQES=0x7ff,
        // TO=0xf, DSTRD=0, MPSMIN=0. Build one by hand and read every field back.
        let cap = Cap(0x7ffu64 | (0xf << 24) | (2 << 32) | (1 << 48));
        assert_eq!(cap.max_queue_entries(), 0x800);
        assert_eq!(cap.timeout_500ms(), 0xf);
        assert_eq!(cap.doorbell_stride(), 2);
        assert_eq!(cap.min_page_size(), 8192);
    }

    #[test]
    fn doorbells_land_where_the_spec_puts_them() {
        // §3.1.24-25 worked example, stride 0: SQ0 tail at 0x1000, CQ0 head at 0x1004,
        // SQ1 tail 0x1008, CQ1 head 0x100c.
        assert_eq!(doorbell(0, Doorbell::SubmissionTail, 0), 0x1000);
        assert_eq!(doorbell(0, Doorbell::CompletionHead, 0), 0x1004);
        assert_eq!(doorbell(1, Doorbell::SubmissionTail, 0), 0x1008);
        assert_eq!(doorbell(1, Doorbell::CompletionHead, 0), 0x100c);
        // A stride spreads them: with DSTRD=2 each doorbell is 16 bytes apart.
        assert_eq!(doorbell(1, Doorbell::SubmissionTail, 2), 0x1020);
    }

    #[test]
    fn a_read_command_encodes_the_spec_dwords() {
        let c = Command::read(7, 1, 0x1_0000_0008, 8, 0xdead_b000, 0);
        assert_eq!(c.0[0], 0x02 | 7 << 16, "opcode and cid share dword 0");
        assert_eq!(c.0[1], 1, "nsid");
        assert_eq!((c.0[6], c.0[7]), (0xdead_b000, 0), "prp1 split across 6/7");
        assert_eq!((c.0[10], c.0[11]), (8, 1), "slba split across 10/11");
        assert_eq!(c.0[12], 7, "the block count is 0-based on the wire");
    }

    #[test]
    fn queue_creation_packs_size_and_pairing() {
        let cq = Command::create_io_cq(1, 3, 16, 0x2000);
        assert_eq!(cq.0[10], 3 | 15 << 16, "qid, and the 0-based size");
        assert_eq!(cq.0[11], 1, "physically contiguous, interrupts off");
        let sq = Command::create_io_sq(2, 3, 16, 3, 0x3000);
        assert_eq!(sq.0[10], 3 | 15 << 16);
        assert_eq!(sq.0[11], 1 | 3 << 16, "PC, and the paired cqid");
    }

    #[test]
    fn completions_decode_and_the_phase_gates_ownership() {
        let cq = CqState::new(4);
        // The controller writes phase=1 on its first lap: owned.
        let fresh = Completion::from_dwords([0, 0, 5 | 1 << 16, 42 | 1 << 16]);
        assert_eq!((fresh.sq_head, fresh.sq_id, fresh.cid), (5, 1, 42));
        assert_eq!(fresh.status, 0);
        assert!(cq.owned(&fresh));
        // A zeroed (never-written) entry is not.
        let stale = Completion::from_dwords([0; 4]);
        assert!(!cq.owned(&stale));
        // An error status decodes: dword 3 bits 17.. carry it.
        let failed = Completion::from_dwords([0, 0, 0, 1 << 16 | 0x2 << 17]);
        assert_eq!(failed.status, 0x2, "Invalid Field in Command");
    }

    #[test]
    fn the_phase_flips_every_lap_and_only_at_the_wrap() {
        let mut cq = CqState::new(3);
        let mut flips = 0;
        let mut last = true;
        for _ in 0..9 {
            assert!(cq.head() < 3);
            cq.pop();
            let phase_now = cq.owned(&Completion::from_dwords([0, 0, 0, 1 << 16]));
            if phase_now != last {
                flips += 1;
                last = phase_now;
            }
        }
        assert_eq!(flips, 3, "nine pops of a 3-ring wrap exactly three times");
    }

    #[test]
    fn the_submission_ring_walks_its_slots_in_order() {
        let mut sq = SqState::new(4);
        // With completions consuming as we go (head following), the ring cycles forever.
        for i in 0..12u16 {
            let slot = sq.push();
            assert_eq!(slot, i % 4);
            assert_eq!(sq.tail(), (i + 1) % 4);
            sq.note_head(sq.tail()); // the controller caught up
        }
    }

    #[test]
    #[should_panic(expected = "submission ring full")]
    fn a_full_submission_ring_is_a_driver_bug_not_a_wrap() {
        let mut sq = SqState::new(2);
        sq.push(); // tail 1, head 0: one free slot left is the full condition
        sq.push();
    }

    #[test]
    fn prp_pairs_cover_one_and_two_pages_and_refuse_more() {
        // A page-aligned single page: PRP1 only.
        assert_eq!(prp_pair(0x5000, 4096, 4096), Some((0x5000, 0)));
        // An offset transfer spilling into the next page: PRP2 is that page's boundary.
        assert_eq!(prp_pair(0x5800, 4096, 4096), Some((0x5800, 0x6000)));
        // Exactly two aligned pages: still expressible.
        assert_eq!(prp_pair(0x5000, 8192, 4096), Some((0x5000, 0x6000)));
        // More needs a PRP list, which this driver does not build.
        assert_eq!(prp_pair(0x5800, 8192, 4096), None);
        assert_eq!(prp_pair(0x5000, 0, 4096), None, "empty transfers refused");
    }

    #[test]
    fn identify_namespace_yields_size_and_shift() {
        let mut data = [0u8; 4096];
        data[0..8].copy_from_slice(&0x4000u64.to_le_bytes()); // NSZE: 16384 blocks
        data[26] = 0; // FLBAS: format 0
        data[130] = 9; // format 0's LBADS: 512-byte blocks
        let id = parse_identify_namespace(&data).unwrap();
        assert_eq!(id.blocks, 0x4000);
        assert_eq!(id.lba_shift, 9);
        assert_eq!(id.bytes(), 0x4000 * 512);
        assert_eq!(id.blocks_per(4096), Some(8));

        data[130] = 13; // an LBA format this driver cannot serve
        assert!(parse_identify_namespace(&data).is_none());
        assert!(parse_identify_namespace(&[0u8; 100]).is_none(), "truncated");
    }
}

/// Machine-checked proofs (`script/verify`; notes/verification.md).
///
/// The ring arithmetic runs forever against a device that writes into the same memory, so the
/// properties proved are the ones a lapped or hostile ring could otherwise erode: indices stay
/// inside the ring for every reachable state, the phase discipline is exactly one flip per lap,
/// and the doorbell/PRP arithmetic is total (no register value the controller reports can make an
/// offset computation panic or two doorbells collide).
#[cfg(kani)]
mod verification {
    use super::*;

    /// **The completion head stays in bounds and flips the phase exactly at the wrap**, for every
    /// reachable state, not just the ones a test happened to walk.
    #[kani::proof]
    fn cq_pop_stays_in_bounds_and_flips_only_at_the_wrap() {
        let entries: u16 = kani::any();
        let head: u16 = kani::any();
        let phase: bool = kani::any();
        kani::assume(entries >= 2 && head < entries);
        let mut cq = CqState {
            entries,
            head,
            phase,
        };
        let ret = cq.pop();
        assert!(cq.head < entries);
        assert_eq!(ret, cq.head);
        // The phase flipped if and only if the head wrapped to zero.
        assert_eq!(cq.phase != phase, cq.head == 0);
    }

    /// **A submission push from any non-full state lands in bounds**, and the doorbell value it
    /// leaves behind is a legal tail.
    #[kani::proof]
    fn sq_push_stays_in_bounds() {
        let entries: u16 = kani::any();
        let tail: u16 = kani::any();
        let head: u16 = kani::any();
        kani::assume(entries >= 2 && tail < entries && head < entries);
        kani::assume((tail + 1) % entries != head); // not full, the asserted precondition
        let mut sq = SqState {
            entries,
            tail,
            head,
        };
        let slot = sq.push();
        assert!(slot < entries);
        assert!(sq.tail() < entries);
    }

    /// **No two doorbells collide**, for any queue ids and any stride the controller can report:
    /// distinct (queue, role) pairs always compute distinct offsets, so a tail write can never
    /// land on a head register. This is the arithmetic the kernel's volatile writes trust.
    #[kani::proof]
    fn distinct_doorbells_never_collide() {
        let qa: u16 = kani::any();
        let qb: u16 = kani::any();
        let ra = if kani::any() {
            Doorbell::SubmissionTail
        } else {
            Doorbell::CompletionHead
        };
        let rb = if kani::any() {
            Doorbell::SubmissionTail
        } else {
            Doorbell::CompletionHead
        };
        let dstrd: u32 = kani::any();
        if (qa, ra) != (qb, rb) {
            assert!(doorbell(qa, ra, dstrd) != doorbell(qb, rb, dstrd));
        }
    }

    /// **PRP construction is total and never spans more pages than it names.** For any base,
    /// length and power-of-two page size, `prp_pair` either refuses or returns a pair whose two
    /// pointers cover the whole transfer: PRP2 is zero exactly when one page suffices, and is
    /// page-aligned whenever it is used (the spec's requirement on every pointer after the first).
    #[kani::proof]
    fn prp_pair_is_total_and_page_disciplined() {
        let base: u64 = kani::any();
        let len: u64 = kani::any();
        let shift: u32 = kani::any();
        kani::assume((9..=16).contains(&shift));
        let page = 1u64 << shift;
        if let Some((prp1, prp2)) = prp_pair(base, len, page) {
            assert_eq!(prp1, base);
            let offset = base & (page - 1);
            if offset + len <= page {
                assert_eq!(prp2, 0);
            } else {
                assert_eq!(prp2 & (page - 1), 0);
                assert_eq!(prp2, (base & !(page - 1)) + page);
            }
        }
    }

    /// **Identify parsing is total for any device response.** The 4096 bytes come from the
    /// controller's DMA; no contents may panic the parse, and an accepted answer always carries a
    /// shift the block driver's arithmetic (`blocks_per(4096)`) can serve.
    #[kani::proof]
    fn identify_parse_is_total_and_bounds_the_shift() {
        let data: [u8; 384] = kani::any();
        if let Some(id) = parse_identify_namespace(&data) {
            assert!((9..=12).contains(&id.lba_shift));
            let per = id.blocks_per(4096).unwrap();
            assert!((1..=8).contains(&per));
        }
    }
}
