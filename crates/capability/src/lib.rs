//! Capability spaces. **A file descriptor table that can point at anything.**
//!
//! See notes/capabilities.md and DECISIONS.md §10. The one sentence:
//!
//! > A capability is a file descriptor that can point at *anything*, not just files.
//!
//! Same mechanism, generalized. **The unforgeability is boring, and that is the point**: this
//! table lives in kernel memory and userspace never sees a byte of it. Userspace sees an
//! integer. You cannot fabricate slot 7 for exactly the same reason you cannot fabricate `fd 7`.
//! There is no cryptography here and there is no magic. There is an array and a bounds check.
//!
//! Pure logic, so it compiles for the host and its tests run in milliseconds (DECISIONS §7).
//! Nothing in here knows what a console is; the kernel supplies the object type.
//!
//! Allocation-free since milestone 14 phase B.1: the table is a const-generic array, so a
//! cspace's size is part of its type and creating one cannot touch a heap. The kernel picks the
//! size once (16, in kernel/src/cap.rs); the tests and proofs pick small ones.
//!
//! # Examples
//!
//! Rights are a lattice, and the only operation that ever runs on the transfer path is
//! **intersection**: a capability you pass on can never carry more than you held. That is the
//! monotonicity the whole model rests on, so it is what an example should show.
//!
//! ```
//! use capability::Rights;
//!
//! let held = Rights::READ.union(Rights::WRITE);
//! assert!(held.allows(Rights::READ));
//! assert!(!held.allows(Rights::GRANT));
//!
//! // Handing it on with a mask can only ever narrow it.
//! let passed_on = held.intersect(Rights::READ.union(Rights::GRANT));
//! assert_eq!(passed_on, Rights::READ);
//! assert!(passed_on.is_subset_of(held));
//! ```
//!
//! There is no operation that widens, which is worth stating as code because it is a claim about
//! what is *absent*:
//!
//! ```
//! use capability::Rights;
//!
//! let read_only = Rights::READ;
//! // The best you can do with ALL on the other side is get back what you already had.
//! assert_eq!(read_only.intersect(Rights::ALL), read_only);
//! assert!(!read_only.intersect(Rights::ALL).allows(Rights::WRITE));
//! ```
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `caps`. Refused `caps` (a container's
//! name for what is the capability model: it exports `Cap`, `Rights`, `Object`, `Reap` and
//! `CSpace`) and `cap_space` (names one of five exports, and stutters as `cap_space::CSpace`).
//! `CSpace` itself stays, because it is seL4's own spelling.

#![no_std]
// milestone 68's doc ratchet: every public item in this crate is documented, and
// `script/lint`'s -D warnings keeps it that way. See notes/doc-coverage.md for the
// crates that are not there yet.
#![warn(missing_docs)]

/// What you may do with a capability.
///
/// **Rights only ever narrow.** [`CSpace::derive`] will not widen them, and that is not a policy
/// we remembered to enforce, it is the only operation the type offers. If delegation could
/// widen authority, the whole model is theatre: anyone could hand themselves the rights they
/// were denied by passing a capability to themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    /// No rights at all.
    pub const NONE: Rights = Rights(0);
    /// The right to read the object's contents.
    pub const READ: Rights = Rights(1 << 0);
    /// The right to modify the object's contents.
    pub const WRITE: Rights = Rights(1 << 1);

    /// The right to **pass this capability on**. Without it, you may use a thing and not lend it.
    ///
    /// This is the right that makes delegation a decision rather than an accident. Unix has no
    /// equivalent, which is why "the child inherits every fd" is the default and why
    /// `FD_CLOEXEC` had to be invented as an afterthought.
    pub const GRANT: Rights = Rights(1 << 2);

    /// The right to **learn what exists**, as distinct from acting on it (milestone 126).
    ///
    /// This is the kernel-level twin of `fs_proto`'s directory `ENUMERATE`, and it is the same
    /// argument one layer down: listing is a larger power than reading something you were handed,
    /// so it is a right of its own rather than a corner of `READ`. The name is deliberately the
    /// word the filesystem already uses, because a reader who has met one has met both.
    ///
    /// **Why it had to exist.** `endpoint::SURVEY` first shipped needing `READ`, and `READ` on a
    /// supervision endpoint is also what `RECV` and `REAP` take. A viewer granted `READ` could
    /// therefore reap a child, which is acting on a member of a domain rather than naming one.
    /// calef ruled on 2026-08-17 that **a domain names its members and never acts on them**, and
    /// one bit for three operations cannot express that. With this right the wrong operation is
    /// not refused to a viewer, it is unnameable by one.
    ///
    /// **Deliberately not pre-wired to other objects.** Only `Endpoint` consults it today. `Aspace`
    /// wants it when `pmap` is built (observe a mapping without being able to map) and `Untyped`
    /// wants it when `free` is built (ask what is committed without being able to `SPLIT` or
    /// `DESTROY`), and both are named in milestone 126's strata rather than built ahead of a
    /// consumer that can say what it needs. A right defined for three hypothetical callers is the
    /// speculative abstraction this tree declines.
    pub const ENUMERATE: Rights = Rights(1 << 3);

    /// **Every defined bit.** [`from_bits`](Self::from_bits) masks against this, so a right that is
    /// missing here is silently dropped at every delegation: adding a constant above without
    /// widening this mask produces a right that cannot survive being passed on.
    pub const ALL: Rights = Rights(0b1111);

    /// The raw bit pattern, for carrying rights across a boundary as an integer (a syscall
    /// register). [`from_bits`](Self::from_bits) is the inverse.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Rebuild rights from a raw bit pattern, keeping only defined bits. **The reverse of
    /// [`bits`](Self::bits), for rights that crossed a boundary as an integer.** Delegation names
    /// the rights to narrow a capability to by their bits (they travel in a syscall register), and
    /// this turns those bits back into a `Rights` the subset check can vet. Undefined bits are
    /// dropped, so a caller cannot conjure a right that does not exist.
    pub const fn from_bits(bits: u32) -> Rights {
        Rights(bits & Rights::ALL.0)
    }

    /// Every right either side holds.
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    /// Only the rights both sides hold. The one operation on the delegation path: it can only
    /// narrow, never widen, which is the monotonicity the whole model rests on.
    pub const fn intersect(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
    }

    /// Do I hold *at least* these rights?
    pub const fn allows(self, needed: Rights) -> bool {
        self.0 & needed.0 == needed.0
    }

    /// Is `self` no more than `other`? The subset test that [`CSpace::derive`] turns on.
    pub const fn is_subset_of(self, other: Rights) -> bool {
        self.0 & !other.0 == 0
    }
}

