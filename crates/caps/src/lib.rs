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

#![no_std]

/// What you may do with a capability.
///
/// **Rights only ever narrow.** [`CSpace::derive`] will not widen them, and that is not a policy
/// we remembered to enforce, it is the only operation the type offers. If delegation could
/// widen authority, the whole model is theatre: anyone could hand themselves the rights they
/// were denied by passing a capability to themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const READ: Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);

    /// The right to **pass this capability on**. Without it, you may use a thing and not lend it.
    ///
    /// This is the right that makes delegation a decision rather than an accident. Unix has no
    /// equivalent, which is why "the child inherits every fd" is the default and why
    /// `FD_CLOEXEC` had to be invented as an afterthought.
    pub const GRANT: Rights = Rights(1 << 2);

    pub const ALL: Rights = Rights(0b111);

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

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

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
    pub object: O,
    pub rights: Rights,
}

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

    pub const fn len(&self) -> usize {
        N
    }

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
/// `cargo kani -p caps`).
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
}
