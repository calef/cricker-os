# Security audit, 2026-08-17: the authority that was minted overnight

**Kind:** security. **Lens:** newly minted authority, read adversarially: the seven ABI constants and
one new right that landed between 2026-08-15 and this run, rather than the tree at large.
**Findings:** fixed 2, minted 1, accepted 3.

**Nothing exploitable was found.** No privilege escalation, no path that widens rights, no way to
reach an object the caller was not handed. The one thing a reader should carry off is a **counting
channel**, finding 1: a viewer holding the narrowest capability this system can express learns how
many threads exist outside its domain, though it can never name one. It falsified a sentence the
shell printed, and that sentence is now corrected.

## Why this ran, and why this lens

`script/audits` fired on an **event trigger**, not a count. For the security kind any change to the
ABI constant surface fires, and it moved 43 -> 50 (+7); components moved 104 -> 110. §74's ruling is
event triggers first, a count second, the calendar a backstop, and the event here is specific enough
to pick the lens by itself: **the capability surface grew seven constants and one brand-new right in
one night.**

So the scope is that surface and the code that consults it, read as an attacker would:

- `abi::endpoint::SURVEY` (method 6) and `sched::survey_supervised`, the supervision-subtree walk;
- `capability::Rights::ENUMERATE` (bit 3), the widening of `Rights::ALL` to `0b1111`, and every
  `RETYPE_OBJ` arm that now mints `Rights::ALL` in place of a hand-listed triple;
- `fs_proto::fs::STATFS` (op 18), which takes no right and any handle, deliberately;
- milestone 40's manual-index constants and `apropos`, the shell builtin that searches the
  documentation store.

**The rotation is the point.** The four security audits on record took the whole kernel, the
hand-written assembly, the shared pages, and untrusted counterparty input. None of them took *newly
minted authority*, which is the one lens that can only be run soon after the mint: the shape this
looks for is a right or a method that is correct in isolation and wrong in combination with what was
already there, and that goes stale as soon as callers grow around it.

### What was deliberately not examined

An outside reviewer should start here.

- **The pre-existing rights model.** `derive`, `SEND_CAP` and `CAP_INSERT` were read only for whether
  the *new* bit changed their behaviour. The 2026-07-15 whole-kernel pass owns them otherwise.
- **The directory-capability model that `fs_proto`'s `ENUMERATE` belongs to.** It has its own
  negative-control catalogue (`kernel/src/user/dir_capability_tests.rs`, with an escape bitmask per
  attack) and was not re-audited. Only the new `STATFS` row was read.
- **`manual::index`'s shard parser as an untrusted-input surface.** `apropos` reads index bytes out
  of the filesystem, and a shell whose grant carries `WRITE` on the store could feed it crafted
  bytes. That is squarely the **previous** audit's lens (untrusted counterparty input) applied to a
  surface that did not exist when it ran, and re-running a lens is what the rotation exists to
  prevent. It is handed off below.
- **The RISC-V path.** Every mechanism examined here is architecture-neutral Rust in
  `kernel/src/sched.rs`, `kernel/src/syscall.rs` and `crates/`, and the kernel test landed in this
  lane runs on both instruction sets. No `arch/` code was read.
- **Multi-core races on the new method.** `survey_supervised` takes `SCHED` and gives it back between
  entries, which is a documented snapshot bargain rather than a race, and the audit accepted that
  reasoning without re-deriving it. A concurrency pass over `SCHED` is not this lens.
- **Timing.** Every channel found here is a *value* channel. Nothing was measured for timing, and the
  covert channels named below have no measured bandwidth.
- **`Rights::ENUMERATE` on objects that do not consult it.** `Aspace` and `Untyped` receive the bit
  from `RETYPE_OBJ` and no arm reads it, which confers nothing today and was checked (finding 4) but
  not modelled for what `pmap` and `free` will want.

## The five questions this lane was briefed with

### 1. Did widening `Rights::ALL` widen anything unintended? No, and it got stricter.

There are exactly **two** callers of `Rights::from_bits` in the kernel, and both are in
`kernel/src/syscall.rs`: `endpoint::SEND_CAP` (line 164) and `tcb::CAP_INSERT` (line 386). Both have
the same three-step shape, and the order is what matters:

