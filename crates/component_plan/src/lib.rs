//! **`component_plan`: what a component declares it needs, and how a supervisor wires it**
//! (milestone 23, DECISIONS §41).
//!
//! A component is a swappable, vendor-shippable unit behind a stable contract. §41 says what makes
//! one substitutable: *any program that speaks the protocol and holds the right capabilities **is**
//! the component*. The protocol half of that sentence has always been in a crate. The capability
//! half was a set of literal arrays in the operator's own source, which is the defect milestone 23's
//! roadmap block names in six words: **"endowments are literals in the operator's source."** This
//! crate is the capability half, lifted out to where a declaration can be read, checked, and proved.
//!
//! It performs no IO and makes no syscalls. It turns two declarations into a decision: a
//! [`Requirements`] (what the component says it needs) plus a [`Provisions`] (what the supervisor
//! will route to *this* child) into a [`Plan`] (exactly the capabilities and mappings to endow) or a
//! typed [`Refusal`]. See notes/component-manifest.md.
//!
//! # It is not `grant_plan::Manifest`, and the difference is one sentence
//!
//! [`grant_plan::Manifest`](../grant_plan/struct.Manifest.html) declares **what a human at a prompt
//! may designate** for a program; [`Requirements`] declares **what a supervisor must route before a
//! component can serve anyone**. Four things follow from that, and they are why this is a sibling
//! rather than a subtype:
//!
//! - **There is no command line and no human.** `grant_plan`'s thesis is that designation is
//!   authorization: a person names a file and the naming is the grant. A component is started by a
//!   supervisor with nothing typed, so every field about placing a typed token (`arg`, `flags`,
//!   `file`, `dir`) has no meaning here, and there is no prompt at which to refuse.
//! - **The vocabularies do not overlap.** Nothing in `grant_plan::Manifest` can name an endpoint, a
//!   device, or a shared page; nothing here can name an argument or a short option.
//! - **A component *serves*, and a program only consumes.** [`Direction`] is the axis this crate
//!   adds, and it is the axis §41's central refusal test turns on: `Serve` is `READ` on an endpoint
//!   and `Use` is `WRITE`, so a client of the stable endpoint cannot become its server. `grant_plan`
//!   has no field that could carry that, and no program it spawns would ever set one.
//! - **The keys are different, and this is the one that decides drop-in.** `grant_plan::Manifest` is
//!   reachable only through `Prog`, a closed enum with a static table compiled into the shell, so
//!   admitting a new component would mean editing the shell. [`Requirements`] is a value the
//!   *contract* owns, so a build that speaks the contract is wired by code that never heard of it.
//!
//! What the two **do** share is a shape, and the shape is the reusable idea rather than the struct:
//! a declaration, plus what the wirer holds, checked into an endowment or a typed refusal, before
//! anything is spawned. `grant_plan::plan` and [`plan`] are the same function over different worlds.
//!
//! # A manifest is a request; the provisions are the authority
//!
//! This is Fuchsia's `use`/`offer` split, and it is what keeps a vendor's manifest from being a
//! privilege-escalation surface. A component names its needs, and it can name anything it likes. The
//! supervisor decides, per child, which of its own objects each name resolves to, and a name it did
//! not list resolves to nothing: [`Refusal::Unprovided`], **before the component is built**. So a
//! manifest cannot widen a component's authority; it can only fail to be satisfiable.
//!
//! The corollary is the property that makes a component substitutable at all: **the name is the
//! component's and the object is the supervisor's.** `chatty`'s manifest asks to *use* an endpoint it
//! calls `service`; on the direct channel the operator routes that to the shared service endpoint,
//! and on the queued channel it routes the same name to the broker's front endpoint. One
//! declaration, two wirings, and the component cannot tell which it got.
//!
//! # Structure is a compile error; provisioning is a runtime refusal
//!
//! The two halves are checked on different rungs of the ladder, deliberately.
//!
//! A malformed *declaration* (a role declared twice, two mappings at one address, a component that
//! asks to exist in zero pages) is caught by [`Requirements::problem`], which is a `const fn`. Every
//! real manifest in this tree asserts on it in a `const` item, so a malformed one **does not
//! compile**, on both architectures, without any test having to run.
//!
//! A *provisioning* mismatch cannot be caught that way, because what a supervisor holds is only
//! known when it runs. That is [`plan`]'s job, and its answer is a value the supervisor reports.
//!
//! # EXAMPLES
//!
//! A component declares two capabilities and one page, and a supervisor wires it:
//!
//! ```
//! use component_plan::{CapNeed, Direction, MapNeed, PageKind, Provisions, Requirements};
//!
//! /// The contract's capability half. Lives beside the contract's wire half, not in the operator.
//! pub const TALKER: Requirements = Requirements {
//!     contract: "talker",
//!     caps: &[
//!         CapNeed { role: "service", direction: Direction::Serve },
//!         CapNeed { role: "report", direction: Direction::Use },
//!     ],
//!     maps: &[MapNeed { role: "witness", va: 0x0300_0000, kind: PageKind::Shared }],
//!     pages: 32,
//! };
//!
//! // Malformed declarations do not compile. This one is well formed, so the assertion holds.
//! const _: () = assert!(TALKER.problem().is_none());
//!
//! // The slot a component reads is derived from its own declaration, so the two cannot drift.
//! pub const SERVICE: u64 = component_plan::slot_of(&TALKER, "service");
//! assert_eq!(SERVICE, 0);
//!
//! // The supervisor routes each name to one of its own slots, per child.
//! let plan = component_plan::plan(
//!     &TALKER,
//!     &Provisions { held: &[("service", 5), ("report", 1), ("witness", 9)] },
//! )
//! .expect("every declared role is routed");
//!
//! // Rights come from the declared direction. The supervisor never spells one.
//! assert_eq!(plan.caps(), &[(5, abi::rights::READ), (1, abi::rights::WRITE)]);
//! assert_eq!(plan.maps(), &[(0x0300_0000, 9, abi::aspace::MAP_RW)]);
//! assert_eq!(plan.pages(), 32);
//! ```
//!
//! A supervisor that does not hold what the component asks for is refused, and nothing is built:
//!
//! ```
//! # use component_plan::{CapNeed, Direction, Provisions, Refusal, Requirements};
//! # const TALKER: Requirements = Requirements {
//! #     contract: "talker",
//! #     caps: &[CapNeed { role: "service", direction: Direction::Serve }],
//! #     maps: &[],
//! #     pages: 32,
//! # };
//! let refused = component_plan::plan(&TALKER, &Provisions { held: &[("report", 1)] });
//! assert_eq!(refused, Err(Refusal::Unprovided { role: "service" }));
//! ```
//!
//! # BUGS
//!
//! **A manifest is compiled in, not shipped in the archive beside the build.** So a vendor cannot
//! today hand over a binary and have its needs read out of it; what this crate removes is the
//! endowment from the *operator*, not from the tree. Reading a manifest out of an ELF section or an
//! archive member is a format two programs agree on, which is the expensive, irreversible category
//! (AGENTS.md), so it is deliberately left for the architect rather than decided by a lane. See
//! notes/component-manifest.md for what that decision would cost and buy.
//!
//! **[`Requirements::pages`] is a property of the build, not of the contract**, and it is the only
//! field here that is. How many pages an instance needs depends on its image size, its stack and its
//! page tables, so two vendors' builds of one contract can honestly differ. Declaring it per contract
//! is right only while every build of a contract fits the same number, which is true in this tree and
//! is not true in general. It is the strongest single argument for eventually shipping the manifest
//! with the binary.
//!
//! **A malformed declaration's compile error names the manifest, not the role.** `const` evaluation
//! reports an assertion that failed, so the reader is told which `Requirements` is wrong and has to
//! look for the duplicate themselves. [`Requirements::problem`] returns the [`Refusal`] with the role
//! in it, and a host test that calls it prints the good message; the `const` assertion cannot.
//!
//! **[`MAX_CAPS`] and [`MAX_MAPS`] are fixed, and a component past them is refused rather than
//! served.** There is no allocator here and a [`Plan`] is a value, so the bound is a real limit and
//! not a hint. Eight and four are roughly double what any manifest in this tree asks for.
//!
//! **Nothing checks that a supervisor's routed object is the *kind* the role wants.** A supervisor
//! that routes a frame where the component declared an endpoint gets a plan, and the kernel refuses
//! the `CAP_INSERT` or the component's first `RECV` instead. The declaration carries the direction
//! and the address, which is what a supervisor can get wrong silently; the object type is what the
//! kernel already checks loudly.
//!
//! Name: provisional, minted by this lane (milestone 23). It echoes `grant_plan` on purpose,
//! because the two crates are the same shape over different worlds and the echo is the part worth
//! teaching; the distinguishing word carries the difference. Refused `component_manifest` and
//! `manifest` (a second `Manifest` in a tree that already has `grant_plan::Manifest` is the
//! comprehension disaster this crate's own header argues against), and `capability_declaration` (a
//! declaration is only half of it; the wiring is the other half and is where the checking lives).

