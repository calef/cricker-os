#![no_std]
// `Result<_, ()>` is deliberate throughout the build path, and clippy's `result_unit_err` is asking
// for an error type this layer has nothing to put in. Every failure here is a syscall that already
// returned its own error to the caller through the ABI; a second, richer error would be inventing
// detail the kernel did not provide. The unit is honest: it means "the syscall said no", and the
// caller's recourse is the same regardless of which one.
//
// Per-crate rather than per-item because it is one decision about one calling convention, and
// DECISIONS §38's rule is against blanket dead-code suppression hiding unreachable code, which this
// is not: nothing here is hidden, and the lint is a style opinion about an error type.
#![allow(clippy::result_unit_err)]
// `ChildEndowment::new()` is an empty endowment, and a `Default` impl would give a second spelling for the
// same thing in a crate whose whole job is that two binaries agree on one spelling.
#![allow(clippy::new_without_default)]
//! **The supervision tree: the shared half** (milestone 22 phase B.2).
//!
//! Three programs make up the tree that shrinks init's authority (`root_supervisor`, `spawner`, `sub_server_supervisor`,
//! plus the `flaky` sub-server they manage), and this is what they share: the protocol words they
//! speak, and the userspace ELF loader they build children with.
//!
//! **The loader here is the tree's only one** (milestone 96). It began as a generalization of
//! `system_initializer`'s `build_child` while the two inits kept copies of their own, which meant
//! the same hundred and thirty lines existed three times with a fault slot each. Milestone 22's
//! lane recorded that and declined to unify it mid-flight, because a boot failure would then have
//! been ambiguous between two changes; `system_initializer` calls this one now, and nothing else builds
//! a process in userspace.
//!
//! What it takes: the builder's own budget and the budget the child is built *from* as separate
//! arguments (they are the same for a server building out of its own memory, and different for a
//! spawner building each child in its own reclaimable region), a capability for the reserved fault
//! slot so the child is born supervised, capabilities for **named** slots the caller picks, and
//! **blobs** to copy into the child. That last one is what lets a construction sub-server hold
//! exactly the one program image it is allowed to build, instead of the whole initrd.
//!
//! # Examples
//!
//! **A caveat first, because it is a limitation rather than a footnote.** This crate takes an
//! unconditional `user_rt` dependency, so `script/test`'s host pass excludes it (the exclusion list
//! is in `xtask` and is derived and checked by `script/lint`). The examples below run under
//! `cargo test --doc -p supervision_proto` on an aarch64 host and **are not checked by the gate**.
//!
//! [`ChildEndowment`] is the crate's real interface, and reading one is meant to tell you the
//! complete authority of the thing about to run. Every field is public and there is a
//! nothing-endowment to build from, which is what keeps a later field from being a change to every
//! caller:
//!
//! ```
//! use supervision_proto::{CHILD_STACK_PAGES, ChildEndowment};
//!
//! // A sub-server that gets exactly one endpoint, is born supervised, and holds nothing else.
//! // `..ChildEndowment::new()` is the intended shape: what is not listed is not granted.
//! let endow = ChildEndowment {
//!     caps: &[(4, 0b11)], // our slot 4, read/write, landing in the child's slot 0
//!     fault: Some(7), // our slot 7 holds its supervision endpoint
//!     ..ChildEndowment::new()
//! };
//!
//! assert_eq!(endow.caps.len(), 1);
//! assert!(endow.maps.is_empty() && endow.blobs.is_empty() && endow.placed.is_empty());
//! assert_eq!(endow.stack_pages, CHILD_STACK_PAGES);
//!
//! // A construction sub-server, holding exactly the one program image it may build. That is what
//! // `blobs` buys: the child is handed *data* it has no capability to reach, so the sub-server
//! // never needs the whole initrd.
//! let image = &[0x7f, b'E', b'L', b'F'][..];
//! let builder = ChildEndowment { blobs: &[(0x2000_0000, image)], ..ChildEndowment::new() };
//! assert_eq!(builder.blobs[0].1.len(), 4);
//! assert!(builder.fault.is_none()); // unsupervised, and visibly so
//! ```
//!
//! The two budgets [`build_child`] takes are the design's load-bearing distinction and are easy to
//! pass in the wrong order, so it is worth stating what each one is for:
//!
//! ```no_run
//! # use supervision_proto::{ChildEndowment, build_child, tcb_start};
//! # fn demo(own: u64, per_child: u64, elf: &elf::Elf) -> Result<(), ()> {
//! let endow = ChildEndowment { fault: Some(7), ..ChildEndowment::new() };
//!
//! // `own` pays for OUR scratch mappings; `per_child` is what the child is made of. Passing a
//! // per-child region as the second argument is what makes a single `DESTROY` reap the whole
//! // instance, and passing our own would free our page tables under the child.
//! let tcb = build_child(own, per_child, elf, &endow)?;
//! tcb_start(tcb, 0, 0, 0);
//! # Ok(())
//! # }
//! ```
//!
//! Name: recorded (milestone 46, and notes/naming.md's crate section). The wire contract was
//! spelled four ways (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept;
//! `*_proto` won on 2026-07-30 under DECISIONS §39, and `script/lint` has checked it since. That
//! rule plus the service the stem names produces this name, which is the whole of what `recorded`
//! claims: calef ruled on the rule, and never on this crate.
//! The stem is the tree's word for the restart discipline. The type it exports as `ChildEndowment` is an
//! open naming question of its own (DECISIONS §69): a verb where the tenet says noun, naming the
//! same idea as `grant_plan::Endowment` one construction step apart.

