# 82. Ambient authority is the problem; replacing the ecosystem, not confining it, is the end state

**Status: DECIDED.** calef, 2026-08-13, stating the project's thesis in full for the first time. §14
recorded what this kernel *is*; this records what problem it exists to solve, why the attempt is
worth making now rather than a decade ago, and what winning looks like. It **amends §14's end
state** and leaves its technical shape intact.

## The problem

**A program's authority comes from who ran it, not from what it was handed.** That is ambient
authority, and it is the default in every mainstream operating system. A process inherits the whole
of its user's reach: every file that user can read, every socket that user can open, every other
process that user can signal.

The consequences are not exotic. A package in a build tree reads the developer's private keys. An
archive extractor writes outside the directory it was pointed at. A document viewer, asked to render
one file, can enumerate the disk. None of those are bugs in those programs. They are the model
working exactly as designed, and the industry's response has been three decades of adding fences
after the fact: chroot, containers, seccomp, AppArmor, sandboxes, and now permission prompts. Each
narrows some ambient authority for some program, and each is opt in, bypassable, and separately
configured.

**A capability system does not narrow ambient authority. It never grants it.** A program holds
exactly what it was handed and can name nothing else, so the extractor cannot traverse out because
there is no "out" it can express.

## The proposal

A **proven microkernel**, **capabilities**, and **Rust**, doing three different jobs that are easy to
conflate:

- **Capabilities** remove ambient authority. This is the thesis.
- **The microkernel** makes the trusted core small enough that proving it is tractable at all.
- **Rust** removes the memory-safety class by construction, so the proof does not have to carry it.
  §14's argument stands: seL4's proof bears the entire safety burden because C gives it nothing.

## Why now, which is the part that is actually new

**The capability model is old and was never refuted.** KeyKOS, EROS and Coyotos were not shown to be
wrong. seL4 is proven, shipping, and confined to narrow embedded niches. What defeated the others was
never the model; it was the price of the ecosystem. An operating system whose security depends on
programs being written to hold explicit authority needs those programs to exist, and rewriting the
world was not a thing anyone could afford.

**The claim is that LLMs move that price**, and this project is the first evidence for it. CLAUDE.md's
second principle records the measurement: on 2026-08-05, 24 days from the first commit, 63 milestones
built, 43 crates, 54 user programs, roughly 124,000 lines of Rust, 112 proof harnesses, two
architectures, with a booting kernel, a shell, a filesystem, a network stack and a compositor.

That is the argument for the timing, and it is worth more scepticism than the rest of this section.
See the qualifications.

## The end state: replacement, not confinement

**This is what amends §14.** That section adopted seL4's resolution, which is to verify a small
trusted core and run *real, unverified workloads* in confined userspace above it. That remains the
technical shape of the system and is not retracted.

What changes is the destination. **Confinement treats the C and ambient-authority ecosystem as
permanent** and builds a box strong enough to hold it. **Replacement treats the box as the thing that
makes a rewrite worth doing**, and expects the software above it to be rewritten in Rust, holding
explicit authority, over time.

That reclassification is not rhetorical. Under §14, porting a real program is a **demonstration** that
the box works. Under this section it is the **product**. Milestone 121 (`ripgrep`) is the worked
example: as a demo it proves confinement; as product it is one program of an ecosystem, and the
question becomes how narrow a grant it can run under rather than whether it runs at all.

## What would falsify this

Recorded because a thesis that cannot be wrong is not a thesis.

- **If ported programs are only useful under grants wide enough to be ambient in practice**, the
  model has lost on the axis that matters, and the fences-after-the-fact approach was right all
  along. The measurable form: the width of the grant a ported program actually needs.
- **If the port economics do not hold outside new code**, then "now" is wrong even if the model is
  right. This project has demonstrated writing new code to a new design very fast. It has not yet
  demonstrated porting an existing ecosystem, which is a different problem.
- **If the proof does not scale past the capability core**, the "proven" half is decoration, and what
  remains is an ordinary microkernel with good hygiene.

## Three qualifications, because the thesis will be attacked at its weakest joint

**The LLM claim is the newest and the least evidenced, including by us.** What is demonstrated is new
code to a new design, not ports of existing software, and porting is where the difficulty is semantic
compatibility rather than typing speed. Milestone 64's measurement cuts both ways: 35 of 50 crates
built with no change, which is encouraging, while the failures cluster precisely where an LLM helps
least, on C build scripts and on crates whose last `cfg_if` arm assumes unix.

**The Rust half is close to table stakes; the capability half is the actual claim.** Rust in the
kernel and Rust rewrites of userland are already well-funded industry direction, and they deliver
memory safety with ambient authority entirely intact. That path does not need us. What almost nobody
is attempting at scale is removing ambient authority, and it is the harder sell because it changes
how programs are *written* rather than what they are written in.

**Cheap porting makes bad porting cheap, and this is the real risk to the thesis.** A program written
for a Unix assumes it may walk a tree, open a path, and find its configuration at a known location. If
porting becomes nearly free, the default outcome is an ecosystem of ports that quietly reconstruct
ambient authority inside a capability system, at which point the box holds nothing and the whole
exercise is theatre. The discipline that prevents it is the first thing volume erodes.

This tree already has the counter-example done correctly. Milestone 40 gave `doc` **no directory
capability at all** and built its search index on the host, precisely so that a viewer could not
discover what it was not given. That is the standard a port has to meet, and meeting it is work that
no amount of translation speed performs for you.

## BUGS

- **This states a destination, not a plan.** No milestone here is on the critical path to
  "replace the ecosystem", and nothing in this section says which would be. It ranks work only
  through CLAUDE.md's first principle, which is the customer path.
- **"Replace the ecosystem" is unbounded and therefore unfalsifiable as stated.** The falsification
  conditions above are the honest part; the slogan is not. The tree should never claim more than the
  workloads it actually runs, which today is none of somebody else's.
- **Nothing measures grant width.** The first falsification condition names the number that would
  settle the argument, and no tooling produces it. Until something does, the claim that ports run
  under narrow authority is an intention rather than a result.
