//! The syscall boundary. **Three calls.**
//!
//! DECISIONS.md §4 rule 3 said the syscall surface stays narrow and explicit, *"a boundary, not a
//! habit."* §8 said milestone 7 was a hard decision point and that hacking one in without the
//! conversation meant the plan had failed. §10 had the conversation and chose capabilities.
//!
//! This is what that buys:
//!
//! ```text
//!   exit(code)                          authority over yourself
//!   yield()                             likewise
//!   invoke(cap, method, a0, a1, a2)     EVERYTHING ELSE
//! ```
//!
//! No `open`. No `read`. No `write`. No `fork`. **A process can only act on things it was
//! handed.** The ABI lives in `crates/abi`, which both the kernel and every user program depend
//! on, so the boundary is *one artifact* rather than two files that agree by luck.
//!
//! # No pointer ever crosses this boundary
//!
//! There used to be a `user_slice` here: the console `write` syscall took a `(ptr, len)` from
//! userspace and the kernel read the user's memory, which is why it needed the `AT S1E0R`
//! confused-deputy defence. Milestone 8 moved the console to a userspace server and deleted that
//! path. Today every argument is a scalar in a register (a capability slot, a method, a `va`, a
//! word), so the kernel follows no user pointer and there is no deputy to confuse. The primitive
//! that made the old check possible, `mmu::user_can_read`, is kept for the next syscall that does
//! take a user pointer.

use crate::arch::exceptions::TrapFrame;
use crate::arch::mmu;
use crate::cap::{Object, Rights};
use crate::sched;
use abi::Error;

/// Called from the `svc` arm of `exception_dispatch`.
pub fn dispatch(frame: &mut TrapFrame) {
    // The syscall number and arguments come from the trap frame through arch accessors, not raw
    // register indices, because the ABI register file differs per architecture (aarch64 `svc` with
    // the number in x8 and args in x0..x5; RISC-V `ecall` with the number in a7 and args in a0..a5).
    // `TrapFrame::{syscall_nr, arg, set_arg}` hide that mapping so this dispatcher stays portable.
    // See DECISIONS §10/§17.
    let nr = frame.syscall_nr();

    // `exit` never comes back, so it is not part of the result-writing path below.
    if nr == abi::SYS_EXIT {
        sched::exit();
    }

    let result: Result<i64, Error> = match nr {
        abi::SYS_YIELD => {
            sched::yield_now();
            Ok(0)
        }
        // Drop a capability from the caller's own cspace (milestone 19d). Deleting an empty slot
        // is a no-op, not an error: a loader recycling slots should not have to track emptiness.
        abi::SYS_CAP_DELETE => {
            let _ = sched::delete_current_cap(frame.arg(0));
            Ok(0)
        }
        abi::SYS_INVOKE => invoke(
            frame,
            frame.arg(0),
            frame.arg(1),
            frame.arg(2),
            frame.arg(3),
            frame.arg(4),
        ),
        _ => Err(Error::BadSyscall),
    };

    // The return value goes back in the first argument register, which the trap-restore path pops
    // into the register the user is waiting on. Writing to the trap frame IS writing to the user's
    // registers.
    frame.set_arg(
        0,
        match result {
            Ok(v) => v as u64,
            Err(e) => (e as i64) as u64,
        },
    );
}