use user_rt::{cap_delete, invoke};

// ===========================================================================================
// The report protocol. Every process in the tree holds a WRITE view of one report endpoint and
// says what happened on it; the kernel test is the receiver. Mirrored in kernel/src/user.rs
// (`authority_tests`), the same way the net client's selectors are.
// ===========================================================================================

/// init dropped its construction authority. `w1` = 1 if using the dropped budget then failed,
/// `w2` = the negated error code (1 = `NoSuchSlot`: the slot is empty, there is nothing to name).
pub const REPORT_INIT_DROPPED: u64 = 1;
/// A sub-server instance ran. `w1` = which attempt it is (0 = the original, 1+ = a restart).
pub const REPORT_SERVER_RAN: u64 = 2;
/// The sub-server's supervisor saw its child die. `w1` = tid, `w2` = the event (fault or exit).
pub const REPORT_SUP_SAW_DEATH: u64 = 3;
/// The supervisor's retry budget ran out; its policy is to stop. `w1` = restarts attempted.
pub const REPORT_SUP_GAVE_UP: u64 = 4;
/// Something in the tree could not be built. `w1` = a stage code, so a broken tree is debuggable
/// rather than silent.
pub const REPORT_FAILED: u64 = 9;

// ===========================================================================================
// The construction protocol: the supervisor asks, the spawner builds. This is the authority split
// made concrete. The supervisor holds no memory at all, so it cannot build anything itself; the
// spawner holds a budget and exactly one program image, so it cannot build anything else.
//
// **Building is all that is asked here now** (DECISIONS §32). There used to be a `REQ_REAP` too,
// because reaping was `Untyped::DESTROY` and only the spawner held the region capability, so the
// supervisor had to proxy the reap through the process that could. §32 put the reap on the
// supervision endpoint the supervisor already holds, so the hop is gone and with it the handle the
// spawner had to invent to name an instance the kernel names by tid.
// ===========================================================================================

/// `send(req, REQ_BUILD, attempt, 0)` -> `(REP_BUILT, 0, 0)` or `(REP_FAILED, 0, 0)`.
pub const REQ_BUILD: u64 = 1;

/// Reply code: the build succeeded.
pub const REP_BUILT: u64 = 1;
/// Reply code: the build failed (out of budget, a bad image, or a loader refusal).
pub const REP_FAILED: u64 = 3;

// ===========================================================================================
// The loader.
// ===========================================================================================

/// The page size the loader maps in.
pub const PAGE: u64 = 4096;

/// Where a child's stack top sits. One address for every process this system builds, which is what
/// lets [`configure_child`] compute the entry `sp` without being told.
pub const CHILD_STACK_VA: u64 = 0x0050_0000;

