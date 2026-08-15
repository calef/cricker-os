//! **The NVMe block driver's volatile half** (milestone 53's storage half; notes/nvme.md).
//!
//! Everything an NVMe driver *touches*, over the arithmetic `crates/nvme` computes: volatile
//! register reads and writes into the controller's BAR0, command copies into the queue memory,
//! doorbell rings, and the polling loop that watches a completion's phase tag. The split is the
//! `pci` module's exactly, and rule 2 shapes the driver itself: [`Nvme`] takes the register
//! window's virtual base and one DMA region, passed in, and reaches for nothing else. What does
//! reach into the kernel is [`bring_up`], the policy function, which is what finds the controller
//! on the bus (`pci::find_nvme_device`), allocates the DMA region, and **confines the device to it
//! in hardware** before the controller is enabled: on both `virt` boards the IOMMU denies an
//! unlisted requester id by default (milestone 16b), so a controller that has not been confined
//! cannot fetch its first command, and this driver's bring-up is proof the confinement admits
//! exactly what it should.
//!
//! **Kernel-resident, and that is a recorded limitation rather than the design** (notes/nvme.md's
//! BUGS). The virtio drivers run at EL0 behind a `Virtio` capability whose kernel half validates
//! every queue address; NVMe has no such capability, and inventing one is new syscall surface,
//! which is a design fork (§10, §16) this milestone does not take. Until it is decided, the driver
//! serves the same block interface the FS server's block servers speak, one 4096-byte filesystem
//! block per transfer ([`fs_proto::blk`]'s unit), from kernel context.
//!
//! Name: unrecorded. Introduced 2026-08-15 with milestone 53's NVMe block driver, as the volatile
//! half of the `nvme` crate, the crate/module pairing `pci` and `virtio` already use. Provisional,
//! awaiting ratification with the crate's name.

use nvme::{Cap, Command, Completion, CqState, Doorbell, IdentifyNamespace, SqState, regs};

use crate::arch::mmu;

/// One transfer unit: a filesystem block, the same unit the blk-IPC protocol moves, so a future
/// NVMe-backed block server serves the FS server without a translation layer.
pub const BLOCK_SIZE: usize = fs_proto::blk::BLOCK_SIZE;

/// The DMA region's layout, in page offsets. Six pages, one purpose each, allocated contiguously
/// by [`bring_up`] and confined as one region: the two admin rings, the two I/O rings, the
/// identify buffer, and the one-block data buffer.
const ADMIN_SQ_PAGE: u64 = 0;
const ADMIN_CQ_PAGE: u64 = 1;
const IO_SQ_PAGE: u64 = 2;
const IO_CQ_PAGE: u64 = 3;
const IDENTIFY_PAGE: u64 = 4;
const DATA_PAGE: u64 = 5;
const DMA_PAGES: u64 = 6;

/// Sixteen entries per queue: one page holds 64 (submission) or 256 (completion), but this driver
/// completes each command before submitting the next, so depth buys nothing and a small ring keeps
/// the wrap (where the phase discipline earns its keep) inside every test run.
const ENTRIES: u16 = 16;

/// The one I/O queue pair this driver creates.
const IO_QID: u16 = 1;

/// The one namespace QEMU's `-device nvme,drive=` carries. Namespace ids are 1-based.
const NSID: u32 = 1;

/// How many polls of a status or completion before the driver declares the controller hung. QEMU
/// completes synchronously inside the doorbell write, so a bound this size is pure paranoia; on
/// hardware it is roughly a second of spinning, which is longer than CAP.TO promises.
const SPIN_BOUND: u32 = 50_000_000;

/// Everything that can go wrong under this driver, each naming the phase it died in, because "the
/// disk did not come up" from a five-step bring-up is the kind of message this project's own
/// debugging notes complain about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// CSTS.RDY never followed CC.EN within the spin bound (either direction).
    ControllerTimeout,
    /// CSTS.CFS: the controller declared a fatal internal error.
    ControllerFatal,
    /// A completion never arrived for a submitted command.
    CompletionTimeout,
    /// The controller completed a command with a nonzero status field (the field is carried).
    Command(u16),
    /// The identify data described a namespace this driver cannot serve (LBA format outside
    /// 512..=4096 bytes, or a size of zero).
    UnsupportedNamespace,
    /// CAP describes a controller this driver cannot drive (queues too shallow, or a minimum
    /// page size above the kernel's 4 KiB frames).
    UnsupportedController,
}

