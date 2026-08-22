use super::*;
use crate::cap::{Rights, endpoint_cap, irq_cap, untyped_cap, virtio_cap};
use crate::sched::EpId;

/// The block server's role in the driver binary (must match user/src/{hello,blk}.rs and virtio.rs).
const ROLE_BLK_SERVER: u64 = 32;

/// The heap budget the FS server draws RedoxFS's allocations from. RedoxFS keeps a 128 KiB
/// compress buffer, block buffers, and small tree structures for the images phase 2 serves; 8 MiB
/// is comfortable and is the process's hard ceiling (its `HEAP_MAX` matches).
const FS_BUDGET_PAGES: u64 = 2048;

/// **Extra stack pages for the FS server**, below the single page `run` maps, so the process
/// gets `1 + FS_STACK_PAGES` pages in one contiguous run down from [`USER_STACK_TOP`].
///
/// RedoxFS recurses through its tree, htree and transaction code with **8 KiB frames** (one
/// `read_block::<TreeList<..>>` activation carries a whole 4096-byte block plus scratch), so this
/// is not a "generous round number" question, it is a measured one. It used to be 32, and 32 was
/// **528 bytes short** the moment milestone 31 phase 2 added `CREATE` and `TRUNCATE`: the FS
/// server took one more level of tree recursion, ran off the bottom of its stack, and was killed
/// by a data abort mid-request, which left every client blocked on a `CALL` that would never be
/// answered. See [`fs_stack_used`] for the instrument that now measures this instead of guessing,
/// and `the_fs_servers_stack_still_has_headroom` for the test that fails the day it is too small
/// again. notes/fs-server.md carries the story.
///
/// 96 is chosen against the measurement, not above it: the high-water is **135,696 bytes** (and
/// 127,408 as measured by milestone 37, an unattributed 8 KiB lower; notes/fs-server.md carries
/// both and the reasoning), and
/// `1 + 96` pages is 397,312, which leaves room for roughly thirty more 8 KiB activations. That
/// margin is the point, because recursion depth here tracks the *tree* depth, which grows with
/// the image; a size proven on a 16 MiB fixture is not proven on a real disk. 384 KiB of frames
/// once per boot is cheap next to the FS server's 8 MiB heap budget.
const FS_STACK_PAGES: u64 = 96;

/// The pattern every FS-server stack page is filled with before the process starts. Nothing but
/// the FS server's own stack writes ever touch these frames, so a word that still reads as this
/// is a word the stack never reached, and the deepest changed word is the high-water mark. The
/// value is deliberately not 0 and not a plausible pointer, so a poisoned word cannot be mistaken
/// for real data (or vice versa) in a dump.
const STACK_POISON: u64 = 0xC71C_5E57_C71C_5E57;

// The VAs each process expects its mappings at. Each MUST match that program's source.
const DMA_VA: u64 = 0x0000_0000_0090_0000; // block server DMA region, 1 + BLK_PAGES pages (crates/virtio)
const BLK_PAGE_FS: u64 = 0x5000_0000; // FS server's block region (fs_server.rs BLK_PAGE)
// FS server's file region (fs_server.rs FILE_PAGE): BLK_PAGES pages above BLK_PAGE_FS, so growing
// the block channel (milestone 138 step 4) cannot walk into it.
const FILE_PAGE_FS: u64 = BLK_PAGE_FS + (BLK_PAGES as u64) * FRAME_SIZE;
const FILE_VA_CLIENT: u64 = 0x0000_0000_0060_0000; // client's file page (fs_test_client.rs FILE_VA)

/// A std program's half of the same agreement (notes/abi.md §4, notes/std.md). Both constants
/// MUST match the std PAL's `sys/pal/nife/rt.rs`: the slot it looks for the FS-service
/// endpoint in, and the VA it expects the shared file page at. A std program's slot layout
/// differs from the hand-written client's because std already owes slots 0 and 1 to its heap and
/// its stdout, and 2 and 3 to `std::net`.
const FS_DIR_SLOT: u64 = 4;
const FS_PAGE_STD: u64 = 0x0000_0000_1100_0000;

/// A fresh, zeroed frame, returned by physical address. Zeroed so no stale RAM is ever visible
/// across a share, and (for the DMA frame) so the device never reads a stale descriptor.
fn frame() -> u64 {
    let p = crate::memory::alloc()
        .expect("no frame for the fs service")
        .addr();
    // SAFETY: fresh frame, reachable through the direct map.
    unsafe { core::ptr::write_bytes(mmu::phys_to_virt(p) as *mut u8, 0, FRAME_SIZE as usize) };
    p
}

/// **How many pages the file channel spans**, straight from the contract, so this wiring cannot
/// disagree with the two programs that speak it.
const FILE_PAGES: usize = fs_proto::fs::TRANSFER_PAGES;

/// **How many pages the blk channel spans** (milestone 138 step 4), straight from the contract, the
/// same reason [`FILE_PAGES`] is.
const BLK_PAGES: usize = fs_proto::blk::TRANSFER_BLOCKS;

/// **The file channel: [`FILE_PAGES`] fresh, zeroed, physically contiguous frames**, returned by
/// the base physical address (milestone 138 step 3).
///
/// Contiguous rather than a list of frames, and that is what keeps this change from spreading. Both
/// halves of the agreement already pass the channel around as one `u64`; a run of frames keeps that
/// signature, and every mapping site becomes a loop over [`FILE_PAGES`] instead of a struct change
/// in nine places. The block server's DMA region is already wired exactly this way (two contiguous
/// pages at `DMA_VA`), so this is the tree's existing shape for a multi-page share rather than a
/// new one.
///
/// It is zeroed for the same reason one frame was: no stale RAM is ever visible across a share.
fn file_channel() -> u64 {
    let p = crate::memory::alloc_contiguous(FILE_PAGES)
        .expect("no contiguous run for the fs service's file channel")
        .addr();
    // SAFETY: a fresh run of FILE_PAGES frames, reachable through the direct map.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(p) as *mut u8,
            0,
            FILE_PAGES * FRAME_SIZE as usize,
        );
    };
    p
}

