# 31. A capability shell: designation is authorization

**Status: BUILT.** Closed 2026-08-17 by the lane that built phase 3's last item, init building a
`fs_subtree_caretaker` per grant. The gate is `script/shell-check` on both ISAs: it types `caps rm
rmtree/rm-solo`, `rm -v rmtree/rm-solo` and `ls rmtree | wc` at the real prompt, so the authority is
previewed, the name the command line designated is removed, and the two names beside it inside the
same capability are still there. `rm gate.txt` beside them is the one grant shape that is still a
refusal, and it is a refusal about what a name *is* rather than about a missing feature; see "The two
shapes a grant cannot take" below, which names the design fork it waits on.

(The Gate paragraph that stood here said `NONE`, and then named the one remaining item; a BUILT
milestone gates nothing, so it is gone rather than stale.)

**In brief.** The command line becomes a **grant expression**: naming a resource in a command IS the capability grant (`wc report.txt` passes one readable file cap; `wc` alone can read nothing, and the refusal is "no such capability", not EPERM); untyped budgets as first-class grants; a SHILL-style manifest per program checked at spawn; a `caps` command printing a process's whole endowment. **Phase 1 built, both ISAs**: `grant_plan` (host-tested parse + manifest + spawn protocol), the shell over the existing surface, `--mem N` made real by the `budgeter` program, manifest refusals, `caps`/`caps <command>` introspection; one kernel fix, `Untyped::SPLIT` now grants the child `GRANT` (DECISIONS §16 amendment). **Phase 2 built, both ISAs**: the FS contract's `CREATE`/`TRUNCATE` (so `std::fs::write` works), and per-file grants as a **caretaker process** (`fs_file_caretaker`) that narrows a directory capability to one file in one direction, proven by a read-only and a writable attacker. One scope note, **retired 2026-08-16**: the interactive shell used to refuse a named file because its boot wired no FS service. Milestone 50 wired it, and `wc gate.txt` at the real prompt is gated on both ISAs; what remains is init building a caretaker per grant. **The grammar shown here is milestone 47's**, which deleted the `run` verb and the `file:` designator this milestone shipped with; the mechanism did not change, only the spelling. Notes: grant-expression.md, program-manifest.md, fs-server.md

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

**Why the status was PARTIAL, and the paragraph that was true until 2026-08-16.** It read: the
mechanism is complete and gated on both ISAs, but this milestone's headline is about *the one interface
a human touches*, and at that interface `wc report.txt` is still a refusal, because the boot that
starts the interactive shell wires no FS service and it holds no directory to narrow.

**That is no longer true, and the headline is now gated at the real prompt on both ISAs.**
`xtask/src/main.rs:5368` runs `wc gate.txt` at the interactive shell and expects `2 4 24`, with
`wc gate.txt | wc` beside it, `caps wc gate.txt` printing the endowment, and **bare `wc` refused as the
negative control**, which is the claim stated as a pair rather than asserted. `holdings()` is flipped
(`user/src/swish.rs:128`, `dir: nav.dir.is_some()`), the kernel's shell boot path grants an FS service
on both ISAs (`kernel/src/user.rs:1351` for riscv64, `user/src/hello.rs:390` for aarch64), and the
interactive runner carries a RedoxFS disk (`xtask/src/main.rs:5646`). The harness that was said not to
exist is `script/shell-check`, which is the gate for `user/src/system_initializer.rs` and runs both
legs.

**Why this block did not say so for twelve days**, which is worth recording because it is the §76
defect class arriving sideways rather than by neglect: the work landed under **milestone 50**, whose
commit `43a2967e` says it in its own message, "Milestone 31 phase 3 is 'wire an FS service into the
interactive boot and flip holdings()', and milestone 50 did both. Nothing read the result." A
milestone's status is maintained by its own lane, and nothing maintains it when another lane finishes
its work. Found 2026-08-17 by the status-accuracy sweep.

**Phase 3 built (both ISAs), 2026-08-17: init builds a `fs_subtree_caretaker` per grant.** Init used
to delete the file service during the boot, with a comment in `crates/system_initializer` saying what
would have to change; the shell's copy of that endpoint carries no `GRANT`, so the shell held nothing
it could hand a caretaker and `rm` was a refusal at the prompt for six weeks. Init now keeps the
service for the life of the boot (two of its sixteen cspace slots, taking a directory-granted spawn's
peak to fifteen) and builds a caretaker per grant out of **the client's own region**, which is
DECISIONS §92 read through §40's mechanism: a caretaker's serve loop never returns, so one built in a
region of its own never comes home and §16's LIFO rule then pins the region above it too.

Three things fell out that were latent defects rather than new work, and each is worth naming because
each was invisible until something real ran. `rm` declared the sink contract and never sent its
end-of-stream, so `rm -rv logs | wc` had never been expressible. `fs_subtree_caretaker` panicked on a
refused descent, which cost a kernel test a watchdog and would have cost the prompt the machine, since
init is the waiter and has no second thread. And `job_undertaker` trapped on a refused reclaim, which
is exactly what the *first* reclaim of a two-process region is by construction: the endpoint sweep
wakes the caretaker so it can be collected, and a thread that can be scheduled is refused with §16's
kill armed. It now yields and retries, bounded, and still traps at the end so a real leak stays loud.

**The two shapes a grant cannot take**, both refused at the prompt with nothing spawned, and neither
is a permission:

- **A grant on the root of the shell's namespace.** A caretaker's whole attenuation is one `OPENDIR`
  *into* the granted directory, and the root has no name to descend into; `fs_proto` has no verb
  meaning "the directory I already hold, with fewer rights". So `rm gate.txt` at the top prompt is a
  refusal and `rm rmtree/rm-solo` works, and the difference is one level of path. **This is a design
  fork and belongs to calef**, because both answers are permanent: a narrowing verb on the contract
  (small in the server, `Rights::attenuate` with no name resolution, and an addition to something two
  programs agree on) or an interactive boot whose shell is rooted one component below the image root
  (nothing on the wire, and it changes what every other command at that prompt means).
- **A directory more than one level down, or a set of more than one name.** The first is a *chain* of
  caretakers, which §92 chose supervision partly to make free; the second is
  `fs_nameset_caretaker`, which takes its set in a frame rather than in argument words. Init builds
  one subtree caretaker per grant today, so `caps rm globmany/m-*.txt` still previews an authority
  that cannot yet be delivered.

**Deliverable.** Invert Unix's authority model at the command line. A Unix child inherits your
entire authority; a nife command line is a **grant expression**: every argument that
designates a resource passes a narrowed capability, and nothing else flows. `run wc report.txt`
grants exactly one readable file capability, because typing the name IS the grant (Miller's
principle: designation is authorization); `run wc` alone spawns a process that can read
nothing, and the failure is "you hold no such capability", legible, not EPERM. Untyped budgets
become first-class grants (`run --mem 16 prog`), the most nife-native piece of the
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
layer); sits behind 28's terminal contract. **Effort: 4 lanes built** (the grant expression; then CREATE/TRUNCATE and per-file grants; then
phase 3's larger half landing free under milestone 50; then init building the caretaker per grant,
2026-08-17). Gating the interactive boot, which this line long named as the remaining cost, turned
out to be `script/shell-check` and already built.