/// The stack a child gets when its builder does not say otherwise ([`ChildEndowment::new`]). Four pages,
/// which is enough for the supervision tree's programs; the flaky sub-server would be fine with one.
///
/// **A child at the interactive prompt gets three times this** (`system_initializer::CHILD_STACK_PAGES`),
/// and the difference is deliberate rather than drift: the prompt's children run the shell's
/// redirection path, whose frames grew twice under measurement. The number is a field on [`ChildEndowment`]
/// so a caller states it, because a builder that silently inherits somebody else's stack size finds
/// faults that builder does not have (notes/pipes.md).
pub const CHILD_STACK_PAGES: u64 = 4;

/// An ever-advancing scratch window: where we temporarily map each child frame to fill it. Never
/// unmapped, so a per-call reset would collide with a previous child's mappings (the bug 19d.2c
/// found and this inherits the fix for).
static SCRATCH_NEXT: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0x1000_0000);

/// **Everything a child is born holding.** The same idea as the kernel's `Spawn`: read one of these
/// and you know the complete authority of the thing about to run.
pub struct ChildEndowment<'a> {
    /// Capabilities to insert, `(our_slot, rights)`, landing in the child's slots 0, 1, 2, ...
    pub caps: &'a [(u64, u64)],
    /// Capabilities to insert at a slot the caller **names**, `(child_slot, our_slot, rights)`,
    /// after [`caps`](ChildEndowment::caps). One caller today: a declared diagnostic stream (DECISIONS §67),
    /// which cannot take the next free slot because how many low slots a child gets depends on what
    /// else the command line granted it, and a program that probes one slot number needs that
    /// number not to move.
    ///
    /// It cannot collide with [`fault`](ChildEndowment::fault): that one lands in the last slot of the
    /// cspace, and a manifest's diagnostics slot is one the program reads at startup, far below it.
    pub placed: &'a [(u64, u64, u64)],
    /// Pages of ours to map into the child, `(child_va, our_slot, mode)`.
    pub maps: &'a [(u64, u64, u64)],
    /// Bytes to copy into fresh pages in the child, at consecutive VAs from `va`. This is how a
    /// child is handed *data* it has no capability to reach: a program image, in our case.
    pub blobs: &'a [(u64, &'a [u8])],
    /// Our slot holding the child's supervision endpoint (DECISIONS §26). Placed in the reserved
    /// `FAULT_EP_SLOT`, where `START` reads it and clears it, so the child is born supervised and
    /// cannot forge messages on its own death channel.
    pub fault: Option<u64>,
    /// Stack pages, mapped down from [`CHILD_STACK_VA`]. See [`CHILD_STACK_PAGES`] for why this is
    /// a field a caller sets rather than one number for the whole tree.
    pub stack_pages: u64,
}

impl<'a> ChildEndowment<'a> {
    /// An endowment of nothing: no capabilities, no mappings, no supervision, and the default
    /// stack. Every field is public, so the intended use is `..ChildEndowment::new()` at the end of a struct
    /// literal, which is also what keeps a later field from being a change to every caller.
    pub const fn new() -> Self {
        Self {
            caps: &[],
            placed: &[],
            maps: &[],
            blobs: &[],
            fault: None,
            stack_pages: CHILD_STACK_PAGES,
        }
    }
}

/// Build a child from `elf` and configure it, ready for [`tcb_start`]. The whole job in one call,
/// which is what every caller but the hot-swap operator wants.
///
/// `own_ut` pays for **our** scratch mappings (they are ours, and a child's region must not have our
/// page tables freed under it when the child is reaped). `build_ut` is what the child itself is made
/// of: its address space, frames, stack, and TCB. Passing a per-child region as `build_ut` is what
/// makes a single `DESTROY` reap the whole instance.
pub fn build_child(
    own_ut: u64,
    build_ut: u64,
    elf: &elf::Elf,
    endow: &ChildEndowment,
) -> Result<u64, ()> {
    let (tcb, aspace) = build_child_space(own_ut, build_ut, elf, endow)?;
    configure_child(tcb, aspace, elf.entry())?;
    Ok(tcb)
}

