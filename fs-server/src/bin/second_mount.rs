//! **The second-mount repro, on the host** (fix/redoxfs-second-mount).
//!
//! A second mount of a *used* RedoxFS image fails its write on device. The open question is why, and
//! the recorded hypothesis is accumulated mount state driving the FS server past its 8 MiB heap cap.
//! This binary settles it without an emulator: it runs the real engine under the **same allocator the
//! FS server uses** (`uheap`, the algorithm behind `user_rt::heap::UntypedHeap`), grown incrementally
//! and capped exactly the way `fsserver.rs` caps it, and it does the two mounts in one process:
//!
//! 1. create the fixture image, then mount it and write (this is what makes the image *used*),
//! 2. drop the mount the way a dying process does (no unmount), then **mount again and write**.
//!
//! The disk image lives in a `static`, off the heap, because on device the image is on a disk reached
//! over blk IPC and never occupies the server's heap. Keeping it out is what makes the cap mean the
//! same thing here as there.
//!
//! The cap is the experiment's dial:
//!
//! ```text
//! cargo run --manifest-path fs-server/Cargo.toml --bin second_mount            # 8 MiB, the device cap
//! CRICKER_HEAP_MIB=64 cargo run --manifest-path fs-server/Cargo.toml --bin second_mount
//! ```
//!
//! If mount 2's write fails at 8 MiB and succeeds at 64, the failure is heap exhaustion and the
//! hypothesis holds. If it fails at both, or succeeds at both, the hypothesis is dead and the note
//! must say so instead of carrying a plausible-sounding guess.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use fs_server::{BLOCK, BlockDisk, BlockIo, Server};
use redoxfs::{FileSystem, Node, TreePtr};

/// The pool the capped allocator draws from. Large enough to serve any cap we dial in; the *cap*,
/// not the pool, is what limits the heap. BSS, so it costs nothing in the binary.
const POOL_SIZE: usize = 512 * 1024 * 1024;
static mut POOL: [u8; POOL_SIZE] = [0; POOL_SIZE];

const PAGE: usize = 4096;
/// The FS server's growth floor (`user_rt::heap::MIN_GROW_PAGES`).
const MIN_GROW_PAGES: usize = 8;

/// The heap cap in bytes, from `CRICKER_HEAP_MIB`, defaulting to the FS server's 8 MiB
/// (`fsserver.rs::HEAP_MAX`, which `FS_BUDGET_PAGES` in kernel/src/user.rs matches).
fn heap_cap() -> usize {
    std::env::var("CRICKER_HEAP_MIB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(8)
        * 1024
        * 1024
}

/// High-water mark of bytes the allocator has committed, so the run can report how close it came to
/// the cap. This is the number that decides the hypothesis.
static COMMITTED: AtomicUsize = AtomicUsize::new(0);
/// Set when an allocation fails because the cap refused to grow: the signature of exhaustion.
static HIT_CAP: AtomicBool = AtomicBool::new(false);

struct Inner {
    heap: uheap::Heap,
    base: usize,
    committed: usize,
    cap: usize,
}

impl Inner {
    /// Grow by at least `need` bytes, geometrically, bounded by the cap. A faithful copy of
    /// `user_rt::heap::UntypedHeap::grow`, so exhaustion happens here for the same reasons.
    fn grow(&mut self, need: usize) -> bool {
        if self.base == 0 {
            // uheap requires 16-aligned donations; a static [u8; N] is only byte-aligned.
            let raw = &raw mut POOL as *mut u8 as usize;
            self.base = (raw + 15) & !15;
            self.cap = heap_cap();
        }
        let need_pages = need.div_ceil(PAGE);
        let want_pages = need_pages
            .max(self.committed / PAGE)
            .max(MIN_GROW_PAGES)
            .min((self.cap - self.committed) / PAGE);
        if want_pages < need_pages {
            HIT_CAP.store(true, Ordering::Relaxed);
            return false; // the cap is closer than the request; growing cannot help
        }
        let start = self.base + self.committed;
        self.committed += want_pages * PAGE;
        COMMITTED.store(self.committed, Ordering::Relaxed);
        // SAFETY: this slice of the pool is donated once, never freed, 16-aligned and page-sized.
        unsafe { self.heap.add_region(start as *mut u8, want_pages * PAGE) };
        true
    }
}

struct CappedHeap {
    locked: AtomicBool,
    inner: UnsafeCell<Inner>,
}
// SAFETY: every access to `inner` is behind the `locked` spinlock.
unsafe impl Sync for CappedHeap {}

impl CappedHeap {
    const fn new() -> Self {
        CappedHeap {
            locked: AtomicBool::new(false),
            inner: UnsafeCell::new(Inner {
                heap: uheap::Heap::new(),
                base: 0,
                committed: 0,
                cap: 0,
            }),
        }
    }
    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }
    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

// SAFETY: forwards the GlobalAlloc contract to uheap under the lock; the pool never moves.
unsafe impl GlobalAlloc for CappedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.lock();
        // SAFETY: exclusive under the lock.
        let inner = unsafe { &mut *self.inner.get() };
        let p = loop {
            if let Some(p) = inner.heap.alloc(layout) {
                break p.as_ptr();
            }
            let slack = layout.align().saturating_sub(PAGE);
            let need = uheap::effective_size(layout) + slack;
            if !inner.grow(need) {
                break std::ptr::null_mut(); // OOM: std's handler aborts, as on device
            }
        };
        self.unlock();
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(p) = std::ptr::NonNull::new(ptr) else {
            return;
        };
        self.lock();
        // SAFETY: exclusive under the lock; forwarded GlobalAlloc contract.
        unsafe { (*self.inner.get()).heap.dealloc(p, layout) };
        self.unlock();
    }
}

