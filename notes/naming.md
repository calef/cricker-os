# Naming things in cricker-os

What the tree's names mean, which conventions are rules, and which of those a machine checks.

Written at milestone 46, alongside the rename that made four of the names honest. The headline rule
and its argument are [DECISIONS §39](../DECISIONS.md); this note is the working reference and covers
the parts §39 does not: crates, scripts, where a document goes, and the two numbering schemes that
look alike and are not.

## The rule everything else is a corollary of

**A name is a claim, and it is made before a reader sees a line of code.** That is the whole
argument. A wrong name is the same defect as a stale comment, except that a comment can be skipped
and a name cannot: every reader of every call site reads it.

Two ways a name can be a false claim, and the tree had both:

- **It claims a model we rejected.** `netd`, `compd`, `gpud`, `termd`. The `-d` suffix says "Unix
  daemon", and a daemon is defined by what it detaches from: no controlling terminal, inherited
  ambient authority, a pid file, started by a privileged init. This OS has none of those. `netd` held
  five explicit capabilities, could not name its own callers, was supervised, and could be reaped by
  something that lacked the authority to build it. About as far from a daemon as a long-running
  process gets.
- **It claims a reader who does not exist.** `linedisc` was the correct Unix term of art. Chris did
  not recognise it, and he built the system. That is evidence about the name, not about him. It
  became `lineedit` and then `line_editor`, which someone who has never opened a tty manual
  understands immediately.

So: **name a component for what it is, and prefer a word that parses without prior Unix exposure.**
`blk`, `spawner`, `console`, `input`, `painter`, `window`, `kbd` were always right, and were always
the majority. The four `-d` names were the outliers.

The shell is the one exception, and it is deliberate: it is called **`swish`**, not `shell`, because
shell names are identities rather than descriptions (`bash`, `zsh`, `fish`, `rc`). The argument is in
milestone 63's roadmap block. `capsh` was the obvious candidate and is unavailable: Linux's libcap
ships `capsh(1)`, a capability shell wrapper, so a reader arriving from Linux would assume ours is
that tool.

## Components

A **component** is the shippable unit: one binary in `user/src/`, one `[[bin]]` in `user/Cargo.toml`,
one entry in the initrd archive. A **service** is what a component offers. A **contract** is the wire
protocol it offers it over. "Server" is a fine role word inside a component (`fs_server` serves the FS
service). "Daemon" appears nowhere.

- Lowercase, `snake_case`, no suffix. `net_stack`, `compositor`, `display`, `line_editor`,
  `fs_subtree_caretaker`. One word where one word will do, an underscore where the name is a
  qualifier applied to a thing; the 2026-08-01 rule below retired the older "no separators" wording.
- **Never `-d`.** Not `netd`, not a future `logd` or `authd`. Checked.
- **`c_` means "written in C", and it spans two unrelated milestones.** `c_shim`, `c_seam` and
  `c_confiner` are milestone 36's foreign-language seam (DECISIONS §31); `c_swappable` is milestone
  23's replacement demo, the C half of the `rust_swappable` / `c_swappable` pair. The prefix means
  the same thing in both places and the milestones have nothing to do with each other, so do not
  read the four of them as one family.
- Abbreviate only where the abbreviation is the ordinary name of the thing: `blk`, `kbd`, `pci`. If
  you have to expand it in the doc comment to make the file readable, it was not the ordinary name.
- The binary name, the source file name and the archive entry name are the same string. `xtask`'s
  `mkinitrd` pairs them positionally in a flat array, so a mismatch is a runtime "program not found"
  rather than a compile error, which is exactly the kind of thing to keep boring.
- The one deliberate exception: `builder` is packed as `init`, because `init` is the entry the kernel
  loads by name. The name in the archive is a role; the name in `user/src/` is the program.

Fixtures and benchmarks (`heeder`, `spinner`, `flaky`, `allocator_exerciser`, `worker`, `coremark`,
`os_primitives_benchmarker`) live in `user/` next to the real components and are not components.
Milestone 39's directory-layout work is where that gets separated; the naming rule is the same either
way.

