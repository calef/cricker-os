# 120. The rename: the OS becomes `nife`, and the project gets an organization

**Status: NOT-STARTED.** Minted 2026-08-13 by Chris, who chose the name and the shape in the same
conversation that established why the merge queue could not be switched on.

**Gate: NONE.** The naming is decided and nothing technical blocks a start. Two steps are Chris's to
perform rather than to decide, because they need his GitHub account: creating the organization and
transferring the repository. The in-tree rename can be prepared before either.

## What changes

| Thing | Today | Becomes |
|---|---|---|
| the organization | none; `calef`, a user account | `crickertech` |
| the OS | `cricker-os` | **`nife`** |
| the distribution | does not exist | **`basalt`**, reserved now and built later |
| the shell, and every other program | `swish`, ... | unchanged |

## The convention, which is the part a stranger has to be able to infer

Two names imply a scheme, and this one is worth stating outright because third parties will need it
and principle 3 says they must not have to ask:

| Tier | Named after | Because |
|---|---|---|
| the kernel | **the core**, `nife` | one thing, at the centre, with nothing beneath it |
| distributions | **rocks**, `basalt` and whatever others choose | an aggregate assembled from many pieces |
| programs and components | **minerals**, `kamacite` and `taenite` held for this | what a rock is made of |

**A rock is an aggregate of minerals, and a distribution is an aggregate of programs.** That is what
the words already mean rather than an analogy stretched to fit, and it is what makes the scheme
inferable from two examples instead of needing a specification.

Three properties make it work for people this project will never meet. It is **unbounded**: there are
hundreds of named rocks, so nobody needs permission, a registry, or an allocation from us, and the
alternative (everyone wanting to be `nife-something`) is a namespace we would have to police. It is
**self-classifying**: a reader meeting `anthracite` knows it is a distribution and not a kernel
before reading anything about it. And it lets a distribution **state its character in its own name**,
which is what makes a convention get adopted rather than resented: `pumice` for a light one,
`conglomerate` for one that bundles everything, `quartzite` for a hardened one, `flint` for a
toolkit.

**Nothing here is reserved beyond `basalt`.** Naming a distribution requires no coordination with
this project, and that sentence is the difference between an ecosystem and a trademark.

## Why `nife`

**Nife** is Eduard Suess's name, from *Das Antlitz der Erde*, for the Earth's nickel-iron core: Ni
plus Fe. It is the layer everything else rests on and the one nothing is beneath, which is a
microkernel described geologically rather than by analogy. §39's test is that a name is a claim made
before a reader sees a line of code, and this claim is true of the thing.

Three things it buys beyond the denotation:

- **The alloy family has receipts.** NiFe is Edison's nickel-iron cell, which runs for thirty to
  fifty years and of which century-old examples still work; permalloy and mu-metal, the standard
  materials for **magnetic shielding**; and Invar, chosen when a thing must not move. Durable,
  shielding, dimensionally stable is an unusually apt set for a system claiming dependability.
- **It is four letters.** `nife`, `nife-dev`, `nifefs`. Terseness is an emergent pressure on words
  people type constantly, and this one starts short rather than being abbreviated later.
- **It contains `kamacite`.** Kamacite and taenite are nickel-iron alloys, so the meteorite story
  stays available, and both remain free as component names inside a family that means something.

**The pronunciation is declared rather than inherited**: *nife*, said like **knife**. Suess wrote in
German, where it is closer to NIF-eh, and no English speaker produces that on sight. `nginx` settles
its own pronunciation in one line and so does this. The knife reading is apt rather than merely
tolerable: sharp, single-purpose, and a held tool with no authority of its own.

**The honest cost, recorded because it does not go away**: "nife" autocorrects to "knife" in every
search box, so the name will always need a companion word to be findable.

### The names that were refused, and why

Milestone 115's point is that the refusals are the valuable half.

- **`patina`**. The idiomatic figurative sense in English is a thin attractive surface over
  something worse, which is the opposite of the claim a verification project makes. Also
  architecturally backwards: a patina is the layer on top, and a microkernel is the bottom.
- **`lemma`**. Architecturally exact (the small proved thing larger results are built on) and
  rejected by Chris on taste.
- **`keystone`**. Unavailable: Berkeley's Keystone is an open-source framework for TEEs **on
  RISC-V**, which is this project's second architecture. Same `capsh` failure: a reader arriving from
  RISC-V security would assume ours is that.
- **`psyche`**. The asteroid is an exposed planetesimal iron core, which is the denotation we
  wanted, but it is a NASA mission and a psychology term.
- **`siderite`, `kamacite`**. Both good, both longer, and kamacite is a phase *inside* a nife rather
  than the material itself. Kept in reserve as component names.

**The distribution's name went through the same test and two candidates lost it.** `sial` and `sima`,
Suess's own terms for the crust, were the first proposal and were dropped for one reason: Chris had
to ask how to say them. That is §39's decisive evidence, the same kind that retired `linedisc`, and
it lands differently here than it does for the kernel. `nife` earns its declared pronunciation
because the wrong reading (*knife*) is sharp, single-purpose and a held tool, so the mistake flatters
the project. `sial` has no such compensation: its wrong readings mean nothing at all, and a family in
which every member needs a pronunciation note stops being a signature and becomes a tax on every
conversation. `granite` replaced them and was itself passed over, because IBM ships a prominent
family of open models under that name. **`basalt`** is the most common volcanic rock on the planet's
surface, which is exactly the workhorse claim a first-party distribution wants, and it is the rock
that `sima` actually is, so the layering survives the words being retired.