/// An initialized NVMe controller with one I/O queue pair, serving whole filesystem blocks.
/// Constructed only by [`Nvme::new`]; holding one is holding a disk that has already proven it can
/// answer admin commands.
pub struct Nvme {
    /// BAR0's virtual base: the register file and, from 0x1000, the doorbells.
    regs: u64,
    /// The doorbell stride from CAP, needed for every ring.
    dstrd: u32,
    /// The DMA region, both addresses: commands are built through `dma_va`, the controller is
    /// told `dma_phys`.
    dma_phys: u64,
    dma_va: u64,
    admin_sq: SqState,
    admin_cq: CqState,
    io_sq: SqState,
    io_cq: CqState,
    /// The namespace's geometry, from IDENTIFY: size and blocks-per-filesystem-block.
    ns: IdentifyNamespace,
    /// The next command identifier. Echoed back in completions and checked there; monotonic and
    /// wrapping, so a stale completion cannot impersonate a fresh one within 65536 commands.
    cid: u16,
}

impl Nvme {
    /// Bring the controller from reset to serving I/O: reset, admin queues, enable, identify the
    /// namespace, create the I/O queue pair. `regs_va` is BAR0's virtual base; `dma_phys`/`dma_va`
    /// name the same zeroed, physically contiguous [`DMA_PAGES`]-page region through the two
    /// address spaces. The caller has already confined the device to that region if an IOMMU is
    /// active; nothing in here can tell, which is the point of the confinement being outside.
    pub fn new(regs_va: u64, dma_phys: u64, dma_va: u64) -> Result<Nvme, Error> {
        let mut c = Nvme {
            regs: regs_va,
            dstrd: 0,
            dma_phys,
            dma_va,
            admin_sq: SqState::new(ENTRIES),
            admin_cq: CqState::new(ENTRIES),
            io_sq: SqState::new(ENTRIES),
            io_cq: CqState::new(ENTRIES),
            ns: IdentifyNamespace {
                blocks: 0,
                lba_shift: 9,
            },
            cid: 0,
        };
        let cap = Cap(c.reg64(regs::CAP));
        if cap.max_queue_entries() < ENTRIES as u32 || cap.min_page_size() > frames::FRAME_SIZE {
            return Err(Error::UnsupportedController);
        }
        c.dstrd = cap.doorbell_stride();

        // A controller that was already enabled (a warm reboot, or a previous test) must be taken
        // down before the admin queue registers may change; RDY follows EN in both directions.
        if c.reg32(regs::CSTS) & nvme::CSTS_RDY != 0 {
            c.wr32(regs::CC, 0);
            c.wait_rdy(false)?;
        }

        // The admin queue pair, by register: attributes (both depths, 0-based), then the two
        // page-aligned ring bases. The rings are zeroed pages, which is what makes the phase
        // discipline sound: the controller's first lap writes phase 1 into memory that reads 0.
        c.wr32(regs::AQA, (ENTRIES as u32 - 1) << 16 | (ENTRIES as u32 - 1));
        c.wr64(regs::ASQ, dma_phys + ADMIN_SQ_PAGE * frames::FRAME_SIZE);
        c.wr64(regs::ACQ, dma_phys + ADMIN_CQ_PAGE * frames::FRAME_SIZE);
        c.wr32(regs::CC, nvme::cc_enabled());
        c.wait_rdy(true)?;

        // IDENTIFY the namespace: its block count and LBA format are the two facts the block
        // arithmetic below stands on, and refusing an exotic format here beats corrupting it later.
        let prp = dma_phys + IDENTIFY_PAGE * frames::FRAME_SIZE;
        let cmd = Command::identify(c.next_cid(), nvme::CNS_NAMESPACE, NSID, prp);
        c.transact(0, cmd)?;
        // SAFETY: the identify page is ours (inside the region bring_up allocated), and the
        // controller finished writing it before the completion above was posted (NVMe's ordering
        // guarantee); the barrier in `transact` ordered those writes before this read.
        let data = unsafe {
            core::slice::from_raw_parts(
                (dma_va + IDENTIFY_PAGE * frames::FRAME_SIZE) as *const u8,
                frames::FRAME_SIZE as usize,
            )
        };
        c.ns = nvme::parse_identify_namespace(data).ok_or(Error::UnsupportedNamespace)?;
        if c.ns.blocks == 0 {
            return Err(Error::UnsupportedNamespace);
        }

        // The I/O pair, by admin command, completion queue first: the submission queue names its
        // completion queue, so creating them in the other order is an Invalid Queue Identifier.
        let cq_prp = dma_phys + IO_CQ_PAGE * frames::FRAME_SIZE;
        let cmd = Command::create_io_cq(c.next_cid(), IO_QID, ENTRIES, cq_prp);
        c.transact(0, cmd)?;
        let sq_prp = dma_phys + IO_SQ_PAGE * frames::FRAME_SIZE;
        let cmd = Command::create_io_sq(c.next_cid(), IO_QID, ENTRIES, IO_QID, sq_prp);
        c.transact(0, cmd)?;
        Ok(c)
    }

