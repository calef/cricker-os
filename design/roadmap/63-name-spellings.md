# 63. Directory and package names: one spelling per thing

**Status: BUILT, 2026-08-01, both ISAs.** Raised 2026-08-01, after `fsserver` was fixed and the
survey behind it found the rest.

**Every table and paragraph below keeps the OLD spellings**, because this block is the record of the
decision and a name's argument is unreadable once the name it argued against is gone. Everywhere
else in the tree carries the new ones. What landed, and the three things that did not, are in
[notes/naming.md](../../notes/naming.md).

## The standard, which is derived rather than invented

The naming tenet (CLAUDE.md) covers crates, programs, modules, shell entry points and markdown. It
says nothing about **directories**, and the tree has three spellings as a result.

The rule that already fits the tree and needs only to be written down:

- **A directory that holds a Rust package is named exactly as the package**, so `snake_case`. Thirteen
  of the multiword directories under `crates/` already do this (`fs_proto`, `dma_validate`,
  `supervision_proto`).
- **Any other directory is lowercase, and hyphenated if it needs two words**, the same convention as
  markdown filenames and `script/` entry points, because a directory is a path element and paths are
  hyphenated in the world outside this repository.

That is not a new tier. It is the existing "each domain keeps its own convention" applied one level
out: a package directory is a Rust name, and everything else is a path.

## Crate renames settled in review (2026-08-01)

