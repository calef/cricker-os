# 39. Repository structure for a loosely-coupled OS, and the road to a distribution

**Status: RECORDED.**

**Prior art to read before designing packaging:** `design/haiku-bfs-and-packages.md`. Haiku's `packagefs`
**activates** packages rather than installing them, composing the filesystem view from a set of read-only
package files instead of letting installers mutate shared directories. It reached a shape close to milestone
47's conclusion that **installing a program is granting it into a namespace**, from an entirely different
motive (atomic, rollback-able installs), which is the useful kind of convergence.

**In brief.** **Analysis recorded, no decision taken.** The tree is a monorepo for a deliberately loosely-coupled system, and it is straining in measurable ways: `user/` is 28 binaries and 9,324 lines in one crate that is also a shared library, `fs_server/` has already escaped into its own workspace for real dependency reasons, `crates/` conflates kernel proof crates with wire contracts and userspace runtime so the boundary a third party cares about is invisible, and every crate is version 0.1.0. Four options are written up with their trade-offs (restructure in place; multiple workspaces in one repo; split repos; monorepo plus a later distribution *manifest* repo), along with a naming argument (**components** and **services**, never "daemons", because a Unix daemon is defined by the ambient authority this OS does not have) and the observation that milestone 31's program manifest plus §22's measured-boot hashing are already three quarters of a package format

**Why it matters.** **the structure has to serve the thesis, and one constraint dominates.** A single `script/test` proving the whole system on both ISAs is this project's credibility mechanism and what makes rule 5 a gate rather than an aspiration; splitting repos trades that for decoupling nothing external needs yet. Recommendation recorded (monorepo now, distribution as a separate manifest repo, executed as multiple workspaces, not before 23 forces it) so the eventual decision starts from evidence rather than from taste

**Status: analysis recorded, NO DECISION TAKEN (2026-07-30, Chris's request).** Deliberately a
roadmap milestone rather than a `design/decisions/` entry, because nothing was decided; §-sections are
for decisions, and recording an undecided question as one would be a lie about its status. This
block exists so the analysis is not lost and so the eventual decision starts from evidence.

The question Chris raised: cricker-os is a monorepo for a microkernel, but it is a collection of
deliberately loosely-coupled things, and the structure may not support that long term. Plus a
naming question (should the userspace servers be "services" or "daemons"), and the observation that
a Linux-distribution-shaped layer will eventually sit on top of the OS components.

## Where the current structure is straining, measured rather than felt

- **`user/` is one crate doing two incompatible jobs.** 28 binaries, 34 files, 9,324 lines. It is
  also a library: `net_transport` and `socket_test_client` are shared modules sitting
  beside the programs that consume them. So no component can express "I need the virtio driver bits
  but not the network stack", every component rebuilds when any shared module changes, and no
  component can take a dependency without handing it to all 28.
- **One component has already escaped, for real reasons.** `fs_server/` is its own workspace with its
  own `Cargo.lock`, because RedoxFS's default features pull `fuser` (whose build script panics on
  macOS) and its core wants `std` under test. Milestone 36 did the same to the toolchain by requiring
  a cross-capable clang. Two instances is a pattern: the first components with genuine dependency
  needs of their own had to leave.
- **`crates/` conflates three audiences with different rules**, so the boundary a third party would
  care about is invisible: kernel proof crates (`capability`, `paging`, `frames`, `regions`, `slots`,
  `asid`, `intrusive`, `dtb`, `elf`, `dma_validator`, `measured_boot`, `user_heap`, Kani-proved and nobody
  else's business), wire contracts (`fs_proto`, `gfx_proto`, `line_editor`, `compositor`, `abi`, the
  **only** things an external component needs), and userspace runtime (`user_rt`, `grant_plan`,
  `crickerfs`, `pci`).
- **Every crate is `version = "0.1.0"`.** Correct for internal crates, fatal for a published
  contract, and contracts are exactly what milestone 23's live replacement makes into a compatibility
  surface.
- **Not everything in `user/` is a service.** `heeder`, `spinner`, `flaky`, `allocator_exerciser`, `worker`,
  `builder`, `coremark`, `os_primitives_benchmarker` are fixtures and benchmarks. Mixing them with `net_stack`, `display`,
  `compositor` and `line_editor` is much of why the directory reads as shapeless.

## Naming: components and services, not daemons

A technical objection rather than an aesthetic one. A Unix **daemon** is defined by what it detaches
from: no controlling terminal, inherited ambient authority, a pid file, started by init holding the
system's privilege. Every one of those is something this OS deliberately does not have, and
importing the word imports the model. The project has already had to push back on exactly that pull
(§27's "open-by-path exists only inside the server", §24's "Ctrl-C is a capability, not a signal").

The project's own word already carries the thesis: milestone 23 is "a capability-routed **component**
OS with live replacement", "a vendor-shippable unit behind a stable contract". Proposed vocabulary,
matching what DECISIONS already says:

- a **component** is the shippable unit, a binary plus its manifest (`components/`);
- a **service** is what it offers over a contract ("the FS service");
- a **contract** is the wire protocol (`contracts/`).

"Server" stays a fine role word inside a component (`fs_server` serves the FS service). "Daemon" gets
dropped.

## The four options

