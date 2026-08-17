# The component manifest

*Milestone 23's second residual, built 2026-08-17. `crates/component_plan`, the four declarations in
`crates/swap_proto`, and the operator that no longer contains an endowment. The mechanism it serves
is DECISIONS §41 and notes/live-replacement.md; read those first if you want the swap itself.*

## The defect, in the roadmap's own six words

> **endowments are literals in the operator's source**

§41's central sentence is that *any program which speaks the protocol and holds the right
capabilities **is** the component*. The protocol half of that had been in a crate since the
milestone was built. The capability half was four arrays inside `swapper`:

```rust
let instance_caps = [
    (w.svc, abi::rights::READ),   // swap_proto::SVC:  we may answer here
    (REPORT, abi::rights::WRITE), // swap_proto::RPT
    (w.note, abi::rights::WRITE), // swap_proto::NOTE
    (w.poke, abi::rights::READ),  // swap_proto::POKE
];
```

Three things are wrong with that, and only the first is the one the block names.

**A different vendor's build is not a drop-in**, because what to hand a component lived in the
program that starts one. Swapping in `c_swappable` worked because somebody had typed its endowment
into `swapper` too.

**The slot agreement was a comment.** `swap_proto`'s own header said "the operator's
`ChildEndowment.caps` lists them in this order, so they land in these slots", and that sentence was
the only thing holding two files together. A reordered array would have produced a component
receiving on its report channel and sending its answers to the operator's coordination channel, with
nothing to see but a hang.

**The rights were typed by hand at six call sites.** `READ` on an endpoint means the holder may park
in `RECV_CAP`; `WRITE` means it may only ask. §41's whole confinement claim is that a client of the
stable endpoint cannot become its server, and the test that proves it endows an attacker with the
honest client's exact capabilities. One character wrong in `client_caps` and that test fails for a
reason no reader could find from the operator's source.

## Is a component manifest the same thing as `grant_plan::Manifest`?

It has to be asked, because `crates/grant_plan` already has a `Manifest` and it already does the
declaring-what-you-need job: `ArgSpec`, `MemSpec`, `FileSpec`, `DirSpec`, the short options a program
accepts, `clock`, `domain`. `caps <program>` prints a program's endowment before anything spawns, and
`InputSpec::Required` carries `writes_while_reading` so a program that writes while reading cannot be
declared without saying so. A second parallel manifest concept with a similar name would be a
comprehension disaster in a tree whose first tenet is that a reader meets a name before anything else.

**The answer is that they are siblings, not one thing, and the difference fits in a sentence.**
`grant_plan::Manifest` declares **what a human at a prompt may designate** for a program;
`component_plan::Requirements` declares **what a supervisor must route before a component can serve
anyone**. Four falsifiable differences follow, and they are checkable against the code rather than
matters of taste.

**There is no command line and no human.** `grant_plan` exists to make designation authorization: a
person types a file name and the typing is the grant. Every field of its manifest is about placing a
token from a line into a slot, or about refusing a line at the prompt. A component is started by a
supervisor with nothing typed. There is no token to place and no prompt to refuse at.

**The vocabularies are disjoint.** Nothing in `grant_plan::Manifest` can name an endpoint, a device
or a shared page. Nothing in `Requirements` can name an argument or a short option. The nearest thing
to an overlap is memory, and even there `MemSpec::Required { min, max }` is a *range a person picks
from* with `--mem N`, while a component's page count is an exact number the supervisor must supply.

**A component serves; a program only consumes.** This is the new axis, and it is the one §41's
central refusal turns on. `Direction::Serve` is `READ` and `Direction::Use` is `WRITE` on the same
endpoint object. `grant_plan` has no field that could carry it, and no program the shell spawns would
ever set one, because a shell-spawned program is a client of everything it holds.

**The keys are different, and this is the one that decides drop-in.** `grant_plan::Manifest` is
reachable only through `Prog`, a **closed enum with a static table compiled into the shell**. Adding a
component to it would mean adding a variant and recompiling the shell, which is the exact opposite of
what a vendor drop-in is. `Requirements` is a value the *contract crate* owns, so a build that speaks
the contract is wired by code that never heard of that build.

