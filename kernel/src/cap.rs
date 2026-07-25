//! What a capability in cricker-os can point at.
//!
//! The table itself is `crates/caps`, which is pure logic and knows nothing about this file.
//! This is the kernel's half: **the set of nouns.**
//!
//! DECISIONS.md §10 and notes/capabilities.md.

/// Every kind of thing a process can be handed.
///
/// **One entry, and that is the milestone-8 result rather than a stub.** There used to be a
/// `Console` variant: the kernel owned the PL011 and printed on a user's behalf. Milestone 8
/// deleted it. The console is now a userspace server reached by `SEND` on an endpoint, so
/// everything a process can name is an endpoint, and **the kernel no longer knows what a UART
/// is** on any path a user program can take.
///
/// The list grows deliberately, and each addition is a decision:
///
/// - `Frame` is now here, because **IPC carries control and shared memory carries data** (§10). A
///   shared buffer used to be mapped in at spawn, wired once by the kernel; a `Frame` makes
///   delegating memory a runtime operation a process does itself. See notes/frames.md.
/// - `Untyped` at milestone 11, if we take §10's deferred axis, at which point the kernel stops
///   allocating and this enum stops being the interesting part of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    /// An IPC endpoint, by **generational name** (milestone 19a: an endpoint is page-resident,
    /// and this is its `slots` name in the scheduler's registry, stale-safe like a Tid).
    ///
    /// Invoking it is a `SEND` or a `RECV` (which one you may do is a matter of rights). Since
    /// milestone 8 this is how a process reaches the console: it holds a `WRITE` capability on
    /// the console server's endpoint, and printing is sending.
    Endpoint(crate::sched::EpId),

    /// **Untyped memory** (milestone 11): a capability to a chunk of raw physical memory the
    /// process may retype into pages. Invoking it grows the process's address space out of its
    /// own budget, and the kernel allocates nothing to do it. See kernel/src/untyped.rs.
    Untyped(usize),

    /// A hardware interrupt, by INTID.
    ///
    /// **The capability that lets a driver own an interrupt without owning any privilege.** Its
    /// holder can `WAIT` for the interrupt (blocking until it fires) and `ACK` it (re-enabling it
    /// at the GIC after the device has been serviced). The kernel's handler does nothing device-
    /// specific: it masks the line and turns the interrupt into a message. Everything that knows
    /// what the *device* is lives in the userspace driver. This is milestone 9's version of the
    /// milestone-8 move (the console driver left; now the interrupt does too).
    Irq(u32),

    /// **A physical page**, by its physical address, that the holder may map into its own address
    /// space and delegate to others.
    ///
    /// The object §10 named as the shared-memory capability: *IPC carries control, shared memory
    /// carries data*. A shared buffer used to be a page the kernel mapped into both parties at
    /// spawn, wired once and never movable. A `Frame` makes it a runtime object instead: a process
    /// retypes one out of its own untyped (`Untyped::RETYPE`), maps it (`Frame::MAP`), and hands it
    /// (or a read-only view of it, since delegation narrows) to a peer over an endpoint. The peer
    /// maps the *same physical page* and the two share memory, composed by the processes rather
    /// than arranged for them. The address is the identity: a process can never forge one, because
    /// the only ways to get a `Frame` are to retype it or be handed it, and both keep the object.
    Frame(u64),

    /// **A device's MMIO page**, by physical address (milestone 19d.2): a delegatable authority
    /// to map a *specific* device's registers, **device-typed** (nGnRnE, uncacheable, unreordered
    /// — the only attributes MMIO tolerates). The kernel mints one per known device (it alone
    /// knows device physical addresses) and hands it to init; init delegates it to the driver it
    /// builds and maps it into that driver's space. This is what turns "the kernel maps the UART
    /// at spawn" into "device access is a capability", so a userspace init can bring up drivers.
    /// Distinct from `Frame` precisely because a `Frame` maps *normal cacheable* memory, which for
    /// MMIO would let the CPU cache and reorder register accesses: catastrophic for a device.
    DeviceFrame(u64),

    /// **A one-shot reply channel to a blocked caller** (milestone 12), named by the caller's
    /// thread id.
    ///
    /// The kernel mints one at a `CALL` rendezvous and hands it to the server. It is never
    /// forgeable and nameable no other way, so invoking it delivers the reply to *exactly* that
    /// caller and consumes the capability. "One reply, to this caller, exactly once" is therefore a
    /// kernel guarantee rather than a server convention, which is the whole point of the object.
    /// See DECISIONS §12 and notes/ipc-naming.md.
    Reply(crate::thread::Tid),

    /// **An address space under construction** (milestone 19b), by generational name in the
    /// user-aspace registry. The object itself is its L0 root page, resident in the page
    /// retyped from its creator's untyped; the kernel-side record (ASID, backing region) sits
    /// behind this name. `WRITE` lets the holder map frames into it; nothing can run in it
    /// until TCBs arrive (19c).
    Aspace(u64),

    /// **A thread under construction** (milestone 19c.3), by generational Tid: an embryo TCB a
    /// process is assembling. `WRITE` lets the holder configure, grant into, and start it. The
    /// Tid is stale-safe like every generational name, so a capability outliving its thread
    /// resolves to nothing rather than to a stranger.
    Tcb(crate::thread::Tid),

    /// A virtio device's **transport**, by id (into the kernel's virtio device table).
    ///
    /// The DMA-confinement capability. The device has no IOMMU, so the kernel keeps the two
    /// DMA-critical powers — programming the queue's ring addresses and ringing the device — and
    /// validates that every descriptor stays within the driver's own DMA region before the device
    /// sees it. The holder drives the device (status, features, submit) through this, but cannot
    /// point it outside its region. See kernel/src/virtio.rs.
    Virtio(usize),
}