## Why an organization, and why it is not called `nife`

**The concrete unblock is milestone 119.** GitHub's merge queue is available only in repositories
owned by an **organization**; this one is owned by a user account, which is why the setting is absent
from the ruleset page rather than merely hard to find. 119 names the merge queue as one of two
structural levers and leaves it to Chris; this milestone is what makes it reachable at all.

**The organization is deliberately not named after the OS.** An org named `nife` makes `basalt` and
`swish` read as subsidiaries of the kernel rather than as peers, and it has no room for anything that
is not the OS. Canonical ships Ubuntu and Red Hat ships Fedora for the same reason. `crickertech` is
already Chris's domain, costs nothing, and gives the old name a retirement rather than a deletion: it
stops being the product and becomes the publisher.

## The work, measured

**1,001 occurrences of `cricker` across 255 files**, counted from the merged tree on 2026-08-13:

| Identifier | Count |
|---|---|
| `cricker-os` | 264 |
| `crickerfs` | 218 |
| `cricker` (bare) | 195 |
| `cricker-dev` | 28 |
| `cricker-attrs` | 24 |
| `crickerfs_roundtrip` | 21 |
| the rest (`cricker-qemu`, `cricker-farm`, `cricker_first_lba`, ...) | ~30 |

### Five hazards, each with a recorded reason

1. **This is not a `sed`.** CLAUDE.md records a blind `sed` that swept a rename across the tree and
   rewrote the very row recording that the name had been *refused*, destroying an expensive record
   with a cheap edit. A thousand occurrences in two hundred files is exactly the scale at which that
   recurs. Rename by category, reviewing each: identifiers, filenames, prose, and the history that
   must not be rewritten because it describes what the name used to be.

2. **§45's partition GUID must not move.** That decision says a `cricker-os` partition is
   `EC5CC08B-D749-4434-AC38-A274C50385BA` **and that never changes**. The GUID is an on-disk
   identifier that existing images already carry, so the rename must amend §45's prose and leave its
   number alone. A rename that renumbers an on-disk identifier is a data migration wearing a
   cosmetic disguise.

3. **`cricker-dev` is account-wide.** It is a `rustup toolchain link` in `~/.rustup/toolchains`, not a
   file in this tree, and every worktree and lane resolves it. Renaming it is a machine-level
   coordination step: relink from the main checkout, and expect the first lane that gates afterwards
   to take the new name.

4. **`crickerfs` becomes `nifefs` and the format does not change.** `NAME_LEN` is 32 bytes and
   `nifefs` is shorter than what it replaces, so nothing in the archive layout moves and every image
   regenerates from the crate anyway.

5. **Do not split the build.** §80 landed on 2026-08-13 and decided one build for the kernel and
   everything that runs on it, with the threshold being *ownership*: the moment this project runs a
   program whose source is not here (milestones 64, 99, 66). Reserving `basalt` as a name and an empty
   repository is free and forecloses nothing. Creating a second build today would contradict a
   decision made the same week.

## Ordering, and it matters

Every merged pull request adds call sites, and every open pull request conflicts with a
tree-wide rename. Those pull in opposite directions, so the sequence is not arbitrary:

1. **Drain the open queue to zero.** In flight as of 2026-08-13: #123, #130, #131, #135, #138, #141,
   #142, #149.
2. **Create the organization and transfer the repository.** Chris; nobody else can.
3. **Rename in one reviewed pass**, with no lanes open, so the rename conflicts with nothing.
4. **Relink the toolchain** from the main checkout.
5. **Enable the merge queue**, which milestone 119 owns. The `merge_group` triggers it needs are
   already written (pull request #149): without them every required check waits forever on a run
   that never starts, and the symptom looks like a hung queue rather than a missing trigger.

## Prior art

The technical prior-art questions do not apply to a rename, but the organizational one does. The
separation of publisher from product is the norm rather than an invention (Canonical and Ubuntu, Red
Hat and Fedora, the Rust project and `rustc`), and the failure it prevents is visible in projects
whose org and flagship share a name: the second product always reads as a subsidiary of the first.

Redox is the closest neighbour in this tree's own reference set and does the opposite, publishing
`redox-os/redox`, which is the shape this milestone declines.

## BUGS

- **A rename cannot be un-published.** CLAUDE.md's own reversibility tenet puts names in the
  expensive column: trivial to change mechanically, expensive in a reader's head. The mitigation is
  timing rather than technique, and it is the reason to do this now: the project has one customer and
  no audience, so the cost is at its lifetime minimum and rises from here.
- **GitHub redirects the old URL but not everything else.** Clones keep working through the redirect;
  a hard-coded `calef/cricker-os` in a workflow, a badge, or a bookmark does not necessarily.
- **This milestone does not deliver `basalt`.** It reserves the name and the repository. What a
  distribution actually packages is `design/what-a-distribution-packages.md`, and nothing here
  answers it.