| Now | Settled | Why |
|---|---|---|
| `shell` | **`swish`** | the shell gets a proper name rather than a category. See the block below. |
| `capsh` | **`grant_plan`** | it plans grants from a command line; `sysinit` executes them, and that boundary is real. Not named for `swish`, because **seven things use it** (`swish`, `sysinit`, `rm`, `heeder`, `hello`, `fs_nameset_caretaker`, `kernel/src/user.rs`), so naming it for one consumer repeats `dwarden`'s defect. `designation`, `designate` and `designator` were considered and rejected together: **the user designates by typing a name**, and this crate's job starts after that, so all three put it in a role it does not hold. Synonyms of grant (`endow`, `award`, `confer`, `bestow`, `allot`, `furnish`) were rejected because "grant" is already this tree's word and a synonym is a decoder ring; `endow` is additionally taken by `supervision_proto::Endow`. |
| `vterm` | **`display_terminal`** | the tree's own phrase for it, verbatim from its header: "The display terminal (milestone 29, the display ladder's text)." Deliberately **not** the same name as its crate, unlike `compositor` and `line_editor`: the crate is named for the **protocol** it implements (the VT standard, bytes in and a grid out) and the program for its **role** (the terminal on the display), and both facts are worth keeping. It also sits next to `display`, the virtio-gpu driver it is a client of, so the display ladder reads straight from the filenames. `text_console` was rejected because `console` is already a program. |
| `sysinit` | **`system_initializer`** | `system_builder` is unavailable and the collision is the real defect: `builder` already calls itself "a minimal init: **the system builder**", so two programs claim the same phrase. `session_initializer` was rejected for squatting milestone 49's vocabulary, since sessions arrive with users and login and this program manages none. `shell_init` was rejected on evidence: `sysinit` looks the shell up **by name in the initrd**, so it brings up whatever is packed as `shell` rather than `swish` specifically, and it also stays alive as the **spawn service**, which is not a shell concern at all. |
| `elbench` | **`os_primitives_benchmarker`** | **25 bytes: this one requires `NAME_LEN` to be raised first** (see below). `el` was aarch64-only vocabulary for a program that runs on both ISAs, since RISC-V has U/S/M-mode and no exception levels. `os_primitives` is §14's own phrase and disambiguates: in a kernel tree, bare "primitives" reads as *synchronization* primitives. `benchmarker` rather than `benchmark` because **this tree already uses "benchmark" for the output**: `bench/baseline.txt` holds the committed measurements and `notes/benchmarks.md` is about the numbers. The agent noun names the producer, distinct from the product, and joins `broker`, `spawner`, `painter`, `budgeter`, `compositor` and `credentialer`. (`coremark` is not a counter-example: it is a proper noun, EEMBC's industry benchmark.) |
| `vnet` | **`net_transport`** | named for its **role**, not the device class, because naming it `virtio_net` would collide: `crates/virtio` also drives net. It is the adapter that presents smoltcp's `phy::Device` so frames can cross the virtqueue, which is a different job from the driver underneath. Same principle that made `display_terminal` beat `video_terminal` for the program: distinguish by what a thing does, not by what hardware it touches. |
| `rootsup` | **`root_supervisor`** | 15 bytes. `sup` was the abbreviation, so `root_sup` would have relocated the problem rather than fixed it. |
| `subsup` | **`sub_server_supervisor`** | 21 bytes. Exact where `sub_supervisor` is ambiguous: it supervises **a sub-server**, rather than being a supervisor beneath another one. "Sub-server" is established vocabulary, 44 occurrences across `DECISIONS.md`, `supervision_proto`, the kernel and the notes, so the name is built from a word a reader has already met. |
| `allocdemo` | **`allocator_exerciser`** | `alloc` is the crate it *uses*; the allocator is what it *proves*. It wires `user_rt::heap::UntypedHeap` and shows freed memory is reusable rather than leaked. **`exerciser`, not `demo`**: see below. |
| `user-std/` + package `hellostd` | **`std_exerciser`** (directory and package) | the worst mismatch in the tree: directory and package disagreed and neither described the contents. It is "the std proof: an ordinary Rust program, no `no_std`, running on the native capability ABI", one binary whose three behaviours are chosen by the authority it was granted. Same milestone 27 as the allocator one, so they are siblings by construction and now read as the pair they are. |
| `credential` | **`credentialer`** | an agent noun in the `broker`/`swapper`/`painter` family, and a **real profession**: a credentialer verifies licenses against records they hold and never hands the record back, which is this service exactly. I argued for the plain noun on the `clock`/`entropy` resource pattern and was wrong twice: `credentialer` is not a coinage, and **this service will never give you a credential**, so naming it for the resource implies the one thing it exists to refuse. |
| `credcli` | **`credentialer_test_client`** | 24 bytes, exactly the current cap, comfortable once `NAME_LEN` moves. |
| `fsclient` | **`fs_test_client`** | see the note below on why all three carry `test`. |
| `netcli` | **`socket_test_client`** | a client of the socket contract (`socket_proto`), which is what its own first line claims. It drives three fixed exchanges against QEMU user-mode networking (slirp's built-in TFTP, a real DNS query that leaves the machine and is therefore non-gating, and a TCP echo round trip) and reports `OK` or a stage code so the kernel test fails loudly rather than hanging. |
| `netstack` | **`net_stack`** | same: `net` is already this tree's word. |
| `dma_validate` | **`dma_validator`** | it calls itself "the DMA-confinement **validator**" in its own first line; the name simply did not. |
| `measure` | **`measured_boot`** | "measured boot" is the standard term for boot-time hashing, so this gains the guard-rail benefit too: a reader who knows secure-boot vocabulary recognises it. |
| `compose` | **`compositor`** | the noun, and accurate about scope: the whole compositor problem (scene, clipping, damage arithmetic, contract), not just one of them. `compositor_proto` was rejected earlier because this is a logic crate, not a wire contract; `scene` undersells the arithmetic. **Sharing a name with the program is the point, not a collision**: the crate is that program's logic lifted out to be host-testable, and `coremark` and `lineedit` already do exactly this. |
| `lineedit` (crate **and** program) | **`line_editor`** | the crate's own header calls it "a sans-IO **editor**", and there is no `Editor` type to stutter against (it exports `proto`, `expand_output` and the `OP_*` constants). `line_discipline` was rejected as overclaiming: that term covers the whole tty layer including echo, canonical mode, signals and flow control, and this crate is narrower. `line_edit` was rejected for being a verb phrase where its sibling `video_terminal` is a noun. |
| `uheap` | **`user_heap`** | the `u` was *userspace*, and `user_rt` already establishes `user_` as the prefix for it. |
| `vt` | **`video_terminal`** | the true expansion: DEC's VT100 and VT220 were **Video** Terminals. `virtual_terminal` was proposed and rejected as wrong twice, since that is not what VT stood for and "virtual terminal" already names Linux's virtual consoles. `screen_grid` was rejected because the crate carries 63 escape-sequence references: the grid is the output and interpreting the protocol is the work. calef's reason for expanding rather than keeping `vt`: a reader can relate it back to the thing they already know, with less ambiguity than two letters that could read as *vector table* in a kernel. |
| `caps` | **`capability`** | the crate is the capability *model*, not a container: it exports `Cap`, `Rights`, `Object`, `Reap` and `CSpace`. `cap_space` was considered and rejected because it names one of five exports and stutters as `cap_space::CSpace`. `CSpace` itself stays: it is seL4's own spelling. |

These are folded in here rather than given their own milestone because a crate rename is a
directory rename, which is what this milestone already is.

## `swish`: the shell has a name now (calef, 2026-08-01)

`shell` is a category, not a name. `bash`, `zsh`, `fish` and `rc` are names; this project's most
demonstrable artifact was filed under the noun for what kind of thing it is.

**`capsh` was the obvious candidate and is unavailable.** Linux's libcap ships `capsh(1)`, a
"capability shell wrapper" for testing POSIX capabilities, which is adjacent enough that a reader who
knows Linux capabilities would assume ours is that tool.

**Why `swish` rather than something descriptive.** Shell names are identities rather than
descriptions: `fish` describes nothing and nobody minds. But this one happens to carry the thesis
anyway, which is the combination shell names almost never manage.

A swish is the basketball shot that goes through the net **touching nothing**. That is least
authority in one word: the command reaches exactly what it designated and nothing else. `wc
report.txt` touches `report.txt` and not one thing more, and it does so structurally rather than by
a check that could be wrong.

It also reads as a shell on sight, because the `sh` is built in, the same trick `bash` plays with a
pun.

**`sheesh` was considered and set aside on two grounds**, both recorded because they are the kind of
thing that is obvious only once said. It carries a timestamp: the word spiked as a meme around
2020-21, where `bash` and `fish` are era-neutral, and this project expects to be shown off years from
now. And *sheesh* is an interjection of **exasperation**, while this shell's most characteristic
behaviour is **refusing things** by design. The name and the experience would have pointed the same
direction, and "the shell that says no" reading as a complaint is a risk a name should not carry for
free. `swish` inverts both: a precision word on a precision property.

Two wrinkles, neither disqualifying: Sweden's mobile payment system is called Swish (different
domain, no confusion in a terminal), and `swish` contains "wish", which is faintly the wrong idea for
a system where you do not ask for authority, you hold it.