What the two **do** share is a shape, and the shape is the reusable idea rather than the struct: a
declaration, plus what the wirer holds, checked into an endowment or a typed refusal, before anything
is spawned. `grant_plan::plan(run, holdings, expansion)` and `component_plan::plan(reqs, provisions)`
are the same function over different worlds, and each crate's refusal enum is the same kind of
answer. That parallel is why the crate is called `component_plan` and not `component_manifest`: the
echo is the part worth teaching, and the distinguishing word carries the difference.

## Where a manifest lives, and why it is the contract's and not the build's

The declarations are `const` values in `crates/swap_proto`, next to the wire format they belong to.
Four of them: `CONSOLE`, `BACKEND`, `CLIENT`, `BROKER`.

**A `*_proto` crate declares what two programs agree on, and what a component must hold is the other
half of that agreement.** Putting it there is the same rule as rule 7 one level out. It also means
`rust_swappable` and `c_swappable` are wired from **one** declaration, which is the drop-in claim in
its smallest true form: two builds of one contract, in two languages, and the operator does not
distinguish them.

The interesting consequence is that a manifest is **not the build's request**. A build that needed
something a console component does not need would not be substitutable for one that did not, so
letting each build declare its own needs would be letting an untrusted party ask for authority. That
is Fuchsia's `use`/`offer` split and it is the security posture worth stating out loud:

> **A manifest is a request; the provisions are the authority.**

A component names its needs and may name anything it likes. The supervisor decides, **per child**,
which of its own objects each name resolves to, and a name it did not list resolves to nothing:
`Refusal::Unprovided`, before the component exists. So a manifest can never widen a component's
authority. It can only fail to be satisfiable.

The corollary is the property that makes a component substitutable at all:

> **The name is the component's and the object is the supervisor's.**

`swap_proto::CLIENT` asks to *use* an endpoint it calls `service`. On the direct channel the operator
routes that name to the shared service endpoint; on the queued channel it routes the same name to the
queue broker's front endpoint. One declaration, two routings, and `chatty` cannot tell which it got.
That is what makes a component's **peer** substitutable and not only the component.

## What the manifest declares, and what it deliberately does not

| declared | not declared |
|---|---|
| capabilities, by role name and direction, **in cspace slot order** | supervision: a component does not choose whether it is watched (§32) |
| pages, by role name and the virtual address its own code reads | start arguments: which log entries this instance writes, which of `chatty`'s three roles it is |
| how many pages of budget one instance is built out of | stack size, and everything else `ChildEndowment` defaults |

The line is that **a manifest declares what a component needs, not everything about how it is
started**. Supervision is done *to* a component and is the supervisor's call. Start arguments are
configuration rather than authority: they carry no capability, and a component that was lied to about
them misbehaves rather than escaping.

## The two rungs, and why they are different rungs

AGENTS.md's ladder says to reach for the highest rung that fits. This mechanism ends up on two,
because the two halves of "is this declaration right" are answerable at different times.

**Structure is a compile error.** `Requirements::problem()` is a `const fn`, and every real
declaration in the tree asserts on it in a `const` item:

```rust
const _: () = assert!(CONSOLE.problem().is_none());
```

A role declared twice, two pages at one address, or a component asking to exist in zero pages does
not compile, on both architectures, without any test having to run. Each of those three is a mistake
that would otherwise be invisible at run time: a component reading one of two slots, one mapping
silently winning, or a region split refusing for no visible reason.

**The slot numbers are derived, which is the same rung reached a second way.** `swap_proto::SVC` is
not `0` any more; it is `component_plan::slot_of(&CONSOLE, "service")`, computed at compile time from
the declaration the operator wires from. A role the manifest does not declare **does not compile**.
`chatty` and `broker` derive their own the same way, and because one binary serves both `CONSOLE` and
`BACKEND` there are four more assertions pinning the two declarations to the same slots.

**Provisioning is a runtime refusal**, because what a supervisor holds is only known when it runs.
That is `plan()`'s job, and its answer is a value the operator reports rather than a fault it takes.