/// A capability: **an object, and what you may do with it.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap<O> {
    /// What this capability names.
    pub object: O,
    /// What the holder may do with it.
    pub rights: Rights,
}

impl<O> Cap<O> {
    /// **Mint a fresh capability to `object` carrying no more authority than this one.** The rule a
    /// fresh mint site *outside* [`CSpace::derive`] must honor by hand so that authority never
    /// widens: the child inherits the parent's rights, never a superset.
    ///
    /// `Untyped::SPLIT` is such a site (kernel/src/cap.rs, milestone 31): it carves a child budget
    /// off a parent untyped and mints a capability to it, and because that mint is not a `derive` the
    /// "derive never widens rights" proof does not reach it. Routing it through here puts it under the
    /// [`split_never_widens_rights`](../src/lib.rs) proof, so a spend-only (GRANT-less) untyped cannot
    /// split itself a GRANT-bearing child and manufacture the right its own capability was denied. See
    /// DECISIONS §16 and notes/verification.md.
    pub fn mint_child(&self, object: O) -> Cap<O> {
        Cap {
            object,
            rights: self.rights,
        }
    }
}

/// **May this supervision endpoint collect this thread's corpse?** (DECISIONS §32.)
///
/// The whole authorization decision of `endpoint::REAP`, as pure logic, so it is proved for every
/// input rather than tested on the cases we thought of. The kernel calls exactly this
/// (`sched::reap_supervised`) and then does the reclaim; nothing here mints, widens, or returns a
/// capability, which is why a supervisor gains no memory authority by reaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reap {
    /// Authorized: this endpoint supervises that thread, and the thread is a corpse.
    Permitted,
    /// The thread is supervised here but still running. Collecting a corpse is not killing.
    StillAlive,
    /// No thread by that name is supervised by *this* endpoint: it never existed, it is
    /// unsupervised, it belongs to another supervisor, or its name is stale.
    NotSupervised,
}