**Two suffixes carry a category, and the distinction between them is real** (milestone 63).
An **`_exerciser`** puts a capability of the system under load and sees whether it holds, with no
contract being probed from outside: `allocator_exerciser` interleaves allocation and free and then
demands a large allocation fit in pages already committed, and `std_exerciser` is an ordinary Rust
program on the native ABI whose three behaviours are chosen by the authority it was granted. A
**`_test_client`** exercises a service contract from outside, with a server on the other end:
`fs_test_client`, `socket_test_client`, `credentialer_test_client`. The `test` is not noise. The
unqualified names (`fs_client`, `socket_client`, `credentialer_client`) belong to the real clients
milestones 54 and 55 will need, and giving them to test programs squats them.

## Crates

### The one rule, and who applies it (2026-08-01)

**`snake_case`, everywhere, with no second tier.** Crates already did this (`fs_proto`, `user_rt`);
programs did not, and **0 of 57** carried an underscore, so multiword names were squished. The three
worst were `fsclient`, `sysinit` and `credcli`; they are `fs_test_client`, `system_initializer` and
`credentialer_test_client` now (milestone 63).

An earlier draft had two tiers: short names for programs a user types, underscores for programs only
the system spawns. It was rejected, and the reason generalises. **The category is not a stable
property of a program.** `wc` was internal plumbing and became a prompt-typed pipeline stage inside a
day, and a convention keyed to something that changes produces renames. It is also not how Unix got
its names: the terseness of `ls` is emergent pressure on words people type constantly, not a rule
anyone wrote down, and codifying an emergent property turns it into a classification chore every
contributor has to get right.

So one rule, no branch. A short name for a typed command is a *choice its author makes*, not a
convention to apply; nobody needs a rule to know `wc` beats `word_count`.

**Chris names the crates, the programs, and the shared modules.** Same shape as `DECISIONS.md`
section numbers: global to the tree, so decided by the person who can see the whole tree. A lane
ships a **provisional** name, says so in its report, and expects it to change. Nobody renames on
their own initiative either, because a rename is a naming decision with extra steps. The reason is
that names are what make this OS legible to humans and to LLMs, and in a capability system a name is
often the only thing that says what a program can *do*.

**Standard terms are already right and must not be touched.** `elf`, `pci`, `dtb`, `gpt`, `ipc`,
`paging`, `glob`, `asid`, `socket_proto` are names a reader knows from outside this project, so they
cost nothing to learn. This tenet is a naming authority, not a renaming mandate, and renaming `elf`
would destroy the recognition the whole thing exists to buy.

**One constraint:** `crickerfs` caps archive names at `NAME_LEN = 32` bytes, so a program's name is
bounded. Crates are not in the archive and are unbounded.

It was 24 until 2026-08-01, when it had started deciding names rather than bounding them: two settled
names were within four bytes of it and `os_primitives_benchmarker` exceeded it. Raising it costs
directory entries per block, and nothing else now that `Fs` no longer holds an entry array. See
[crickerfs.md](crickerfs.md) for the numbers. The rule that survives the raise: **do not let the
limit pick a name, and do not spend a format change on bytes nothing needs.** 32 clears the longest
settled name by seven bytes, which is a budget rather than the three bytes that were left before.

## Crates

`crates/` holds four audiences under one directory, and **naming does not distinguish them**, which
is a known gap rather than a decision.

- **Kernel logic**, host-tested and Kani-reachable: `capability`, `paging`, `frames`, `regions`,
  `slots`, `asid`, `intrusive`, `ipc`, `dma_validator`, `measured_boot`, `user_heap`.
- **Wire contracts**, spelled `*_proto` and checked for it by `script/lint`: `fs_proto`,
  `socket_proto`, `sink_proto`, `cred_proto`, `clock_proto`, `entropy_proto`, `gfx_proto`,
  `ntp_proto`, `supervision_proto`, `swap_proto`. Plus `abi`, which is the syscall boundary and
  predates the suffix.