#![no_std]

/// **How many capabilities one component may declare.** A [`Plan`] is a value in a `no_std` program
/// with no allocator, so this is a hard bound rather than a hint: a manifest past it is
/// [`Refusal::TooManyCaps`].
///
/// Eight, which is roughly double the four that the widest manifest in this tree asks for, and which
/// leaves room below `abi::fault::FAULT_EP_SLOT` for the placed slots a component might also want.
pub const MAX_CAPS: usize = 8;

/// **How many pages one component may declare.** [`MAX_CAPS`]'s twin, for the mapping half. Four,
/// against the two that the widest manifest here asks for.
pub const MAX_MAPS: usize = 4;

/// **Which way an endpoint points**, which is the axis this crate adds over a program's manifest and
/// the reason a component's declaration is a different object from a program's.
///
/// It is a required field with no default, and that is deliberate in `writes_while_reading`'s shape
/// (AGENTS.md's rung one): a component **cannot declare an endpoint without saying whether it
/// answers on it**. The rights fall out of the answer, so the supervisor never spells `READ` or
/// `WRITE` and cannot spell one wrong.
///
/// The stakes are §41's central refusal. `SEND` and `RECV` are gated by different rights on the same
/// object, so the same endpoint handed out two ways is a one-way pipe in whichever direction each
/// holder was trusted with. A typo that gave a client `READ` would let it park in `RECV_CAP` and take
/// its own server's requests, and the test that catches that
/// (`a_client_of_the_stable_endpoint_cannot_become_its_server`) would fail for a reason no reader
/// could see from the operator's source. There is now nothing to typo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// **The component answers here.** Requests arrive on this endpoint and it replies: `READ`.
    Serve,
    /// **The component asks here, and only asks.** It sends and never receives: `WRITE`.
    Use,
}