/// Act on a capability.
///
/// **The lookup is the security mechanism, and it is a bounds check.** `slot` is an index into
/// *this thread's* table, which lives in kernel memory. An empty slot is `NoSuchSlot`: not
/// "permission denied", but *there is nothing there*. That difference is what no-ambient-authority
/// feels like from the inside.
fn invoke(
    frame: &mut TrapFrame,
    slot: u64,
    method: u64,
    a0: u64,
    a1: u64,
    a2: u64,
) -> Result<i64, Error> {
    let cap = sched::current_cap(slot).map_err(|_| Error::NoSuchSlot)?;

    match cap.object {
        Object::Endpoint(ep) => match method {
            // SEND takes WRITE, RECV takes READ. The *same* endpoint, handed out with different
            // rights, is a one-way pipe in whichever direction each holder was trusted with.
            abi::endpoint::SEND => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // The three words are already in registers. **Nothing is read from user memory**,
                // so there is no pointer to validate and no confused-deputy question to ask. That
                // is the fastpath, and it is why IPC carries control and not bulk data (§10).
                sched::ipc_send(ep, [a0, a1, a2]);
                // If the endpoint was revoked (stale, or reclaimed while we blocked), the send never
                // happened: report it rather than a silent success. Object revocation, notes/.
                if sched::take_ipc_aborted() {
                    return Err(Error::NoSuchSlot);
                }
                Ok(0)
            }
            abi::endpoint::RECV => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                let msg = sched::ipc_recv(ep);
                if sched::take_ipc_aborted() {
                    return Err(Error::NoSuchSlot); // endpoint revoked; the message is a placeholder
                }
                // Word 0 goes back the way every syscall result does, in x0 (dispatch writes it
                // from our return value). Words 1..4 we place directly, because a syscall return is
                // one register and a message is up to five: ordinary IPC fills three and leaves the
                // top two zero, and a fault/exit notification (DECISIONS §26) fills all five.
                frame.set_arg(1, msg[1]);
                frame.set_arg(2, msg[2]);
                frame.set_arg(3, msg[3]);
                frame.set_arg(4, msg[4]);
                Ok(msg[0] as i64)
            }

            // Delegation. `a0` is the slot of the capability to pass on, `a1` the rights to narrow
            // it to, `a2` one data word. Two rights are in play and they are different questions:
            // WRITE on *this* endpoint (may I send here?) and GRANT on the *delegated* capability
            // (was I trusted to pass it on?). Without GRANT you may use a thing and not lend it.
            abi::endpoint::SEND_CAP => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                let src = sched::current_cap(a0).map_err(|_| Error::NoSuchSlot)?;
                if !src.rights.allows(Rights::GRANT) {
                    return Err(Error::NotPermitted); // holder may not pass this on
                }
                let narrowed = Rights::from_bits(a1 as u32);
                if !narrowed.is_subset_of(src.rights) {
                    return Err(Error::NotPermitted); // delegation may only narrow, never widen
                }
                sched::ipc_send_cap(
                    ep,
                    a2,
                    crate::cap::Cap {
                        object: src.object,
                        rights: narrowed,
                    },
                );
                if sched::take_ipc_aborted() {
                    return Err(Error::NoSuchSlot); // endpoint revoked; the delegation did not happen
                }
                Ok(0)
            }
            abi::endpoint::RECV_CAP => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                let msg = sched::ipc_recv_cap(ep);
                if sched::take_ipc_aborted() {
                    return Err(Error::NoSuchSlot); // endpoint revoked; the message is a placeholder
                }
                // x1 carries the slot the received capability landed in, or NO_CAP if the message
                // brought none; x2 the second data word (a CALL's, or 0). x0 returns the first word.
                frame.set_arg(1, msg[1]);
                frame.set_arg(2, msg[2]);
                Ok(msg[0] as i64)
            }

            // Call: send two words and block until replied. The kernel mints a one-shot Reply cap
            // naming us into the server (delivered by its RECV_CAP); we return here only when the
            // server invokes it. See §12 and notes/ipc-naming.md. Sending needs WRITE, like SEND.
            abi::endpoint::CALL => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                let reply = sched::ipc_call(ep, [a0, a1]);
                if sched::take_ipc_aborted() {
                    return Err(Error::NoSuchSlot); // endpoint revoked; no call, no reply
                }
                frame.set_arg(1, reply[1]); // r1; r0 returns in x0 below
                Ok(reply[0] as i64)
            }
            _ => Err(Error::BadMethod),
        },

        // A one-shot reply to a specific caller (§12). Minted by the kernel at a CALL rendezvous,
        // named by the caller's tid, consumed on use.
        Object::Reply(tid) => match method {
            abi::reply::REPLY => {
                // Minted WRITE-only, so a narrowed derivative could not answer; and minted without
                // GRANT, so it could not have been delegated here in the first place.
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                sched::ipc_reply(tid, [a0, a1]);
                // One-shot: consume it, so a second reply is NoSuchSlot and the caller cannot be
                // answered twice. This is the guarantee a pre-wired reply endpoint cannot make.
                let _ = sched::delete_current_cap(slot);
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },

        // Another process's memory, under construction (19b). WRITE on the aspace cap is the
        // authority to shape it; the frame's own rights gate what kind of mapping, exactly as
        // frame::MAP; the va gate is the proved paging::is_user_page_va, as everywhere.
        Object::Aspace(name) => match method {
            abi::aspace::MAP_INTO => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                let va = a0;
                if !paging::is_user_page_va::<crate::arch::mmu::Format>(va) {
                    return Err(Error::BadPointer);
                }
                let frame = sched::current_cap(a1).map_err(|_| Error::NoSuchSlot)?;
                // The mappable object is a Frame (normal memory) or a DeviceFrame (a device's
                // MMIO, device-typed): the driver a userspace init builds gets its registers this
                // way (19d.2). a2 chooses the shape for a Frame; a DeviceFrame is always
                // device-typed read/write and needs WRITE on the cap.
                let (phys, flags) = match frame.object {
                    Object::DeviceFrame(phys) => {
                        if !frame.rights.allows(Rights::WRITE) {
                            return Err(Error::NotPermitted);
                        }
                        (phys, paging::Flags::user_device())
                    }
                    Object::Frame(phys) => {
                        // 0 read-only, 1 read/write, 2 executable code (a loader's child .text).
                        // Code is W^X: user_code is RX, never writable, so it needs only READ.
                        let flags = match a2 {
                            abi::aspace::MAP_RW => {
                                if !frame.rights.allows(Rights::WRITE) {
                                    return Err(Error::NotPermitted);
                                }
                                paging::Flags::user_data()
                            }
                            abi::aspace::MAP_CODE => {
                                if !frame.rights.allows(Rights::READ) {
                                    return Err(Error::NotPermitted);
                                }
                                paging::Flags::user_code()
                            }
                            _ => {
                                if !frame.rights.allows(Rights::READ) {
                                    return Err(Error::NotPermitted);
                                }
                                paging::Flags::user_rodata()
                            }
                        };
                        (phys, flags)
                    }
                    _ => return Err(Error::WrongObject),
                };
                match crate::user::user_aspace_map(name, va, phys, flags) {
                    Ok(()) => {
                        // When userspace maps a frame it wrote executable (a spawner building a
                        // child's code, MAP_CODE), the instruction fetcher must be made to see the
                        // bytes the writer stored: RISC-V's `fence.i`, aarch64's dcache-clean +
                        // icache-invalidate, both behind `sync_icache`. The kernel-side ELF loader
                        // does this (user.rs map_segments); this is the userspace-built path, which
                        // a fast spawn+reap loop (bench::spawn_el0) is the first thing to stress. A
                        // child that fetches unsynced code takes an illegal-instruction fault at its
                        // entry.
                        if flags.is_user_executable() {
                            crate::arch::sync_icache(
                                crate::arch::mmu::phys_to_virt(phys),
                                paging::PAGE_SIZE as usize,
                            );
                        }
                        Ok(0)
                    }
                    Err(paging::MapError::OutOfFrames) => Err(Error::OutOfMemory),
                    Err(_) => Err(Error::BadPointer), // misaligned, already mapped, unknown space
                }
            }
            _ => Err(Error::BadMethod),
        },

        // A thread under construction (19c.3). WRITE on the TCB cap is the authority to shape
        // and start it. Every method refuses a thread that is not an embryo, in the scheduler.
        Object::Tcb(tid) => match method {
            abi::tcb::CONFIGURE => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // a2 is the aspace cap slot; it must be a WRITE Aspace cap, and it is consumed.
                let aspace = sched::current_cap(a2).map_err(|_| Error::NoSuchSlot)?;
                let Object::Aspace(aspace_name) = aspace.object else {
                    return Err(Error::WrongObject);
                };
                if !aspace.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                sched::configure_tcb(tid, a0, a1, aspace_name)?;
                // Consume the aspace cap: it is the thread's now, and a second bind must not find
                // it. (The space already left the registry, so the cap is inert regardless; this
                // keeps the caller's cspace honest.)
                let _ = sched::delete_current_cap(a2);
                Ok(0)
            }
            abi::tcb::CAP_INSERT => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // a0 = the cap to give the child, a1 = rights to narrow it to. GRANT-gated and
                // narrowing-only, exactly as SEND_CAP: you may endow a child only with authority
                // you were trusted to pass on, and only narrowed. a2 = target slot: 0 is first-free
                // (the original behaviour, so every existing caller is unchanged), n is slot n - 1
                // (a supervisor placing a fault endpoint in the reserved slot, milestone 22).
                let src = sched::current_cap(a0).map_err(|_| Error::NoSuchSlot)?;
                if !src.rights.allows(Rights::GRANT) {
                    return Err(Error::NotPermitted);
                }
                let narrowed = Rights::from_bits(a1 as u32);
                if !narrowed.is_subset_of(src.rights) {
                    return Err(Error::NotPermitted);
                }
                let target = (a2 != 0).then(|| a2 - 1);
                let child_slot = sched::tcb_insert_cap(
                    tid,
                    crate::cap::Cap {
                        object: src.object,
                        rights: narrowed,
                    },
                    target,
                )?;
                Ok(child_slot as i64)
            }
            abi::tcb::START => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                sched::start_tcb(tid, [a0, a1, a2])?; // the child's x0, x1, x2 (19d/19e)
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },

        Object::Untyped(region) => match method {
            abi::untyped::MAP => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // Retype a page out of the untyped and map it, writable, at `a0` in the caller's
                // own address space. Both the page and any page tables come from the untyped, so
                // the KERNEL ALLOCATES NOTHING: `mmu::map_current_user_page`'s only source of
                // memory is the closure below, which bumps the untyped's watermark.
                let va = a0;
                // Reject the cheap failures BEFORE retyping a page for them: a non-page-aligned
                // or non-low-half address can never be mapped, and without this pre-check each
                // such attempt would silently spend a page of the process's own untyped (a
                // self-inflicted budget leak the audit noted). An already-mapped `va` still costs
                // one page, which is process-local and bounded by the untyped. The gate itself is
                // proved: every address it admits is aligned and in the low half (see
                // `paging::is_user_page_va` and its harness).
                if !paging::is_user_page_va::<crate::arch::mmu::Format>(va) {
                    return Err(Error::BadPointer);
                }
                match mmu::map_current_user_page(va, paging::Flags::user_data(), || {
                    crate::untyped::retype_page(region)
                }) {
                    Ok(phys) => {
                        // Record the mapping so it can be revoked before the region is ever
                        // reclaimed (§13). Untyped::MAP pages are process-private, but they still
                        // must be unmapped before untyped::destroy frees the region under them.
                        // The record is paid from the caller's own address-space budget (phase
                        // C); if it cannot afford the record, it cannot keep the mapping: an
                        // unrecorded mapping is invisible to revocation, the §13 hole.
                        if !crate::revoke::record_mapping(phys, mmu::current_user_root(), va) {
                            mmu::unmap_user_at(mmu::current_user_root(), va);
                            return Err(Error::OutOfMemory);
                        }
                        Ok(0)
                    }
                    Err(paging::MapError::OutOfFrames) => Err(Error::OutOfMemory),
                    Err(_) => Err(Error::BadPointer), // misaligned, already mapped, or wrong half
                }
            }
            // Retype a page into a page-resident KERNEL OBJECT the caller now owns (19a):
            // the object lives in the carved page, the region is pinned (a live endpoint's page
            // must never be freed under a blocked thread), and the caller gets full rights on
            // its own object, delegation narrowing them as ever.
            abi::untyped::RETYPE_OBJ => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                match a0 {
                    abi::objtype::ENDPOINT => {
                        let ep = sched::create_endpoint_from(region).ok_or(Error::OutOfMemory)?;
                        let slot = sched::grant(crate::cap::endpoint_cap(
                            ep,
                            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
                        ))
                        .map_err(|_| Error::OutOfMemory)?;
                        Ok(slot as i64)
                    }
                    // An address space (19b): the page becomes the L0 root, the untyped becomes
                    // the space's backing region for tables and records (one budget model; see
                    // the abi doc and design/init-and-granular-spawn.md).
                    abi::objtype::ASPACE => {
                        let name =
                            crate::user::user_aspace_create(region).ok_or(Error::OutOfMemory)?;
                        let slot = sched::grant(crate::cap::aspace_cap(
                            name,
                            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
                        ))
                        .map_err(|_| Error::OutOfMemory)?;
                        Ok(slot as i64)
                    }
                    // A thread (19c.3): the page holds an embryo TCB, born in no queue and not
                    // runnable until CONFIGURE + START. The page is the creator's region's.
                    abi::objtype::TCB => {
                        let tid = sched::create_tcb(region).ok_or(Error::OutOfMemory)?;
                        let slot = sched::grant(crate::cap::tcb_cap(
                            tid,
                            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
                        ))
                        .map_err(|_| Error::OutOfMemory)?;
                        Ok(slot as i64)
                    }
                    _ => Err(Error::BadMethod), // no such object type
                }
            }
            abi::untyped::RETYPE => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                // Retype a page into a Frame capability the caller now holds, instead of mapping it
                // in one shot. The caller gets full rights on its own frame (read, write, and the
                // right to pass it on); delegation is where those narrow. Nothing is mapped yet.
                let phys = crate::untyped::retype_page(region).ok_or(Error::OutOfMemory)?;
                let slot = sched::grant(crate::cap::frame_cap(
                    phys,
                    Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
                ))
                .map_err(|_| Error::OutOfMemory)?; // cspace full
                Ok(slot as i64)
            }
            // Carve a child untyped off this one (subdivision), so a spawner can give each child its
            // own reclaimable region. `a0` is the child's page count. See untyped::split.
            abi::untyped::SPLIT => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                let child = crate::untyped::split(region, a0).ok_or(Error::OutOfMemory)?;
                // The child inherits THIS capability's rights, never more (milestone 31). SPLIT is a
                // fresh mint, so it must honor the derive-never-widens invariant by hand: a process
                // holding a spend-only (GRANT-less) untyped must not SPLIT itself a GRANT-bearing
                // child over the same memory and manufacture the right its capability withheld.
                // Rights narrow monotonically from the delegable root budget down; init holds that
                // root with GRANT and hands narrowed budgets on. See cap::untyped_cap_rights and
                // DECISIONS §16.
                let slot = sched::grant(crate::cap::untyped_cap_rights(child, cap.rights))
                    .map_err(|_| Error::OutOfMemory)?; // cspace full
                Ok(slot as i64)
            }
            // Reclaim this region and every object retyped from it (object revocation): tear the
            // objects down and return the memory. Refused (NotPermitted) while a live thread still
            // occupies it, or if it has been split into children (destroy those first). Generational
            // names make every capability to the reclaimed objects stale on next use.
            abi::untyped::DESTROY => {
                if !cap.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                sched::reclaim_region(region).map_err(|_| Error::NotPermitted)?;
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },

        Object::Frame(phys) => match method {
            abi::frame::MAP => {
                // a0 = va, a1 = writable (0/1), a2 = an untyped slot the page tables come from.
                let va = a0;
                if !paging::is_user_page_va::<crate::arch::mmu::Format>(va) {
                    return Err(Error::BadPointer);
                }
                // A read/write mapping needs WRITE on the frame; a read-only one needs READ. This
                // is where a delegated, narrowed frame is confined: a peer handed READ alone can
                // map it to look, never to change it.
                let flags = if a1 != 0 {
                    if !cap.rights.allows(Rights::WRITE) {
                        return Err(Error::NotPermitted);
                    }
                    paging::Flags::user_data()
                } else {
                    if !cap.rights.allows(Rights::READ) {
                        return Err(Error::NotPermitted);
                    }
                    paging::Flags::user_rodata()
                };
                // Page tables come from an untyped the caller holds, so mapping a frame, like
                // everything a process spends, comes out of its own budget and not the kernel's.
                let ut = sched::current_cap(a2).map_err(|_| Error::NoSuchSlot)?;
                let Object::Untyped(region) = ut.object else {
                    return Err(Error::WrongObject);
                };
                if !ut.rights.allows(Rights::WRITE) {
                    return Err(Error::NotPermitted);
                }
                match mmu::map_current_user_frame(va, phys, flags, || {
                    crate::untyped::retype_page(region)
                }) {
                    Ok(()) => {
                        // Record the mapping so a later REVOKE (or untyped::destroy) can pull this
                        // page out of every holder before it is reused (§13). Unrecordable means
                        // unmappable, at the mapper's own expense (phase C): see Untyped::MAP.
                        if !crate::revoke::record_mapping(phys, mmu::current_user_root(), va) {
                            mmu::unmap_user_at(mmu::current_user_root(), va);
                            return Err(Error::OutOfMemory);
                        }
                        Ok(0)
                    }
                    Err(paging::MapError::OutOfFrames) => Err(Error::OutOfMemory),
                    Err(_) => Err(Error::BadPointer), // misaligned, already mapped, or wrong half
                }
            }

            // Un-share: unmap this page from every holder and delete every capability to it,
            // including the caller's own. Needs GRANT (you were trusted to lend the frame, so you
            // may take it back); a read-only consumer, handed it without GRANT, cannot revoke the
            // owner. Does not reclaim the page (untyped is spend-only); that is untyped::destroy. §13.
            abi::frame::REVOKE => {
                if !cap.rights.allows(Rights::GRANT) {
                    return Err(Error::NotPermitted);
                }
                crate::revoke::revoke_frame(phys);
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },

        // A device's MMIO page is passive: it is not invoked, only handed to MAP_INTO as the
        // page to map (19d.2). Invoking it directly is a caller error.
        Object::DeviceFrame(_) => Err(Error::BadMethod),

        Object::Virtio(id) => {
            if !cap.rights.allows(Rights::WRITE) {
                return Err(Error::NotPermitted);
            }
            use crate::virtio::TransportError;
            let map = |e: TransportError| match e {
                TransportError::DmaEscape => Error::DeviceRefused,
                _ => Error::WrongObject,
            };
            match method {
                abi::virtio::READ_REG => crate::virtio::read_register(id, a0)
                    .map(|v| v as i64)
                    .ok_or(Error::WrongObject),
                abi::virtio::WRITE_REG => crate::virtio::write_register(id, a0, a1 as u32)
                    .map(|_| 0)
                    .map_err(map),
                abi::virtio::SETUP_QUEUE => crate::virtio::setup_queue(id, a0 as u16, a1 as u16)
                    .map(|_| 0)
                    .map_err(map),
                abi::virtio::NOTIFY => crate::virtio::notify(id, a0 as u16).map(|_| 0).map_err(map),
                _ => Err(Error::BadMethod),
            }
        }

        Object::Irq(intid) => match method {
            // WAIT blocks on the endpoint the kernel routed this interrupt to. The interrupt
            // arrives as a message (sched::irq_notify), exactly like any other.
            abi::irq::WAIT => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                let ep = sched::irq_route(intid).ok_or(Error::WrongObject)?;
                let m = sched::ipc_recv(ep);
                Ok(m[0] as i64)
            }
            // ACK re-enables the interrupt at the controller. The kernel masked it when it fired;
            // now that the driver has serviced the device, it is safe to let it fire again. This
            // names `arch::irq`, not a specific controller: the GIC on aarch64, the PLIC on RISC-V.
            abi::irq::ACK => {
                if !cap.rights.allows(Rights::READ) {
                    return Err(Error::NotPermitted);
                }
                crate::arch::irq::enable(intid);
                Ok(0)
            }
            _ => Err(Error::BadMethod),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **`SPLIT` never widens rights: a spend-only untyped splits into spend-only children.** SPLIT
    /// gates only on `WRITE`, and mints a fresh capability to the child budget, so it must honor the
    /// derive-never-widens invariant by hand or a process could manufacture authority it was denied:
    /// hold a deliberately `GRANT`-less untyped, `SPLIT` it, and receive a `GRANT`-bearing child over
    /// the same memory, then delegate what its own capability could not. This drives the real syscall
    /// path (not the region-level `untyped::split`, which carries no rights) and pins the child's
    /// rights to the parent capability's, at the mint site the Kani proofs do not cover. The contrast
    /// arm shows the delegable root does pass `GRANT` down, so the inheritance is real, not a blanket
    /// deny. Milestone 31; see DECISIONS §16 amendment.
    #[test_case]
    fn split_inherits_the_parent_capabilitys_rights_never_widening() {
        // `for_user_entry` is the portable frame constructor (both ISAs); SPLIT does not read it.
        let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);

        // A spend-only (WRITE, no GRANT) untyped, exactly what a leaf child is handed.
        let spend_only_region = crate::untyped::create(8).expect("a region to split");
        let parent =
            sched::grant(crate::cap::untyped_cap(spend_only_region)).expect("grant parent");
        assert!(
            !sched::current_cap(parent)
                .unwrap()
                .rights
                .allows(Rights::GRANT),
            "the parent capability must lack GRANT for this test to mean anything",
        );

        // SPLIT through the real handler. WRITE permits it; the child must inherit WRITE only.
        let child_slot = invoke(&mut frame, parent, abi::untyped::SPLIT, 2, 0, 0)
            .expect("split succeeds: the parent holds WRITE") as u64;
        let child = sched::current_cap(child_slot).expect("the child capability exists");
        assert!(
            !child.rights.allows(Rights::GRANT),
            "escalation: a GRANT-less untyped split into a GRANT-bearing child",
        );
        // Because it lacks GRANT it cannot be delegated: SEND_CAP and CAP_INSERT both refuse without
        // it (the exact gate at syscall.rs lines ~143 and ~319), so the child is non-transferable.
        assert!(
            !child.rights.allows(Rights::GRANT),
            "a spend-only child must be un-delegatable",
        );
        let Object::Untyped(child_region) = child.object else {
            panic!("SPLIT must mint an untyped");
        };

        // Contrast: the delegable root (READ|WRITE|GRANT, what init holds) passes GRANT to its
        // children, so a spawner can hand a budget on. Inheritance, not a blanket deny.
        let root_region = crate::untyped::create(8).expect("a root region");
        let root = sched::grant(crate::cap::untyped_root_cap(root_region)).expect("grant root");
        let root_child_slot =
            invoke(&mut frame, root, abi::untyped::SPLIT, 2, 0, 0).expect("split the root") as u64;
        let root_child = sched::current_cap(root_child_slot).expect("root child exists");
        assert!(
            root_child.rights.allows(Rights::GRANT),
            "a delegable root must split into delegable children",
        );
        let Object::Untyped(root_child_region) = root_child.object else {
            panic!("SPLIT must mint an untyped");
        };

        // Clean up: reclaim children (LIFO top of each parent) then parents, and drop the cap slots,
        // so the test returns every frame it borrowed and leaves the test thread's cspace as it found
        // it (the free-frame baseline that later tests measure against).
        sched::reclaim_region(child_region).expect("reclaim the spend-only child");
        sched::reclaim_region(spend_only_region).expect("reclaim the spend-only parent");
        sched::reclaim_region(root_child_region).expect("reclaim the root child");
        sched::reclaim_region(root_region).expect("reclaim the root parent");
        for slot in [parent, child_slot, root, root_child_slot] {
            let _ = sched::delete_current_cap(slot);
        }
    }
}