## The three that violate it

| Now | Should be | Severity |
|---|---|---|
| `fs-server/`, package `fs-server` | `fs_server/`, package `fs_server` | consistent with itself, inconsistent with the other 37 crates |
| `tools/redoxfs-host/`, package `redoxfs-host` | `tools/redoxfs_host/`, package `redoxfs_host` | same |
| `user-std/`, package **`hellostd`** | one name, spelled once | **the real defect** |

**`user-std` is the one worth doing even if the others are deferred.** The directory says one thing,
the package says another, and the package name is squished besides. Neither name describes what is
in it: `user-std/src/main.rs` is "the std proof (milestone 27): an ordinary Rust program, no
`no_std`, running on the native capability ABI", which is one of this project's better
demonstrations and is currently filed under a name that suggests a hello-world.

## Why this is its own milestone rather than part of 61

Milestone 61 is already moving about 532 tokens plus eight programs, and a directory rename touches
roughly forty files by path. Two renames in flight would collide in `notes/`, `DECISIONS.md` and
`kernel/src/user.rs`, which is exactly the avoidable collision CLAUDE.md has three rules about. **This
starts after 61 lands.**

## BUGS

- **A hyphenated package name is not wrong in the wider ecosystem**, and that is the argument against
  doing this at all. `wasm-bindgen` and `tracing-subscriber` are ordinary, Cargo normalises a hyphen
  to an underscore for `use`, and nothing is broken today. The case for the change is internal
  consistency (37 crates against 3) rather than correctness, and it should be weighed as such.
- **`target/` and `targets/` sit next to each other** and mean unrelated things: build output, and the
  custom target JSON specs (`aarch64-unknown-nife.json`). Nothing enforces the distinction and one
  is gitignored while the other is tracked. Worth folding in.

## `exerciser`, not `demo` (calef, 2026-08-01)

**"Exercise" is this tree's own verb**, 130 uses across it, and these programs use it about
themselves: "exercises the capability-shaped contract", "exercises the platform", "every line
exercises a PAL surface". `demo` was never the word they reached for.

It is also **real systems vocabulary** rather than a coinage: memory exercisers, bus exercisers and
disk exercisers have meant "a program that puts a subsystem through its paces" for decades, which
puts it in the guard-rail category of terms a reader already knows.