impl Direction {
    /// The rights a child is endowed with for an endpoint it declared this way. The only place in
    /// the tree a component's endpoint rights are decided.
    pub const fn rights(self) -> u64 {
        match self {
            Direction::Serve => abi::rights::READ,
            Direction::Use => abi::rights::WRITE,
        }
    }
}

/// **One capability a component declares it needs**, named by the component and routed by the
/// supervisor.
///
/// The `role` is the component's own word for what it needs the object *for*, never the
/// supervisor's word for the object. That is what lets one declaration be wired two ways: `chatty`
/// asks to use `service`, and whether that is the shared endpoint or a queue broker's front endpoint
/// is the supervisor's business and invisible to the component.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapNeed {
    /// The component's name for this need. Unique within one [`Requirements`]; a repeat is
    /// [`Refusal::DuplicateRole`] at compile time.
    pub role: &'static str,
    /// Which way it points. See [`Direction`] for why this has no default.
    pub direction: Direction,
}

/// **What kind of page a mapping need is for.** Two kinds, because the two behave differently at the
/// one moment a live replacement cares about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageKind {
    /// Ordinary memory the supervisor shares with the component, mapped read/write.
    Shared,
    /// A device's registers.
    ///
    /// Called out as its own kind for one reason, and it is §41's: revocation is by physical page, so
    /// a device the supervisor is about to take back **cannot be installed at build time**. A plan
    /// sorts these last and [`Plan::devices`] hands them over separately, so the operator can endow
    /// everything else early and map the registers on the far side of the revoke, which is what keeps
    /// the down window four syscalls wide.
    DeviceRegisters,
}

impl PageKind {
    /// The mapping mode this kind is installed with. Device registers are mapped device-typed
    /// read/write by the kernel whatever mode is asked for, so the mode a device need carries is
    /// the conservative one rather than a claim.
    pub const fn mode(self) -> u64 {
        match self {
            PageKind::Shared => abi::aspace::MAP_RW,
            PageKind::DeviceRegisters => abi::aspace::MAP_RO,
        }
    }
}

/// **One page a component declares it needs, and the address it needs it at.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapNeed {
    /// The component's name for this need, routed exactly as a [`CapNeed`]'s is.
    pub role: &'static str,
    /// The virtual address the component reads it at. It is in the declaration rather than in the
    /// supervisor because it is a fact about the component's own code: the component compiles the
    /// address into a `read_volatile`, and a supervisor that mapped it somewhere else would produce
    /// a component that faults for a reason a long way from the mistake.
    pub va: u64,
    /// Shared memory or a device's registers. See [`PageKind`].
    pub kind: PageKind,
}

/// **What a component declares it needs.** The capability half of a contract, beside the wire half.
///
/// The order of [`caps`](Requirements::caps) is load-bearing: it **is** the component's cspace slot
/// order, because `supervision_proto::ChildEndowment::caps` lands its entries in slots 0, 1, 2, ...
/// Before this crate that agreement was a comment in two files. Now the component derives its own
/// slot numbers from this list with [`slot_of`], so a reordered manifest is a compile error rather
/// than a component reading the wrong slot at run time.
///
/// What is **not** here is as deliberate as what is. Supervision is not declared, because a
/// component does not choose whether it is watched: that is the supervisor's decision and it is done
/// *to* the component (§32). Start arguments are not declared either, because they are
/// configuration rather than authority: which log entries this instance writes and whether it was
/// handed a device are facts about the wiring, and the component is told them rather than asking.
/// A manifest declares what a component **needs**, not everything about how it is started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Requirements {
    /// The contract this component implements, for a reader and for a diagnostic. Not matched
    /// against anything: what makes a build substitutable is that it speaks the protocol, and a
    /// string it carries cannot prove that.
    pub contract: &'static str,
    /// The capabilities it needs, **in cspace slot order**. See the struct docs.
    pub caps: &'static [CapNeed],
    /// The pages it needs mapped, each at the address its own code reads.
    pub maps: &'static [MapNeed],
    /// How many pages of the supervisor's budget one instance is built out of: its segments, its
    /// stack, its address-space root, its TCB, its page tables and its revocation log.
    ///
    /// The one field here that is a property of the **build** rather than of the contract, which is
    /// the crate's second `BUGS` entry and the strongest argument for eventually shipping a manifest
    /// with the binary that satisfies it.
    pub pages: u64,
}

