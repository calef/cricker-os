# 64. Enough `std` to run somebody else's crate

**Status: NOT-STARTED.** Raised 2026-08-01, from a question with a number behind it: does milestone
27 mean ordinary Rust programs run here?

## What 27 actually delivered, and where it stops

`std` on the native ABI is **BUILT**, and the proof program is real: `println!`, `Vec`, `String`,
`Instant`, `SystemTime` and `std::random` all work through the PAL in `patches/std-cricker/`.

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

## BUGS

- **"Runs unmodified" is the claim to be careful with.** A crate that compiles is not a crate that
  works, and a crate that works under one grant may fail under another, because on this system what a
  program can do depends on what it holds. The acceptance evidence has to be a crate doing its job
  with a stated endowment, not a green build.
- **The PAL patches std's own source**, so every function added here is more surface for
  `toolchain drift` to break against a future nightly. That is a real recurring cost and the reason
  to add only what a probe demands.
- **Threads open a scheduling question this project has not answered.** `std::thread::spawn` implies
  a thread the program owns; the kernel has TCBs and a budget model, and which of those a `std`
  thread is has never been decided.

**Effort: not estimated**, deliberately. The measurement is the first deliverable: pick the probes,
build them, and report what breaks.
