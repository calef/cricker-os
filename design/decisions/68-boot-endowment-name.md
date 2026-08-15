# 68. `BootEndowment::unused` wants a truer name

**Status: PROPOSED.** (raised 2026-08-04; waiting on calef, who names things.)

**What.** The field holds the capabilities aarch64's boot path is handed because it shares that path
with milestone 19d's test roles: a report endpoint and a test SGI. The interactive system never uses
them and deletes them with the device authority.

**The problem.** They are not unused. The test roles use them. "Unused" is true only from the
interactive system's point of view, which is one of the two callers.

**Options.** `not_ours` (states whose they are, from the caller's side); `test_roles_only` (states
whose they are, absolutely); leave `unused` and let the doc comment carry the nuance.

**Recommendation.** Something possessive. The field's whole job is to say "these arrived for someone
else", and a name that says so removes the need for the reader to hold the doc comment in mind.

**Blocked.** Nothing. Cosmetic, and cheap while `crates/system_initializer` is one day old.