1. the source capability must hold `GRANT`, or `NotPermitted`;
2. `Rights::from_bits(a1 as u32)`, which masks against `ALL`;
3. `narrowed.is_subset_of(src.rights)`, or `NotPermitted`.

Step 3 is what makes step 2's widening safe, and the widening actually **narrows what the syscall
accepts**. Before, a caller passing bit 3 got `Rights::NONE` back from `from_bits`, and
`NONE.is_subset_of(anything)` is vacuously true, so the delegation *succeeded* and conferred nothing.
Now bit 3 survives the mask and the subset check refuses it unless the source really holds
`ENUMERATE`. A caller that used to get a silent zero-rights capability now gets a loud
`NotPermitted`. That is a behaviour change worth knowing about and it is in the safe direction.

No other path turns userspace bits into rights. `abi::rights` values reach the kernel only through
those two registers; every other rights value in the kernel is a compile-time constant.

The `RETYPE_OBJ` change is also not a widening, and the distinction is one the tree already draws
correctly. `RETYPE_OBJ` and `RETYPE` mint `Rights::ALL` on a **new object** (an endpoint, an address
space, a TCB, a frame) carved out of the region, which is the recorded "the creator gets full rights
on its own object" invariant. `SPLIT`, which mints a capability naming **the same memory** as its
parent, still routes through `Cap::mint_child` and inherits the parent's rights, and
`split_never_widens_rights` still proves it. The two mint sites are different in kind and are treated
differently; adding `ENUMERATE` to the first did not blur them.

**One pre-existing property surfaced and is not a finding.** A holder of a `GRANT`-less
(spend-only) untyped can `RETYPE_OBJ` it into an endpoint and receive a `GRANT`-bearing capability to
that endpoint, so a `GRANT`-less budget does not confine delegation of objects built out of it. That
was equally true of the hand-listed `READ|WRITE|GRANT` and is what "full rights on its own object"
means. No document claims otherwise, so there is nothing to correct.

### 2. Can `ENUMERATE` be escalated to `READ`, or vice versa? No.

`allows` is `self.0 & needed.0 == needed.0` and the two bits do not overlap (bit 0 and bit 3), so
neither implies the other in either direction. `derive` refuses any requested set that is not a
subset of the source. The two `from_bits` sites are covered above. `Rights::ALL` is the only place a
rights set is minted wholesale, and it names a fresh object.

Two things were checked specifically because they would not be visible from the rights lattice:

- **No rights check anywhere uses a proxy for `allows`.** Grepping the kernel and `crates/capability`
  for `rights.bits()`, `!= Rights::NONE` and direct comparisons finds only test and proof code. Every
  authorization is `allows` or `is_subset_of`.
- **Nothing shipped holds both `READ` and `ENUMERATE` on a supervision endpoint.**
  `system_initializer` gives `job_undertaker` `READ` alone (it reaps and cannot look) and a `ps`
  `ENUMERATE` alone (it looks and cannot reap). The separation the split bought is realised rather
  than merely available, which was worth verifying because the milestone's own argument only required
  that it be *expressible*. Recorded in notes/process-view.md, which did not say it.

### 3. Is `SURVEY`'s cursor safe against a hostile caller? Safe, and leaky. See finding 1.

**Safe:** every out-of-range, stale and crafted cursor is handled, and none of them can be used to
name a thread outside the domain.

- `usize::try_from(cursor).unwrap_or(usize::MAX)` clamps rather than wrapping, and
  `slots::Table::iter_from`'s contract is that `from >= N` yields nothing. `survey(cap, u64::MAX)`
  returns `DONE`, which the existing test asserts. A cursor past the end is "nothing more", not a
  refusal, so a caller feeding its own cursors back cannot fall off the end into an answer that reads
  as "you may not look".
- A crafted cursor selects only a **starting slot**. The loop then skips every thread the membership
  predicate rejects, so the entry returned is always a member. `capability::survey_includes` is the
  boundary, it is `matches!(fault_ep, Some(ep) if ep == invoked_ep)` and nothing else, and it is
  Kani-proved in both directions.
- A stale cursor cannot resolve to the wrong thread, because a slot index is stable in the sense a
  resume needs: slot `k` holds that entry or is empty, never somebody else's.
