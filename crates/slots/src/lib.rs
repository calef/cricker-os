//! A fixed-capacity generational table: **names that die with what they named.**
//!
//! Milestone 14 phase A (design/kernel-objects-from-untyped.md, decision D2). The scheduler's
//! thread table used to be a `BTreeMap<Tid, Box<Thread>>` fed by a global counter: every spawn
//! allocated map nodes, and the map was unbounded. This replaces it with an array, and it
//! replaces the counter with something better than a counter.
//!
//! # The name is the safety mechanism
//!
//! A name here packs `(generation, slot)` into one `u64`. Lookup is an index and one compare.
//! When an entry is removed, its slot's generation is bumped, so every name minted for the old
//! occupant **stops resolving, forever**, even after the slot is reused. A stale name is not a
//! dangling reference and not somebody else's thread: it is `None`, safely.
//!
//! That property is why this table is the *first step toward* capability-only thread naming
//! rather than a detour from it (see the design doc). Long-lived capabilities (`Reply` today)
//! carry thread names; a name that can never resolve to the wrong thread is what makes that
//! sound without seL4's CDT. The properties are proved below, not argued.
//!
//! # The honest limits
//!
//! - **Capacity is fixed.** `insert_with` returns `None` when all `N` slots are live. The
//!   caller decides what a full table means (for the kernel: spawn fails, the same contract as
//!   out-of-memory today, and N becomes a documented limit of the image).
//! - **Generations are 32-bit.** A slot must be reused 2^32 times before an ancient name could
//!   resolve again. At one spawn per microsecond against a single slot, that is an hour and a
//!   quarter of doing nothing else; a real workload spreads reuse over every free slot. Recorded
//!   because "never" is not the same as 2^32, but this is not a bound anything will meet.
//! - **Insert scans for a free slot, O(N).** Spawn-rate work, not switch-rate work. The switch
//!   path only ever does `get`/`get_mut`, which are O(1).

#![cfg_attr(not(test), no_std)]

/// A fixed-capacity table whose entries are named by `(generation, slot)` pairs packed in a
/// `u64`: generation in the high 32 bits, slot in the low 32.
///
/// Two consequences of that packing, both load-bearing for the kernel:
///
/// - The very first insert into a fresh table is named `0` (slot 0, generation 0), which is how
///   the boot thread keeps its traditional tid without a special case.
/// - With any sane `N`, no name ever equals `u64::MAX`, so a caller may keep using it as a
///   "no thread" sentinel (`cpu::NO_TID`).
pub struct Table<T, const N: usize> {
    slots: [Option<T>; N],
    /// The current generation of each slot: the one a live occupant's name carries, and the one
    /// the *next* occupant's name will carry after a bump. Survives the occupant on purpose.
    gens: [u32; N],
    /// Live-entry count, maintained so `len` is O(1).
    live: usize,
}

impl<T, const N: usize> Table<T, N> {
    pub const fn new() -> Self {
        // A compile-time guard on the packing: slots must fit in the low 32 bits.
        const { assert!(N > 0 && N <= u32::MAX as usize) };
        Self {
            slots: [const { None }; N],
            gens: [0; N],
            live: 0,
        }
    }

    const fn name(slot: usize, generation: u32) -> u64 {
        ((generation as u64) << 32) | slot as u64
    }

    const fn unpack(name: u64) -> (usize, u32) {
        ((name & 0xffff_ffff) as usize, (name >> 32) as u32)
    }

    /// Insert, minting the entry's name first so the entry can carry it: `f` receives the name
    /// and returns the value to store. `None` (and `f` never called) if every slot is live.
    pub fn insert_with(&mut self, f: impl FnOnce(u64) -> T) -> Option<u64> {
        let slot = self.slots.iter().position(|s| s.is_none())?;
        let name = Self::name(slot, self.gens[slot]);
        self.slots[slot] = Some(f(name));
        self.live += 1;
        Some(name)
    }

    /// **The validated slot index behind a name**, or `None` exactly when the name is dead or
    /// garbage. This is the first half of every lookup, exposed (milestone 14 phase B.2) because
    /// the kernel stores TCBs in a parallel pool with pool slot i = table slot i, so the index
    /// *is* the storage address. `get`/`get_mut`/`remove` are built on this, so every harness
    /// that proves a stale name never resolves proves it for `slot_of` too.
    pub fn slot_of(&self, name: u64) -> Option<usize> {
        let (slot, generation) = Self::unpack(name);
        if *self.gens.get(slot)? != generation {
            return None;
        }
        self.slots[slot].as_ref()?;
        Some(slot)
    }

    /// Resolve a name. `None` for a name whose entry was removed (however long ago), a name from
    /// a previous occupant of the slot, or garbage. **Never** the wrong entry.
    pub fn get(&self, name: u64) -> Option<&T> {
        let slot = self.slot_of(name)?;
        self.slots[slot].as_ref()
    }

    pub fn get_mut(&mut self, name: u64) -> Option<&mut T> {
        let slot = self.slot_of(name)?;
        self.slots[slot].as_mut()
    }

    /// Remove and return the entry, bumping the slot's generation: from this moment `name` (and
    /// every copy of it, wherever it is held) resolves to `None`, forever.
    pub fn remove(&mut self, name: u64) -> Option<T> {
        let slot = self.slot_of(name)?;
        let value = self.slots[slot].take()?;
        self.gens[slot] = self.gens[slot].wrapping_add(1);
        self.live -= 1;
        Some(value)
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Every live entry, in slot order. For whole-table sweeps (revocation walks every cspace);
    /// nothing on a hot path iterates.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.slots.iter_mut().filter_map(|s| s.as_mut())
    }