/// The decision, from the three facts the kernel has: the thread's recorded supervision endpoint
/// (`None` if the thread does not resolve at all, or resolves and is unsupervised), the endpoint
/// actually being invoked, and whether the thread is dead.
///
/// **Ordering matters and is deliberate.** The supervision check comes first, so a supervisor
/// learns nothing about a thread it does not supervise: a live stranger and a stale name give the
/// same answer. Only once the relationship is established does the liveness answer differ, because
/// only then is the distinction the holder's business.
pub const fn reap_decision(fault_ep: Option<u64>, invoked_ep: u64, dead: bool) -> Reap {
    match fault_ep {
        Some(ep) if ep == invoked_ep => {
            if dead {
                Reap::Permitted
            } else {
                Reap::StillAlive
            }
        }
        _ => Reap::NotSupervised,
    }
}

/// **Is this thread inside the domain that endpoint supervises?** (milestone 126,
/// `endpoint::SURVEY`.)
///
/// The whole scoping decision of a process view, as pure logic, so it is proved for every input
/// rather than tested on the cases we thought of. The kernel walks its thread table and reports an
/// entry exactly when this says so (`sched::survey_supervised`).
///
/// **It is deliberately the same predicate [`reap_decision`] authorizes with**, and that is the
/// point rather than a saving: the set of threads a supervisor may *see* is the set whose deaths
/// arrive on its endpoint, so a domain cannot drift out of agreement with the relationship the
/// kernel already maintains. Two predicates would have been two things to keep in step.
///
/// Liveness is not an input. A corpse is still in the domain, which is what makes `ps` able to show
/// a `Dead` child the supervisor has not collected yet.
pub const fn survey_includes(fault_ep: Option<u64>, invoked_ep: u64) -> bool {
    matches!(fault_ep, Some(ep) if ep == invoked_ep)
}

/// Why a cspace operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// **The slot is empty.** Note what this is *not*: it is not "permission denied." There is
    /// nothing there. The thing you tried to name does not exist, for you.
    NoSuchSlot,
    /// You asked to derive a capability with rights you do not hold.
    CannotWiden,
    /// The table is full.
    NoFreeSlot,
}

/// A thread's capability table.
///
/// **Flat, not a tree.** seL4 uses a tree of CNodes with guard bits, which buys enormous sparse
/// capability spaces and costs a great deal of explanation. We do not need it, and a flat array
/// is honest: it is an fd table with a type tag on each entry, which is exactly what a capability
/// space *is*. If we ever need the sparse version, it is a change to this file and nothing else.
pub struct CSpace<O, const N: usize> {
    slots: [Option<Cap<O>>; N],
}

impl<O: Copy, const N: usize> Default for CSpace<O, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: Copy, const N: usize> CSpace<O, N> {
    /// **A brand-new process holds nothing.**
    ///
    /// This is the whole decision, expressed as a constructor. Under Unix a fresh process
    /// inherits every fd its parent had and can `open()` anything its uid permits. Here it can
    /// name nothing at all until somebody hands it something.
    pub const fn new() -> Self {
        CSpace {
            slots: [const { None }; N],
        }
    }

    /// The table's fixed capacity, `N`. Never changes; a cspace's size is part of its type.
    pub const fn len(&self) -> usize {
        N
    }