/// Write the file channel's mappings into `maps`, `va` upward against `phys` upward, and answer how
/// many entries that took. One call per party that shares the channel, so the "map every page of
/// it" rule lives in one place rather than in each of them.
///
/// **`pages` is how much of the channel this party maps**, and it is a parameter rather than always
/// [`FILE_PAGES`] because a client is entitled to map less: a client that only ever moves one page
/// needs one page, and `fs_proto::fs::TRANSFER_PAGES` says so. The FS server is the one party that
/// must map all of it.
///
/// Visible to the rest of `user` because the SMB adapter is wired somewhere else entirely
/// (`virtio_service`, since it is a client of the net stack rather than of this module) and shares
/// the same channel with the same FS server. Two spellings of "map every page of it" is exactly the
/// drift the foot gun at `fs_proto::fs::TRANSFER_PAGES` punishes, so there is one.
pub(super) fn map_channel(maps: &mut [Mapping], va: u64, phys: u64, pages: usize) -> usize {
    for (i, m) in maps.iter_mut().take(pages).enumerate() {
        *m = Mapping {
            va: va + i as u64 * FRAME_SIZE,
            phys: phys + i as u64 * FRAME_SIZE,
            flags: Flags::user_data(),
        };
    }
    pages
}

/// How many FS servers one boot can start, and therefore how many banks of poisoned stack pages
/// [`FS_STACK_PHYS`] holds: the ordinary one, and milestone 37's two (the server that is killed
/// mid-transaction, and the one that mounts the disk it left behind).
///
/// They cannot share stack frames. The killed server is still executing its trap when the
/// recovery server starts, so reusing its pages would have one process writing another's stack,
/// and the bug would present as the recovery mount failing for no reason.
const FS_SERVERS: usize = 3;

/// A fresh frame filled with [`STACK_POISON`], for one of the FS server's stack pages, and
/// remembered in [`FS_STACK_PHYS`] so the depth actually reached can be read back afterwards.
fn poisoned_stack_frame(server: usize, index: usize) -> u64 {
    let p = crate::memory::alloc()
        .expect("no frame for the fs server's stack")
        .addr();
    // SAFETY: fresh frame, reachable through the direct map, exactly FRAME_SIZE bytes.
    let words = unsafe {
        core::slice::from_raw_parts_mut(mmu::phys_to_virt(p) as *mut u64, FRAME_SIZE as usize / 8)
    };
    words.fill(STACK_POISON);
    FS_STACK_PHYS[server][index].store(p, core::sync::atomic::Ordering::Relaxed);
    p
}

/// The physical frames behind every FS server's extra stack pages: one bank per server, index 0
/// being the page directly below [`USER_STACK_VA`]. Written once at wiring, read by
/// [`fs_stack_used`]. Zero means "this boot never started that server".
#[allow(clippy::declare_interior_mutable_const)] // an atomic array is exactly what this is for
static FS_STACK_PHYS: [[core::sync::atomic::AtomicU64; FS_STACK_PAGES as usize]; FS_SERVERS] =
    [const { [const { core::sync::atomic::AtomicU64::new(0) }; FS_STACK_PAGES as usize] };
        FS_SERVERS];

/// **How deep the FS server's stack actually went**, in bytes below [`USER_STACK_TOP`], and how
/// much it was given. `None` if this boot wired no FS service.
///
/// Read by scanning the poison: the deepest word that is no longer [`STACK_POISON`] is the
/// deepest the process ever wrote. This is a measurement of the whole run so far, not a sample,
/// because nothing ever un-writes a stack word. The base page `run` maps is counted as fully used
/// (it holds the entry frame and cannot be scanned from here), which is true and is why the
/// number starts at one page.
///
/// The point of it is that a stack size is otherwise a number nobody can defend. The one before
/// this was 528 bytes too small, and the way we found out was a mystery hang.
///
/// The high-water is a **maximum over every FS server this boot started**, which is why
/// milestone 37's recovery mount is covered by it too. A mount that has to walk back a
/// generation is the case most likely to recurse further than a clean one, so it is exactly the
/// case this instrument should be watching.
#[cfg_attr(not(test), allow(dead_code))]
pub fn fs_stack_used() -> Option<(u64, u64)> {
    use core::sync::atomic::Ordering;
    let total = (FS_STACK_PAGES + 1) * FRAME_SIZE;
    let mut deepest = FRAME_SIZE; // the base page, always used
    let mut wired = false;
    for bank in FS_STACK_PHYS.iter() {
        for (i, slot) in bank.iter().enumerate() {
            let phys = slot.load(Ordering::Relaxed);
            if phys == 0 {
                continue;
            }
            wired = true;
            // SAFETY: a frame this module allocated and still owns, via the direct map.
            let words = unsafe {
                core::slice::from_raw_parts(
                    mmu::phys_to_virt(phys) as *const u64,
                    FRAME_SIZE as usize / 8,
                )
            };
            // Page `i` spans [USER_STACK_VA - (i+1)*FRAME, USER_STACK_VA - i*FRAME). Word `w` in
            // it sits that far above the page's base, so a touched word means at least this much
            // depth.
            if let Some(w) = words.iter().position(|&x| x != STACK_POISON) {
                let depth = (i as u64 + 2) * FRAME_SIZE - w as u64 * 8;
                deepest = deepest.max(depth);
            }
        }
    }
    wired.then_some((deepest, total))
}

/// **One boot, one FS service**, remembered here.
///
/// The block server owns the RedoxFS device: a second wiring would put a second driver on the
/// same virtio slot and re-bind its interrupt, so the two client tests (the hand-written
/// `fs_test_client` and the std program) share one wired service instead. Whichever runs first pays
/// for the wiring and receives the two readiness endpoints; the other sees `None` for them,
/// because a readiness sentinel is sent once and has already been taken. Plain atomics rather
/// than a lock: the only writer is the boot/test thread that calls these functions.
static WIRED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static FILE_EP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static FILE_SHARED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The wired service: the file-service endpoint clients `CALL`, the physical frame they share
/// with the FS server, and (only on the call that did the wiring) the block server's and FS
/// server's readiness endpoints.
type Service = (EpId, u64, Option<(EpId, EpId)>);

/// Wire the block server and the FS server if this boot has not already, else hand back what is
/// already running. `None` means no RedoxFS disk is attached.
fn ensure(blk_image: &'static [u8], fs_server_image: &'static [u8]) -> Option<Service> {
    use core::sync::atomic::Ordering;

    if WIRED.load(Ordering::Acquire) {
        return Some((
            FILE_EP.load(Ordering::Relaxed),
            FILE_SHARED.load(Ordering::Relaxed),
            None,
        ));
    }
    let (blk_ready, ready, file_ep, file_shared) = wire_servers(blk_image, fs_server_image)?;
    FILE_EP.store(file_ep, Ordering::Relaxed);
    FILE_SHARED.store(file_shared, Ordering::Relaxed);
    WIRED.store(true, Ordering::Release);
    Some((file_ep, file_shared, Some((blk_ready, ready))))
}