- **It cannot probe the tid space of a domain the caller does not supervise.** The caller supplies a
  cursor, never a tid, and the kernel never reports a non-member's tid or state. `REAP` is the arm
  that takes a caller-supplied tid, and it deliberately answers `NotSupervised` for both "not yours"
  and "never existed" so the two cannot be distinguished. `SURVEY` does not widen that, and a viewer
  holding `ENUMERATE` alone cannot express a `REAP` at all.

**Leaky:** the cursor and the tid are machine-wide slot indices. That is finding 1.

### 4. Does `STATFS` leak, taking no right and any handle? Yes, and the tree already said so.

`fs::STATFS`'s own `BUGS` section states it before this audit did: "**It answers about the whole
image, never about a subtree.** A holder of a narrow subtree capability learns the volume's free
space, which is more than its own namespace tells it." Recorded-accepted with a real reason (there
are no quotas, so there is no smaller number that would be true, and inventing one would be §42's
silent degradation). That is the disposition working as designed, and re-litigating it is not this
audit's business.

The reachability was verified rather than assumed. `fs_subtree_caretaker` is table-driven and
forwards by `fs_proto::verb`, with `Policy::Forward` on the `STATFS` row, so a subtree-confined
client's request reaches the server with the caretaker's substituted handle. `verb::of` bounds the
opcode at `FIRST..=LAST`, and `LAST` was updated to `STATFS`, so op 18 is carried rather than falling
through a permissive default; an opcode outside the range is one `EINVAL` site in the caretaker, not a
blind forward. That was checked because a newly numbered opcode is exactly where a permissive default
bites, and it is better here than checking could have hoped: `fs_proto` carries compile-time
assertions that `TABLE` and `POLICY` each have a row per verb, so adding an opcode and forgetting its
row fails the build rather than producing a caretaker that silently does or does not offer it. That is
the top rung, it predates this audit, and it is the reason this question had a short answer.

**What the entry did not cover, and what this audit added:** the number *moves*. The recorded entry
is about what a confined holder learns once; polling is a different character of problem. Two
programs confined to disjoint subtrees of one image, sharing no capability and unable to name each
other's names, can communicate: one modulates the free count by writing and truncating, the other
polls. So they learn about a *different* subtree, which was the question's second half, and the
answer is yes. Extended in place as finding 5.

For the specific case the brief named, a confined program holding one narrow write-only file grant:
it learns the volume's total and free bytes, and nothing else. Not a name, not a handle, not a
directory. The handle check is real: a client with no endpoint into the server cannot ask at all, so
there is no ambient "how full is the disk" call. The design intent, that a write-only grant must be
able to ask whether its next write fits, is served and is why demanding `dir::READ` would have been
worse.

### 5. Does `apropos` leak the existence of anything? Not today. It grants nothing.

Three properties, all verified in the code:

- **It is a shell builtin, so its authority is the shell's.** It opens the store with
  `nav.name_call(fs::OPENDIR, fs::ROOT, index::STORE_DIR, nav.rights)`, and `fs::ROOT` is whatever
  root the shell was granted. A shell behind a subtree caretaker resolves `doc` inside that subtree
  or gets `ENOENT`. There is no path out of the grant and nothing ambient.
- **It returns names and mints nothing.** No capability is created, delegated or spawned. Opening a
  page is a separate line the reader types, which is where a capability actually moves.
- **The shipped store is four bundles and six pages**, all benign technical documentation:
  `notes/manual.md`, `notes/pipes.md`, `notes/line-discipline.md`, `notes/ipc-naming.md`,
  `notes/stack.md`, `notes/glob.md`. Nothing about credentials, keys, the threat model, or a host
  path beyond the repository-relative page paths the index carries by design so a result can say
  where a page came from.

**The one thing to watch is the growth path, and it is finding 6.** `DOC_BUNDLES` in
`xtask/src/main.rs` is a hand-written array, and it is the only thing between the shipped image and
any markdown in this repository. Adding `SECURITY.md`, `DECISIONS.md` or `design/decisions/` to a
bundle would put this project's own threat model and its recorded security compromises into a store
that every program reaching a prompt can search. Accepted, with the check written down.