| | Shape | Buys | Costs |
|---|---|---|---|
| **A** | One workspace, restructured directories (`kernel/`, `components/`, `contracts/`, `runtime/`, `fixtures/`, `tools/`) | Legibility, cheapest | Does not fix per-component dependencies unless each component also becomes its own crate, which is the actual work |
| **B** | One repo, multiple workspaces (generalize what `fs_server/` already does, driven by `xtask --manifest-path`) | Real dependency isolation; a component can use `std` or a foreign toolchain without infecting the kernel build | More lock files, slower cold builds, more complex xtask |
| **C** | Split repos: kernel, components, distribution | Maximum decoupling; what an ecosystem with third-party components looks like | **The integration gate**, see below |
| **D** | Monorepo now; distribution as a separate *manifest* repo later | Keeps the gate; distro consumes released artifacts, the way Yocto, Buildroot and Alpine aports separate recipes from sources | Defers the decoupling question rather than answering it |

## The constraint that decides it

**The single-command gate across both ISAs is the project's credibility mechanism.** One `script/test`
boots the kernel and proves the whole system on aarch64 and riscv64, including every component's
confinement, and rule 5 (DECISIONS §19) says parity is a gate and not an aspiration. Split into
separate repos and that becomes a multi-repo CI problem where the integration proof either lives
somewhere awkward or quietly stops running on every change. For a demonstrator whose entire argument
is "measured, both architectures, same suite", that is an expensive thing to trade for directory
cleanliness *before any external party needs it*.

**Recommendation: D, executed as B, and not before milestone 23 forces it.**

## The packaging observation worth acting on early

Most of a package format already exists and is not called one. Milestone 31's per-program **manifest**
(SHILL-adapted: declared endowment, checked at spawn) is package metadata. §22's measured boot already
hashes a component against a trust root. A distribution needs manifest, hash, version, and contract
version; three of those four exist. Naming that as the packaging layer would make the distribution an
assembly step rather than a new subsystem, and would give the contracts a reason to carry real version
numbers, which is what lets components evolve independently at all.

## Publishing crates is a different question from splitting the repo (Chris, 2026-07-31)

Chris asked whether the crates should get their own repos and builds, since some are useful outside
cricker-os. **Two decisions hide in that, and only one is expensive.**

`cargo publish -p calendar` works from a workspace. The thirty crates with no external dependencies
have no path dependencies to strip either, so **crates can be handed to the world without touching
the repo structure at all.** Splitting into separate repositories is the costly move, and the
constraint above already rules on it: a single `script/test` proving the whole system is what makes
parity a gate, and split repos let a crate be green in isolation while broken in the OS. That is the
exact failure rule 5 exists to catch, plus cross-repo changes become multi-PR dances.

**How many are actually useful outside is fewer than it feels: two to four of about thirty.**

| Crate | External value |
|---|---|
| `gpt` | **Real.** `no_std`, zero I/O, 8 machine-checked proofs. crates.io's `gpt` uses `std::io`, so an I/O-free proof-carrying one is a genuine gap |
| `calendar` | **Real**, competing with `time` and `chrono`; the differentiator is the proofs plus strict `no_std` |
| `dtb`, `pci` | Plausible, though `fdt` already occupies much of that space |
| `ntp_proto` | Overlaps heavily with ntpd-rs's mature `ntp-proto` |
| Everything else | Bound to our kernel model (`capability`, `slots`, `frames`, `regions`, `ipc`, `paging`, `asid`) or specific to us (`crickerfs`, `grant_plan`, `dma_validator`) |

**The argument for publishing is a thesis argument, not a utility one**, and that is the version worth
acting on. A `no_std`, I/O-free GPT parser carrying eight machine-checked proofs is a **publishable
artifact**, and it is stronger evidence for the verified-Rust claim than any write-up, because a
stranger can run the harnesses. Same for `calendar` over all 3,652,425 days in range. Publishing then
is not about sharing utilities; it is about putting the verification claim somewhere it can be checked
by people with no stake in agreeing with us.

**The trigger: wait until each crate has a real in-tree consumer.** `calendar` shipped 2026-07-30 and
`date` does not exist yet; `gpt` has no caller at all. **An API that has never had a consumer is not
ready to publish**, because the first real caller always finds the shape wrong, and after publication
that costs a semver break rather than an edit. So `date` exercises `calendar`, `mkfs`/partitioning
exercises `gpt`, the NTP client exercises `ntp_proto`: publish after, not before.

**Two frictions to know now.** The crates.io names `gpt` and `calendar` are almost certainly taken
(worth checking rather than assuming), so they would need prefixing, which quietly weakens the
generally-useful-library pitch. And publishing a proof-carrying crate is a **promise to strangers that
the proofs keep passing across Kani versions**, which is a maintenance obligation §14 does not ask
for: we are a demonstrator, not a library vendor. `design/capsicum-and-the-retrofit-question.md`
records CloudABI dying partly of maintenance rather than of being wrong.

**Recommendation, not a decision:** keep the monorepo, publish selectively once each crate has an
in-tree consumer, and treat publication as evidence rather than as a product line.

## The cheap first move, which commits to none of the four

**Split `user/` three ways**: `components/` for the services, `fixtures/` for the test programs, and
lift `virtio`, `net_transport`, `socket_proto`, `suptree` into `runtime/` crates. That ends the
crate-is-both-a-program-collection-and-a-library problem, makes dependencies expressible, and leaves
the gate untouched.

**Whichever option is chosen, do the move as one mechanical commit with the pairing audited.**
Renaming directories touches `xtask`'s `--bin` lists and the initrd packing, and a union merge in
exactly that code dropped a `--bin` flag on 2026-07-29 and duplicated a loop header the same day. It
must not be folded into feature work.