    /// The disk's capacity in bytes: the blk-IPC `SIZE` answer.
    pub fn size_bytes(&self) -> u64 {
        self.ns.bytes()
    }

    /// Read filesystem block `block` from the disk into [`Nvme::buffer`]: the blk-IPC `READ`.
    pub fn read_block(&mut self, block: u64) -> Result<(), Error> {
        self.transfer(block, false)
    }

    /// Write [`Nvme::buffer`]'s bytes to filesystem block `block`: the blk-IPC `WRITE`.
    pub fn write_block(&mut self, block: u64) -> Result<(), Error> {
        self.transfer(block, true)
    }

    /// The one-block transfer buffer the reads land in and the writes come from, the same shape
    /// as the shared page a block server transfers through.
    pub fn buffer(&mut self) -> &mut [u8; BLOCK_SIZE] {
        // SAFETY: the data page is inside the region bring_up allocated for exactly this, no
        // transfer is in flight (`transfer` completes each command before returning, and takes
        // `&mut self` like this does), and BLOCK_SIZE is one frame.
        unsafe { &mut *((self.dma_va + DATA_PAGE * frames::FRAME_SIZE) as *mut [u8; BLOCK_SIZE]) }
    }

    /// One whole-block transfer, both directions: the LBA arithmetic from the identified
    /// geometry, the PRP from the data page, one command, one polled completion.
    fn transfer(&mut self, block: u64, write: bool) -> Result<(), Error> {
        // 4096 is a whole number of logical blocks for every format `parse_identify_namespace`
        // admits (shift 9..=12), so this cannot fail; `expect` documents it.
        let per = self
            .ns
            .blocks_per(BLOCK_SIZE as u64)
            .expect("identify admitted a format 4096 is not a multiple of");
        let slba = block * per as u64;
        let data = self.dma_phys + DATA_PAGE * frames::FRAME_SIZE;
        let (prp1, prp2) = nvme::prp_pair(data, BLOCK_SIZE as u64, frames::FRAME_SIZE)
            .expect("one page-aligned block is always PRP-expressible");
        let cid = self.next_cid();
        let cmd = if write {
            Command::write(cid, NSID, slba, per, prp1, prp2)
        } else {
            Command::read(cid, NSID, slba, per, prp1, prp2)
        };
        self.transact(IO_QID, cmd)
    }