/// Everything [`build_child`] does **except** the final `CONFIGURE`: lay each segment W^X at the VA
/// it names, map a stack, copy the blobs in, map `maps`, retype a TCB, insert the endowment.
/// Returns `(tcb, aspace)`, both still held by us.
///
/// Split out for milestone 23's live replacement (DECISIONS §41), which needs to do one more thing
/// to the child's address space *between* building it and configuring it: map in a device the
/// operator could not hand over any earlier, because revoking it from the outgoing owner would have
/// taken the incoming owner's copy too. `CONFIGURE` consumes the aspace capability, so once it has
/// run there is no way to reach the child's memory again; this is where that seam is.
pub fn build_child_space(
    own_ut: u64,
    build_ut: u64,
    elf: &elf::Elf,
    endow: &ChildEndowment,
) -> Result<(u64, u64), ()> {
    let aspace = retype_obj_from(build_ut, abi::objtype::ASPACE)?;

    for seg in elf.segments() {
        let mode = if seg.is_executable() {
            abi::aspace::MAP_CODE
        } else if seg.is_writable() {
            abi::aspace::MAP_RW
        } else {
            abi::aspace::MAP_RO
        };
        let (start, end) = seg.page_range(PAGE);
        let mut va = start;
        while va < end {
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + PAGE).min(file_hi);
            let src = if lo < hi {
                Some((
                    (lo - va) as usize,
                    &seg.data[(lo - file_lo) as usize..(hi - file_lo) as usize],
                ))
            } else {
                None
            };
            fill_and_map(own_ut, build_ut, aspace, va, src, mode)?;
            va += PAGE;
        }
    }

    for k in 0..endow.stack_pages {
        let stack_frame = retype_frame_from(build_ut)?;
        let va = CHILD_STACK_VA - k * PAGE;
        // SAFETY: `invoke` traps to the kernel, which validates the capability and the method
        // before acting (user_rt's contract). A caller cannot break an invariant by passing a
        // bad slot or method; it gets an error back.
        if unsafe {
            invoke(
                aspace,
                abi::aspace::MAP_INTO,
                va,
                stack_frame,
                abi::aspace::MAP_RW,
            )
        } != 0
        {
            return Err(());
        }
        cap_delete(stack_frame);
    }

    // The blobs: fresh read-only pages carrying bytes we chose. Read-only because a program image
    // is data the child reads, and a child that could rewrite the image it was handed could hand a
    // different one on.
    for &(base, bytes) in endow.blobs {
        let mut off = 0usize;
        while off < bytes.len() {
            let n = core::cmp::min(PAGE as usize, bytes.len() - off);
            fill_and_map(
                own_ut,
                build_ut,
                aspace,
                base + off as u64,
                Some((0, &bytes[off..off + n])),
                abi::aspace::MAP_RO,
            )?;
            off += n;
        }
    }

    for &(va, our_slot, mode) in endow.maps {
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, our_slot, mode) } != 0 {
            return Err(());
        }
    }

    let tcb = retype_obj_from(build_ut, abi::objtype::TCB)?;
    for &(our_slot, rights) in endow.caps {
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, our_slot, rights, 0) } < 0 {
            return Err(());
        }
    }
    for &(child_slot, our_slot, rights) in endow.placed {
        // `target = n` lands the capability in slot `n - 1`; 0 would mean "first free", which is the
        // behaviour this call exists to avoid.
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe { invoke(tcb, abi::tcb::CAP_INSERT, our_slot, rights, child_slot + 1) } < 0 {
            return Err(());
        }
    }
    if let Some(fault) = endow.fault {
        // The spawn-slot convention: target slot `n + 1` means "slot n", so the supervision endpoint
        // lands in the reserved last slot rather than wherever first-free fell.
        // SAFETY: as above: the kernel validates the capability and the method.
        if unsafe {
            invoke(
                tcb,
                abi::tcb::CAP_INSERT,
                fault,
                abi::rights::READ,
                abi::fault::FAULT_EP_SLOT + 1,
            )
        } < 0
        {
            return Err(());
        }
    }
    Ok((tcb, aspace))
}

