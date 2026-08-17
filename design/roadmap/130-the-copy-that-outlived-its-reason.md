# 130. The copy that outlived its reason: one trap instruction, forty-eight sites

**Status: IN-PROGRESS.** Raised 2026-08-17 from a code-smell survey calef asked for. The number is
**provisional**, minted by the lane against a tree whose highest milestone was 129; expect the
integrator to renumber.

**Gate: NONE.** Nothing blocks it. Every finding is in tooling or in userspace boilerplate, the
syscall surface does not move, no dependency is added, and no wire format changes.

## What this is

Four findings, ranked by what they cost. The survey that produced them is worth stating in full,
because the honest headline is that **the tree is clean**: 155,000 lines carry eleven TODO-shaped
markers, each with a recorded reason, and thirty-six `#[allow]`s against one workspace lint table.
A scanner pointed at this repository mostly finds deliberate decisions. These four are the
exceptions, and only the first is interesting.

### First: the panic handler, and the reason that went stale

Forty-eight sites across `user/`, `crates/` and `fs_server/` inline the same two `asm!` blocks:
`brk #0` on aarch64, `ebreak` on riscv64. Fifty-eight `#[panic_handler]`s in userspace have drifted
into **seven variants** of the same intent.

The reason it is like this is written down, which is what makes it worth fixing rather than worth
arguing about. `crates/user_rt/src/lib.rs:14` records a deliberate decision not to put the handler
in the runtime crate: a panic handler is per-final-binary, putting one in a library forces it on
every program that links the crate and collides with any program wanting its own, and "each binary
keeps its own **one-line** handler; it is trivial."

**The first clause is still right and the last one stopped being true.** The handler is fifteen
lines with two `unsafe` blocks and two `// SAFETY:` comments, so the tree now asserts a
load-bearing safety invariant eighty-eight times by copy-paste, against a `DECISIONS` §61 note
saying a SAFETY comment is an assertion and not a formality. And the same file's header claims
`user_rt` is "the one place in userspace that names" the two ABIs, which forty-eight files
falsify.

**The drift is real, and one instance is semantically different.**
`user/src/terminal_sink_caretaker.rs:101` calls `exit()` and never traps. That is a different
outcome, not a different spelling: `sched::exit` reports `EVENT_EXIT` where `sched::fault` reports
`EVENT_FAULT` (`kernel/src/sched.rs:1185-1196`), so a panic there would tell a supervisor the
program finished cleanly. **It is latent, not a live bug**, and the block says so rather than
inflating it: the adapter is built with `fault: None`
(`crates/system_initializer/src/lib.rs:605-618`), so nothing observes the difference today. It
becomes real the day someone endows that spawn site with a supervision endpoint, which is a
one-line change.

**The fix already exists in the tree and never propagated.** `supervision_proto::fail()` and
`swap_proto::fail()` are byte-identical copies of the asm, lifted into shared crates, and thirteen
programs already delegate their handler to one of them. So the work is not inventing an
abstraction whose requirements are unknown, which is the thing `user_rt`'s own header is careful
about; it is finishing a lift that stopped at thirteen of fifty-eight and landed in two protocol
crates rather than in the runtime crate that documents itself as owning the two-ABI surface.