impl Requirements {
    /// **Whether this declaration is well formed at all**, independent of any supervisor.
    ///
    /// A `const fn`, and that is the point: every real manifest in this tree asserts on it in a
    /// `const` item, so a malformed declaration does not compile. Three things are checked, and each
    /// one is a mistake that would otherwise be silent:
    ///
    /// - **A role declared twice.** The component would be handed two slots and read one of them,
    ///   and nothing at run time distinguishes that from a correct wiring.
    /// - **Two pages at one address.** One mapping silently wins; which one depends on the order the
    ///   supervisor happened to install them.
    /// - **Zero pages.** A component nothing pays for cannot be built, and the failure would arrive
    ///   as a region split refusing for no visible reason.
    ///
    /// Over-long declarations are **not** checked here, because [`MAX_CAPS`] is this crate's limit
    /// rather than the declaration's defect: it is [`plan`]'s refusal, where a reader meets the
    /// bound.
    pub const fn problem(&self) -> Option<Refusal> {
        if self.pages == 0 {
            return Some(Refusal::NoPages);
        }
        let mut i = 0;
        while i < self.caps.len() {
            let mut j = i + 1;
            while j < self.caps.len() {
                if str_eq(self.caps[i].role, self.caps[j].role) {
                    return Some(Refusal::DuplicateRole {
                        role: self.caps[i].role,
                    });
                }
                j += 1;
            }
            i += 1;
        }
        let mut i = 0;
        while i < self.maps.len() {
            let mut j = i + 1;
            while j < self.maps.len() {
                if str_eq(self.maps[i].role, self.maps[j].role) {
                    return Some(Refusal::DuplicateRole {
                        role: self.maps[i].role,
                    });
                }
                if self.maps[i].va == self.maps[j].va {
                    return Some(Refusal::OverlappingVa {
                        va: self.maps[i].va,
                    });
                }
                j += 1;
            }
            i += 1;
        }
        None
    }
}

/// **What a supervisor will route to one child**, as its own slot per role name.
///
/// Per child rather than per supervisor, and that is Fuchsia's routing rather than an ambient
/// environment: the same supervisor hands the same role name to two children and may resolve it to
/// two different objects, which is exactly what makes a component's peer substitutable. A role the
/// supervisor does not list resolves to nothing, so a manifest can never widen its own authority.
///
/// A supervisor may list roles nothing needs. That is not an error: it holds more than any one child
/// requires, and refusing the surplus would make one table per child mandatory for no gain.
#[derive(Clone, Copy, Debug)]
pub struct Provisions<'a> {
    /// `(role name, the supervisor's own cspace slot holding the object)`.
    pub held: &'a [(&'a str, u64)],
}

impl<'a> Provisions<'a> {
    /// The slot this supervisor routes `role` to, or `None`.
    pub fn slot_for(&self, role: &str) -> Option<u64> {
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].0 == role {
                return Some(self.held[i].1);
            }
            i += 1;
        }
        None
    }
}

/// **Why a component could not be wired.** A value the supervisor reports, never a panic: a
/// supervisor that cannot satisfy a manifest has a legible thing to say and a child it did not
/// build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The supervisor routes nothing to a role the component declared. **The security-relevant
    /// one**: it is what stops a manifest from being a request the supervisor is obliged to honour.
    Unprovided {
        /// The component's name for the need that went unrouted.
        role: &'static str,
    },
    /// One role name appears twice in a declaration.
    DuplicateRole {
        /// The name that repeats.
        role: &'static str,
    },
    /// Two pages are declared at one virtual address.
    OverlappingVa {
        /// The address they collide at.
        va: u64,
    },
    /// The declaration asks for more capabilities than [`MAX_CAPS`].
    TooManyCaps {
        /// How many it asked for.
        asked: usize,
    },
    /// The declaration asks for more pages than [`MAX_MAPS`].
    TooManyMaps {
        /// How many it asked for.
        asked: usize,
    },
    /// The declaration asks to exist in zero pages.
    NoPages,
}

impl Refusal {
    /// **One word, so a refusal can be reported over IPC.** A supervisor inside a test has two
    /// registers and no way to send a string, and a refusal that only exists as a `&'static str` in
    /// a dead process is a refusal nothing can assert on.
    pub const fn code(self) -> u64 {
        match self {
            Refusal::Unprovided { .. } => 1,
            Refusal::DuplicateRole { .. } => 2,
            Refusal::OverlappingVa { .. } => 3,
            Refusal::TooManyCaps { .. } => 4,
            Refusal::TooManyMaps { .. } => 5,
            Refusal::NoPages => 6,
        }
    }

    /// What a person is told. `grant_plan::Refusal::message`'s convention: a sentence about this
    /// system's model, not a Unix errno translated.
    pub const fn message(self) -> &'static str {
        match self {
            Refusal::Unprovided { .. } => "this supervisor routes nothing to a role it declares",
            Refusal::DuplicateRole { .. } => "it declares one role name twice",
            Refusal::OverlappingVa { .. } => "it declares two pages at one address",
            Refusal::TooManyCaps { .. } => "it declares more capabilities than a plan can carry",
            Refusal::TooManyMaps { .. } => "it declares more pages than a plan can carry",
            Refusal::NoPages => "it declares no memory to be built in",
        }
    }
}

/// **Exactly what to endow one child with**, in the form `supervision_proto::ChildEndowment` takes.
///
/// A value with no allocation, bounded by [`MAX_CAPS`] and [`MAX_MAPS`], so a supervisor holds it on
/// its stack and hands out slices of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    caps: [(u64, u64); MAX_CAPS],
    ncaps: usize,
    maps: [(u64, u64, u64); MAX_MAPS],
    nmaps: usize,
    ndevices: usize,
    pages: u64,
}