- **Format and hardware parsers**: `elf`, `dtb`, `pci`, `gpt`, `crickerfs`.
- **Userspace libraries**: `user_rt`, `grant_plan`, `virtio`, `video_terminal`, `line_editor`,
  `bitfont`, `glob`, `calendar`, `cred`, `compositor`, `coremark`, `c_seam`.

**`compositor` and `line_editor` are the two that look like contracts and are not**, and an earlier
version of this section listed them as such. Both are *logic* crates that happen to contain a
protocol module: `compositor` is the scene, the clipping and damage-rectangle arithmetic, and the
composition itself; `line_editor` is a sans-IO editor with a `line_editor::proto` inside it. Renaming
either to `*_proto` would promise a wire definition and deliver an algorithm, which is exactly the
kind of claim §39 is about. The `*_proto` check is right to leave them alone.

What the names actually do, over the 39 directories under `crates/`:

- **One word where one word will do**, which is 21 of the 39: `abi`, `capability`, `compositor`,
  `elf`, `frames`, `ipc`, `paging`, `regions`, `slots`, `virtio`.
- **Underscore when the two halves are separate concepts** and the name reads as a qualifier applied
  to a thing, which is the other 18: `fs_proto` is the proto *for* fs, `gfx_proto` the proto *for*
  gfx, `dma_validator` the validation *of* DMA, `user_rt` the runtime *for* userspace, `user_heap`
  the heap *for* userspace, `measured_boot` the measurement *of* boot.

**Milestone 63 deleted the third bullet, which used to read "run together when the result is one
word".** It was a real observation (`capsh`, `lineedit`, `uheap`, `crickerfs`, `bitfont`), and it was
the rule that produced every abbreviation a reader had to decode. `crickerfs` survives it, because
`procfs` is the shape of a filesystem name outside this project and nobody writes `proc_fs`; the
other four are `grant_plan`, `line_editor`, `user_heap` and `bitfont` now. The boundary that
remains, between one word and two, is judgement, and the guard rail is that a **standard term keeps
its standard spelling** (see above).

The one place it became a real inconsistency is worth fixing and is checked: **the wire contract was
spelled four ways** (`fs_proto`, `gfx_proto`, `netproto`, `line_editor::proto`) for one concept.
`*_proto` wins for crates, because it is what the actual crates already were, and `socket_proto` has
since graduated from a module inside `net_stack` into a crate under that name.

**A crate that is a component's engine takes the component's name** (`line_editor` the sans-IO
editing crate, `line_editor` the binary that wires it to endpoints; `compositor` and `coremark` are
the same pair). They are the same thing at two layers, and giving the engine a second name is how
`termd`/`linedisc` happened in the first place. Where a note needs to tell them apart it says "the
`line_editor` crate" and "the `line_editor` binary".

**One pair deliberately does not share a name**, and the reason is worth keeping: the crate is
`video_terminal` and the program that wires it is `display_terminal`. The crate is named for the
**protocol** it implements (the VT standard, bytes in and a character grid out) and the program for
its **role** (the terminal on the display, next to `display`, the virtio-gpu driver it is a client
of). Both facts are true and neither name says the other.

## Scripts

Two directories, on purpose, and the split is by audience.

