# 64. Enough `std` to run somebody else's crate

**Status: PARTIAL** since 2026-08-04 (PR #113), through four passes (2026-08-17, and three on
2026-08-18). The measurement's deliverable, the prioritised gap list that milestones 99 and 66
consume, is in `notes/crates-io-on-nife.md`: **50 crates.io crates, 43 built, 7 failed**, where 43 is
against this tree as it ships and **39 is against it without `entropy_backend`**, which is the number
this block carried until the third pass.

**Gate: NONE.** The fourth pass had to establish that rather than assume it, because the third
pass's report said the opposite: *"the ranked list is now genuinely exhausted for a lane under these
constraints: every remaining row is a decision."* **That sentence was wrong twice**, and this block
carried a version of it too:

- **`std::process::exit` was a trap instruction**, found by building the gate the third pass said
  was missing. Not a decision, and not on the list, because the list cannot see a function that ends
  the process. Fixed here.
- **`TcpListener` is a binding, not a decision.** Rank 21 was a contract gap when it was written and
  stopped being one at milestone 107; `sys/net/connection/nife.rs`'s own doc comment has said so
  since, in the words *"small and mechanical"*. It is the largest piece of ordinary work left in
  this milestone and nothing about it needs calef.

The block's own sequencing still holds: run the measurement phase first and independently, pick the
probe crates, build them, and let the failures name the work. The `File::open` resolution question is
a design fork inside it, to be raised before code and answered jointly with milestone 47's namespace
half rather than twice.

**The measurement is a script now**, `script/crate-probes` (name provisional), and that is the third
pass's first deliverable. It had been a prose recipe re-derived by hand three times, and two of those
three produced a wrong headline. A probe is a `[[bin]]` whose `main` calls the crate, both halves
learned by recording `diesel` as a pass twice; a probe that fails is rebuilt for the **host** and
reports `BODY` rather than `FAIL` if the host fails too, which is the check fifty hand-written call
sites could not give anyone.

**That split was recorded as 35/15, then 39/11, and is 43/7.** The first correction (2026-08-17) was
arithmetic: the headline summed the four failure-class headings, and four crates are in two classes
at once (`zip` and `ring` in A and C, `gix-config` and `gix` in A and B). The second (2026-08-18) is
that 39/11 describes the tree *before* `entropy_backend`, which had landed the same day the number
was re-derived; `rand`, `uuid`, `gix-object` and `gix-actor` build now, and the second run confirming
they still fail without it is one flag rather than an argument (`script/crate-probes --no-backend`).

**What is closed.** Five ranks turned out to be bindings rather than verbs (`create_dir`, `read_dir`,
`remove_file`, `remove_dir`, `rename` were all dispatched by the FS server since milestones 47 and
48; only the client refused). The second pass added four more, working the list from the top:

- **Rank 1, the `getrandom` backend**, which was 8 of the 11 build failures and is the one gap on the
  list that is not a `std` gap at all. `entropy_backend` is the fix, on `getrandom`'s own
  documented custom-backend hook over `std::random::SystemRng`. **`rand`, `uuid`, `gix-object` and
  `gix-actor` build for nife now**, and the other four no longer fail on `getrandom` at all.
- **Rank 4, `env`**, and it is the sting in a second place: `env::var` was recorded as "no PAL at
  all", which sounded harmless because `getenv` answered `None` honestly. The same fallback's `env()`
  is a `panic!`, so **`std::env::vars()` aborted the process**, and it compiled perfectly.
- **Rank 8, `File::set_len`** and **rank 26, `fs::copy`**. Rank 9 (`symlink_metadata`) needed nothing:
  the row was stale, std routes it to `lstat`, which this PAL binds.

**The third pass asked a different question, and it is the one worth carrying.** Not "what is next on
the ranked list" but **"what do the neighbouring functions in that module do"**, which is where the
second pass's best finding had come from by accident. Run across every module the PAL falls through
rather than binds, it found three more std calls that **abort a nife process**, all of which compiled
perfectly: `std::env::temp_dir()`, `std::env::split_paths()` and `std::process::id()`. Two are now
answered and one is a constant with its reasoning recorded (`sys/paths/nife.rs`, `sys/process/nife.rs`);
`std_exerciser` asserts both the answers and the four neighbouring refusals on both ISAs.