    /// Whether every slot is currently empty.
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(|s| s.is_none())
    }

    /// Look up a slot. **The entire security mechanism, and it is a bounds check.**
    pub fn get(&self, slot: u64) -> Result<Cap<O>, Error> {
        self.slots
            .get(slot as usize)
            .copied()
            .flatten()
            .ok_or(Error::NoSuchSlot)
    }

    /// Look up a slot and require rights.
    ///
    /// Returns [`Error::NoSuchSlot`] for an empty slot even when the caller wanted rights, which
    /// is deliberate: **"you may not" and "there is nothing there" are different answers**, and
    /// the second one leaks less.
    pub fn get_with(&self, slot: u64, needed: Rights) -> Result<Cap<O>, Error> {
        let cap = self.get(slot)?;
        if !cap.rights.allows(needed) {
            return Err(Error::CannotWiden);
        }
        Ok(cap)
    }

    /// Put a capability in the first free slot. Used at spawn, to hand a process its world.
    pub fn insert(&mut self, cap: Cap<O>) -> Result<u64, Error> {
        let slot = self
            .slots
            .iter()
            .position(|s| s.is_none())
            .ok_or(Error::NoFreeSlot)?;
        self.slots[slot] = Some(cap);
        Ok(slot as u64)
    }

    /// Put a capability in a **specific free slot**, refusing an occupied or out-of-range one.
    ///
    /// The targeted counterpart of [`insert`](Self::insert). A supervisor uses it to place a
    /// child's supervision endpoint in the reserved fault slot (abi's `FAULT_EP_SLOT`) rather than
    /// wherever first-free would fall, and refusing an occupied slot keeps that reservation honest:
    /// two things must never share the slot the kernel reads at `START`.
    pub fn insert_at(&mut self, slot: u64, cap: Cap<O>) -> Result<u64, Error> {
        let s = self.slots.get_mut(slot as usize).ok_or(Error::NoSuchSlot)?;
        if s.is_some() {
            return Err(Error::NoFreeSlot);
        }
        *s = Some(cap);
        Ok(slot)
    }

    /// Put a capability in a specific slot, replacing whatever was there.
    pub fn put(&mut self, slot: u64, cap: Cap<O>) -> Result<(), Error> {
        let s = self.slots.get_mut(slot as usize).ok_or(Error::NoSuchSlot)?;
        *s = Some(cap);
        Ok(())
    }

    /// **Copy a capability into another slot, with rights that are no greater.**
    ///
    /// The only way authority moves. `rights` is intersected with what the source slot actually
    /// holds, and if the caller asked for more than that, it is an error rather than a silent
    /// clamp: **asking to widen is a bug in the caller, and a loader that quietly grants less
    /// than was asked for is a loader nobody can reason about.**
    pub fn derive(&mut self, from: u64, to: u64, rights: Rights) -> Result<(), Error> {
        let src = self.get(from)?;

        if !rights.is_subset_of(src.rights) {
            return Err(Error::CannotWiden);
        }

        self.put(
            to,
            Cap {
                object: src.object,
                rights,
            },
        )
    }

    /// Drop a capability. The object may still exist; **we simply can no longer name it.**
    pub fn delete(&mut self, slot: u64) -> Result<(), Error> {
        let s = self.slots.get_mut(slot as usize).ok_or(Error::NoSuchSlot)?;
        s.take().ok_or(Error::NoSuchSlot)?;
        Ok(())
    }
}

/// Machine-checked proofs of the capability model (DECISIONS §14, the verification thesis).
///
/// These are not tests. A test in the module below checks the handful of cases we thought to
/// write down; each harness here asks Kani (a bounded model checker) to prove a property for
/// **every** input, symbolically. `kani::any()` is an unconstrained value, so
/// `derive_never_widens_rights` covers all 2^32 source-rights and all 2^32 requested-rights
/// patterns at once, which is the whole difference between "we tested READ cannot become WRITE"
/// and "no reachable state widens rights."
///
/// The module is behind `#[cfg(kani)]`, so an ordinary `cargo build`/`cargo test` never sees it;
/// only `cargo kani` sets that cfg and links the `kani` intrinsics. Run with `script/verify` (or
/// `cargo kani -p capability`).
#[cfg(kani)]
mod verification {
    use super::*;

    /// Every capability is a subset of itself: the reflexive base case of the derivation order.
    #[kani::proof]
    fn subset_is_reflexive() {
        let a = Rights(kani::any());
        assert!(a.is_subset_of(a));
    }

    /// **Rights cannot be laundered through a chain.** If B is derived from A and C from B, then C
    /// is no more than A. This is why a *flat* subset check suffices and we never need to walk a
    /// derivation tree to bound a capability: subset is transitive, so the chain can only narrow.
    #[kani::proof]
    fn subset_is_transitive() {
        let (a, b, c) = (
            Rights(kani::any()),
            Rights(kani::any()),
            Rights(kani::any()),
        );
        kani::assume(a.is_subset_of(b));
        kani::assume(b.is_subset_of(c));
        assert!(a.is_subset_of(c));
    }

    /// **Userspace cannot forge a right.** `from_bits` takes an attacker-controlled syscall register
    /// (any u32) and the result holds only defined rights, for every possible input.
    #[kani::proof]
    fn from_bits_cannot_forge_a_right() {
        let raw: u32 = kani::any();
        assert!(Rights::from_bits(raw).is_subset_of(Rights::ALL));
    }

    /// The two ways of asking the question agree: "a is no more than b" is exactly "b holds at
    /// least a". Proving them equivalent means a bug in one would show up against the other.
    #[kani::proof]
    fn subset_matches_allows() {
        let (a, b) = (Rights(kani::any()), Rights(kani::any()));
        assert_eq!(a.is_subset_of(b), b.allows(a));
    }

