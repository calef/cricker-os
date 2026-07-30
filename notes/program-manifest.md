# The program manifest: a component contract in embryo

Milestone 31, phase 1. A **manifest** is a program's declared endowment: what it expects to be
granted, written down where the shell can check a command against it before spawning anything. It
is the SHILL idea (OSDI 2014: capability contracts for scripts) shrunk to what phase 1 needs, and
it is milestone 23's component contract in its smallest honest form. The type and the checker live
in `capsh` (host-tested); this note is the why and the format.

## The problem it solves

Without a manifest, a mismatch between what a command grants and what a program needs surfaces late
and badly. Grant a program too little and it hangs or faults deep inside, on a capability it assumed
was in a slot that is empty. Grant it something it does not understand and the authority leaks
silently. Both are mystery failures at runtime, far from the command that caused them.

The manifest moves the failure to the prompt. The shell checks the command's grants against the
named program's manifest **at spawn**, before a child exists, so a mismatch is a legible refusal on
the line you typed:

```text
$ run budgeter
  budgeter: needs a memory grant; add --mem <pages>
```

Nothing was built, nothing hung. The contract was checked where you could still read it.

## The format

A manifest declares what a program expects to be handed (`capsh::Manifest`):

```rust
struct Manifest {
    arg:     ArgSpec,   // Required | Forbidden   -- does it take an integer argument?
    mem:     MemSpec,   // Forbidden | Required { min, max }  -- a memory grant, in pages?
    file:    FileSpec,  // Forbidden | Required { writable }  -- one file (phase 2)
    reports: bool,      // is it endowed the shared result endpoint?
    interruptible: bool,// does ^C reach it (milestone 24)?
}
```

### The file endowment declares a direction, not a name

`FileSpec::Required { writable }` is the phase-2 addition, and the interesting part is the split it
makes: **the manifest declares the direction, the command line designates the file.** So `run wc
report.txt` reads and `run tee report.txt` writes, and the human never types a mode.

That is SHILL's shape and it is the right one on both halves. Whether a program writes is a property
of what the program does; it is fixed, publishable, and not the caller's to guess. Which file it
should touch is entirely the caller's business and belongs on the line. Authority is still exactly
what the command says, because the program's half is written down where the shell (and the human) can
read it: `caps run wc file:report.txt` prints "read-only, and nothing else on the disk".

One file, not a list. A program that needs two needs a manifest that says so, and that is a later
widening rather than something to leave ambiguous now.

The two phase-1 programs:

| program    | arg        | mem                  | file      | reports |
|------------|------------|----------------------|-----------|---------|
| `worker`   | Required   | Forbidden            | Forbidden | yes     |
| `budgeter` | Forbidden  | Required 1..=64 pages | Forbidden | yes     |

No shipped program declares a file yet, because the shell it would be spawned from holds no directory
to narrow (notes/grant-expression.md says why, and why that refusal is true rather than pending). The
`FileSpec::Required` logic is not therefore untested: `plan_against` takes an **explicit** manifest
rather than reading the static table, so the host tests check a manifest shape no program declares.
That split was worth making anyway, because milestone 23 needs exactly it: a manifest that travels
with a component, checked by a composer that did not write the program.

`worker` needs its `n` and no memory; granting `--mem` to it is a refusal. `budgeter` exists to
spend a budget, so it *requires* `--mem` (the lower bound of 1 makes "budgeter with no grant" a
refusal), with an upper bound the shell's own budget can actually back.

## The check, and its order

`capsh::plan` resolves a parsed `run` against the manifest and yields either an `Endowment` (exactly
what to grant) or a typed `Refusal`. The checks run in a deliberate order: a designated resource the
shell **cannot back at all** (a `file:PATH`, until milestone 32) is reported before any manifest
quibble, because "you named something I hold no capability for" is the milestone's headline and
should win over "and also your --mem is out of range." After that come the un-placeable extra
argument, the program name, then the argument and memory rules.

Each refusal carries a fixed message (`Refusal::message`), host-tested so the wording cannot drift,
and the shell prefixes the program name. The strings are part of the deliverable: a refusal must
read like the capability model, not like errno. See [grant-expression.md](grant-expression.md) for
the full refusal catalog.

## Why it lives in the shell, not the kernel

The manifest is a **userspace** contract, checked by the party doing the granting. The kernel does
not read it, does not enforce it, and does not need to: even if the shell skipped the check and
granted a program too little, the program would fault on an empty slot and die, harming only itself,
because there is no ambient authority to fall back on. The manifest is not a security boundary; the
capability model is. The manifest is a **usability** boundary, turning a deep mystery hang into a
one-line refusal at the prompt. That is exactly the altitude SHILL's contracts sit at, and exactly
what milestone 23's components will formalize: a component that declares the capabilities it needs,
checked by whoever wires it up, so a bad wiring is caught at composition, not at runtime.

## What grows from here

- **milestone 23** turns this from a static table keyed by program name into a contract a component
  *ships with*, so the shell (or any composer) checks a program it did not write against the
  program's own declaration. The shape is the same; the manifest just travels with the binary.
- **milestone 32** adds file and directory grants to the endowment vocabulary, so a manifest can
  declare "one readable file" and the checker can match a `file:PATH` designator against it. The
  `ArgSpec`/`MemSpec` pattern extends directly to a `FileSpec`.