## Findings

### 1. MINTED: the survey cursor counts threads the viewer cannot name

**The strongest finding, and the only one with a measured demonstration.** One disposition, per this
directory's rule, and it is `minted`: the mechanism needs a milestone. The claim correction described
below is not a second finding, it is step 4 of the procedure (re-baseline the docs in the same lane)
applied to this one.

`sched::survey_supervised` returns `slot as u64 + 1` as `next_cursor`, where `slot` indexes
`Scheduler::threads`, the **whole machine's** thread table. And a tid is a `slots` generational name,
`(generation << 32) | slot`, so the low half of every tid a survey reports *is* that same machine-wide
index and the high half counts how many times that slot has been recycled since boot, machine-wide.

`slots::Table::insert_with` allocates **first-free**, so slot indices are dense and ordered by
creation among the free set. A viewer holding `ENUMERATE` alone, which is exactly what a `ps` is
granted and the narrowest thing that can look at all, can therefore work out:

- **that other threads exist**, from a single member, because its member's slot index bounds below the
  number of slots occupied when that member was created;
- **how many threads were created between two of its own members**, by subtracting their cursors;
- **churn in a slot machine-wide**, from the generation half, which only ever increases and counts
  other domains' thread lifetimes.

Two domains that can each spawn can turn this into a **covert channel** with no shared capability at
all: one modulates global slot allocation by spawning and exiting, the other polls its own members'
cursors. Bandwidth unmeasured.

**The test.** `kernel/src/user/survey_tests::the_survey_cursor_counts_threads_the_viewer_cannot_name`
builds a member, then a stranger in a *different* domain, then a second member, and asserts three
things: that the tid's low half plus one equals the cursor (the ABI fact, which cannot flake), that
the cursor advances by at least two between two adjacent members (the leak, the extra step being the
stranger), and that the stranger is still not nameable (the half that must not regress). It runs on
both instruction sets with the rest of the suite.

**What is not affected, which is most of the claim.** A viewer cannot name a thread outside its
domain, learn its tid or its state, or reap it. `a_domain_is_exactly_the_children_of_the_endpoint_that_was_granted`
still proves that and was not weakened. This is a counting channel beside the confinement property,
not a hole in it.

**Corrected in this lane: the claim.** `caps ps` printed "and not learn that a process outside this
domain exists", which is false as stated. It now reads "and not learn anything about a process
outside this domain but that it exists", and notes/process-view.md's worked example was corrected to
match. An overclaim in a security-relevant output is itself a finding, and correcting the sentence is
the half an audit lane can finish.

**The disposition, proposed as a milestone: scope the cursor to the domain.** Severity low, and it is on the
confinement thesis rather than beside it: this system's argument is that a listing is a capability
rather than a fact about the machine, and an arithmetic channel that reports facts about the machine
is a crack in the demonstration even where it is not a vulnerability. Two options, and the
recommendation is the second:

1. **A domain-local thread name.** Correct and expensive: a tid appears in `endpoint::REAP`, in
   `abi::fault`'s death message and in `ps`'s output, so this is a wire-format change under the
   "anything two programs agree on" rule, which is the category that is not cheap to un-ship.
2. **An opaque cursor**, the slot index combined with a per-endpoint value. Closes the subtraction
   channel, leaves the generation half of the tid open, and touches one function. Recommended as the
   first increment because it is the half that is a channel rather than a datum, and because it
   changes nothing two programs agree on.

Would I still recommend option 2 if both were the same amount of work? No, and this sentence is here
because the tenet requires it: option 1 is the better design and I am recommending option 2 partly
because it is smaller. The honest weighing is that option 2 closes the channel a hostile pair could
*use* while option 1 additionally closes a datum a curious program could *read*, and the second is
worth less. But the effort is in the argument and a reader should see it.

### 2. FIXED: nothing checked that the kernel's rights and the ABI's rights agree

`abi::rights` is what userspace **names** a right with: it travels in a syscall register at 79 call
sites outside `crates/abi` as this report lands, from `system_initializer`'s grant tables through
every `SEND_CAP` and `CAP_INSERT` in `user/src/`. `capability::Rights` is what the kernel **means** by
one. Both crates are
dependency-free and cannot see each other, so the two are two independent arrays of magic numbers,
and until this lane **nothing compared them**.