**None of the three could have appeared on the ranked list**, and that is the finding rather than the
functions. The list is built from PAL functions that answer `Unsupported`, and a function that aborts
never answers. `env::temp_dir` did have a row, at rank 16, reading "no PAL at all ... needs a
namespace answer, not a PAL one", which is how a fatal defect got filed as a design question; its
demand was undercounted by four probes too. It also corrects this note's most-quoted sentence:
`tempfile` did not "build, link, and return an error", it **died inside `std::env::temp_dir`** before
reaching its own refusal.

**The fourth pass turned the third pass's method into a check, and the check found a fifth.**
`cargo xtask std-aborts` (name provisional) asks cargo's dep-info which `library/std/src/sys/**`
sources rustc compiled for the nife targets and greps exactly those for bodies that end a process,
against a list carrying a reason per entry. It runs inside `script/test`. That is rung two of
AGENTS.md's ladder where this block's own BUGS section had rung four, and it was worth building
rather than reading a fourth time because **`std::process::exit()` was `crate::intrinsics::abort()`
and no amount of reading would have found it.** `sys/exit.rs` is not a `sys/<module>/mod.rs`
backend; its `cfg_select!` lives inside a function, so "read every module the PAL falls through"
has nothing to read. It hid behind a second thing too: `_start` calls the PAL's `rt::exit` on
`main`'s return directly, and `std::process::exit` is std's **only** caller of `sys::exit::exit`, so
every std test in this tree ended by the path that worked. A clean exit was arriving at its
supervisor as `EVENT_FAULT` with a pc and a faulting address.

**What that leaves open, and it is a wire format rather than a PAL arm.** `sched::exit()` is
`depart(EVENT_EXIT, 0, 0)`: the §26 message has no field for an exit code, so a supervisor can tell
exit from crash and cannot tell `exit(0)` from `exit(1)`. That is the expensive category (something
two programs agree on) and is recorded in notes/std.md rather than guessed at here.

