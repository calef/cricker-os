# 13. Capability revocation + untyped reclamation

**Status: BUILT.**

**In brief.** Unmap a page from every holder; reclaim a region safely. **Built (frame scope), §13.**

**Why it matters.** safe teardown, a TCB property

**Built (milestone 13), scoped to frame revocation; see DECISIONS §13.** The full derivation tree is
deferred, the way the argument earlier in the roadmap predicted: revoke-all-derivatives serves the
reclamation triggers, and subtree granularity waits for a driver. The rest of this block is the
proposal it was built from.

**Deliverable.** A capability-derivation tree and a recursive `revoke` that unmaps an object from
every holder, so authority can be retracted from a live peer and a page can finally be reclaimed.

**Why.** The deepest thing left in the capability model, and it unblocks everything about
reclamation. `untyped::destroy` already exists, dead, as a tripwire: today frames are spend-only and
never reused, which is the *only* reason teardown's dangling mappings are safe rather than a
use-after-free.

**Prior art.** seL4's CDT plus recursive revoke, a first-class kernel object there.

**Blocking precondition.** design/open-design-ideas.md (revocation) and
notes/capability-lifecycle.md state the invariant this must not break: **no reclamation of any kind
until revocation lands.** This milestone is that work, and the precondition is why it comes before
14.