The shape: a `trap()` in `user_rt`, a `user_rt::panic_handler!()` macro that expands to the
`#[panic_handler]` in each binary, and the two `fail()`s delegating instead of duplicating. The
macro is what preserves the per-final-binary property the original decision was right about, so
this overturns the stale half of that note and keeps the sound half. **Both names are provisional**
(CLAUDE.md: names are calef's).

This is CLAUDE.md's ladder, rung one against rung zero. The current arrangement holds only because
forty-eight authors each remembered.

### Second: `mkinitrd` does one job three ways

`xtask/src/main.rs:3249` builds the aarch64 archive with nineteen hand-rolled `let` bindings of
seven identical lines each, then a loop over a thirty-three-name array doing exactly the same
thing, then a hand-written `files` vector re-listing the first nineteen by the same string
literals. Its riscv sibling `initrd_riscv` already does it correctly: one `(archive_name,
bin_name)` table, one loop.

Folding the nineteen into the existing array deletes about a hundred and fifty lines and drops the
cost of adding a user program from four edits to two.

**What this is not:** the two archives genuinely differ (aarch64 omits `system_initializer`, `blk`,
`driver` and `hello`), and that is deliberate and already recorded at `xtask/src/main.rs:743`. The
survey checked for drift between them and found none. The smell is the three mechanisms, not the
contents.

### Third: two long functions, and the length turns out to be load-bearing

**Tried, measured, and deliberately not done.** This is the finding, rather than a thing left
undone, and it is worth more written down than the refactor would have been.

`kernel_main` is 908 lines (`kernel/src/main.rs:93`), of which 281 are comments.
`syscall::invoke` is 581 (`kernel/src/syscall.rs:97`). The obvious seams in `kernel_main` are
already marked by `cfg` blocks: a 515-line `#[cfg(target_arch = "riscv64")]` boot report, and a
285-line `#[cfg(not(any(test, feature = "bench")))]` banner and init handoff. Both capture nothing
from the enclosing scope but `dtb`, so extracting them is mechanically trivial, and the lane did
it: the two bodies came out **byte-identical**, and `kernel_main` dropped to 112 lines.

**It broke the build, in a way that is a real property rather than a lint being fussy.** With the
blocks inline, all four features (`bench`, `shell`, `smb_serve`, `initboot`) compile with **zero
warnings on both architectures**. Extracted, `bench` and `shell` warn on riscv64 and `smb_serve`
warns on both. The cause is that these features park early: each is a `cfg`-gated block ending in
`arch::halt()` or `bench::run()`, and everything after it is unreachable in that configuration.
One divergent function absorbs that; two functions do not, and `-D warnings` is a gate.

The code already said so, and nobody had connected it to the length. `kernel/src/main.rs:768`
explains that `smb_serve` parks in place "instead of compiling the tour and the init handoff out,
so this feature manufactures no dead code for the lint to chase". That property holds **because**
`kernel_main` is one function with one divergent tail. Both candidate splits were tried, and both
signatures for the extracted function (`-> !` and `-> ()`); the unit return was worse. Extracting
either block breaks a different feature, so there is no version of this split that is free.

So `kernel_main` is long because it is the single divergent boot path, and that is a design, not a
defect. `syscall::invoke` was not touched: its length is one arm per object method, which is the
shape of the thing it dispatches.

**What this costs, honestly:** a reader still meets a 908-line function. The mitigation is that the
reason is now recorded here and the experiment does not need repeating. If someone wants this
split later, the thing to solve first is the early-park pattern, not the function.

### Fourth: `xtask` has no error type

6,785 lines in a single `main.rs` with no module structure, forty-eight functions returning bare
`bool`, and a hundred and thirteen `return false` sites each preceded by an `eprintln!`. Every
failure is printed and flattened where it happens, so nothing composes and no caller can branch on
why a step failed.

Largest item, and it loses the ranking function's tiebreak: it is build tooling, not the
demonstrator, so it goes last and may well be split into its own milestone.

## Scope note

This milestone is boilerplate and tooling. It moves no syscall, adds no dependency, changes no wire
format, and takes no `DECISIONS` section: the reasoning lives here and in `notes/`, per the rule
about lanes and global resources. If the `user_rt` change turns out to want a `DECISIONS` entry
(it overturns a recorded decision in a doc header, which is arguably enough), that is the
integrator's to mint at merge.

## BUGS

The branch this was built on, `claude/code-smells-review-3uipoy`, has a prefix `script/lint` does
not recognise (§77's list). The name was mandated by the harness that opened the lane and could not
be changed from inside it. **CI is unaffected**: `pull_request` runs build the merge commit and so
run detached, which that check skips by design. A local `script/lint` on the branch fails on the
prefix and on nothing else. Either §77's list grows a prefix for agent lanes, which is milestone
128's territory, or the harness learns to use `feature/`; both are calef's call and neither is
this milestone's.