**What remains, and why each one stops here rather than being unfinished.** Rank 2, the
`std::os::unix` fallthrough, wants a **uid** and a **file mtime set** that this system does not have
in the form the crates ask for, and answering would be a Unix fiction over a capability refusal.
Rank 3, `thread::spawn`, is this block's own scheduling question, **now decided (§105): declined for
now**, for want of a customer (see BUGS), and has **no build failures behind it**. Rank 19, `Metadata::modified`, is one field in `FSTAT`'s reply away and
that makes it a wire-format change. **Rank 21, `TcpListener`, is the exception to this whole
paragraph and is the next lane's work**: it was a contract gap when the list was written and stopped
being one at milestone 107, so what is left is a PAL binding (`bind` -> `OP_LISTEN` on a socket id
this PAL allocates, `accept` -> `OP_ACCEPT` into a second id with a frame attached) plus a listen
grant on the stack the program is spawned with. `sys/net/connection/nife.rs` calls it *"small and
mechanical"* in its own doc comment and notes/net.md's "The inbound half" says what it takes. It is
also on the customer path: milestone 55 wants Samba-shaped code, and nothing that serves a share
accepts no connections. Everything else that resolves a path waits on the `File::open`
fork below, and the third pass drew the line at exactly that edge: **fix the ones that abort, leave
the ones that refuse.** `current_dir`, `current_exe`, `chdir` and `home_dir` can each say no in their
own signature and still do; `temp_dir` and `split_paths` could not, so they were answered from §27's
existing decision (`.` is the granted directory, `one_name`'s own words) rather than from a new one.

The sting the measurement found is the one worth carrying forward: **a green build is not evidence.**
`tempfile` compiles, links, and returns "operation not supported" at run time. Raised 2026-08-01,
from a question with a number behind it: does milestone 27 mean ordinary Rust programs run here?


## What 27 actually delivered, and where it stops

`std` on the native ABI is **BUILT**, and the proof program is real: `println!`, `Vec`, `String`,
`Instant`, `SystemTime` and `std::random` all work through the PAL in `patches/std-nife/`.

The bound is in the PAL's own answers:

| module | functions | answering `Unsupported` |
|---|---|---|
| `time` | 8 | 0 |
| `stdio` | 5 | 3 |
| `thread` | 6 | **4** |
| `fs` | 54 | **32** |

`std::fs` has the metadata surface (`size`, `perm`, `modified`, `is_dir`, `read`, `write`, `append`,
`truncate`) and answers `Unsupported` for most of the rest. **That is honest rather than broken**
(§42: declare what you offer), and it is exactly what milestone 27's own text claims: it widens real
workloads to *"most of crates.io **that stays off fs and threads**"*. The qualifier is doing the work
in that sentence, and this milestone is about removing it.

## Why now rather than at 27

The pieces that were missing then exist now. The FS service and its wire contract (§27), the three
caretakers and their verb table (§56), extended attributes (§54), and `fs_test_client`'s worked grant
path all landed after 27 did. `std::fs` could not have been backed by a capability-shaped filesystem
that did not yet exist.

And it is on the critical path in a way the roadmap does not currently say: **milestone 55 wants
Samba-shaped code**, and nothing realistic in that space stays off `fs` and threads.

## How to scope it, which is the whole method

**Do not fill in functions by guessing which matter.** Pick real crates, build them, and let the
failures name the work. The gap that matters is the one a chosen dependency actually hits, and a PAL
completed by inspection would be a large amount of code justified by nobody's use.

Candidate probes, roughly in order of how much they would teach:

- a pure-computation crate with no IO, to establish the floor,
- a serialization crate, which pulls in `alloc` patterns and trait-heavy generics,
- something that opens a file by path, which is where **the capability question bites**: `File::open`
  takes a path and this system has no ambient authority, so either the PAL resolves against a
  granted directory or the call must keep answering honestly,
- something that spawns a thread, which is the other half.

**The `File::open` question is a design fork, not an implementation task**, and it should be raised
before code is written. §50 chose `bind` over stored paths and §48 settled resolution; how a
`std::fs::File::open("config.toml")` finds its directory capability, or refuses to, is the same
question one layer up. It may be that the honest answer is a program namespace (milestone 47's `PATH`
analysis) rather than a PAL trick.

## The relationship with milestone 47, in both directions

**64 needs 47, in tiers rather than all at once.**

- **Tier one, a bare name against one granted directory**, needs nothing from 47's remaining work.
  `File::open("config.toml")` where the program holds a directory capability resolves the way
  `fs_test_client` and the caretakers already resolve names, on machinery that exists: §27's
  contract, §47's rights ladder, §56's verb table.
- **Tier two, anything that traverses**, needs a namespace to resolve *against*, and that is 47's
  unbuilt half. `Path::new("assets").join("x.png")`, an absolute path, or a program wanting two
  directories all land here.

So 64 can start and get a useful distance before it blocks. It will block **sooner than tier one
suggests**, because real crates rarely open a bare name in a single directory; they join paths.

**And 47 may need 64 more than the reverse.** `bind` is a decided mechanism with no forcing use case:
§50 records it as unbuilt, needing "a mount table per process and resolution through it", and nothing
in the shell strictly requires one. A `std` program calling `File::open` with a path is a **concrete
demand for exactly that machinery**. The same is true of `PATH`, where 47 concluded there is no search
because there is no ambient namespace to search, and that a program namespace **is** an endowment.
64 would be its first real customer.

**Sequencing that follows from this.** Run 64's measurement phase first and independently: pick the
probe crates, build them, let the failures name the work. It costs 47 nothing and produces the
evidence for how much namespace 64 actually needs, which is the question 47's remaining scope should
be sized against. **Then answer `File::open`'s resolution once, as a fork spanning both**, rather
than twice. Answered inside 64's PAL it will be a trick; answered as 47's namespace it is the design
both milestones already point at.

## The fourth pass: `TcpListener`, which was a binding and not a decision (2026-08-18)

**Status does not move: still PARTIAL, gate still NONE.** The ranked list is not exhausted (rank 2's
`std::os::unix` fallthrough, rank 3's threads and rank 19's `modified` all still want answers calef
has not been asked for), so a flip to BUILT would be a claim this pass did not earn. What this pass
did earn is the largest piece of *ordinary* work the list had left.

`std::net::TcpListener` was `Unsupported`, and it had stopped being a contract gap at milestone 107:
`LISTEN` and `ACCEPT` have been on the wire, with a listen grant behind them, since that milestone
landed. The PAL is now bound to them, and nothing about it needed a decision. **`bind` is
`OP_LISTEN`, `accept` is `OP_ACCEPT` into a second socket id with a frame attached**, and a listener
carries no frame at all, because a listener carries no bytes (DECISIONS §25).

**The authority is the interesting half, and it is what the gate is built around.** A listening port
is a grant `net_stack` was spawned with, so the same `std_exerciser` binary does two different things
on two boots and neither of them is a fallback:

| the stack it was spawned over | what `TcpListener::bind` answers | what the program prints |
|---|---|---|
| `NO_LISTEN_GRANT` | `PermissionDenied` | `listen refused`, then the outbound transcript |
| `listen_grant(7778, 7778)` | granted | `listen ok`, and it serves two host connections |

Both transcripts are pinned byte for byte on both ISAs, so a change that widened the grant check
would turn `listen refused` into `listen ok` and fail in a diff rather than pass with more authority
than it was given. The granted run also asserts the two refusals inside its own grant: a port
outside the range is `PermissionDenied` and the granted port asked for twice is `AddrInUse`.

**What this unblocks** is milestone 55, and it is not a small step: nothing that serves a share
accepts no connections. A `std` program that can listen is the difference between "a crate compiles"
and "a server runs".

## BUGS

- **"Runs unmodified" is the claim to be careful with.** A crate that compiles is not a crate that
  works, and a crate that works under one grant may fail under another, because on this system what a
  program can do depends on what it holds. The acceptance evidence has to be a crate doing its job
  with a stated endowment, not a green build.
- **The PAL patches std's own source**, so every function added here is more surface for
  `toolchain drift` to break against a future nightly. That is a real recurring cost and the reason
  to add only what a probe demands.
- **Threads opened a scheduling question this project has now answered: declined, for now.**
  `std::thread::spawn` implies a thread the program owns; the kernel has TCBs and a budget model,
  and which of those a `std` thread is stayed open after the second pass, because all four of the
  crates behind rank 3 (`rayon`, `crossbeam-channel`, `tokio`, `ignore`) already compile and link, so
  nothing was blocked on the code and everything was blocked on the answer. **The fork is written up
  in full in notes/thread-spawn-fork.md** (pull request #394, 2026-08-22): `Tcb::CONFIGURE` consumes
  the `Aspace` capability it binds and the `Thread` struct owns it outright, so no two TCBs can share
  one address space today, which is what a `std` thread actually needs. Two real shapes a fix could
  take were costed (a kernel-level shared VSpace, seL4's own answer and the lineage
  `objtype::ASPACE` already cites, versus sibling processes kept aliased by replicated frame
  mappings, whose apparent cheapness, no syscall touched, does not survive contact with what a
  growing heap needs from an allocator) alongside declining outright. **calef decided 2026-08-22
  (§105): decline.** No customer for real shared-memory threads exists yet; `thread::spawn` stays
  `Unsupported` permanently rather than pending, and milestone 149's Rayon-parallel NPB-Rust variants
  stay out of scope until one does. §105 is the record; nothing here forecloses building the
  seL4-shaped option later.
- **`env` is a table nobody seeds.** The environment backend added in the second pass is honest and
  it is also a stub in one direction: a program can set its own variables and cannot be *given* one,
  because there is no endowment to carry it. That is milestone 47's namespace, and until it lands
  every `env::var` a crate reads is `None`. `chrono`'s `TZ` and `clap`'s colour detection both take
  that path, which is fine and is not the same as working.

- **The sweep is a gate now** (`cargo xtask std-aborts`, fourth pass), and it is honest about its
  boundary. It covers `library/std/src/sys/**` only, because `sys` is the platform layer and a panic
  above it (`path.rs`, `thread/scoped.rs`) is a caller's bug that behaves identically on Linux; a
  first version that swept all of std found about forty of those and none of the platform kind. The
  cost of that boundary is real: **portable std code that is only reachable on a platform this thin
  is invisible to it**, and finding those still needs somebody reading. It also proves reachability
  of a *body*, never of a *call*. See notes/std.md, "What still ends a nife process".

**Effort: not estimated**, deliberately. The measurement is the first deliverable: pick the probes,
build them, and report what breaks.