And it is more precise. A demo *shows something off*; an exerciser *puts it under load and sees
whether it holds*. `allocator_exerciser` does interleaved allocation and free in arbitrary order,
drop-and-reuse, and a final large allocation that must fit in pages already committed, proving freed
memory is genuinely reusable. Its own header calls that "the allocator **workload**".

It is an agent noun, so it joins `broker`, `spawner`, `painter`, `credentialer` and `benchmarker`,
which is where the noun rule points.

**This category is distinct from the `_test_client` trio and the distinction is real.** A client
exercises a **service contract from the outside**, with a server on the other end; that is what
`client` means in those names. An exerciser demonstrates a capability of the system in itself, with
no contract being probed from a client side. `std_test_program` was considered and rejected for
importing the clients' vocabulary into the wrong family.

## Why the three clients carry `test` (calef, 2026-08-01)

`fs_client`, `credentialer_client` and `socket_client` are **the names the real things will want**,
and the real things are coming: milestone 55 needs an actual credentialer client for SMB
authentication, milestone 54 needs actual socket clients, and any program that wants files is an FS
client. Giving those names to test programs squats them, and the bill arrives later as a rename or as
something worse like `real_fs_client`.

It also fails the tenet's own test. `fs_client` predicts "a client of the FS service", not "the
program the kernel spawns to prove the FS contract holds". The qualifier is the distinguishing fact,
not noise.

I argued the opposite first, for consistency with two names already recorded here. That was
consistency in the wrong direction: three names consistently squatting the good ones.

**`witness` was the alternative and is not a coinage**, which is worth recording since I first called
it insider vocabulary and was wrong. It is standard in proof theory (the concrete object
demonstrating an existential claim), in model checking (a counterexample trace, the world Kani and
CBMC already live in here), and in cryptography (the zero-knowledge witness, Bitcoin's SegWit). The
tree already uses it: "the extended-attribute witness", "witness pages". It was set aside because
`client` carries real information about what the program *is* that `witness` does not, and because
this project's stated audience arrives from Linux rather than from formal methods.

## Raise `NAME_LEN` FIRST, because one rename now depends on it

**DONE, 2026-08-01, ahead of the rename and on its own merits.** `NAME_LEN` is 32, `ENTRY_LEN` 40,
`DIR_BLOCKS` 6, `MAX_FILES` 76 (up from 63), and the magic is `CRKR0002`. `os_primitives_benchmarker`
fits with seven bytes to spare, so the rename below is unblocked. **One thing in the paragraphs below
was wrong and is worth reading before trusting them:** the kernel-stack cost had already been
retired, because `Fs` stopped holding a fixed entry array when the FS-server stack bug was fixed, so
the raise was much cheaper than the trade described here. The measured numbers and the reasoning are
in [notes/nifefs.md](../../notes/nifefs.md). The paragraphs are kept as written because the
decision to do this first, rather than under pressure from a name, is the part that generalises.

`nifefs` caps archive names at 24 bytes, and **three naming decisions have crowded it while a
fourth exceeds it**: `fs_subtree_caretaker` at 20, `sub_server_supervisor` at 21, and
`os_primitives_benchmarker` at **25, which does not fit at all**.

That makes this a **prerequisite rather than a tidy-up**, and the ordering matters: raise the cap on
its own merits, with the costs below written down, and *then* land the rename. Choosing a worse name
to fit a limit, or raising a limit because a name demanded it, are both the wrong way round. Three bytes of headroom is not a
budget, it is a trap, and discovering it a third time as a build error during an unrelated change is
the expensive way to find out.

It is a real trade rather than a free win. `NAME_LEN` sits inside `ENTRY_LEN = 32`, so widening it
costs directory entries per block (`MAX_FILES` is 63 at `DIR_BLOCKS = 4`) and it costs **kernel
stack**, because `Fs` holds `entries` as a fixed array that is a stack local in the boot and spawn
paths. The FS server was once found to have died 528 bytes short of stack, so this is not headroom to
spend casually. There is no data migration, because every image regenerates from the crate.

Do it here, with the numbers written down, rather than under pressure from a name that will not fit.

**Effort: small**, and almost entirely mechanical, but it touches paths in `script/`, `xtask`,
`deny.toml`, CI, and a long tail of notes.