    /// Submit one command on queue `qid` and poll its completion: the copy into the ring, the
    /// publish barrier, the tail doorbell, the phase-gated poll, the head doorbell. Both queues
    /// ride this one path, which is why the admin bring-up is itself a test of the I/O machinery.
    fn transact(&mut self, qid: u16, cmd: Command) -> Result<(), Error> {
        let (sq_page, cq_page) = if qid == 0 {
            (ADMIN_SQ_PAGE, ADMIN_CQ_PAGE)
        } else {
            (IO_SQ_PAGE, IO_CQ_PAGE)
        };
        let expect_cid = (cmd.0[0] >> 16) as u16;

        let slot = if qid == 0 {
            self.admin_sq.push()
        } else {
            self.io_sq.push()
        };
        // SAFETY: slot < ENTRIES and ENTRIES 64-byte entries fit one frame, so the write stays
        // inside the submission ring's page of our own DMA region.
        unsafe {
            let dst =
                (self.dma_va + sq_page * frames::FRAME_SIZE + slot as u64 * 64) as *mut [u32; 16];
            core::ptr::write_volatile(dst, cmd.0);
        }
        // Publish the command before the doorbell: the controller is another observer, and the
        // doorbell write must not be reordered ahead of the entry it announces. dma_wmb is a full
        // barrier on both ISAs (DSB SY / fence), so it also orders the poll reads below against
        // this ring, which is why one barrier a side is enough.
        crate::arch::dma_wmb();
        let tail = if qid == 0 {
            self.admin_sq.tail()
        } else {
            self.io_sq.tail()
        };
        self.wr32(
            nvme::doorbell(qid, Doorbell::SubmissionTail, self.dstrd),
            tail as u32,
        );

        // Poll the completion ring at the head slot until the phase tag says the entry is this
        // lap's. Volatile reads through the direct map; the controller DMAs into the same page.
        let head = if qid == 0 {
            self.admin_cq.head()
        } else {
            self.io_cq.head()
        };
        let cqe =
            (self.dma_va + cq_page * frames::FRAME_SIZE + head as u64 * 16) as *const [u32; 4];
        let mut done: Option<Completion> = None;
        for _ in 0..SPIN_BOUND {
            // SAFETY: head < ENTRIES and ENTRIES 16-byte entries fit one frame; reads of our own
            // DMA region are always safe, whatever the device is writing there.
            let c = Completion::from_dwords(unsafe { core::ptr::read_volatile(cqe) });
            let owned = if qid == 0 {
                self.admin_cq.owned(&c)
            } else {
                self.io_cq.owned(&c)
            };
            if owned {
                done = Some(c);
                break;
            }
            if self.reg32(regs::CSTS) & nvme::CSTS_CFS != 0 {
                return Err(Error::ControllerFatal);
            }
            core::hint::spin_loop();
        }
        let c = done.ok_or(Error::CompletionTimeout)?;
        // Order the completion's phase read before the payload reads that follow (identify parse,
        // block bytes): on a weakly-ordered CPU nothing else stops the data read hoisting above
        // the flag read. Full barrier, as above.
        crate::arch::dma_wmb();

        // Consume: advance the head (flipping phase on wrap), tell the controller, and let the
        // submission ring reuse what the controller has read.
        let new_head = if qid == 0 {
            self.admin_cq.pop()
        } else {
            self.io_cq.pop()
        };
        self.wr32(
            nvme::doorbell(qid, Doorbell::CompletionHead, self.dstrd),
            new_head as u32,
        );
        if qid == 0 {
            self.admin_sq.note_head(c.sq_head);
        } else {
            self.io_sq.note_head(c.sq_head);
        }

        // The one-at-a-time discipline makes any other cid a protocol violation worth dying on
        // legibly rather than misattributing a completion.
        assert!(
            c.cid == expect_cid,
            "NVMe completion for cid {} while {} was in flight",
            c.cid,
            expect_cid
        );
        if c.status != 0 {
            return Err(Error::Command(c.status));
        }
        Ok(())
    }

    fn next_cid(&mut self) -> u16 {
        self.cid = self.cid.wrapping_add(1);
        self.cid
    }