    /// The slot index of every live entry, in slot order; each index appears at most once. The
    /// pool-backed kernel table iterates its parallel storage with this.
    pub fn live_slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|_| i))
    }
}

impl<T, const N: usize> Default for Table<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-checked proofs (DECISIONS §14). These are the properties the design doc says
/// capability payloads will one day lean on, proved before anything leans on them. A two-slot
/// table exhibits every case that matters: the slot in question, a different slot, and reuse.
#[cfg(kani)]
mod verification {
    use super::*;

    /// **A removed name never resolves again**, even after its slot is reused. This is the
    /// stale-Tid safety the kernel currently gets from map-lookup-fails, kept under reuse: the
    /// old name and the new occupant's name differ (the generation moved), so `get`, `get_mut`,
    /// and `remove` all refuse the old one. The one stated precondition: the slot's generation
    /// has not wrapped all the way around (see the crate doc on 2^32).
    #[kani::proof]
    fn a_removed_name_never_resolves_again() {
        let mut t: Table<u8, 2> = Table::new();
        let name = match t.insert_with(|_| kani::any()) {
            Some(n) => n,
            None => return,
        };
        t.remove(name);

        // Reuse the slot: the new occupant must not be reachable through the dead name.
        let reused = t.insert_with(|_| kani::any());
        if let Some(new_name) = reused {
            assert_ne!(new_name, name);
        }
        assert!(t.get(name).is_none());
        assert!(t.get_mut(name).is_none());
        assert!(t.remove(name).is_none());
    }

    /// **Two live entries never share a name**, and each name resolves to its own entry: the
    /// packing cannot alias two occupants.
    #[kani::proof]
    fn live_names_are_distinct_and_resolve_to_their_own_entry() {
        let mut t: Table<u8, 2> = Table::new();
        let (a_val, b_val): (u8, u8) = (kani::any(), kani::any());
        let a = t.insert_with(|_| a_val).unwrap();
        let b = t.insert_with(|_| b_val).unwrap();

        assert_ne!(a, b);
        assert_eq!(t.get(a).copied(), Some(a_val));
        assert_eq!(t.get(b).copied(), Some(b_val));
    }

    /// **A garbage name resolves to nothing.** For any u64 whatsoever, resolution either fails
    /// or lands on an entry whose minted name is exactly that u64: there is no input that
    /// reaches an entry through a name the table never issued. (The u64 arrives in syscall
    /// registers by way of capability bookkeeping; this is the "cannot forge" claim for names.)
    #[kani::proof]
    fn a_name_the_table_never_minted_resolves_to_nothing() {
        let mut t: Table<u8, 2> = Table::new();
        let minted = t.insert_with(|_| kani::any());
        let probe: u64 = kani::any();
        if t.get(probe).is_some() {
            assert_eq!(Some(probe), minted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boot-thread contract: the first insert into a fresh table is named 0.
    #[test]
    fn the_first_name_is_zero() {
        let mut t: Table<&str, 8> = Table::new();
        assert_eq!(t.insert_with(|_| "boot"), Some(0));
        assert_eq!(t.get(0), Some(&"boot"));
    }

    /// The entry can carry its own name, which is how a Thread learns its Tid.
    #[test]
    fn the_entry_is_handed_its_name() {
        let mut t: Table<u64, 8> = Table::new();
        let name = t.insert_with(|n| n).unwrap();
        assert_eq!(t.get(name), Some(&name));
    }

    #[test]
    fn a_full_table_refuses_and_does_not_call_the_closure() {
        let mut t: Table<u32, 2> = Table::new();
        t.insert_with(|_| 1).unwrap();
        t.insert_with(|_| 2).unwrap();
        assert_eq!(t.insert_with(|_| unreachable!("closure ran on a full table")), None);
        assert_eq!(t.len(), 2);
    }

    /// Remove, then reuse: the slot comes back, the name does not.
    #[test]
    fn reuse_changes_the_name_and_kills_the_old_one() {
        let mut t: Table<&str, 1> = Table::new();
        let first = t.insert_with(|_| "first").unwrap();
        assert_eq!(t.remove(first), Some("first"));

        let second = t.insert_with(|_| "second").unwrap();
        assert_ne!(first, second, "a reused slot must mint a fresh name");
        assert_eq!(t.get(first), None, "the dead name resolved");
        assert_eq!(t.get(second), Some(&"second"));
    }

    #[test]
    fn iter_mut_sees_only_live_entries() {
        let mut t: Table<u32, 4> = Table::new();
        let a = t.insert_with(|_| 1).unwrap();
        t.insert_with(|_| 2).unwrap();
        t.remove(a);

        let live: Vec<u32> = t.iter_mut().map(|v| *v).collect();
        assert_eq!(live, vec![2]);
        assert_eq!(t.len(), 1);
    }

    /// Out-of-range slots and wrong generations are both just `None`.
    #[test]
    fn garbage_names_are_none() {
        let mut t: Table<u32, 2> = Table::new();
        t.insert_with(|_| 7).unwrap();
        assert_eq!(t.get(1), None); // valid slot, nothing there
        assert_eq!(t.get(99), None); // no such slot
        assert_eq!(t.get(1 << 32), None); // slot 0, wrong generation
        assert_eq!(t.get(u64::MAX), None); // the NO_TID sentinel never resolves
    }
}