- **`script/`** is the front door: [Scripts to Rule Them All](https://github.com/github/scripts-to-rule-them-all)
  names, one short file each, **no extension**, lowercase, hyphenated if more than one word
  (`qemu-check`, `ci-qemu`, `toolchain-bump`, `vendor-verify`, `supply-chain`). These are what a
  person types. The canonical set (`setup`, `test`, `server`, `console`, ...) keeps its standard
  names even where a different word would be more descriptive: the entire value is that the command
  is the same in every repo that follows the pattern.
- **`scripts/`** is the helper drawer: `.sh` extension, called by other scripts and by `xtask`, not
  by people (`qemu-bounded.sh`, `qemu-runner.sh`, `qemu-runner-riscv.sh`).

Every `script/` entry needs a row in [scripts.md](scripts.md); `script/lint` fails without one, and
fails in the other direction too if `README.md` names a script that does not exist.

## Directories (milestone 63)

The tenet covered crates, programs, modules, shell entry points and markdown, and said nothing about
**directories**, so the tree carried three spellings. Two rules, and neither is a new tier:

- **A directory that holds a Rust package is named exactly as the package**, so `snake_case`. The
  directory and the package are one thing with one name.
- **Any other directory is lowercase, and hyphenated if it needs two words**, the same convention as
  markdown filenames and `script/` entry points, because a directory is a path element and paths are
  hyphenated in the world outside this repository.

Three directories violated the first rule and all three moved in milestone 63: `fs-server/` (package
`fs-server`) is `fs_server/`, `tools/redoxfs-host/` is `tools/redoxfs_host/`, and `user-std/`, whose
package was called `hellostd` and matched neither, is `std_exerciser/` twice over.

A hyphenated package name is not wrong in the wider ecosystem. `wasm-bindgen` and
`tracing-subscriber` are ordinary and Cargo normalises a hyphen to an underscore for `use`, so
nothing was broken. The case was internal consistency, 36 crates against 3, and it should be read
that way rather than as a correctness fix.

**Two things that look alike and are not:** `target/` is gitignored build output and `targets/` is
the tracked custom target JSON (`aarch64-unknown-cricker.json`). Nothing enforces the distinction.

## Where a document goes

Three places, and the distinction is what the document is *for*, not what it is about.

| | holds | shape |
|---|---|---|
| `design/` | the option space, before a decision | "here are four answers and three are bad" |
| `DECISIONS.md` | the decision, and the argument that settled it | numbered `§N`, append-only |
| `notes/` | what exists, and what building it taught us | a running glossary, indexed in `notes/README.md` |

`design/roadmap.md` is the exception that proves the split: it lives in `design/` because a milestone
block is an argument for doing something, not a record of having done it, even after the milestone
ships and the block gains a "Built" line.

A note is not optional. Every concept and every finding gets one, indexed in
[notes/README.md](README.md), because for a demonstration OS the documentation is part of the
deliverable rather than a courtesy to the author.

## `§N` is not milestone N, and they collide

**DECISIONS section numbers and roadmap milestone numbers are separate schemes over the same small
integers.** There are 41 sections and 39 milestone blocks, so almost every number means two things:

| N | DECISIONS `§N` | roadmap milestone N |
|---|---|---|
| 24 | the two-tier Ctrl-C | a Virtualization.framework board |
| 28 | SMP placement | the line discipline |
| 31 | the foreign-language C seam | the capability shell |
| 39 | this naming rule | components, services, and the directory layout |

This has already produced a wrong citation in the tree, not a hypothetical one: milestone 50's block
cited "§31's FileSpec", which points at the C seam and has nothing to do with file grants. The thing
it meant was milestone 31 phase 2, granting against the §27 filesystem contract. Fixed in c0643bc.

So:

- **Write `§N` only for DECISIONS.** Never for a milestone.
- **Write "milestone N" in full.** Never bare `N`, and never `§N`.
- Prefer number **and** name on first mention in a block ("milestone 31, the capability shell"),
  which is what makes the wrong one visible.

`script/decisions --check` verifies that every cited `§N` resolves to *some* section. It cannot
verify that it resolves to the *right* one: a well-formed wrong citation is indistinguishable from a
correct one from the outside. Worth knowing before trusting that gate for more than it claims.

## Branches

Eight prefixes were in use when this was written, including both `feature/` and `feat/` for the same
idea. One spelling, and `feature/` is the older one:

`milestone/` (a roadmap milestone), `fix/` (a bug with a name), `bench/` (measurement work),
`audit/` (reading rather than writing), `integration/` (joining lanes), `finalize/` (landing them).
Plus `main`, and the tooling's own `worktree-agent-*`, which no person types.

Checked, for the current branch only, and skipped on a detached HEAD.

## What is checked, and what cannot be

`script/lint`'s `naming conventions` block enforces four things, all cheap greps because lint runs
constantly:

1. **No name ending in `-d`**, over `user/src/*.rs`, `user/Cargo.toml`'s `[[bin]]` names, and
   `crates/*`. Four characters or more, so `kbd` is an abbreviation rather than a daemon. Words that
   genuinely end in `d` go in `naming_allow` **with a reason**, the same shape as a per-item
   `#[allow]`; `asid` (Address Space IDentifier) is the one there today.
2. **The word "daemon" appears nowhere**, outside `DECISIONS.md` and `design/`, which are where the
   argument about the word lives and therefore have to be able to name it.
3. **Contract crates spelled `*_proto`.**
4. **The current branch carries a recognised prefix.**
5. **No `#[path]` module is shared by two or more binaries** (CLAUDE.md rule 7). This is the newest
   and the one with teeth: it counts consumers per include target and fires at two. A module with a
   single consumer is an ordinary submodule and is fine, because the rule is about *agreement between
   binaries*, not file layout. The allow-list is **empty**, which is the intended steady state.
   `virtio` was its one entry for about an hour: it could not be a crate while it reached back into
   whichever binary included it for `check`, and the resolution was to **delete `check`** rather than
   pass it in. Rust already has the per-binary "how this program dies" hook, `#[panic_handler]`, and
   both binaries already had one executing the same instruction by two different routes. An entry
   here needs a reason of that calibre.

Everything else here is prose because it needs judgement and no checker can supply it. In particular
**a checker cannot catch the jargon half of §39**: `linedisc` would have passed all four rules above.
It ends in `c`, contains no daemon, is not a proto crate, and had a perfectly good branch. What
caught it was a person reading the name and not knowing what it meant, and that remains the test.

Two limits worth stating rather than discovering: the checks read the filesystem for names and use
`git grep` for the word, so an **untracked** file with "daemon" in it is invisible until it is added
(the same blind spot the conflict-marker check has), and check 1 sees the *names* of things rather
than the things, so a component whose name is fine and whose behaviour is a daemon is not its
problem.

## BUGS

What milestone 63 did **not** rename, each on purpose, so the next reader does not "fix" one of them
by mistake.

- **The boot mode is still called `shell`, and the program is `swish`.** `cargo xtask shell` and the
  kernel's `--features shell` name a *configuration* (boot straight to a prompt, milestone tour
  compiled out), not the binary. Renaming them would have been a naming decision nobody made. The
  cost is that a reader who greps `shell` in `xtask` and in `kernel/Cargo.toml` meets a word that no
  longer names a program.