## How it is gated, and what the gate actually shows

The existing three live-swap tests are unchanged in what they assert and now run entirely on
manifest-wired components: the client's own verdict from its own replies, the operator's witness page
read after every writer is dead, the attacker holding the honest client's exact capabilities, the
control that must fail (a post-revoke device read that faults, with the kernel as witness), and the C
replacement over §31's seam. Both architectures, the same suite. **That is the demonstration**: the
whole flagship still holds with the endowment removed from the operator, and `c_swappable` is wired
from the same declaration `rust_swappable` is.

Two things were added.

**A refusal that a person can recognise.** Each channel plans a **real** manifest it cannot satisfy,
and reports the refusal:

- The direct channel plans `BROKER`, which declares `requests` and `backend`. That channel routes
  neither, because the queue rung is the other role's system.
- The queued channel plans `CONSOLE`, which declares `uart`. That channel routes no device, because
  §41 keeps the device story off the queue channel on purpose.

Neither is a fixture. Each is the other role's component, asked for by a supervisor that genuinely
cannot provide for it. The test asserts the refusal is the *typed* one (a role went unrouted) and
that it arrives **ahead of every build step and every instance that started**, which is what makes a
manifest a request the supervisor may refuse rather than an instruction it carries out half way.

**A greppable claim.** `user/src/swapper.rs` contains no `abi::rights::*` and no
`abi::aspace::MAP_R*`. There is nothing left in the operator to type wrong.

Plus thirteen host tests and three Kani harnesses on the pure logic, which is where `component_plan`
lives for exactly that reason. The three proofs are the ones a case-by-case test cannot close: that no
routing table ever gives a component a right its declaration did not ask for (and never `GRANT`), that
a missing route refuses rather than falling through to slot zero (which in every program in this tree
is its construction budget), and that the device split is a partition for every declaration, whichever
order and kinds were declared.

## EXAMPLES

**Read what a component needs, without running anything.** The declaration is a `const` in the
contract crate, so it is the thing to open:

```sh
grep -A20 'pub const CONSOLE' crates/swap_proto/src/lib.rs
```

**Add a component to a system that already has one.** Three steps, and none of them is in the
supervisor:

1. Declare it in the contract crate it implements, and assert it is well formed:

   ```rust
   pub const RENDERER: Requirements = Requirements {
       contract: "renderer",
       caps: &[
           CapNeed { role: "frames", direction: Serve },
           CapNeed { role: "report", direction: Use },
       ],
       maps: &[MapNeed { role: "canvas", va: 0x0400_0000, kind: PageKind::Shared }],
       pages: 32,
   };
   const _: () = assert!(RENDERER.problem().is_none());
   ```

2. Derive the slots the component itself reads, so the two cannot drift:

   ```rust
   pub const FRAMES: u64 = component_plan::slot_of(&RENDERER, "frames");
   pub const RPT: u64 = component_plan::slot_of(&RENDERER, "report");
   ```

3. In the supervisor, say which of *its* objects answers to each name, and start it:

   ```rust
   let to_renderer = Provisions {
       held: &[("frames", my_ep), ("report", REPORT), ("canvas", my_page)],
   };
   let Ok(plan) = component_plan::plan(&RENDERER, &to_renderer) else { bail(70) };
   start_child(&image, &plan, faultep, [0, 0, 0], 71);
   ```

**Substitute a component's peer without touching the component.** Route its `service` name somewhere
else. That is the whole of what the queued channel does to `chatty`:

```rust
let to_producer = Provisions {
    held: &[("service", front), ("report", REPORT), ("operator", w.note)],
};
```

**Hand a device over on the far side of a revoke.** `component_plan` sorts device mappings last, so
both halves are slices of one plan:

```rust
// Build with everything except what the revoke is about to take.
let (tcb, aspace) = build_child_space(ROOT_UT, region, &elf, &ChildEndowment {
    caps: plan.caps(), maps: plan.maps_without_devices(), ..
})?;
// ... Frame::REVOKE ...
for &(va, slot, mode) in plan.devices() {
    invoke(aspace, abi::aspace::MAP_INTO, va, slot, mode);
}
```