impl Plan {
    /// `(the supervisor's slot, the child's rights)` per declared capability, **in declared order**,
    /// which is the child's slot order. Feeds `ChildEndowment::caps` directly.
    pub fn caps(&self) -> &[(u64, u64)] {
        &self.caps[..self.ncaps]
    }

    /// `(child va, the supervisor's slot, mode)` per declared page. Feeds `ChildEndowment::maps`.
    ///
    /// Device registers sort last. See [`maps_without_devices`](Plan::maps_without_devices) for the
    /// reason and for the swap that needs it.
    pub fn maps(&self) -> &[(u64, u64, u64)] {
        &self.maps[..self.nmaps]
    }

    /// **Everything but the device registers**, for a supervisor that is about to revoke a device.
    ///
    /// §41: revocation is by physical page, so a replacement endowed with the registers before the
    /// revoke would lose its copy to the revoke, and since the kernel mints a device capability once
    /// at boot nothing could hand one back. A supervisor building a replacement therefore endows it
    /// with this, revokes, and then installs [`devices`](Plan::devices) into the child's address
    /// space. That is what keeps the down window four syscalls wide.
    pub fn maps_without_devices(&self) -> &[(u64, u64, u64)] {
        &self.maps[..self.nmaps - self.ndevices]
    }

    /// The device mappings [`maps_without_devices`](Plan::maps_without_devices) left out, to be
    /// installed on the far side of a revoke.
    pub fn devices(&self) -> &[(u64, u64, u64)] {
        &self.maps[self.nmaps - self.ndevices..self.nmaps]
    }

    /// How many pages of the supervisor's budget one instance costs.
    pub fn pages(&self) -> u64 {
        self.pages
    }
}

/// **Check a component's declaration against what a supervisor will route to it**, and yield the
/// endowment or say why not.
///
/// This is the whole of the mechanism, and every check in it is one a supervisor would otherwise
/// have made by hand or not at all. Nothing here spawns, maps or invokes: a refusal happens
/// **before the component exists**, which is the property the guest test asserts on by reporting the
/// refusal ahead of the first build step.
pub fn plan(reqs: &Requirements, provisions: &Provisions<'_>) -> Result<Plan, Refusal> {
    if let Some(problem) = reqs.problem() {
        return Err(problem);
    }
    if reqs.caps.len() > MAX_CAPS {
        return Err(Refusal::TooManyCaps {
            asked: reqs.caps.len(),
        });
    }
    if reqs.maps.len() > MAX_MAPS {
        return Err(Refusal::TooManyMaps {
            asked: reqs.maps.len(),
        });
    }

    let mut out = Plan {
        caps: [(0, 0); MAX_CAPS],
        ncaps: reqs.caps.len(),
        maps: [(0, 0, 0); MAX_MAPS],
        nmaps: reqs.maps.len(),
        ndevices: 0,
        pages: reqs.pages,
    };

    for (i, need) in reqs.caps.iter().enumerate() {
        let Some(slot) = provisions.slot_for(need.role) else {
            return Err(Refusal::Unprovided { role: need.role });
        };
        out.caps[i] = (slot, need.direction.rights());
    }

    // Two passes over the mapping needs so device registers land at the end of the array whatever
    // order they were declared in. That ordering is what lets `maps_without_devices` be a slice of
    // this same array rather than a second copy a `no_std` caller has nowhere to put.
    let mut n = 0;
    for pass in [PageKind::Shared, PageKind::DeviceRegisters] {
        for need in reqs.maps.iter().filter(|m| m.kind == pass) {
            let Some(slot) = provisions.slot_for(need.role) else {
                return Err(Refusal::Unprovided { role: need.role });
            };
            out.maps[n] = (need.va, slot, need.kind.mode());
            n += 1;
            if pass == PageKind::DeviceRegisters {
                out.ndevices += 1;
            }
        }
    }

    Ok(out)
}

/// **The cspace slot a component reads one of its own needs in**, derived from its declaration.
///
/// This is the rung the whole crate is built to reach. The slot agreement used to be a pair of
/// constants written twice, once as `pub const SVC: u64 = 0` in the contract and once as the position
/// of an entry in a literal array in the operator, with a comment in each file asking the reader to
/// check. Now the constant *is* the position, computed at compile time, and a role that the manifest
/// does not declare **does not compile**.
///
/// # Panics
///
/// If `role` is not declared. In a `const` item, which is the only way this is meant to be called,
/// that is a compile error rather than a panic.
///
/// ```compile_fail
/// # use component_plan::{CapNeed, Direction, Requirements};
/// const R: Requirements = Requirements {
///     contract: "talker",
///     caps: &[CapNeed { role: "service", direction: Direction::Serve }],
///     maps: &[],
///     pages: 32,
/// };
/// // There is no `control` role, so this constant cannot be evaluated.
/// const CONTROL: u64 = component_plan::slot_of(&R, "control");
/// ```
pub const fn slot_of(reqs: &Requirements, role: &str) -> u64 {
    let mut i = 0;
    while i < reqs.caps.len() {
        if str_eq(reqs.caps[i].role, role) {
            return i as u64;
        }
        i += 1;
    }
    panic!("this component declares no capability by that role name")
}