This is not a hypothetical. It is the failure milestone 126 walked into on its way in: that lane added
`ENUMERATE`, `RETYPE_OBJ`'s hand-listed `READ|WRITE|GRANT` silently stopped meaning "full rights",
and the symptom surfaced three steps away as `OutOfMemory` at a prompt. The lane fixed its own
instance by making `Rights::ALL` the invariant inside the kernel. The same class of drift across the
syscall boundary was left unguarded.

Fixed at the **top rung**, as compile-time assertions in `kernel/src/cap.rs`, beside the
`CSPACE_SLOTS` assertion that is there for exactly the same reason and made the idiom obvious. Four
things are now unrepresentable:

- each of `READ`, `WRITE`, `GRANT`, `ENUMERATE` has the same bit in both crates;
- `Rights::ALL` is **exactly** the union of the ABI's four, in both directions. A bit in `abi::rights`
  missing from `ALL` is a right userspace can name that `from_bits` masks to zero, so a delegation
  asking for it succeeds and confers nothing. A bit in `ALL` with no ABI name is a right the kernel
  honours that no manifest can ask for;
- the union fits in `u32`. `abi::rights` is `u64` and `Rights` is `u32`, and the syscall path narrows
  with `a1 as u32`, so a right defined at bit 32 or above would be truncated to nothing on the way in
  and the delegation would appear to succeed. Nothing about the ABI's type stopped somebody writing
  `1 << 32`; now something does.

Cost: zero runtime, zero test time, no new dependency. A fifth right added to one crate and not the
other is a compile error at the file a reader is already in.

**Why the proof suite could not have caught this, which is the part worth carrying off.** The Kani
harness `from_bits_cannot_forge_a_right` asserts `Rights::from_bits(kani::any()).is_subset_of(
Rights::ALL)`, and `from_bits` is implemented as `bits & ALL`. The property is therefore expressed
**relative to `ALL`**, so widening `ALL` weakens the proof by exactly as much as it widens the mask,
silently, and the harness stays green whatever `ALL` becomes. The same is true of the host test
asserting `from_bits(!ALL.bits()) == NONE`. Exactly one assertion in the crate pins an absolute bit
pattern (`from_bits(0b101) == READ | GRANT`), and it covers bits 0 and 2.

This is a general hazard rather than a slip by whoever wrote those harnesses, and it is worth stating
in the vocabulary this project already uses: **a property quantified over every input is still only
as strong as the constant it is stated against.** A model checker cannot tell you that your definition
of "every defined bit" is wrong, because that definition is its premise. The thing that can is an
assertion tying the constant to an independent source of truth, which is what the ABI is here.

### 3. FIXED: notes/process-view.md documented the authority the same note's BUGS section says was removed

The note's body said `SURVEY` "**Needs `READ`**, exactly as `endpoint::REAP` does", its
what-the-viewer-holds table keyed all five rows on `READ`, its `caps ps` example printed `READ`, and
its "where it comes from at the prompt" section said init places the endpoint with `READ`. Its own
`BUGS` section, six paragraphs later, records that holding a domain with `READ` was more authority
than looking needs and that the fix landed the same day.

**Why this is a security finding and not a tidiness one.** A reader building a monitor follows the
table, grants `READ`, and hands a viewer the ability to `RECV` a death message out from under the
real supervisor and to `REAP` a corpse. That is precisely the over-grant the fix removed, prescribed
by the document that records removing it. The kernel refuses it, so nothing is exploitable; what
breaks is the reader's model, and a newcomer who follows a document into a refusal stops trusting the
documents.

Fixed: all four sites, plus a fifth row in the table for the case the split created and a reader
would not predict, which is that `READ` without `ENUMERATE` is refused. `READ` is the *stronger*
right on this object and still does not unlock the view, because the two differ in kind rather than
in degree. Also added the fact from question 2 that nothing shipped holds both.

### 4. ACCEPTED: `START` establishes supervision from a capability whose rights it never reads

`sched::start_tcb` reads the reserved fault slot with `cspace.get(FAULT_EP_SLOT)` rather than
`get_with`, so any `Endpoint` capability there makes the thread supervised, `Rights::NONE` included.
It is the one place in the kernel where a capability's presence authorizes an operation and its
rights are never consulted.