    /// Wait for CSTS.RDY to become `want`, bounded. CFS short-circuits: a controller that has
    /// declared a fatal error will never become ready and the caller should hear which happened.
    fn wait_rdy(&self, want: bool) -> Result<(), Error> {
        for _ in 0..SPIN_BOUND {
            let csts = self.reg32(regs::CSTS);
            if csts & nvme::CSTS_CFS != 0 {
                return Err(Error::ControllerFatal);
            }
            if (csts & nvme::CSTS_RDY != 0) == want {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::ControllerTimeout)
    }

    fn reg32(&self, off: u64) -> u32 {
        // SAFETY: BAR0 is device-mapped (inside the PCI BAR window `mmu::map_everything` maps),
        // `off` comes from `regs`/`doorbell`, both bounded well inside the BAR's 16 KiB.
        unsafe { core::ptr::read_volatile((self.regs + off) as *const u32) }
    }

    fn wr32(&self, off: u64, v: u32) {
        // SAFETY: as `reg32`; the doorbell and control offsets written here are the driver's own.
        unsafe { core::ptr::write_volatile((self.regs + off) as *mut u32, v) }
    }

    /// The spec allows 64-bit registers to be accessed as aligned 32-bit halves (NVMe 1.4 §3.1),
    /// which keeps the accessor surface one width on both ISAs.
    fn reg64(&self, off: u64) -> u64 {
        self.reg32(off) as u64 | (self.reg32(off + 4) as u64) << 32
    }

    fn wr64(&self, off: u64, v: u64) {
        self.wr32(off, v as u32);
        self.wr32(off + 4, (v >> 32) as u32);
    }
}

/// **Find, confine, and initialize the machine's NVMe disk.** `None` when no controller is on the
/// bus (every boot the runner did not attach one), which is a fact about the machine; a controller
/// that is present but fails bring-up prints the phase it died in and also returns `None`, because
/// every caller's next move is the same, and the test that cares asserts presence first.
///
/// The order is the confinement story: the device gets its DMA region *before* the controller is
/// enabled, so there is no instant at which an enabled controller could reach anything else. On a
/// machine with no IOMMU (a plain `virt` boot) the confinement step is skipped and the driver
/// still runs, with nothing but the driver's own arithmetic bounding the addresses; the test
/// boots all have one, so the proven configuration is the confined one.
pub fn bring_up() -> Option<Nvme> {
    let dev = crate::pci::find_nvme_device()?;
    let dma = crate::memory::alloc_contiguous(DMA_PAGES as usize)
        .expect("no DMA region for the NVMe driver")
        .addr();
    // SAFETY: fresh contiguous frames via the direct map, owned by nobody yet. Zeroing is
    // load-bearing for the completion rings: the phase discipline starts from all-zero entries.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(dma) as *mut u8,
            0,
            (DMA_PAGES * frames::FRAME_SIZE) as usize,
        );
    }
    if crate::iommu::active() {
        crate::iommu::confine(
            dev.rid,
            &[paging::domain::DmaRegion {
                base: dma,
                size: DMA_PAGES * frames::FRAME_SIZE,
            }],
        );
    }
    match Nvme::new(mmu::phys_to_virt(dev.bar0), dma, mmu::phys_to_virt(dma)) {
        Ok(disk) => Some(disk),
        Err(e) => {
            crate::println!("  nvme: controller present but failed bring-up: {e:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The whole storage half of milestone 53, end to end**: the controller is found over the
    /// §18 PCIe transport, confined behind the machine's IOMMU, brought from reset through
    /// identify to an I/O queue pair, and then proves it can serve the blk-IPC verbs: SIZE
    /// answers the attached image's real size, WRITE persists a block, READ brings it back, and
    /// an untouched block still reads as the zeros the runner wrote.
    ///
    /// One test on purpose: bring-up is not idempotent state to share between test cases (a
    /// second `bring_up` would re-confine and re-create queues against a live controller), so the
    /// sequence lives in one place with the ordering visible.
    #[test_case]
    fn the_nvme_disk_serves_the_block_interface_end_to_end() {
        // The test flow always attaches a controller (xtask sets NIFE_NVME on both ISA legs),
        // so absence is a lost QEMU flag, not a machine without a disk: assert, don't skip.
        let mut disk = bring_up().expect("the test boot attaches an NVMe controller; none came up");

        // On every test boot an IOMMU fronts the PCIe bus on both ISAs, so what this run proved
        // is the confined configuration; say so if that ever silently stops being true.
        assert!(
            crate::iommu::active(),
            "the NVMe test expects the machine's IOMMU; without it the confinement claim is untested"
        );

        // SIZE: the runner's image is 8 MiB (xtask's mknvmedisk); identify must agree exactly.
        assert_eq!(disk.size_bytes(), 8 * 1024 * 1024);

        // WRITE a block whose bytes are a function of their offset, far enough in that a driver
        // confusing block and LBA units (the classic factor-of-8) would land visibly elsewhere.
        const BLOCK: u64 = 37;
        for (i, b) in disk.buffer().iter_mut().enumerate() {
            *b = (i as u64 % 251) as u8; // 251 is prime to 4096, so no page-periodic alias
        }
        disk.write_block(BLOCK).expect("write");

        // Clobber the buffer, READ the block back, and every byte must be the function again.
        disk.buffer().fill(0xaa);
        disk.read_block(BLOCK).expect("read");
        for (i, b) in disk.buffer().iter().enumerate() {
            assert_eq!(
                *b,
                (i as u64 % 251) as u8,
                "byte {i} of the block came back wrong"
            );
        }

        // A block this test never wrote is still the image's zeros: the write landed where it
        // said, not everywhere.
        disk.read_block(BLOCK + 1)
            .expect("read of an untouched block");
        assert!(disk.buffer().iter().all(|b| *b == 0));
    }
}