    /// A small table in an arbitrary state: every slot independently empty or holding a capability
    /// with symbolic object and rights. Three slots is enough to exhibit "the slot in question, a
    /// different slot, and an empty one" in every combination.
    fn any_small_cspace() -> CSpace<u8, 3> {
        let mut cs: CSpace<u8, 3> = CSpace::new();
        for slot in 0..3u64 {
            if kani::any() {
                cs.put(
                    slot,
                    Cap {
                        object: kani::any(),
                        rights: Rights(kani::any()),
                    },
                )
                .unwrap();
            }
        }
        cs
    }

    /// **Consume-on-use is final.** For every table state and every slot (in bounds or not), once
    /// `delete` succeeds the slot answers `NoSuchSlot` to both `get` and a second `delete`. This is
    /// the mechanism that makes the one-shot Reply one-shot (DECISIONS §12): the syscall layer
    /// deletes the Reply capability the instant it is invoked, and this proof says no state exists
    /// in which the deleted slot can be invoked again.
    #[kani::proof]
    fn a_deleted_capability_stays_deleted() {
        let mut cs = any_small_cspace();
        let slot: u64 = kani::any();
        if cs.delete(slot).is_ok() {
            assert_eq!(cs.get(slot).err(), Some(Error::NoSuchSlot));
            assert_eq!(cs.delete(slot).err(), Some(Error::NoSuchSlot));
        }
    }

    /// **Delete is slot-local.** Deleting any slot, in bounds or not, leaves every other slot
    /// exactly as it was. A server holding one-shot Reply capabilities for two callers consumes
    /// one and must still hold the other, or answering caller A would silently orphan caller B.
    #[kani::proof]
    fn delete_touches_only_its_slot() {
        let mut cs = any_small_cspace();
        let victim: u64 = kani::any();
        let other: u64 = kani::any();
        kani::assume(victim != other);
        kani::assume(other < 3);

        let before = cs.get(other);
        let _ = cs.delete(victim);
        assert_eq!(cs.get(other), before);
    }

