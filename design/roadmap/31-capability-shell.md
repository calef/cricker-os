# 31. A capability shell: designation is authorization

**Status: PARTIAL.**

**Gate: NONE.** Phase 3 is exactly one thing with no decision in front of it: wire an FS service
into the interactive boot and flip `holdings()` in `user/src/swish.rs`. The block's own caution is
that nothing in the suite boots the interactive shell, so whoever takes it should gate that boot
first.

**In brief.** The command line becomes a **grant expression**: naming a resource in a command IS the capability grant (`wc report.txt` passes one readable file cap; `wc` alone can read nothing, and the refusal is "no such capability", not EPERM); untyped budgets as first-class grants; a SHILL-style manifest per program checked at spawn; a `caps` command printing a process's whole endowment. **Phase 1 built, both ISAs**: `grant_plan` (host-tested parse + manifest + spawn protocol), the shell over the existing surface, `--mem N` made real by the `budgeter` program, manifest refusals, `caps`/`caps <command>` introspection; one kernel fix, `Untyped::SPLIT` now grants the child `GRANT` (DECISIONS §16 amendment). **Phase 2 built, both ISAs**: the FS contract's `CREATE`/`TRUNCATE` (so `std::fs::write` works), and per-file grants as a **caretaker process** (`fs_file_caretaker`) that narrows a directory capability to one file in one direction, proven by a read-only and a writable attacker. One scope note: the interactive shell still refuses a named file because its boot wires no FS service, so it holds no directory to narrow. **The grammar shown here is milestone 47's**, which deleted the `run` verb and the `file:` designator this milestone shipped with; the mechanism did not change, only the spelling. Notes: grant-expression.md, program-manifest.md, fs-server.md

**Why it matters.** **no-ambient-authority made user-visible**: the inversion of Unix's model at the one interface a human touches. Milestone 23's component contract in embryo, met first at the shell

**Phase 1 built (both ISAs).** The command line is a grant expression: `grant_plan` (a host-tested crate)
parses it and checks it against a per-program manifest; the shell holds its own untyped budget and
delegates from it. `budgeter --mem N` splits N pages off the shell's budget and delegates the
untyped to init, which endows the child; the budgeter maps them and reports the count (15 of 16, the
rest paid for page tables), proving the grant is real, not parsed-and-ignored. Manifest mismatches
and a named file a program declares but this shell cannot back ("you hold no such capability") are
refused at the prompt; `caps` and `caps <command>` print a process's whole endowment. (The spelling
is milestone 47's: it shipped as `run --mem N budgeter` and `file:PATH`.) One kernel change: `Untyped::SPLIT` grants the
child `GRANT` so an untyped is delegable (DECISIONS §16 amendment), which the headline feature
required and no other object type lacked. Notes: grant-expression.md, program-manifest.md.

**Phase 2 built (both ISAs): per-file grants.** The FS service's unit of authority is a *directory*
(DECISIONS §27), and `run wc file:report.txt` says less than that, so the narrowing is a
**caretaker** in Mark Miller's sense: `user/src/fs_file_caretaker.rs` holds the directory
capability, opens the granted name once, and serves the same contract on its own endpoint with a
namespace of exactly one name. Any other name is `ENOENT` (in this scope there is no such name);
`CREATE` is `ENOTDIR` (a file is not a directory); a write without the direction is `EROFS`. Each
refusal is a fact about what the holder has, not a permission that could have said yes.

It is a separate process for two reasons. The FS server receives on one endpoint, so serving a
second narrower one would need a receive over a *set*, which means badging endpoint capabilities
(seL4's answer) and is a design fork, recorded rather than taken. And it makes the claim checkable:
the confined program holds an endpoint to the caretaker and nothing that names the FS server, so "it
cannot reach a second file" is a property of its cspace rather than of a branch it is trusted to
take.

**Proven by an attacker, twice, and the second run is what makes the first mean anything.** It
reports a bitmap of what got through. Read-only grant: every bit clear, against a neighbouring file
that exists and that the caretaker could open. Read/write grant, same shape: the two write bits set
and everything else clear. A caretaker that refused every request passes the first and fails the
second. Phase 2 also landed the contract's `CREATE` and `TRUNCATE` (so `File::create` and
`std::fs::write` work rather than returning `Unsupported`), a name check that was previously true
only by the absence of a path walker, and a measured stack for the FS server after a 528-byte
overflow presented as a mystery 900-second test.

**Why the status is PARTIAL and not BUILT, stated plainly.** The mechanism is complete and gated on
both ISAs, but this milestone's headline is about *the one interface a human touches*, and at that
interface `wc report.txt` is still a refusal. The interactive shell holds no directory to
narrow, because the boot that starts it wires no FS service; the refusal it prints ("you hold no such
capability: this shell was granted no directory to narrow") is **true** rather than a placeholder,
and `caps` says the same. `grant_plan` carries the whole vocabulary (`FileSpec` in the manifest, a
`FileGrant` in the endowment, refusals both ways, `caps` printing the file endowment), and the
decision is a function of what the shell *holds* rather than of the calendar, which phase 1's
hardcoded "arrives with milestone 32" was not.

**Phase 3, then, is exactly one thing:** wire an FS service into the interactive boot (the kernel's
shell boot path, a RedoxFS disk on the interactive runner, and init building the caretaker per
grant), and flip `holdings()` in `user/src/swish.rs`. It was not done here because **nothing in the
test suite boots the interactive shell**, so it would ship unexercised, and a demonstrator's ungated
feature is worse than a recorded gap. Whoever takes it should consider gating that boot first.

**Deliverable.** Invert Unix's authority model at the command line. A Unix child inherits your
entire authority; a cricker-os command line is a **grant expression**: every argument that
designates a resource passes a narrowed capability, and nothing else flows. `run wc report.txt`
grants exactly one readable file capability, because typing the name IS the grant (Miller's
principle: designation is authorization); `run wc` alone spawns a process that can read
nothing, and the failure is "you hold no such capability", legible, not EPERM. Untyped budgets
become first-class grants (`run --mem 16 prog`), the most cricker-os-native piece of the
inversion, with no Unix analog. From SHILL, adapted: a small **manifest** per program declaring
its expected endowment (one readable file, one endpoint, N pages), checked at spawn, so a
mismatch is a refusal at the prompt rather than a mystery hang; this is milestone 23's
component contract in embryo. Introspection is a feature: a `caps` command prints a process's
complete endowment, making §14's "reading one literal tells you a process's whole authority"
interactively true.

**Scoping constraint, honest.** File capabilities need something to point at; phase one grants
what exists (program spawns, endpoints, frames, untyped, device caps), and per-file grants
arrive with milestone 32's FS server, whose handles must be capability-shaped from birth partly
BECAUSE this milestone will point at them.

**Prior art and reuse.** Designs only; nothing portable. SHILL (OSDI 2014: capability
contracts for scripts, on Capsicum) is the academic anchor; Mark Miller's object-capability
line (E, CapDesk, Polaris) supplies the organizing principle; Plash is the Linux attempt worth
reading as the mistake catalog. Feeds 23 and 22 (shrinking ambient authority, met at the human
layer); sits behind 28's terminal contract. **Effort: 2 lanes built** (the grant expression, then
CREATE/TRUNCATE and per-file grants), **1 more estimated** for phase 3, which is one item: gating the
interactive boot so an FS service can be wired into it.
