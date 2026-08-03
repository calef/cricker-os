# What a distribution would package

Speculation, deliberately, about a system years further along than this one: if cricker-os became a
general-purpose OS with distributions built on it, what are the units they would ship?

**What this note does not cover, because other records own it.** Milestone 39 owns the repository
structure question and records four options with a recommendation and no decision. `design/haiku-bfs-and-packages.md`
owns the activation shape, where Haiku's `packagefs` composes a filesystem view rather than mutating
shared directories. Milestone 47 owns the conclusion those rest on, that a program namespace is an
endowment and therefore **installing a program is granting it into a namespace**. This note is about
the tier *above* those: what the shippable units are, and what versioning them costs.

## The question is different here, and the difference is not IPC

The usual microkernel framing is performance: functionality leaves the kernel, so calls become
messages. That argument has been had, and the benchmarks are in `notes/benchmarks.md`.

The packaging framing is more interesting and less discussed. In Linux, the kernel is one binary, so a
distribution differentiates entirely *above* it: same kernel, different userspace. In a microkernel,
much of what Linux calls "the kernel" is userspace components, so **a distribution can differentiate
inside that boundary**: a different filesystem server, a different network stack, a different display
path, chosen at build time or swapped at runtime (`notes/live-replacement.md`).

That is a product difference Linux distributions structurally cannot offer, and it lands in packaging
rather than in performance.

## Three tiers, which the tree already separates

| tier | what | rate of change | today |
|---|---|---|---|
| **contracts** | the wire ABI between components | rarely, and a break is an event | 10 `crates/*_proto` |
| **components** | implementations of a contract | independently | `blk`, `console`, `compositor`, `fs_server`, `net_stack`, ... |
| **programs** | leaf consumers of contracts | freely | 48 `[[bin]]` targets in `user/` |

The separation was not designed for packaging. The contracts are separate crates because Rule 7
forbade `#[path]` modules for anything two binaries share, and the motive was host-testability. The
packaging property is a side effect, and a fortunate one: **the compatibility surface is already
isolated, named, and separately tested.** A contract here is a crate with host tests, not a header
nobody validates.

A distribution then becomes **a choice of components satisfying a set of contracts**, plus programs.

## What installation means when there is no ambient authority

In Linux a package's danger is invisible at install time. `Depends:` and `Conflicts:` describe files
and versions; nothing describes what the thing can reach, because on Linux a binary can reach whatever
the invoking user can.

Here a program is inert until endowed (DECISIONS §10). `Prog::manifest` already declares what a
program may be granted, and `grant_plan::plan` checks an invocation against it **before anything is
spawned**. So:

- **Installing grants nothing.** It adds a name to a namespace, which milestone 47 already establishes
  requires holding the capability being extended.
- **The manifest is the package's security contract**, machine-checkable at install time and enforced
  at spawn time.
- **Least privilege is the floor rather than a policy someone configures**, and a package's declared
  endowment is a reviewable diff. A text editor asking for a raw block device is visible.

That inverts the usual review question from "do I trust this maintainer" to "does this endowment match
what this thing claims to do".

## The hard part, stated plainly

**Linux's monolith is a versioning strategy, not only an architecture.** Everything ships together, so
kernel-internal interfaces can churn freely; the stability promise is made at exactly one boundary,
the syscall ABI, and "we do not break userspace" is enforceable because one tree owns both sides of
every other interface.

A microkernel distributes that problem. If `fs_proto` changes, every implementer and every consumer
must agree, and they are separately versioned and separately shipped. **This, rather than IPC cost, is
the thing that has historically made microkernel ecosystems hard.** A note that predicts packaging
benefits without naming this cost is not being honest.

Two things here reduce it, and neither eliminates it:

- The contracts are **written down and separately compiled**, rather than being an implicit agreement
  between two large components. They are not all small: `fs_proto` is 3,539 lines. What matters is
  that the agreement has a name and a test suite, not that it is short.
- The contracts are **testable independently of their implementations**, so "does this component
  satisfy the contract" is a question with a mechanical answer, which is the beginning of a
  conformance suite.

What is missing is that **no contract carries a version today**. Every crate in the tree is `0.1.0`,
which milestone 39 already flagged. The section below explains why fixing that is neither free nor
urgent, and what the actual trigger is.

## What the strain looks like, measured

Milestone 39 recorded the monorepo straining on 2026-07-30. Four days later:

| | 2026-07-30 | 2026-08-03 |
|---|---|---|
| `[[bin]]` targets in `user/` | 28 | **48** |
| lines in `user/src` | 9,324 | **16,309** |

Both close to doubled in four days. That is not an argument for splitting the repository, which
milestone 39 argues against for a reason that still holds: one `script/test` proving the whole system
on both ISAs is this project's credibility mechanism. It is an argument that milestone 39's analysis
should be re-read against current numbers before its recommendation is executed, because the thing it
measured is moving quickly.

## What to do now, and the recommendation this note got wrong

The monorepo is right today. One team, contracts and implementations co-evolving, atomic changes
across both. Splitting now buys decoupling nothing external needs.

**An earlier draft of this note recommended two things "cheap now and expensive later": versioning the
wire contracts, and turning the manifest into an artifact. That recommendation was wrong, and it is
recorded here rather than deleted because the reasoning is the useful part.**

**Versioning is not cheap, because the request words are full.** `fs_proto` packs its first word to
capacity:

```text
  op      bits 63:56      (OP_SHIFT = 56)
  handle  bits 55:40
  len     bits 39:0
```

`sink_proto` and `gfx_proto` use the same `OP_SHIFT = 56` layout. So a version field has nowhere to
go: it means stealing bits from a live field, adding a word to every message, or introducing a
connect-time handshake. The handshake is almost certainly right, since a version is negotiated once
per connection rather than restated on every request, and that makes this **a protocol design question
rather than a mechanical bump**. Ten contracts, `fs_proto` alone at 3,539 lines, each with tests.

The one existing precedent points the same way and is not ours: `ntp_proto` has `VERSION: u8 = 4`,
which is NTP's own wire version inherited from the RFC.

**And neither has a consumer.** Every component in this tree compiles together from one source, so a
version mismatch is currently unrepresentable, and the manifest is a static Rust table because every
program that reads it is compiled beside it. Recommending both anyway contradicted this note's own
next paragraph, and CLAUDE.md's rule against building an abstraction before its requirements are
known. The rule was right and the recommendation was not.

### The trigger, which is what should be recorded instead

The irreversible step is not the absence of a version. It is **shipping a binary to someone who cannot
rebuild it**, because from that moment a version has to be inferred rather than declared. Nothing has
been shipped.

> When a component is first built outside this tree, or a binary is first distributed to someone who
> cannot rebuild it, the wire contracts need versions and the manifest needs to be an artifact. Until
> then neither has a consumer, and the shape of both should be decided by that consumer's requirements.

The specific trap to avoid in the meantime is visible: `crickerfs` is already a primitive package
format, and it would be easy to grow it into a real one before knowing what it must carry.

## See also

- `design/roadmap.md`, milestone 39: repository structure, four options, no decision taken
- `design/haiku-bfs-and-packages.md`: `packagefs` activation as prior art, and its honest limit
- `design/roadmap.md`, milestone 47: a program namespace is an endowment
- `notes/live-replacement.md`: swapping a running component, and how "the client did not notice" is proven
- DECISIONS §10 (no ambient authority), §46 (taking a dependency is a decision)