**Nothing escalates through it, and the obvious reason is the wrong one.** `START` deletes the slot
before `arm_for_start` makes the thread runnable, so the child never executes an instruction while
holding that capability. The protection is the **deletion**, and the ordering inside `start_tcb` is
load-bearing. That matters because `supervision_proto` places the capability with `abi::rights::READ`
and both it and `system_initializer` present that choice as a deliberate narrowing: three true
sentences that a reader assembles into the false one that placing it with `WRITE` would open a hole.
It would not, and neither would `Rights::NONE`. A maintainer who believes the rights are the
mechanism might reorder `start_tcb` to delete the slot later.

Accepted rather than fixed because the fix is a choice, not a correction: requiring `WRITE` would
refuse every current caller, and requiring `READ` would encode the accident. Which right a
supervision placement should demand is a syscall-surface question and is the architect's. Recorded in
a new `BUGS` section in notes/supervision.md, which had none.

### 5. ACCEPTED: `STATFS` is a channel between subtrees, not only a leak into one

The extension described under question 4, recorded in `fs::STATFS`'s own `BUGS` beside the entry it
extends. Two subtrees on one image are not isolated from each other's write volume, and a deployment
that needs that isolation needs two images. Not fixable at this verb: a confined writer must be able
to ask whether its next write fits, and every true answer moves when the volume moves. Per-subtree
quotas would replace the channel with a private number rather than narrow it, which is a second
reason the neighbouring entry wants them.

### 6. ACCEPTED: `DOC_BUNDLES` is the only thing standing between the image and every markdown file

Today's four bundles are benign. The mechanism that keeps them benign is a hand-written array in
`xtask/src/main.rs`, which is rung four. The check to make, written here because it is the kind of
thing nobody thinks of at the moment they add a page: **adding a document to a bundle publishes it to
every program that can reach a prompt.** `SECURITY.md`, `DECISIONS.md` and `design/decisions/` are the
ones that would matter, because they carry this project's threat model and its recorded security
compromises. No fix proposed: a gate that guessed which documents are sensitive would be wrong in
both directions, and the list is short enough to read in review.

## What wants a lane of its own

- **The cursor-scoping milestone** proposed in finding 1, option 2.
- **`manual::index`'s shard parser, read as untrusted input.** A shell whose grant carries `WRITE` on
  the store can hand `index::search` arbitrary bytes. That is the previous audit's lens on a surface
  that postdates it, so it is a lane rather than a paragraph here.
- **Whether the generation half of a tid should be visible at all.** It is a machine-wide counter
  printed by `ps`, and `crates/ps` records its shape as cosmetic ugliness ("a large and ugly number
  after any slot reuse") without noticing it is information. Adjacent to finding 1 and to the missing
  `CMD` column's reasoning, which already worries about information that is not authority.
- **The `Rights::ENUMERATE` consumers milestone 126 named but did not build**, `Aspace` for `pmap`
  and `Untyped` for `free`. Both now receive the bit from `RETYPE_OBJ` and no arm reads it, so the
  grant is inert; the audit's only note is that when an arm starts reading it, every capability minted
  since 2026-08-17 already carries it, so the new operation is retroactively available to holders who
  were never assessed for it. That is worth deciding rather than discovering.

## Process notes on the mechanism itself

The recorded procedure in `design/audit-reports/README.md` was followed as written and worked. Two
observations for whoever maintains it:

- **Step 4, "re-baseline the docs in the same lane", earned its place here.** Three of six findings
  are documents that overclaim, and two of those three were found by reading a `BUGS` section against
  the body of the same file. A lens aimed at code found most of its material in prose, which is an
  argument for keeping that step rather than a complaint about it.
- **The `Findings` cell counts dispositions but not severity**, so a row reading `fixed 3, minted 1,
  accepted 3` cannot distinguish this audit from one that found something exploitable. The report's
  first paragraph is the only place that says so, and the index is what a reader meets first. Not
  proposed as a change, because a severity column invites arguing about the scale instead of writing
  the paragraph; recorded because the next person to read the index should know what it does not
  carry.