/// Wire and spawn the block server and the FS server. `blk_image` is the driver binary carrying
/// the block-server role (hello on aarch64, `blk` on riscv); `fs_server_image` is the same on
/// both ISAs. Returns `(blk_ready, ready, file_ep, file_shared)`.
fn wire_servers(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
) -> Option<(EpId, EpId, EpId, u64)> {
    let (blk_ep, blk_ready, blk_shared) =
        spawn_block_server(blk_image, crate::virtio::find_block_device_n(1)?);
    let file_shared = file_channel();
    let file_ep = crate::sched::create_endpoint(); // client WRITE (CALL) -> FS server READ
    let ready = crate::sched::create_endpoint(); // FS server WRITE -> the kernel test RECVs
    spawn_fs_server(
        fs_server_image,
        FsServer {
            slot: 0,
            blk_ep,
            blk_shared,
            file_ep,
            file_shared,
            ready,
            budget_pages: FS_BUDGET_PAGES,
            crash: (0, 0, 0),
        },
    );
    Some((blk_ready, ready, file_ep, file_shared))
}

/// **Spawn a block server on one virtio block device.** Extracted from [`wire_servers`] because
/// milestone 37's crash test needs a second one, on its own disk, so that a test which
/// deliberately leaves a filesystem half-written cannot touch the image every other FS test
/// depends on. Returns `(blk_ep, blk_ready, blk_shared)`.
///
/// The DMA region is `1 + BLK_PAGES` contiguous pages: page 0 for the rings, request header and
/// status (block-server-private), and `BLK_PAGES` pages for the data buffer (milestone 138 step 4;
/// it was one page through step 3). Those data pages are ALSO the block region shared with the FS
/// server, so the device DMAs up to `BLK_PAGES` contiguous filesystem blocks straight into the FS
/// server's region in one request, no per-block loop and no copy.
///
/// Bring one virtio-mmio block device up under a confined userspace block server, and hand back the
/// three things a client needs: the request endpoint, the readiness endpoint, and the physical
/// base address of the region the transfers land in.
///
/// `pub(super)` because milestone 57's `disk_service` wires a fourth disk the same way. The FS
/// server is no longer the only thing that wants "a block device, served over IPC, by a process
/// that owns the DMA and nothing else". Its two clients (`disk_surveyor`, `disk_partitioner`) only
/// ever map the first data page and only ever send single-block requests, so they are unmodified
/// by the region's growth, the same compatibility [`fs_proto::blk::TRANSFER_BLOCKS`] documents.
pub(super) fn spawn_block_server(
    blk_image: &'static [u8],
    dev: crate::virtio::VirtioMmioDevice,
) -> (EpId, EpId, u64) {
    let dma = crate::memory::alloc_contiguous(1 + BLK_PAGES)
        .expect("no DMA region for the block server")
        .addr();
    // SAFETY: 1 + BLK_PAGES fresh contiguous frames via the direct map; zero so neither stale
    // descriptors nor stale file bytes are ever visible to the device or the FS server.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(dma) as *mut u8,
            0,
            (1 + BLK_PAGES) * FRAME_SIZE as usize,
        );
    };
    let blk_shared = dma + FRAME_SIZE; // the data pages start right after the rings page

    let blk_ep = crate::sched::create_endpoint(); // FS server WRITE (CALL) -> block server READ
    let blk_ready = crate::sched::create_endpoint(); // block server WRITE -> the kernel test RECVs

    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(dev.intid, irq_ep);
    crate::arch::irq::enable(dev.intid);
    let vid = crate::virtio::register(
        crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        },
        dma,
        (1 + BLK_PAGES) as u64 * FRAME_SIZE, // every page: the device may touch the rings AND the data buffer
        None,                                // virtio-mmio has no IOMMU in front of it
    );
    crate::sched::spawn(move || {
        // The rings page, then the BLK_PAGES data pages, contiguous at DMA_VA.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; 1 + BLK_PAGES];
        maps[0] = Mapping {
            va: DMA_VA,
            phys: dma,
            flags: Flags::user_data(),
        };
        map_channel(&mut maps[1..], DMA_VA + FRAME_SIZE, blk_shared, BLK_PAGES);
        run(
            blk_image,
            Spawn {
                arg0: ROLE_BLK_SERVER,
                arg1: dma, // the DMA region's physical address
                arg2: 0,
                grants: &[
                    endpoint_cap(blk_ep, Rights::READ), // slot 0: RECV blk requests
                    irq_cap(dev.intid),                 // slot 1: the device interrupt
                    virtio_cap(vid),                    // slot 2: the confined transport
                    endpoint_cap(blk_ready, Rights::WRITE), // slot 3: signal readiness once
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the block server");

    (blk_ep, blk_ready, blk_shared)
}

/// Everything one FS-server process is, as a value, because eight positional arguments of which
/// four are bare `u64` endpoint ids is a call nobody can read and a caller can silently get the
/// wrong way round.
struct FsServer {
    /// Which bank of [`FS_STACK_PHYS`] this process's poisoned stack is recorded in. One per
    /// FS server a boot can start, so the high-water instrument covers all of them.
    slot: usize,
    blk_ep: EpId,
    blk_shared: u64,
    file_ep: EpId,
    file_shared: u64,
    ready: EpId,
    /// The untyped budget this server's heap draws from, in frames. The ordinary server gets
    /// [`FS_BUDGET_PAGES`]; milestone 37's two get a fraction of it, because an untyped is
    /// **reserved** rather than merely capped and three 8 MiB reservations do not fit in this
    /// machine's 128 MiB (the first symptom was init failing to get its own budget, several
    /// tests later, which is a long way from the cause). The measured high-water of a real mount
    /// under this allocator is 352 KiB (DECISIONS §27), so [`CRASH_BUDGET_PAGES`] is still five
    /// times the number rather than a guess trimmed until it fit.
    budget_pages: u64,
    /// Milestone 37's crash injection, straight into the process's START arguments:
    /// `(which WRITE request to die in, block writes to allow first, bytes of the last one that
    /// reach the platter)`. All zero disables it, which is every FS server but the crash test's
    /// first one. See `fs_server/src/bin/fs_server.rs`.
    crash: (u64, u64, u64),
}

/// Spawn one FS server: a heap budget, the block-service endpoint (client side), the
/// file-service endpoint (server side), and both shared pages. No device, no DMA.
///
/// It also gets a DEEP stack. `run` maps one stack page (enough for the shallow programs), but
/// RedoxFS recurses through its tree and htree and commits transactions on the stack, and one
/// 4 KiB page overflows immediately (the first `open` faults ~4.2 KiB down). So map extra stack
/// pages below `USER_STACK_VA` out of fresh frames. These are shared-style mappings (not freed on
/// death), a one-time cost per FS server a boot starts.
fn spawn_fs_server(fs_server_image: &'static [u8], cfg: FsServer) {
    let budget =
        crate::untyped::create(cfg.budget_pages).expect("no heap budget for the FS server");
    let mut stack = [0u64; FS_STACK_PAGES as usize];
    for (i, f) in stack.iter_mut().enumerate() {
        *f = poisoned_stack_frame(cfg.slot, i);
    }
    crate::sched::spawn(move || {
        // Build the mapping list: the two shared channels, then the extra stack pages. The FS
        // server maps the whole of both, the one party that must for each: it drives every block
        // the blk channel can carry (up to `fs_proto::blk::TRANSFER_BLOCKS`, milestone 138 step 4)
        // and serves whatever length a client asks for on the file channel, up to
        // `fs_proto::fs::TRANSFER_MAX` (step 3). A client maps only what it uses of the file
        // channel; nothing else maps the blk channel at all.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; BLK_PAGES + FILE_PAGES + FS_STACK_PAGES as usize];
        let n0 = map_channel(&mut maps, BLK_PAGE_FS, cfg.blk_shared, BLK_PAGES);
        let n = n0 + map_channel(&mut maps[n0..], FILE_PAGE_FS, cfg.file_shared, FILE_PAGES);
        for (i, &phys) in stack.iter().enumerate() {
            maps[n + i] = Mapping {
                va: super::USER_STACK_VA - (i as u64 + 1) * FRAME_SIZE,
                phys,
                flags: Flags::user_data(),
            };
        }
        run(
            fs_server_image,
            Spawn {
                arg0: cfg.crash.0,
                arg1: cfg.crash.1,
                arg2: cfg.crash.2,
                grants: &[
                    untyped_cap(budget),                     // slot 0: the heap's untyped budget
                    endpoint_cap(cfg.blk_ep, Rights::WRITE), // slot 1: CALL the block server
                    endpoint_cap(cfg.file_ep, Rights::READ), // slot 2: RECV file requests
                    endpoint_cap(cfg.ready, Rights::WRITE),  // slot 3: signal readiness once
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the FS server");
}

// =======================================================================================
// Milestone 37: the crash test's own service, on its own disk (DECISIONS §34 condition 1)
// =======================================================================================

/// What [`start_crash`] hands back: the two readiness endpoints, and the endpoint the driver
/// reports its acknowledged write on.
///
/// `fs_ready` carries **two** messages in sequence, and that is the design rather than an
/// accident: the FS server's ordinary readiness sentinel when the mount is up, and then
/// `fixture::crash::CUT` from inside the injector, immediately before it traps. The second one
/// is what tells the test the kill was the injector's doing and not something else going wrong,
/// and it is what gives the recovery mount a defined moment to start at instead of a guess.
#[cfg_attr(not(test), allow(dead_code))]
pub struct CrashRun {
    pub blk_ready: EpId,
    pub fs_ready: EpId,
    pub driver_report: EpId,
}

/// The heap budget each of milestone 37's two FS servers draws from. Smaller than
/// [`FS_BUDGET_PAGES`] on purpose: an untyped is a reservation, and three 8 MiB ones do not fit
/// beside everything else this boot builds in 128 MiB. 2 MiB is five times the 352 KiB
/// high-water a real mount under this allocator actually reaches (DECISIONS §27's measurement),
/// and the crash workload is one open and two 66-byte writes.
const CRASH_BUDGET_PAGES: u64 = 512;

/// The crash disk's block-service endpoint and shared block page, remembered between the two
/// halves of the test: the recovery server is a **different process** on the **same** block
/// server, which is endpoint-only naming doing its job. The block server never learns that its
/// client died and was replaced, because it never knew who its client was.
static CRASH_BLK_EP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CRASH_BLK_SHARED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// **Phase one: wire a filesystem on its own disk and kill it mid-transaction** (milestone 37).
///
/// Three processes on virtio block device 2, which nothing else in the boot touches. The disk is
/// dedicated on purpose: this test deliberately leaves a filesystem half-written, and pointing it
/// at the shared fixture would couple every later test to whether this one ran first. DECISIONS
/// §27 records what an order-coupled gate costs (three investigations, three incompatible root
/// causes, none of them real), so the fixture is regenerated per run and owned by one test.
///
/// The FS server is spawned **armed**: die one block write into the second `WRITE` request, with
/// that block torn in half. One is the count that cannot miss, because a write transaction always
/// issues at least one block write and a larger count is a server that never dies and a test that
/// hangs.
#[cfg_attr(not(test), allow(dead_code))]
pub fn start_crash(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    client_image: &'static [u8],
) -> Option<CrashRun> {
    use core::sync::atomic::Ordering;
    let (blk_ep, blk_ready, blk_shared) =
        spawn_block_server(blk_image, crate::virtio::find_block_device_n(2)?);
    CRASH_BLK_EP.store(blk_ep, Ordering::Relaxed);
    CRASH_BLK_SHARED.store(blk_shared, Ordering::Relaxed);

    let file_shared = file_channel();
    let file_ep = crate::sched::create_endpoint();
    let fs_ready = crate::sched::create_endpoint();
    spawn_fs_server(
        fs_server_image,
        FsServer {
            slot: 1,
            blk_ep,
            blk_shared,
            file_ep,
            file_shared,
            ready: fs_ready,
            budget_pages: CRASH_BUDGET_PAGES,
            // Die one block write into the SECOND write request, tearing that block at 2048
            // bytes. The first write is acknowledged and must survive; the second is the one the
            // property is about.
            crash: (2, 1, 2048),
        },
    );

    let driver_report = spawn_fs_client(client_image, file_ep, file_shared, 3, 0, 0, 0);
    Some(CrashRun {
        blk_ready,
        fs_ready,
        driver_report,
    })
}

/// **Phase two: mount the disk the dead server left behind** (milestone 37).
///
/// A fresh FS-server process, on the same block server and the same block page, with its own file
/// endpoint, its own file page and its own stack. It carries nothing over from the process that
/// died: what it recovers, it recovers from the platter, through the ordinary `Server::open` every
/// FS server in this system mounts with. Its readiness sentinel arriving at all is the first half
/// of the result, because that open fails outright on an image it cannot make sense of.
///
/// Returns `(ready, report)`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn recover_crash(fs_server_image: &'static [u8], client_image: &'static [u8]) -> (EpId, EpId) {
    use core::sync::atomic::Ordering;
    let file_shared = file_channel();
    let file_ep = crate::sched::create_endpoint();
    let ready = crate::sched::create_endpoint();
    spawn_fs_server(
        fs_server_image,
        FsServer {
            slot: 2,
            blk_ep: CRASH_BLK_EP.load(Ordering::Relaxed),
            blk_shared: CRASH_BLK_SHARED.load(Ordering::Relaxed),
            file_ep,
            file_shared,
            ready,
            budget_pages: CRASH_BUDGET_PAGES,
            crash: (0, 0, 0), // this one is not armed: it is the one that has to survive
        },
    );
    let report = spawn_fs_client(client_image, file_ep, file_shared, 4, 0, 0, 0);
    (ready, report)
}

/// The most extra stack pages [`spawn_fs_client`] will map below the one `run` gives every
/// program. Small on purpose: a client that needs more than this is a client whose frames want
/// looking at, not a number that wants raising.
const CLIENT_EXTRA_STACK: usize = 8;

/// Spawn one client holding exactly a file-service endpoint, a report endpoint and its view of
/// the shared page. Returns the report endpoint.
///
/// `extra_stack` is pages **below** the single one `run` maps. The hand-written `fs_test_client` roles
/// need none: they hold a handle and a small buffer. The navigating shell (milestone 47's
/// commands) needs some, and the number is a measurement rather than a guess: with none it
/// overflowed by 192 bytes, which presented as a data abort on its own `sp` and then as the
/// 60-second lost-wakeup watchdog, because the kernel test was still waiting for a report from a
/// process that had died. A shell carries a path stack (eight levels of name), a parsed path,
/// and a listing buffer, all by value, so 4 KiB is genuinely tight for it. Same discipline as
/// the FS server's stack (DECISIONS §27): sized by what it did, not by what looks generous.
///
/// **Milestone 50 moved it again, the same way.** `Endowment` grew a sink and a source, each of
/// which can carry a whole `FileGrant`, and the shell now holds one endowment per pipeline
/// stage; four extra pages overflowed by 48 bytes at the same `sp` and with the same symptom.
/// Recorded because it will happen a third time: the shell's stack is sized by the largest value
/// `grant_plan` hands it, so a field added there is a page needed here.
fn spawn_fs_client(
    client_image: &'static [u8],
    file_ep: EpId,
    file_shared: u64,
    role: u64,
    arg: u64,
    arg2: u64,
    extra_stack: usize,
) -> EpId {
    assert!(
        extra_stack <= CLIENT_EXTRA_STACK,
        "an FS client asked for more stack than this wiring maps",
    );
    let report = crate::sched::create_endpoint();
    crate::sched::spawn(move || {
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; FILE_PAGES + CLIENT_EXTRA_STACK];
        // The whole channel, because this is the spawn the throughput benchmark comes through and
        // a client may not ask for more than it mapped (`fs_proto::fs::TRANSFER_PAGES`). It costs
        // fifteen extra page-table entries against the same frames, not fifteen extra frames.
        let n = map_channel(&mut maps, FILE_VA_CLIENT, file_shared, FILE_PAGES);
        for (k, m) in maps[n..n + extra_stack].iter_mut().enumerate() {
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = frame();
        }
        run(
            client_image,
            Spawn {
                arg0: role,
                arg1: arg,
                // A third word, because the `rm` program is started the way a **grant** is
                // (`fs_proto::grant`): a spec word and two words of name. Every other client
                // here takes a role and one number and leaves this zero.
                arg2,
                grants: &[
                    endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                    endpoint_cap(report, Rights::WRITE),  // slot 1: report to the kernel
                ],
                maps: &maps[..n + extra_stack],
            },
        )
    })
    .expect("could not spawn the FS client");
    report
}

/// Wire the service (or reuse this boot's) and spawn the hand-written client
/// (`user/src/fs_test_client.rs`): the file-service endpoint, which IS its directory capability, the
/// report endpoint, and its view of the shared file page. It names nothing else in the system.
///
/// Returns `(readiness, report)`: the two readiness endpoints if this call wired the service,
/// and the endpoint the client reports on.
pub fn start(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    client_image: &'static [u8],
    client_role: u64,
) -> Option<(Option<(EpId, EpId)>, EpId)> {
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    // 0 = the end-to-end proof; 1 = the fs_read benchmark loop.
    let report = spawn_fs_client(client_image, file_ep, file_shared, client_role, 0, 0, 0);
    Some((readiness, report))
}

/// **Wire a per-file grant and the program that holds it** (milestone 31 phase 2,
/// notes/grant-expression.md). This is what `run wc report.txt` resolves to, wired by the
/// kernel's test suite instead of by the shell, so the mechanism is gated on both ISAs.
///
/// Three processes, and the shape is the point:
///
/// ```text
///   FS server ──file IPC──► fs_file_caretaker ──narrowed file IPC──► the confined program
///                (a directory)          (one file, one direction)
/// ```
///
/// The caretaker holds the directory capability. The program holds an endpoint to the caretaker
/// and **nothing that names the FS server**, so "it cannot reach a second file" is a statement
/// about its cspace, not about a check it is trusted to pass. The narrowing is an address
/// space, which is why the attacker test below is a witness rather than an assertion.
///
/// One frame is shared by all three. Every request on both hops is a blocking `CALL`, so the
/// client is parked inside its own call for the whole time the caretaker is using the page; a
/// second frame would buy a copy and no isolation, since the client is entitled to the bytes
/// either way.
///
/// The grant, as one value, because its four fields are one decision: which file, in which
/// direction, handed to which program started how. Splitting them across a long argument list
/// invites a caller to get `rights` and `role` the wrong way round, and both are bare integers.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Grant {
    /// The one name the caretaker will answer for. Must fit [`fs_proto::grant::MAX_NAME`].
    pub name: &'static str,
    /// `grant::READ`, or `READ | WRITE`.
    pub rights: u64,
    /// The confined program's `arg0` (its role) and `arg1`.
    pub role: u64,
    pub arg: u64,
}

/// **Drain the FS service's two readiness sentinels**, which is a sequencing act and not only an
/// assertion.
///
/// Both servers announce with a blocking `SEND`, so each is parked inside its own announcement
/// until somebody receives it. Nothing they serve can be answered before that. Every caller that
/// needs the service to be *running* rather than merely spawned has to come through here first,
/// and [`wait_for_caretaker`] documents what that ordering is load-bearing for.
///
/// `None` means an earlier caller in this boot already wired the service and drained these.
#[cfg_attr(not(test), allow(dead_code))]
pub fn wait_for_service(readiness: Option<(EpId, EpId)>) {
    let Some((blk_ready, fs_ready)) = readiness else {
        return;
    };
    assert_eq!(
        crate::sched::ipc_recv(blk_ready)[0],
        fs_proto::fixture::READY,
        "the block server did not bring the RedoxFS device up",
    );
    assert_eq!(
        crate::sched::ipc_recv(fs_ready)[0],
        fs_proto::fixture::READY,
        "the FS server did not open the RedoxFS image",
    );
}

/// **Wait for a caretaker's startup request to have been answered, before the program it
/// confines exists at all.**
///
/// This is a correction, and the bug it fixes is worth stating where it was made rather than
/// only in the note. All three processes share **one frame** (the module comment above argues
/// that is sound because every request on both hops is a blocking `CALL`, so the client is
/// parked inside its own call for the whole time the caretaker is using the page). That
/// argument holds once the caretaker is serving. It does not hold at **startup**, where the
/// caretaker stages the granted name in the page and then blocks in a `CALL` to the FS server:
/// a confined program that already exists writes its own first name over that page, and the FS
/// server resolves whatever it finds there.
///
/// It is not even a race in the common case. When this call is the one that wires the service,
/// the FS server is parked inside its readiness `SEND`, so the caretaker's descent cannot be
/// answered until [`wait_for_service`] drains it, and the client has that whole window to
/// clobber the page. The caretaker then dies (it will not serve a hole) and its client blocks
/// forever on a call nobody will answer, which is how this presented: a userspace `ebreak`
/// followed by the 60 s lost-wakeup watchdog. It passed on aarch64 and failed on riscv, which
/// is the shape of a timing bug and was one.
///
/// The fix is ordering, not a second page: drain the service, wait for the caretaker's own
/// sentinel, and only then spawn the confined program.
#[cfg_attr(not(test), allow(dead_code))]
fn wait_for_caretaker(caretaker_ready: EpId) {
    assert_eq!(
        crate::sched::ipc_recv(caretaker_ready)[0],
        fs_proto::fixture::READY,
        "the caretaker could not open what it was granted, so there is nothing to attenuate",
    );
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn start_granted(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    caretaker_image: &'static [u8],
    client_image: &'static [u8],
    grant: Grant,
) -> Option<EpId> {
    let Grant {
        name,
        rights,
        role: client_role,
        arg: client_arg,
    } = grant;
    assert!(
        fs_proto::grant::fits(name.as_bytes()),
        "a granted name rides in two argument words; this one does not fit",
    );
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    let narrow_ep = crate::sched::create_endpoint();
    let caretaker_ready = crate::sched::create_endpoint();

    let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
    let spec = fs_proto::grant::spec(name.len(), rights);

    // The caretaker: the directory capability, the narrowed endpoint it serves, and the shared
    // page. The grant itself (the name and the direction) rides in its START arguments, so a
    // per-file grant costs no frame at all.
    crate::sched::spawn(move || {
        run(
            caretaker_image,
            Spawn {
                arg0: lo,
                arg1: hi,
                arg2: spec,
                grants: &[
                    endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                    endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                    endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                ],
                maps: &[Mapping {
                    va: FILE_VA_CLIENT,
                    phys: file_shared,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the file caretaker");

    // Both handshakes before the client exists, in this order and for [`wait_for_caretaker`]'s
    // reason: the servers are parked inside their announcements until they are drained, and the
    // caretaker's own request is staged in the page all three share.
    wait_for_service(readiness);
    wait_for_caretaker(caretaker_ready);

    // The confined program. Its slot 0 looks exactly like a directory capability from inside, and
    // is not one: same protocol, same page, a namespace of one name.
    Some(spawn_fs_client(
        client_image,
        narrow_ep,
        file_shared,
        client_role,
        client_arg,
        0,
        0,
    ))
}

/// **Wire a per-directory grant and the program confined to it** (milestone 47,
/// notes/dir-capability.md). The same three-process shape [`start_granted`] wires, one rung up:
///
/// ```text
///   FS server ──file IPC──► fs_subtree_caretaker ──narrowed file IPC──► the confined program
///            (the image root)         (one subtree, one rights set)
/// ```
///
/// The caretaker holds the root directory capability and the confined program holds an endpoint
/// to the caretaker and **nothing that names the FS server**, which is what makes "it cannot
/// reach the parent directory or a sibling" a statement about its cspace rather than about a
/// branch. That argument is `fs_file_caretaker`'s, and it is load-bearing here for an extra
/// reason: the FS server's handle table is per *server*, so a rights-carrying handle on its own
/// would not confine a program that could still name [`fs_proto::fs::ROOT`].
///
/// `rights` is a [`fs_proto::dir`] mask. It is what the caretaker *asks* for; the FS server
/// intersects it with the root's and refuses if the answer is smaller, so a wiring that asked
/// for more than exists fails at the caretaker's first request rather than silently serving
/// less.
#[cfg_attr(not(test), allow(dead_code))]
pub struct DirGrant {
    /// The directory the caretaker descends into, one component under the image root. Must fit
    /// [`fs_proto::grant::MAX_NAME`].
    pub name: &'static str,
    /// The [`fs_proto::dir`] rights the subtree capability is to carry.
    pub rights: u64,
    /// The confined program's `arg0` (its role) and `arg1`.
    pub role: u64,
    pub arg: u64,
    /// Its `arg2`. Zero for every client that takes a role and a number; the `rm` program is
    /// started with a **grant's** three words instead (a spec and two of name), so it uses all
    /// three and its "role" word is the spec.
    pub arg2: u64,
    /// Stack pages beyond the one `run` maps, for a confined program that needs them. The
    /// hand-written attacker needs none; a shell does (see [`spawn_fs_client`]).
    pub stack_pages: usize,
}

/// **The binary carrying the block server's role**, which is the one thing the two ISAs
/// disagree about here: on aarch64 it is a role of the `init`/hello binary, on riscv the
/// dedicated `blk` one. Every caller goes through this so the disagreement is one `cfg` rather
/// than a second copy of every wiring.
///
/// It panics rather than returning `None` because a boot archive without it is a build that did
/// not finish, not a machine without a disk; the disk's absence is [`root_directory`]'s `None`.
pub fn blk_server_image() -> &'static [u8] {
    #[cfg(target_arch = "aarch64")]
    return program("init").expect("no init program in the initrd archive");
    #[cfg(target_arch = "riscv64")]
    return program("blk").expect("no blk program in the initrd archive");
}

/// **Wire the filesystem and hand back the root directory capability**, for a boot rather than
/// for a test (milestone 50).
///
/// This is the one entry point the interactive boot uses. It brings up the block server and the
/// FS server, drains both readiness sentinels (so the service is *running* and not merely
/// spawned by the time init exists), and returns `(the file-service endpoint, the physical frame
/// its clients map)`. `None` means no RedoxFS disk is attached to this run, which is the normal
/// case for a plain `cargo xtask run`, and every caller treats it as "this boot has no
/// filesystem" rather than as an error.
///
/// The endpoint **is** the directory capability (DECISIONS §27), rooted at the image root. A
/// boot hands it to the shell unnarrowed on purpose: it is the machine's own prompt, and the
/// interesting confinement claims are about what the shell then hands to the programs it spawns.
#[cfg_attr(not(test), allow(dead_code))]
pub fn root_directory(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
) -> Option<(EpId, u64)> {
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    wait_for_service(readiness);
    Some((file_ep, file_shared))
}

/// **Wire a subtree caretaker and hand back the narrowed capability**, without spawning
/// whatever is going to hold it.
///
/// [`start_granted_dir`]'s first half, split out because milestone 50's redirection witness is
/// a shell that holds a directory **and** a terminal, a spawn channel and a budget, so its
/// client half is nothing like [`spawn_fs_client`]'s. What it shares with every other confined
/// program is exactly what is here: a caretaker between it and the FS server, and the page they
/// both map.
///
/// Returns `(narrow_ep, file_shared)`, with both handshakes already drained for
/// [`wait_for_caretaker`]'s reason, so a caller may spawn its client the moment this returns.
#[cfg_attr(not(test), allow(dead_code))]
pub fn narrow_dir(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    caretaker_image: &'static [u8],
    name: &'static str,
    rights: u64,
) -> Option<(EpId, u64)> {
    assert!(
        fs_proto::grant::fits(name.as_bytes()),
        "a granted name rides in two argument words; this one does not fit",
    );
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    let narrow_ep = crate::sched::create_endpoint();
    let caretaker_ready = crate::sched::create_endpoint();

    let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
    let spec = fs_proto::grant::spec(name.len(), rights);

    crate::sched::spawn(move || {
        run(
            caretaker_image,
            Spawn {
                arg0: lo,
                arg1: hi,
                arg2: spec,
                grants: &[
                    endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                    endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                    endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                ],
                maps: &[Mapping {
                    va: FILE_VA_CLIENT,
                    phys: file_shared,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the subtree caretaker");

    // Both handshakes before the client exists. See [`wait_for_caretaker`]: this is the call
    // that found the bug, because a caretaker whose descent is answered only after the service
    // is drained gives its client an unbounded window to write over the staged name.
    wait_for_service(readiness);
    wait_for_caretaker(caretaker_ready);
    Some((narrow_ep, file_shared))
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn start_granted_dir(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    caretaker_image: &'static [u8],
    client_image: &'static [u8],
    grant: DirGrant,
) -> Option<EpId> {
    let DirGrant {
        name,
        rights,
        role: client_role,
        arg: client_arg,
        arg2: client_arg2,
        stack_pages,
    } = grant;
    let (narrow_ep, file_shared) =
        narrow_dir(blk_image, fs_server_image, caretaker_image, name, rights)?;

    // The confined program. Its slot 0 looks exactly like a directory capability from inside,
    // and is one: the same protocol, the same page, a namespace of one subtree.
    Some(spawn_fs_client(
        client_image,
        narrow_ep,
        file_shared,
        client_role,
        client_arg,
        client_arg2,
        stack_pages,
    ))
}

/// Where the nameset caretaker expects its read-only name-set page
/// (`user/src/fs_nameset_caretaker.rs`'s `SET_VA`).
const SET_VA_CARETAKER: u64 = 0x0000_0000_0070_0000;

/// **Wire a set grant and the program confined to it** (milestone 47's globbing lane,
/// notes/glob-grant.md). [`start_granted_dir`]'s shape with a narrower namespace:
///
/// ```text
///   FS server ──file IPC──► fs_nameset_caretaker ──narrowed file IPC──► the confined program
///            (the image root)         (one directory, and only the names in the set)
/// ```
///
/// The interesting difference from every other grant here is **where the grant lives**. A name
/// rides in two `START` argument words; a set does not fit in any number of registers, so it is
/// encoded into a **frame of its own and mapped read-only** into the caretaker, which copies it
/// into a local before it does anything else. That is the honest place for `ARG_MAX` to
/// reappear: it is the size of a capability now, not the size of a buffer, and it is bounded by
/// `fs_proto::nameset::MAX_NAMES` at both ends.
///
/// The set is written **before the caretaker is spawned**, into a frame nothing else has ever
/// been handed, which is why it needs none of [`wait_for_caretaker`]'s ordering care: unlike
/// the shared page, no client can reach it at all.
#[cfg_attr(not(test), allow(dead_code))]
pub struct SetGrant<'a> {
    /// The directory the caretaker descends into, one component under the image root. The set's
    /// names are the names *in* it that the grant designates.
    pub dir: &'static str,
    /// The set, as `(name, is_dir)`: what the shell's expansion produced. At most
    /// `fs_proto::nameset::MAX_NAMES` of them, and an over-long set is a panic here because the
    /// shell refuses it at the prompt (`grant_plan::Refusal::TooManyNames`), so one arriving means
    /// the wiring built a grant no command line could have expressed.
    pub names: &'a [(&'a [u8], bool)],
    /// The [`fs_proto::dir`] rights the caretaker asks for on its descent.
    pub rights: u64,
    /// The confined program's three `START` words. `rm` is started with a grant's spec and two
    /// name words rather than a role and a number; see [`DirGrant`].
    pub role: u64,
    pub arg: u64,
    pub arg2: u64,
    /// Stack pages beyond the one `run` maps.
    pub stack_pages: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn start_granted_set(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    caretaker_image: &'static [u8],
    client_image: &'static [u8],
    grant: SetGrant<'_>,
) -> Option<EpId> {
    let SetGrant {
        dir,
        names,
        rights,
        role: client_role,
        arg: client_arg,
        arg2: client_arg2,
        stack_pages,
    } = grant;
    assert!(
        fs_proto::grant::fits(dir.as_bytes()),
        "a granted directory's name rides in two argument words; this one does not fit",
    );
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    let narrow_ep = crate::sched::create_endpoint();
    let caretaker_ready = crate::sched::create_endpoint();

    // The set, encoded into its own frame before anything can see it. `encode` refuses rather
    // than truncating, and a truncated set would be a capability nobody planned.
    let set_phys = frame();
    let mut encoded = [0u8; fs_proto::nameset::BYTES];
    let n = fs_proto::nameset::encode(names, &mut encoded)
        .expect("this set does not fit one grant, so no command line could have named it");
    // SAFETY: a fresh frame of FRAME_SIZE bytes reachable through the direct map, and `n` is at
    // most `nameset::BYTES`, which is far smaller.
    unsafe {
        core::ptr::copy_nonoverlapping(encoded.as_ptr(), mmu::phys_to_virt(set_phys) as *mut u8, n);
    };

    let (lo, hi) = fs_proto::grant::pack_name(dir.as_bytes());
    let spec = fs_proto::grant::spec(dir.len(), rights);

    crate::sched::spawn(move || {
        run(
            caretaker_image,
            Spawn {
                arg0: lo,
                arg1: hi,
                arg2: spec,
                grants: &[
                    endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                    endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                    endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                ],
                maps: &[
                    Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    },
                    // **Read-only, and that is not decoration.** The set is the namespace this
                    // process serves; a writable mapping would let the one program that must not
                    // be able to widen its own grant do exactly that.
                    Mapping {
                        va: SET_VA_CARETAKER,
                        phys: set_phys,
                        flags: Flags::user_rodata(),
                    },
                ],
            },
        )
    })
    .expect("could not spawn the nameset caretaker");

    wait_for_service(readiness);
    wait_for_caretaker(caretaker_ready);

    Some(spawn_fs_client(
        client_image,
        narrow_ep,
        file_shared,
        client_role,
        client_arg,
        client_arg2,
        stack_pages,
    ))
}

/// **Put a file behind a byte sink** (milestone 50, notes/sink-protocol.md).
///
/// Wires the FS service (or reuses this boot's) and spawns `user/src/sink.rs` in its file role:
/// it holds the FS-service endpoint, a report endpoint, and the page it shares with the FS
/// server, and it serves one endpoint whose only expressible request is "append these bytes".
///
/// `sink` is the capability a program's output slot gets, and **that endpoint is the whole of
/// what the writer holds**, which is the property the milestone rests on: it is created here
/// and handed out with `WRITE` to whoever is redirected and `READ` to the sink.
#[cfg_attr(not(test), allow(dead_code))]
pub struct FileSink {
    /// The FS service's two readiness endpoints, if this call is the one that wired it.
    pub readiness: Option<(EpId, EpId)>,
    /// The byte sink. Goes into a program's output slot with `WRITE`.
    pub sink: EpId,
    /// Readiness first, then `DONE` and the byte total at end of stream.
    pub report: EpId,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn start_file_sink(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    sink_image: &'static [u8],
) -> Option<FileSink> {
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    let sink = crate::sched::create_endpoint();
    let report = crate::sched::create_endpoint();
    crate::sched::spawn(move || {
        run(
            sink_image,
            Spawn {
                arg0: SINK_ROLE_FILE,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(sink, Rights::READ), // slot 0: the byte sink it serves
                    endpoint_cap(file_ep, Rights::WRITE), // slot 1: CALL the FS server
                    endpoint_cap(report, Rights::WRITE), // slot 2: readiness and the total
                ],
                maps: &[Mapping {
                    va: FILE_VA_CLIENT,
                    phys: file_shared,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the file sink");
    Some(FileSink {
        readiness,
        sink,
        report,
    })
}

/// **Read back what the file sink wrote**, in a different process with a different FS session.
///
/// Spawned only after the sink has reported that it closed the file, because the two share the
/// FS server's one file page (the [`wait_for_caretaker`] lesson: one page is sound between
/// parties that are never using it at once, and sequencing is what makes that true).
///
/// It streams the file's contents out **over the sink contract**, so the bytes that reach the
/// test arrive in the same sixteen-byte framing a `println!` does. Returns `(out, report)`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn start_sink_verify(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    sink_image: &'static [u8],
) -> Option<(EpId, EpId)> {
    let (file_ep, file_shared, _) = ensure(blk_image, fs_server_image)?;
    let out = crate::sched::create_endpoint();
    let report = crate::sched::create_endpoint();
    crate::sched::spawn(move || {
        run(
            sink_image,
            Spawn {
                arg0: SINK_ROLE_VERIFY,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(out, Rights::WRITE), // slot 0: where the file's bytes go
                    endpoint_cap(file_ep, Rights::WRITE), // slot 1: CALL the FS server
                    endpoint_cap(report, Rights::WRITE), // slot 2: the size it found
                ],
                maps: &[Mapping {
                    va: FILE_VA_CLIENT,
                    phys: file_shared,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the sink verifier");
    Some((out, report))
}

/// `user/src/sink.rs`'s roles. Kept in sync with that file by name and by this comment; a
/// mismatch spawns the wrong role and hangs, which is why they are named here rather than
/// spelled as bare integers at the two call sites.
const SINK_ROLE_FILE: u64 = 1;
const SINK_ROLE_VERIFY: u64 = 2;

/// The `std::fs` client's heap budget and extra stack. Same magnitudes as the networked std
/// program: it is a full std program (formatting, `Vec`, `String`, `read_to_string`), so it
/// needs the generous heap and the deep stack std's machinery wants.
const STD_FS_HEAP_PAGES: u64 = 256;
const STD_FS_STACK_PAGES: u64 = 32;

/// **Wire the service and endow a std program with a directory capability** (milestone 27 phase
/// two, the FS half).
///
/// This is the one spawn site that makes `std::fs` work: an ordinary std ELF, given the std slot
/// convention (heap untyped at 0, stdout at 1) **plus the FS-service endpoint at slot 4** and
/// the page it shares with the FS server, mapped at the VA the PAL expects
/// (`sys/pal/nife/rt.rs::FS_PAGE`). Slots 2 and 3 are deliberately left EMPTY, which is why
/// the grants go in by explicit slot instead of in order: this program holds a filesystem and no
/// network, and `std::net` must be able to tell.
///
/// The same binary spawned without slot 4 gets `Unsupported` from every `std::fs` call. That is
/// the whole point: the code never chose to have a filesystem, its cspace did.
///
/// Returns `(readiness, stdout)`: the readiness endpoints if this call wired the service, and
/// the program's stdout endpoint for the test to reassemble.
pub fn start_std(
    blk_image: &'static [u8],
    fs_server_image: &'static [u8],
    std_image: &'static [u8],
) -> Option<(Option<(EpId, EpId)>, EpId)> {
    let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
    let report = crate::sched::create_endpoint();
    let heap = crate::untyped::create(STD_FS_HEAP_PAGES).expect("no untyped for the std fs heap");

    // The shared file page, then the deep stack std needs. `run` maps one stack page; std's
    // startup and formatting overflow it immediately, the same reason the other std spawns map
    // extra pages below it.
    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; 1 + STD_FS_STACK_PAGES as usize];
    maps[0] = Mapping {
        va: FS_PAGE_STD,
        phys: file_shared,
        flags: Flags::user_data(),
    };
    for (k, m) in maps[1..].iter_mut().enumerate() {
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = frame();
    }

    crate::sched::spawn(move || {
        // The directory capability goes in at its named slot BEFORE `run` grants in order, so
        // `run`'s two grants land at 0 and 1 and slots 2 and 3 stay empty. See `grant_at`.
        crate::sched::grant_at(FS_DIR_SLOT, endpoint_cap(file_ep, Rights::WRITE))
            .expect("the std fs slot was already occupied");
        run(
            std_image,
            Spawn {
                arg0: 0,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(heap),                   // slot 0: the heap's budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the std fs program");

    Some((readiness, report))
}