- **`caps` is still a shell builtin**, and it no longer shares a name with anything. It was the
  larger half of the 285 occurrences of `caps` in the tree before the rename, and it means "print
  this process's endowment". The crate that used to share the spelling is `capability`.
- **`crates/virtio`'s first line still says "A virtio-blk driver".** It also drives net, serves
  blocks through `run_blk_server`, and carries two deliberate attack roles. The crate keeps its name,
  which is right, but the sentence under it is wrong. Out of scope for 63 and not yet filed anywhere
  else.
- **A rename can reach into the vendored tree, and `script/supply-chain` is what says so.** Our one
  divergence comment in `vendor/redoxfs/Cargo.toml` names `redoxfs_host`, so the rename had to touch
  the vendored file **and** `vendor/redoxfs.divergence.patch` together or
  `script/vendor-verify` fails with "differs from upstream+patches". Milestone 63 changed the patch
  first and the vendored file not at all, and the gate caught it. Edit both, in the same commit.
- **The measured-boot manifest is still `target/init-measure-<arch>.txt`.** It is a build artifact
  name, not a crate reference; `kernel/build.rs` reads it and turns it into `TRUST_ROOT`.
- **Note filenames did not move**, and that is the rule rather than an oversight:
  [fs-server.md](fs-server.md), [shell.md](shell.md), [shell-navigation.md](shell-navigation.md) and
  [line-discipline.md](line-discipline.md) are markdown, so they stay lowercase-hyphenated even
  though the things they describe are now `fs_server`, `swish` and `line_editor`.