pub type Cap = caps::Cap<Object>;

/// A thread's capability table: 16 slots, fixed at the type (milestone 14 phase B.1). The size
/// was already the de-facto limit (`CSpace::empty()` made 16); now it is part of the type and
/// creating a cspace cannot allocate. Growing it is a one-number change here, paid in TCB size.
pub const CSPACE_SLOTS: usize = 16;
pub type CSpace = caps::CSpace<Object, CSPACE_SLOTS>;

pub use caps::{Error, Rights};

/// A capability naming an endpoint, with the given rights.
///
/// **`WRITE` lets the holder `SEND`; `READ` lets it `RECV`.** Hand the two ends of one endpoint
/// out with opposite rights and you have a one-way pipe that neither side can run backwards.
pub fn endpoint_cap(ep: crate::sched::EpId, rights: Rights) -> Cap {
    Cap {
        object: Object::Endpoint(ep),
        rights,
    }
}

/// A capability naming a hardware interrupt. `READ` lets the holder `WAIT` and `ACK` it.
#[allow(dead_code)] // first used by the virtio driver setup in 9b
pub fn irq_cap(intid: u32) -> Cap {
    Cap {
        object: Object::Irq(intid),
        rights: Rights::READ,
    }
}

/// A capability to an untyped memory region. `WRITE` lets the holder retype pages out of it.
pub fn untyped_cap(region: usize) -> Cap {
    Cap {
        object: Object::Untyped(region),
        rights: Rights::WRITE,
    }
}

/// A capability to a virtio device's transport. `WRITE` lets the holder operate it.
pub fn virtio_cap(id: usize) -> Cap {
    Cap {
        object: Object::Virtio(id),
        rights: Rights::WRITE,
    }
}

/// A capability naming a thread under construction (milestone 19c.3). Full rights at creation.
pub fn tcb_cap(tid: crate::thread::Tid, rights: Rights) -> Cap {
    Cap {
        object: Object::Tcb(tid),
        rights,
    }
}

/// A capability naming an address space under construction (milestone 19b). Full rights at
/// creation; delegation narrows, as everywhere.
pub fn aspace_cap(name: u64, rights: Rights) -> Cap {
    Cap {
        object: Object::Aspace(name),
        rights,
    }
}

/// A one-shot reply capability naming the caller `tid` (milestone 12). Minted with `WRITE` (may
/// answer) and **no `GRANT`** (cannot be delegated onward), so it is non-transferable as well as
/// single-use. The kernel is the only minter, at a `CALL` rendezvous.
pub fn reply_cap(tid: crate::thread::Tid) -> Cap {
    Cap {
        object: Object::Reply(tid),
        rights: Rights::WRITE,
    }
}

/// A capability naming a physical page. `READ` lets the holder map it read-only, `WRITE` lets it
/// map it read/write, `GRANT` lets it pass the page on. A freshly retyped frame gets all three;
/// delegation narrows them (a read-only, non-lendable view is `READ` alone).
/// A capability naming a device's MMIO page (milestone 19d.2). `WRITE` lets the holder map it
/// (device access is read/write by nature); minted by the kernel for a known device.
pub fn device_frame_cap(phys: u64, rights: Rights) -> Cap {
    Cap {
        object: Object::DeviceFrame(phys),
        rights,
    }
}

pub fn frame_cap(phys: u64, rights: Rights) -> Cap {
    Cap {
        object: Object::Frame(phys),
        rights,
    }
}