## BUGS

**A manifest is compiled in, not shipped in the archive beside the build.** This is the honest limit
of what landed, and it is the difference between "the endowment is no longer in the operator" and "a
vendor hands over a binary and the system reads its needs out of it". The second needs a format two
programs agree on, which is AGENTS.md's expensive, irreversible category, so a lane does not decide
it. The two candidate shapes, recorded so the decision is a choice rather than a discovery:

- **An ELF section or note in the component's own binary.** One artifact, and the manifest cannot be
  separated from the thing it describes. The cost is real: `crates/elf` parses **program headers
  only** and is a Kani-proven, hostile-input-hardened parser on the boot path. Teaching it section
  headers or notes means extending exactly the parser this tree is most careful about, and the format
  becomes something every future component agrees on.
- **An archive member beside the image**, `rust_swappable.manifest` in the initrd, which is Fuchsia's
  `.cm`. Cheaper (no ELF change; `nifefs` already reads members by name) and weaker, because the two
  can be separated: a supervisor can be handed a binary with somebody else's manifest.

Neither is needed for anything on the customer path today. Both are a wire format, and the
recommendation in the lane's report is to decide it when a second supervisor or an out-of-tree
component actually exists, because until then the parsed format would be a format with one producer
and one consumer that are compiled together.

**`Requirements::pages` is a property of the build, not of the contract**, and it is the only field
here that is. How many pages an instance needs depends on its image size, its stack and its page
tables, so two vendors' builds of one contract can honestly differ. Declaring it per contract is
right only while every build of a contract fits the same number, which is true in this tree today and
is not true in general. It is the strongest single argument for the wire format above.

**A malformed declaration's compile error names the manifest, not the role.** `const` evaluation
reports an assertion that failed, so a reader is told which `Requirements` is wrong and has to find
the duplicate themselves. `Requirements::problem()` returns the `Refusal` with the role in it and a
host test prints the good message; the `const` assertion cannot.

**`MAX_CAPS` and `MAX_MAPS` are fixed at eight and four.** A `Plan` is a value in a `no_std` program
with no allocator, so the bound is a real limit: a manifest past it is refused rather than served.
Both are roughly double what the widest declaration in this tree asks for.

**Nothing checks that a routed object is the *kind* the role wants.** A supervisor that routes a frame
where the component declared an endpoint gets a plan, and the kernel refuses the `CAP_INSERT` or the
component's first `RECV` instead. This is deliberate rather than an omission: the declaration carries
the direction and the address, which is what a supervisor can get wrong *silently*, and the object
type is what the kernel already refuses loudly.

**The role names are provisional.** A lane may not mint a name, and `service`, `report`, `operator`,
`control`, `witness`, `uart`, `requests` and `backend` are the words a reader meets first. So is
`component_plan` itself, and so are `Requirements`, `Provisions`, `Plan`, `CapNeed`, `MapNeed` and
`Direction`.

**Three residuals of milestone 23 remain and this lane did not touch them**: state handoff (the crux,
and the reason the component here is near-stateless), dependency-aware orchestration, and the
hung-component case (§32's watchdog). A manifest is what dependency-aware orchestration will need to
read a dependency graph out of, and it does not carry one yet: a component declares what it needs but
not that another component is what supplies it.

## See also

- DECISIONS §41 (the endpoint is the broker, and a device is revoked by taking it back), §32 (a
  supervisor may collect a corpse without being able to build one), §31 (the foreign-language seam: C
  holds no capabilities and makes no syscalls), §12 (call/reply IPC: a one-shot reply capability),
  §16 (object revocation: reclaim the objects a process built)
- notes/live-replacement.md for the swap itself, its two witnesses, and the latency ladder
- notes/program-manifest.md and notes/grant-expression.md for `grant_plan::Manifest`, the sibling
- notes/verification.md for what the Kani harnesses cover and what they do not
- Fuchsia's component manifests (`use`/`offer`/`expose`) and seL4's CapDL, the two prior arts the
  roadmap block names