/// Byte equality on two strings, in `const` context. `str`'s own `==` is not a `const fn`, and every
/// check in this crate that has to run at compile time needs one.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE: CapNeed = CapNeed {
        role: "service",
        direction: Direction::Serve,
    };
    const REPORT: CapNeed = CapNeed {
        role: "report",
        direction: Direction::Use,
    };
    const WITNESS: MapNeed = MapNeed {
        role: "witness",
        va: 0x0300_0000,
        kind: PageKind::Shared,
    };
    const UART: MapNeed = MapNeed {
        role: "uart",
        va: 0x0310_0000,
        kind: PageKind::DeviceRegisters,
    };

    const CONSOLE: Requirements = Requirements {
        contract: "console",
        caps: &[SERVICE, REPORT],
        maps: &[WITNESS, UART],
        pages: 32,
    };

    fn everything() -> Provisions<'static> {
        Provisions {
            held: &[("service", 5), ("report", 1), ("witness", 9), ("uart", 2)],
        }
    }

    #[test]
    fn a_serve_gets_read_and_a_use_gets_write() {
        let p = plan(&CONSOLE, &everything()).unwrap();
        assert_eq!(p.caps(), &[(5, abi::rights::READ), (1, abi::rights::WRITE)]);
    }

    /// The one that would otherwise be a comment: the plan's cap order is the declaration's order,
    /// which is the component's slot order.
    #[test]
    fn the_declared_order_is_the_slot_order() {
        assert_eq!(slot_of(&CONSOLE, "service"), 0);
        assert_eq!(slot_of(&CONSOLE, "report"), 1);
        let p = plan(&CONSOLE, &everything()).unwrap();
        assert_eq!(p.caps()[slot_of(&CONSOLE, "report") as usize].0, 1);
    }

    #[test]
    fn a_role_the_supervisor_does_not_route_is_refused() {
        let partial = Provisions {
            held: &[("report", 1), ("witness", 9), ("uart", 2)],
        };
        assert_eq!(
            plan(&CONSOLE, &partial),
            Err(Refusal::Unprovided { role: "service" })
        );
    }

    /// A missing *page* refuses exactly as a missing capability does. Worth its own test because the
    /// two are checked in different loops.
    #[test]
    fn a_page_the_supervisor_does_not_route_is_refused_too() {
        let no_device = Provisions {
            held: &[("service", 5), ("report", 1), ("witness", 9)],
        };
        assert_eq!(
            plan(&CONSOLE, &no_device),
            Err(Refusal::Unprovided { role: "uart" })
        );
    }

    /// A supervisor holding more than the component asks for is wired, not refused: it holds what it
    /// holds, and one table per child would be mandatory otherwise.
    #[test]
    fn a_surplus_provision_is_not_an_error() {
        let extra = Provisions {
            held: &[
                ("service", 5),
                ("report", 1),
                ("witness", 9),
                ("uart", 2),
                ("nothing_needs_this", 77),
            ],
        };
        assert!(plan(&CONSOLE, &extra).is_ok());
    }

    /// §41's device deferral, which is the reason [`PageKind::DeviceRegisters`] is a kind at all.
    #[test]
    fn the_device_is_separable_from_the_rest_of_the_mappings() {
        let p = plan(&CONSOLE, &everything()).unwrap();
        assert_eq!(
            p.maps_without_devices(),
            &[(0x0300_0000, 9, abi::aspace::MAP_RW)]
        );
        assert_eq!(p.devices(), &[(0x0310_0000, 2, abi::aspace::MAP_RO)]);
    }

    /// And it is separable whatever order the declaration used, because a plan sorts rather than
    /// trusting the manifest's author to put the device last.
    #[test]
    fn a_device_declared_first_still_sorts_last() {
        const DEVICE_FIRST: Requirements = Requirements {
            contract: "console",
            caps: &[SERVICE],
            maps: &[UART, WITNESS],
            pages: 32,
        };
        let p = plan(&DEVICE_FIRST, &everything()).unwrap();
        assert_eq!(p.devices(), &[(0x0310_0000, 2, abi::aspace::MAP_RO)]);
        assert_eq!(p.maps_without_devices().len(), 1);
    }

    /// A component with no device at all: the empty-device case must not be a panic in the
    /// arithmetic that splits the array.
    #[test]
    fn a_component_with_no_device_has_no_deferred_mapping() {
        const PLAIN: Requirements = Requirements {
            contract: "backend",
            caps: &[SERVICE],
            maps: &[WITNESS],
            pages: 32,
        };
        let p = plan(&PLAIN, &everything()).unwrap();
        assert!(p.devices().is_empty());
        assert_eq!(p.maps_without_devices(), p.maps());
    }

    #[test]
    fn a_role_declared_twice_is_malformed() {
        const TWICE: Requirements = Requirements {
            contract: "confused",
            caps: &[SERVICE, SERVICE],
            maps: &[],
            pages: 32,
        };
        assert_eq!(
            TWICE.problem(),
            Some(Refusal::DuplicateRole { role: "service" })
        );
        assert_eq!(plan(&TWICE, &everything()).unwrap_err().code(), 2);
    }

    #[test]
    fn two_pages_at_one_address_are_malformed() {
        const COLLIDE: Requirements = Requirements {
            contract: "confused",
            caps: &[],
            maps: &[
                WITNESS,
                MapNeed {
                    role: "other",
                    va: WITNESS.va,
                    kind: PageKind::Shared,
                },
            ],
            pages: 32,
        };
        assert_eq!(
            COLLIDE.problem(),
            Some(Refusal::OverlappingVa { va: 0x0300_0000 })
        );
    }

    #[test]
    fn a_component_nothing_pays_for_is_malformed() {
        const FREE: Requirements = Requirements {
            contract: "confused",
            caps: &[],
            maps: &[],
            pages: 0,
        };
        assert_eq!(FREE.problem(), Some(Refusal::NoPages));
    }

    /// Every refusal has a distinct wire code, because the guest reports the code and a collision
    /// would make two different failures indistinguishable in a test.
    #[test]
    fn the_refusal_codes_are_distinct_and_never_zero() {
        let all = [
            Refusal::Unprovided { role: "x" },
            Refusal::DuplicateRole { role: "x" },
            Refusal::OverlappingVa { va: 0 },
            Refusal::TooManyCaps { asked: 0 },
            Refusal::TooManyMaps { asked: 0 },
            Refusal::NoPages,
        ];
        for (i, a) in all.iter().enumerate() {
            assert_ne!(a.code(), 0, "zero would read as \"no refusal\"");
            assert!(!a.message().is_empty());
            for b in &all[i + 1..] {
                assert_ne!(a.code(), b.code());
            }
        }
    }

    #[test]
    fn the_well_formed_declaration_is_the_common_case() {
        assert_eq!(CONSOLE.problem(), None);
    }
}