/// Bind the address space and set the entry point: the last step before [`tcb_start`]. The `aspace`
/// capability is **consumed** by the kernel here, so this is the moment after which the builder can
/// no longer shape the child's memory.
pub fn configure_child(tcb: u64, aspace: u64, entry: u64) -> Result<(), ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe {
        invoke(
            tcb,
            abi::tcb::CONFIGURE,
            entry,
            CHILD_STACK_VA + PAGE,
            aspace,
        )
    } != 0
    {
        return Err(());
    }
    Ok(())
}

/// Retype one page out of `build_ut`, fill it (zeroed, then `src` bytes at an offset) through our own
/// scratch window, and map it into `aspace` at `va`. The one place a page's contents are written, so
/// segments and blobs cannot drift apart.
fn fill_and_map(
    own_ut: u64,
    build_ut: u64,
    aspace: u64,
    va: u64,
    src: Option<(usize, &[u8])>,
    mode: u64,
) -> Result<(), ()> {
    let frame = retype_frame_from(build_ut)?;
    let scratch = SCRATCH_NEXT.fetch_add(PAGE, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe { invoke(frame, abi::frame::MAP, scratch, 1, own_ut) } != 0 {
        return Err(());
    }
    // SAFETY: `scratch` is a page we just mapped read/write in our own address space.
    let dst = unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE as usize) };
    dst.fill(0);
    if let Some((at, bytes)) = src {
        dst[at..at + bytes.len()].copy_from_slice(bytes);
    }
    // SAFETY: as above: the kernel validates the capability and the method.
    if unsafe { invoke(aspace, abi::aspace::MAP_INTO, va, frame, mode) } != 0 {
        return Err(());
    }
    cap_delete(frame);
    Ok(())
}

/// Retype one page of `ut` into a kernel object of `objtype` (`abi::objtype`), returning the
/// capability slot it landed in.
pub fn retype_obj_from(ut: u64, objtype: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::RETYPE_OBJ, objtype, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Retype one page of `ut` into a plain `Frame` capability, unmapped, returning the slot it
/// landed in.
pub fn retype_frame_from(ut: u64) -> Result<u64, ()> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::RETYPE, 0, 0, 0) };
    if r < 0 { Err(()) } else { Ok(r as u64) }
}

/// Carve `pages` off `ut` into a new child untyped we hold in full: the region a single `DESTROY`
/// reclaims. `Err` carries the negated error code, which is how the dropped-authority proof reports
/// *why* a retype from a deleted budget failed.
pub fn untyped_split(ut: u64, pages: u64) -> Result<u64, i64> {
    // SAFETY: as above: the kernel validates the capability and the method.
    let r = unsafe { invoke(ut, abi::untyped::SPLIT, pages, 0, 0) };
    if r < 0 { Err(r) } else { Ok(r as u64) }
}

/// §16 object revocation: reclaim a region and every object retyped from it, by its **owner**. The
/// stronger of the two reaps, because `WRITE` on a region is also what builds a process out of it,
/// and the only one that can tear down a *live* thread (§16's amendment arms the kill). A supervisor
/// collecting a dead child wants `user_rt::reap` instead (§32).
pub fn untyped_destroy(ut: u64) -> bool {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(ut, abi::untyped::DESTROY, 0, 0, 0) == 0 }
}

/// Make `tcb` runnable, with `a0`, `a1`, `a2` in its entry registers. `false` if the thread was
/// not fully configured (no bound address space or no entry point) and the kernel refused.
pub fn tcb_start(tcb: u64, a0: u64, a1: u64, a2: u64) -> bool {
    // SAFETY: as above: the kernel validates the capability and the method.
    unsafe { invoke(tcb, abi::tcb::START, a0, a1, a2) == 0 }
}

/// Trap. A half-built system is not worth limping along, and a fault is legible: the kernel prints
/// the pc and the process dies where the mistake was.
///
/// The instruction moved to [`user_rt::trap`] in milestone 130; this stays because callers here
/// mean something more specific than "trap" (a supervision-protocol step failed, and limping on
/// would build half a system), and that reason is worth a name. What it no longer does is spell
/// the asm out: this body and `swap_proto::fail`'s were byte-identical copies of each other and of
/// forty-six other sites.
pub fn fail() -> ! {
    user_rt::trap()
}