#[global_allocator]
static HEAP: CappedHeap = CappedHeap::new();

/// The image, in a `static` and therefore OFF the heap under test, because on device the image is on
/// a disk and never sits in the FS server's heap.
const IMAGE_SIZE: usize = 16 * 1024 * 1024;
static mut IMAGE: [u8; IMAGE_SIZE] = [0; IMAGE_SIZE];

/// A `BlockIo` over the static image. Both mounts address the same bytes, exactly as they would the
/// same disk across two boots.
struct StaticImage;
impl StaticImage {
    fn bytes() -> &'static mut [u8] {
        // SAFETY: single-threaded repro; the only accessor of IMAGE.
        unsafe { &mut *&raw mut IMAGE }
    }
}
impl BlockIo for StaticImage {
    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> syscall::error::Result<()> {
        let off = block as usize * BLOCK;
        buf.copy_from_slice(&Self::bytes()[off..off + BLOCK]);
        Ok(())
    }
    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK]) -> syscall::error::Result<()> {
        let off = block as usize * BLOCK;
        Self::bytes()[off..off + BLOCK].copy_from_slice(buf);
        Ok(())
    }
    fn size_bytes(&mut self) -> syscall::error::Result<u64> {
        Ok(IMAGE_SIZE as u64)
    }
}

/// Print without allocating, so a report still comes out when the heap is exhausted.
fn mark(s: &str) {
    use std::io::Write;
    let mut e = std::io::stderr();
    let _ = e.write_all(s.as_bytes());
    let _ = e.write_all(b"\n");
    let _ = e.flush();
}

/// The payload every write in this repro uses: same length each time, so a write replaces the last
/// completely (there is no truncate verb), and position-dependent so a stale read cannot match.
fn payload(pass: u8) -> [u8; 61] {
    let mut p = [0u8; 61];
    p[..8].copy_from_slice(b"CRKSND__");
    p[7] = b'0' + pass;
    for (i, b) in p.iter_mut().enumerate().skip(8) {
        *b = (i as u8).wrapping_mul(31) ^ pass;
    }
    p
}

/// Mount the static image, write `pass`'s payload to `scratch`, read it back, and report. Returns the
/// error if any step failed, so the caller can print the errno rather than guess at it.
fn mount_and_write(pass: u8) -> syscall::error::Result<()> {
    let mut srv = Server::open(BlockDisk(StaticImage))?;
    let h = srv.open_file("scratch")?;
    let p = payload(pass);
    let n = srv.write(h, 0, &p)?;
    assert_eq!(n, p.len(), "short write on mount {pass}");
    let mut back = [0u8; 128];
    let m = srv.read(h, 0, &mut back)?;
    assert_eq!(&back[..m], &p[..], "mount {pass} did not read back");
    Ok(())
}

/// How many mount-and-write cycles to run. Two is the reported case; a larger count tests whether the
/// failure is *accumulated* state (each cycle advances the header generation, lengthens the allocator
/// log, and leaves more live tree blocks) rather than merely "not the first".
fn mount_count() -> u8 {
    std::env::var("CRICKER_MOUNTS")
        .ok()
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(2)
}

fn main() {
    let cap_mib = heap_cap() / 1024 / 1024;
    let mounts = mount_count();
    mark(&format!(
        "second_mount: heap cap {cap_mib} MiB, {mounts} mount cycles \
         (CRICKER_HEAP_MIB / CRICKER_MOUNTS override)"
    ));

    // Build the fixture the way the host tool does: an empty filesystem plus the two files.
    let mut fs = FileSystem::create(BlockDisk(StaticImage), None, 0, 0).expect("mkfs");
    fs.tx(|tx| {
        let motd = tx
            .create_node(TreePtr::root(), "motd", Node::MODE_FILE | 0o644, 0, 0)?
            .ptr();
        tx.write_node(motd, 0, b"motd\n", 0, 0)?;
        let scratch = tx
            .create_node(TreePtr::root(), "scratch", Node::MODE_FILE | 0o644, 0, 0)?
            .ptr();
        tx.write_node(scratch, 0, b"(placeholder)", 0, 0)?;
        Ok(())
    })
    .expect("populate");
    drop(fs);

    // Mount 1 is the write that always worked, and it is what makes the image "used". Every mount
    // after it is a second mount of a used image, the case that fails on device, and each one leaves
    // the image more used than the last.
    for pass in 1..=mounts {
        match mount_and_write(pass) {
            Ok(()) => mark(&format!(
                "mount {pass}: write OK (heap {} KiB)",
                COMMITTED.load(Ordering::Relaxed) / 1024
            )),
            Err(e) => {
                mark(&format!("mount {pass}: FAILED errno {}", e.errno));
                report_and_exit(1);
            }
        }
    }

    report_and_exit(0);
}

/// Report the heap high-water mark and whether the cap ever refused a growth, then exit.
fn report_and_exit(code: i32) -> ! {
    let hwm = COMMITTED.load(Ordering::Relaxed);
    mark(&format!(
        "heap high-water {} KiB of {} KiB cap; cap refused a growth: {}",
        hwm / 1024,
        heap_cap() / 1024,
        HIT_CAP.load(Ordering::Relaxed)
    ));
    std::process::exit(code);
}