/// Machine-checked properties of the wiring. The host tests above pin the cases a reader cares
/// about; these rule out shapes a case-by-case test cannot, and the symbolism is in the **routing
/// table** rather than in the declaration.
///
/// That split is forced by the type and is worth understanding rather than working around.
/// [`Requirements`] holds `&'static` slices, so a declaration is a compile-time constant by
/// construction: it is what makes [`Requirements::problem`] and [`slot_of`] evaluable in a `const`
/// item, and it means **no manifest can be synthesised at run time out of bytes**, which is exactly
/// the property the wire-format entry in this crate's `BUGS` section would have to give up. So the
/// declarations below are a bounded enumeration of `const` values (both directions, all four
/// arrangements of the two page kinds), walked by a **concrete** loop, and what is symbolic is what a
/// supervisor actually varies: slot numbers, and which role names are routed at all.
///
/// The concrete loop is not a stylistic choice. Selecting a declaration with `SHAPES[kani::any()]`
/// makes the slice CBMC reads a symbolic pointer, and every slice access downstream of it becomes
/// symbolic too: that formulation put the solver past **seventy thousand** verification conditions
/// for a fact about copying integers, and did not finish in ninety seconds of symbolic execution.
/// Walking the universe concretely and leaving the *routing* symbolic proves the same statement
/// (both are "for every declaration of this width") and is a viable gate. Recorded because the
/// difference is invisible from the assertion.
///
/// See notes/component-manifest.md and notes/verification.md.
#[cfg(kani)]
mod proofs {
    use super::*;

    const SERVE_A: CapNeed = CapNeed {
        role: "a",
        direction: Direction::Serve,
    };
    const USE_A: CapNeed = CapNeed {
        role: "a",
        direction: Direction::Use,
    };
    const SERVE_B: CapNeed = CapNeed {
        role: "b",
        direction: Direction::Serve,
    };
    const USE_B: CapNeed = CapNeed {
        role: "b",
        direction: Direction::Use,
    };

    /// Every arrangement of two capability needs over the two directions. Four, and that is the whole
    /// space for two roles, so "for every declaration of this width" is exact rather than sampled.
    const SHAPES: [Requirements; 4] = [
        Requirements {
            contract: "c",
            caps: &[SERVE_A, SERVE_B],
            maps: &[],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[SERVE_A, USE_B],
            maps: &[],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[USE_A, SERVE_B],
            maps: &[],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[USE_A, USE_B],
            maps: &[],
            pages: 1,
        },
    ];

    /// **A slot a supervisor could route, bounded to the cspace that exists.**
    ///
    /// `abi::CSPACE_SLOTS` is sixteen, so a slot number outside `0..16` is not a routing a supervisor
    /// could make: the kernel refuses it. Bounding it here is not a weakening of the property, which
    /// is about *which* slot a need is wired to rather than how large the number is, and it is what
    /// makes these harnesses a viable gate: unbounded `u64` slots put the solver past seventy
    /// thousand verification conditions for a fact about copying two integers through.
    fn any_slot() -> u64 {
        let s: u64 = kani::any();
        kani::assume(s < abi::CSPACE_SLOTS);
        s
    }