    /// **The central theorem, on the real `CSpace::derive`.** For any source rights and any request,
    /// if the derive succeeds then the capability it stored holds no more than the source did, and
    /// holds exactly what was asked (no silent grant of more). There is no reachable input that
    /// widens authority.
    #[kani::proof]
    fn derive_never_widens_rights() {
        let src_rights = Rights(kani::any());
        let requested = Rights(kani::any());

        let mut cs: CSpace<u8, 2> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: 0u8,
                rights: src_rights,
            },
        )
        .unwrap();

        if cs.derive(0, 1, requested).is_ok() {
            let derived = cs.get(1).unwrap();
            assert!(derived.rights.is_subset_of(src_rights));
            assert!(requested.is_subset_of(src_rights));
        }
    }

    /// **Authority never widens at the other mint site either: `split` inherits, it does not grant.**
    /// `Untyped::SPLIT` mints a child budget outside [`CSpace::derive`] (kernel/src/cap.rs), so
    /// `derive_never_widens_rights` does not reach it; the kernel routes that mint through
    /// [`Cap::mint_child`], and this proves the theorem there. Milestone 35.
    ///
    /// **What the property is, precisely, because DECISIONS §16's amendment makes the obvious phrasing
    /// wrong.** `SPLIT` grants the child `GRANT` so that a budget can be delegated onward, so "SPLIT
    /// never *changes* rights" is false as a description of the feature's intent, and "never widens"
    /// invites the question of what the model permits. What is proved is the strongest of the three and
    /// the one that settles it: **the child's rights are exactly the parent's.** `mint_child` takes no
    /// rights argument, so there is no input by which a caller could ask for more, and equality implies
    /// the subset relation the rest of the model is stated in (asserted too, so the corollary the
    /// syscall layer relies on is checked here and not inferred by a reader).
    ///
    /// The delegability is therefore a property of the *root's* mint, not a widening at `SPLIT`:
    /// `untyped_root_cap` mints once with `READ|WRITE|GRANT`, `SPLIT` inherits whatever the parent
    /// holds, `CAP_INSERT` narrows on the way into a child. Rights down a budget tree are monotonically
    /// non-increasing from the root, a child holds `GRANT` only because the root did, and a spend-only
    /// untyped provably cannot split itself a `GRANT`-bearing child.
    #[kani::proof]
    fn split_never_widens_rights() {
        let parent = Cap {
            object: 7u8,
            rights: Rights(kani::any()),
        };
        let child_object: u8 = kani::any();

        let child = parent.mint_child(child_object);

        // Exactly the parent's, for every one of the 2^32 rights patterns: inheritance, not a grant.
        assert_eq!(
            child.rights, parent.rights,
            "an inheriting mint changed the child's rights",
        );
        // The weaker relation the capability model is phrased in, which the above implies.
        assert!(child.rights.is_subset_of(parent.rights));
        assert_eq!(child.object, child_object);
    }

    /// **A reap is authorized by the supervision relationship, and by nothing else** (DECISIONS
    /// §32). For every pair of endpoint names and every liveness, `Permitted` implies the thread's
    /// recorded fault endpoint *is* the endpoint being invoked and the thread is dead. There is no
    /// input where an unsupervised thread, another supervisor's child, or a stale name (all
    /// `fault_ep` values that do not equal `invoked_ep`, including `None`) is reapable.
    ///
    /// This is the property that makes "a capability that cannot build cannot be made to build via
    /// reap" hold: authorization consumes no capability and produces none. `reap_decision` takes no
    /// `Cap` and returns no `Cap`, so there is no channel through which authority could flow to the
    /// reaper. The proof pins the *gate*; the type pins the absence of a grant.
    #[kani::proof]
    fn reap_is_permitted_only_to_the_supervising_endpoint() {
        let invoked: u64 = kani::any();
        let fault_ep: Option<u64> = if kani::any() { Some(kani::any()) } else { None };
        let dead: bool = kani::any();

        match reap_decision(fault_ep, invoked, dead) {
            Reap::Permitted => {
                assert_eq!(fault_ep, Some(invoked), "reaped through the wrong endpoint");
                assert!(dead, "reaped a live thread");
            }
            // A live child is refused as such only when it really is ours; otherwise the answer
            // must disclose nothing, which is the next harness.
            Reap::StillAlive => {
                assert_eq!(fault_ep, Some(invoked));
                assert!(!dead);
            }
            Reap::NotSupervised => assert_ne!(fault_ep, Some(invoked)),
        }
    }

    /// **The refusal for a thread you do not supervise does not depend on the thread.** For any two
    /// liveness values, a tid whose fault endpoint is not the invoked one answers identically, so a
    /// supervisor cannot use `REAP` to learn whether some other supervisor's child (or a recycled
    /// tid) is alive. The two facts §32 wants distinguishable are distinguishable only *inside* the
    /// relationship.
    #[kani::proof]
    fn a_stranger_reveals_nothing_about_its_liveness() {
        let invoked: u64 = kani::any();
        let fault_ep: Option<u64> = if kani::any() { Some(kani::any()) } else { None };
        kani::assume(fault_ep != Some(invoked));

        assert_eq!(
            reap_decision(fault_ep, invoked, true),
            reap_decision(fault_ep, invoked, false),
        );
        assert_eq!(reap_decision(fault_ep, invoked, true), Reap::NotSupervised);
    }

    /// **A survey shows a thread only to the endpoint that supervises it** (milestone 126). The
    /// scoping half of the same claim the two harnesses above make about the reap: for every pair
    /// of endpoint names, inclusion implies the recorded fault endpoint *is* the invoked one, so
    /// there is no input where an unsupervised thread, another supervisor's child, or a stale name
    /// appears in somebody else's `ps`.
    ///
    /// The `if and only if` matters in both directions here, which it does not for the reap. One
    /// direction is confinement (a stranger is never shown). The other is truthfulness: **every
    /// thread that really is in the domain is reported**, so a monitor that sees an entry missing
    /// can conclude the thread is gone rather than hidden. A view that could silently omit its own
    /// children would be worse than no view.
    #[kani::proof]
    fn a_survey_shows_exactly_the_endpoints_own_children() {
        let invoked: u64 = kani::any();
        let fault_ep: Option<u64> = if kani::any() { Some(kani::any()) } else { None };

        assert_eq!(
            survey_includes(fault_ep, invoked),
            fault_ep == Some(invoked)
        );
    }

    /// **Seeing and collecting are scoped by the same relationship** (milestone 126). For every
    /// input, a thread is in the survey exactly when a reap of it would get past the supervision
    /// check, whatever its liveness. So the domain `ps` reports and the domain a supervisor may
    /// reap from cannot diverge, which is the property that lets one predicate answer both.
    #[kani::proof]
    fn the_view_and_the_reap_have_the_same_scope() {
        let invoked: u64 = kani::any();
        let fault_ep: Option<u64> = if kani::any() { Some(kani::any()) } else { None };
        let dead: bool = kani::any();

        assert_eq!(
            survey_includes(fault_ep, invoked),
            reap_decision(fault_ep, invoked, dead) != Reap::NotSupervised,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Obj {
        Console,
        Frame(u64),
    }

    /// **A new process holds nothing.** The decision, as an assertion.
    #[test]
    fn a_new_cspace_can_name_nothing() {
        let cs: CSpace<Obj, 16> = CSpace::new();

        assert!(cs.is_empty());
        for slot in 0..cs.len() as u64 {
            assert_eq!(cs.get(slot).err(), Some(Error::NoSuchSlot));
        }
    }

    /// You cannot fabricate a capability. There is nothing to guess.
    #[test]
    fn an_unheld_slot_is_not_permission_denied_it_is_nothing() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: Obj::Console,
                rights: Rights::WRITE,
            },
        )
        .unwrap();

        assert_eq!(cs.get(0).unwrap().object, Obj::Console);

        // Every other slot, and every slot past the end.
        assert_eq!(cs.get(1).err(), Some(Error::NoSuchSlot));
        assert_eq!(cs.get(15).err(), Some(Error::NoSuchSlot));
        assert_eq!(cs.get(16).err(), Some(Error::NoSuchSlot));
        assert_eq!(cs.get(u64::MAX).err(), Some(Error::NoSuchSlot));
    }

    /// **Rights only ever narrow.** If delegation could widen, the model is theatre: you would
    /// simply derive yourself a better capability from the one you have.
    #[test]
    fn derive_cannot_widen_rights() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: Obj::Frame(0x1000),
                rights: Rights::READ,
            },
        )
        .unwrap();

        // Narrowing to nothing: fine.
        assert!(cs.derive(0, 1, Rights::NONE).is_ok());
        // Same rights: fine.
        assert!(cs.derive(0, 2, Rights::READ).is_ok());

        // **Asking for WRITE when we only hold READ.**
        assert_eq!(
            cs.derive(0, 3, Rights::WRITE).err(),
            Some(Error::CannotWiden)
        );
        assert_eq!(cs.derive(0, 3, Rights::ALL).err(), Some(Error::CannotWiden));

        // And the failed derive left nothing behind.
        assert_eq!(cs.get(3).err(), Some(Error::NoSuchSlot));
    }

    /// A narrowed capability names the **same object**. Delegation moves authority, not identity.
    #[test]
    fn a_derived_capability_names_the_same_object_with_less_authority() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: Obj::Frame(0x4000),
                rights: Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
            },
        )
        .unwrap();

        cs.derive(0, 5, Rights::READ).unwrap();

        let orig = cs.get(0).unwrap();
        let copy = cs.get(5).unwrap();

        assert_eq!(orig.object, copy.object, "not the same object");
        assert!(orig.rights.allows(Rights::WRITE));
        assert!(!copy.rights.allows(Rights::WRITE), "the copy kept WRITE");
    }

    /// Deriving from an empty slot fails. You cannot lend what you do not hold.
    #[test]
    fn you_cannot_delegate_what_you_do_not_hold() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        assert_eq!(cs.derive(3, 4, Rights::READ).err(), Some(Error::NoSuchSlot));
    }

    #[test]
    fn get_with_refuses_rights_you_do_not_have() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: Obj::Console,
                rights: Rights::READ,
            },
        )
        .unwrap();

        assert!(cs.get_with(0, Rights::READ).is_ok());
        assert_eq!(
            cs.get_with(0, Rights::WRITE).err(),
            Some(Error::CannotWiden)
        );
        assert_eq!(cs.get_with(9, Rights::READ).err(), Some(Error::NoSuchSlot));
    }

    #[test]
    fn deleting_a_capability_makes_the_object_unnameable() {
        let mut cs: CSpace<Obj, 16> = CSpace::new();
        cs.put(
            0,
            Cap {
                object: Obj::Console,
                rights: Rights::ALL,
            },
        )
        .unwrap();

        cs.delete(0).unwrap();
        assert_eq!(cs.get(0).err(), Some(Error::NoSuchSlot));
        assert_eq!(cs.delete(0).err(), Some(Error::NoSuchSlot));
    }

    #[test]
    fn rights_arithmetic() {
        assert!(Rights::ALL.allows(Rights::WRITE));
        assert!(!Rights::READ.allows(Rights::WRITE));
        assert!(Rights::NONE.is_subset_of(Rights::READ));
        assert!(Rights::READ.is_subset_of(Rights::ALL));
        assert!(!Rights::ALL.is_subset_of(Rights::READ));
        assert_eq!(Rights::ALL.intersect(Rights::READ), Rights::READ);
    }

    /// The four cases of §32's reap gate, spelled out as the readable companion to the symbolic
    /// proofs: my corpse, my live child, another supervisor's child, an unsupervised thread (which
    /// is also what a stale tid looks like, because a name that does not resolve carries no
    /// endpoint).
    #[test]
    fn reap_authorizes_only_a_corpse_this_endpoint_supervises() {
        assert_eq!(reap_decision(Some(7), 7, true), Reap::Permitted);
        assert_eq!(reap_decision(Some(7), 7, false), Reap::StillAlive);
        assert_eq!(reap_decision(Some(9), 7, true), Reap::NotSupervised);
        assert_eq!(reap_decision(None, 7, true), Reap::NotSupervised);
    }

    #[test]
    fn a_full_table_says_so() {
        let mut cs: CSpace<Obj, 2> = CSpace::new();
        let cap = Cap {
            object: Obj::Console,
            rights: Rights::ALL,
        };
        assert_eq!(cs.insert(cap), Ok(0));
        assert_eq!(cs.insert(cap), Ok(1));
        assert_eq!(cs.insert(cap).err(), Some(Error::NoFreeSlot));
    }

    /// The rights bits are the syscall wire format: delegation names them in a register, and
    /// `from_bits` is what turns the register back into authority. Milestone 85's mutation run
    /// showed the exact values were pinned nowhere (WRITE's `1 << 1` could become `1 >> 1`, zero,
    /// and delegating WRITE would delegate nothing), and `from_bits` could OR instead of mask, so
    /// undefined bits became defined rights.
    ///
    /// `ENUMERATE` and the `ALL` identity below were added by the 2026-08-17 documentation sweep,
    /// which found this test pinning three bits the day after a fourth landed. The `ALL` assertion
    /// is the one worth having: [`Rights::ALL`]'s own comment warns that a constant defined without
    /// widening that mask is **silently dropped at every delegation**, and until now nothing
    /// checked it. Spelling the union out by hand is deliberate, because a derivation that read
    /// `ALL` to check `ALL` would pass whatever it was given.
    #[test]
    fn rights_bits_are_the_wire_format() {
        assert_eq!(
            [
                Rights::READ.bits(),
                Rights::WRITE.bits(),
                Rights::GRANT.bits(),
                Rights::ENUMERATE.bits()
            ],
            [1, 2, 4, 8]
        );
        // Every defined bit is in ALL, and ALL defines nothing that is not a named right.
        assert_eq!(
            Rights::ALL.bits(),
            Rights::READ.bits()
                | Rights::WRITE.bits()
                | Rights::GRANT.bits()
                | Rights::ENUMERATE.bits()
        );
        // Undefined bits are dropped, never reinterpreted.
        assert_eq!(Rights::from_bits(!Rights::ALL.bits()), Rights::NONE);
        // Defined bits come back as exactly the rights they name.
        assert_eq!(Rights::from_bits(0b101), Rights::READ.union(Rights::GRANT));
        // Union is a union, not a toggle: adding a right twice cannot remove it.
        assert_eq!(Rights::READ.union(Rights::READ), Rights::READ);
    }

    /// `insert_at` is how a supervisor reserves the fault slot, so what matters is that the
    /// capability lands in THAT slot, the slot comes back as the receipt, and an occupied slot
    /// refuses rather than replaces (two things must never share the slot the kernel reads at
    /// START).
    #[test]
    fn insert_at_fills_exactly_the_named_slot() {
        let mut cs: CSpace<Obj, 4> = CSpace::new();
        let cap = Cap {
            object: Obj::Frame(7),
            rights: Rights::READ,
        };
        assert_eq!(cs.insert_at(2, cap), Ok(2));
        assert_eq!(cs.get(2).unwrap().object, Obj::Frame(7));
        assert!(!cs.is_empty());
        // The other slots are still empty, so it did not spray.
        assert_eq!(cs.get(0).err(), Some(Error::NoSuchSlot));
        // Occupied refuses; out of range is a different answer, as everywhere.
        assert_eq!(cs.insert_at(2, cap).err(), Some(Error::NoFreeSlot));
        assert_eq!(cs.insert_at(9, cap).err(), Some(Error::NoSuchSlot));
        // len is the table's size in slots, not its population.
        assert_eq!(cs.len(), 4);
    }
}