    /// **No routing table ever gives a component a right its declaration did not ask for.**
    ///
    /// The security property this crate exists to make unrepresentable. For every declaration of two
    /// needs and **every pair of slot numbers a supervisor could route**, the rights word at slot `i`
    /// is exactly `caps[i].direction.rights()`. So nothing a supervisor does can turn a `Use` into a
    /// `READ` and let a client park itself in `RECV_CAP` on the endpoint it is a client of (§41), and
    /// nothing ever carries `GRANT`: a component that could pass its own authority on is not
    /// confined, and no declaration can ask for one.
    /// **The unwind bound is a termination proof, not a convenience.** Ten, against loops whose real
    /// bounds are two capability needs, two page needs, two routed names and a one-byte role name.
    /// It is needed at all because `SHAPES[i]` with a symbolic `i` makes the slice CBMC reads a
    /// symbolic pointer, so the lengths it walks are symbolic to the solver even though only four
    /// concrete declarations exist. Without a bound `str_eq` unwinds forever.
    #[kani::proof]
    #[kani::unwind(10)]
    fn a_plan_never_grants_a_right_the_declaration_did_not_ask_for() {
        let held: [(&str, u64); 2] = [("a", any_slot()), ("b", any_slot())];
        for reqs in &SHAPES {
            let p = plan(reqs, &Provisions { held: &held }).expect("both roles are routed");
            assert!(p.caps().len() == 2);
            let mut i = 0;
            while i < 2 {
                assert!(p.caps()[i].1 == reqs.caps[i].direction.rights());
                assert!(p.caps()[i].1 == abi::rights::READ || p.caps()[i].1 == abi::rights::WRITE);
                assert!(p.caps()[i].1 & abi::rights::GRANT == 0);
                assert!(p.caps()[i].0 == held[i].1);
                i += 1;
            }
        }
    }

    /// **A missing route refuses rather than falling through to a slot.**
    ///
    /// The failure this rules out is the quiet one. A lookup that fell through to zero would endow a
    /// child with whatever the supervisor keeps in slot 0, which in every program in this tree is its
    /// construction budget. Proved over every arrangement of the two names being routed or not, and
    /// over arbitrary slots.
    #[kani::proof]
    #[kani::unwind(10)]
    fn a_missing_route_refuses_rather_than_falling_through_to_a_slot() {
        // Each entry is present under its own name, or under a name nothing declares.
        let n0: &str = if kani::any() { "a" } else { "z" };
        let n1: &str = if kani::any() { "b" } else { "z" };
        let (s0, s1) = (any_slot(), any_slot());
        let held: [(&str, u64); 2] = [(n0, s0), (n1, s1)];
        for reqs in &SHAPES {
            match plan(reqs, &Provisions { held: &held }) {
                Ok(p) => {
                    // Wired at all only if both names were routed, and then to those slots and no
                    // others.
                    assert!(n0 == "a" && n1 == "b");
                    assert!(p.caps()[0].0 == s0 && p.caps()[1].0 == s1);
                }
                Err(r) => {
                    assert!(n0 != "a" || n1 != "b");
                    assert!(r.code() == Refusal::Unprovided { role: "a" }.code());
                }
            }
        }
    }

    const SHARED_P: MapNeed = MapNeed {
        role: "p",
        va: 0x1000,
        kind: PageKind::Shared,
    };
    const DEVICE_P: MapNeed = MapNeed {
        role: "p",
        va: 0x1000,
        kind: PageKind::DeviceRegisters,
    };
    const SHARED_Q: MapNeed = MapNeed {
        role: "q",
        va: 0x2000,
        kind: PageKind::Shared,
    };
    const DEVICE_Q: MapNeed = MapNeed {
        role: "q",
        va: 0x2000,
        kind: PageKind::DeviceRegisters,
    };

    /// Every arrangement of two page needs over the two kinds, which is the whole space for two
    /// pages: neither a device, either one, or both.
    const PAGE_SHAPES: [Requirements; 4] = [
        Requirements {
            contract: "c",
            caps: &[],
            maps: &[SHARED_P, SHARED_Q],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[],
            maps: &[DEVICE_P, SHARED_Q],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[],
            maps: &[SHARED_P, DEVICE_Q],
            pages: 1,
        },
        Requirements {
            contract: "c",
            caps: &[],
            maps: &[DEVICE_P, DEVICE_Q],
            pages: 1,
        },
    ];

    /// **The device split is a partition, for every declaration.**
    ///
    /// This is the arithmetic §41's four-syscall down window rests on. The operator endows a
    /// replacement with [`Plan::maps_without_devices`] and installs [`Plan::devices`] after the
    /// revoke, so an entry falling in both would be mapped twice and one falling in neither would
    /// leave a component reading an unmapped page. Proved for every arrangement of kinds, including
    /// a device declared first, which is what the sort exists for.
    #[kani::proof]
    #[kani::unwind(10)]
    fn the_device_split_partitions_the_mappings() {
        let held: [(&str, u64); 2] = [("p", any_slot()), ("q", any_slot())];
        for reqs in &PAGE_SHAPES {
            let p = plan(reqs, &Provisions { held: &held }).expect("both pages are routed");
            let (early, late) = (p.maps_without_devices(), p.devices());
            assert!(p.maps().len() == 2);
            assert!(early.len() + late.len() == p.maps().len());

            let mut declared_devices = 0;
            let mut k = 0;
            while k < reqs.maps.len() {
                if reqs.maps[k].kind == PageKind::DeviceRegisters {
                    declared_devices += 1;
                }
                k += 1;
            }
            assert!(late.len() == declared_devices);
            // The late half is all devices and the early half is all ordinary shared pages, so the
            // operator's two-step endowment cannot install one of them at the wrong moment.
            for m in late {
                assert!(m.2 == PageKind::DeviceRegisters.mode());
            }
            for m in early {
                assert!(m.2 == PageKind::Shared.mode());
            }
        }
    }
}
