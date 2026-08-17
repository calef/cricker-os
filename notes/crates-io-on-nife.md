# Somebody else's crate: what crates.io does against the nife `std`

*(Milestone 64, measurement phase, 2026-08-04. Milestone 27 built a `std` platform layer on the
native ABI (notes/std.md) and claimed it widened real workloads to "most of crates.io that stays off
fs and threads". This note is the attempt to find out whether the qualifier is doing as much work as
it sounds like. Fifty crates were taken off crates.io and built against the patched `std`; this is
what happened and why.)*

**The short answer: the qualifier was pessimistic, and the table was measuring the wrong thing.**
`std::fs` answering `Unsupported` for 32 of 54 functions is not what stops crates from building.
**39 of 50 probes built with no change at all**, including `regex`, `serde_json`, `tokio`'s
current-thread runtime, `rayon`, `clap`, `chrono` and `walkdir`. Of the 11 that failed, **eight
failed on one crate**, and it is not part of `std`.

> **The split was recorded as 35/15 and it is 39/11** (re-derived 2026-08-17, milestone 64's second
> pass, against the same fifty crates and the tree of that day). Nothing about the tree changed to
> make it so and no probe changed its answer: the original headline **double-counted the four crates
> that appear in two failure classes at once**. `zip` and `ring` are in class A *and* class C;
> `gix-config` and `gix` are in class A *and* class B. Summing the class headings gives
> 8 + 3 + 3 + 1 = 15, and the distinct crates behind them are eleven. The tables below were right all
> along and always listed 39 passes; only the sentence over them was wrong, which is why nothing
> caught it. Take a count from the merged tree, with a pattern you have checked against the real
> shapes: this note's own BUGS said that about the census and the headline did it anyway.

**The long answer has a sting in it, and it is the block's own BUGS entry made concrete:** a crate
that compiles is not a crate that works. `tempfile` builds and links, and every one of its
operations returns "operation not supported on this platform" at runtime, because it has an explicit
fallback arm for platforms it does not know. Nothing in a green build says so. See
[What compiles and still does nothing](#what-compiles-and-still-does-nothing).

## How to reproduce this

The whole measurement is `cargo build` against the linked `nife-dev` toolchain, one throwaway
crate per probe:

```sh
cargo xtask std-src                      # build/refresh the patched-std farm, link nife-dev
mkdir -p /tmp/probe/src && cd /tmp/probe
cat > Cargo.toml <<'EOF'
[package]
name = "probe"
version = "0.1.0"
edition = "2021"
[workspace]
[dependencies]
regex = "1"
[profile.release]
panic = "abort"
EOF
echo 'fn main() { println!("{}", regex::Regex::new("^a+$").unwrap().is_match("aaa")); }' > src/main.rs

RUSTUP_TOOLCHAIN=nife-dev cargo build --release \
    -Zjson-target-spec \
    -Zbuild-std=core,alloc,std,panic_abort \
    -Zbuild-std-features=compiler-builtins-mem \
    --target /path/to/nife/targets/aarch64-unknown-nife.json
```

**Build a `[[bin]]`, not a `[lib]`, and this is not a detail.** A library target is never linked, so
a crate whose only blocker is a missing C library passes. `diesel` with the `sqlite` feature compiles
clean and then fails at `rust-lld: error: unable to find library -lsqlite3`. The first pass of this
measurement used libraries and recorded `diesel` as a pass.

**And the `main` must CALL the crate**, which is the same rule one notch further in and which the
2026-08-17 re-derivation found by tripping over it. A `[[bin]]` whose body is `fn main() {}` declares
the dependency, compiles it, and still does not link it, so `diesel` passed again until the probe was
given `SqliteConnection::establish(":memory:")` to call, at which point `-lsqlite3` came back exactly
as recorded. The rule that covers both: **a probe proves nothing the linker was not asked to do.**

Set `CARGO_TARGET_DIR` to one shared directory across probes so build-std compiles the patched `std`
once (about 10 seconds) instead of once per crate.

## What happened, by crate

Fifty crates, resolved versions as of 2026-08-04, built for `aarch64-unknown-nife`.

### Built with no change (39)

| crate | version | why it was probed |
|---|---|---|
| `itertools` | 0.14.0 | the floor: pure computation |
| `bitflags` | 2.13.1 | the floor |
| `memchr` | 2.8.3 | the floor |
| `byteorder` | 1.5.0 | the floor |
| `smallvec` | 1.15.2 | the floor |
| `hashbrown` | 0.15.5 | the floor |
| `nom` | 8.0.0 | parser |
| `regex` | 1.13.1 | parser, heavy generics |
| `httparse` | 1.10.1 | wire format |
| `semver` | 1.0.28 | parser |
| `url` | 2.5.8 | parser, 36-crate closure |
| `serde` | 1.0.229 | serialization, derive macros |
| `serde_json` | 1.0.151 | serialization |
| `toml` | 0.9.12 | serialization |
| `csv` | 1.4.0 | serialization over `io::Read` |
| `base64` | 0.22.1 | format |
| `sha2` | 0.10.9 | format |
| `hex` | 0.4.3 | format |
| `flate2` | 1.1.9 | compression (`rust_backend`) |
| `miniz_oxide` | 0.8.9 | compression |
| `log` | 0.4.33 | ubiquitous facade |
| `anyhow` | 1.0.104 | ubiquitous errors |
| `thiserror` | 2.0.19 | ubiquitous errors |
| `once_cell` | 1.21.4 | sync primitive |
| `bytes` | 1.12.1 | buffers |
| `chrono` | 0.4.45 | wall-clock time |
| `time` | 0.3.55 | wall-clock time |
| `tempfile` | 3.27.0 | fs, and see the sting below |
| `walkdir` | 2.5.0 | fs traversal |
| `fs-err` | 3.x | fs, names every call it makes |
| `ignore` | 0.4.33 | fs plus threads |
| `crossbeam-channel` | 0.5.16 | threads |
| `rayon` | 1.12.0 | threads |
| `num_cpus` | 1.17.0 | reports parallelism |
| `mio` | 1.2.2 | (with no features; see `rocket`) |
| `tokio` | 1.53.1 | async runtime, `rt` feature |
| `clap` | 4.6.5 | ubiquitous CLI |
| `tracing` | 0.1.44 | ubiquitous observability |
| `gix-hash` | 0.20.1 | milestone 99 leaf |

Twelve of these were rebuilt as **executables** that call the crate for real (`serde_json` round
trip, `regex` match, `flate2` gzip round trip, `walkdir` over `.`, `chrono::Utc::now`, `clap` parse,
a `tokio` current-thread `block_on`, `csv` records, `tempfile::NamedTempFile::new`,
`fs_err::read_to_string`, `rayon` parallel sum). All twelve linked. **Whether they work is a
different question**, answered below and not by this measurement.

### Failed, and why (11 crates, in four classes)

Every failure is one of four classes. The class matters much more than the crate, which is why the
classes are the headings; **four crates are in two classes at once**, and adding the class headings
up is what produced the wrong 15 above.

| crate | classes |
|---|---|
| `rand`, `uuid`, `gix-object`, `gix-actor` | A only |
| `zip`, `ring` | A, then C |
| `gix-config`, `gix` | A, then B |
| `tar` | B |
| `diesel` | C |
| `rocket` | D |

#### Class A: `getrandom` has no `nife` backend (8 of 11), **closed 2026-08-17**

`rand`, `uuid`, `zip`, `gix-object`, `gix-actor`, `gix-config`, `gix`, `ring` all die on the same
`compile_error!`:

```
error: target is not supported. You may need to define a custom backend see:
       https://docs.rs/getrandom/0.3.4/#custom-backend
```

`getrandom` selects a backend on `target_os`, and there is no `nife` arm. This is not a `std` gap
at all: `std::random::SystemRng` works (milestone 56, slot 6). It is one crate in the ecosystem that
predates us.

**It is also the cheapest thing on this list to fix, and that was measured rather than assumed.**
`getrandom` 0.3 and 0.4 both document a custom backend: build with
`RUSTFLAGS='--cfg getrandom_backend="custom"'` and define one function. With a stub backend that
fills from `std::random::SystemRng`, six of the eight went from failing to building:

| crate | round 1 | with a custom `getrandom` backend |
|---|---|---|
| `rand` 0.9.5 | FAIL | **PASS** |
| `uuid` 1.24.0 | FAIL | **PASS** |
| `gix-object` 0.51.1 | FAIL | **PASS** |
| `gix-actor` 0.36.1 | FAIL | **PASS** |
| `gix-features` 0.45 | FAIL | **PASS** |
| `gix-hash` 0.20.1 | (passed) | **PASS** |
| `zip` 5.1.1 | FAIL | FAIL (class C: `zstd-sys`) |
| `gix-config` 0.48.0 | FAIL | FAIL (class B: `gix-sec`) |
| `gix` 0.74.1 | FAIL | FAIL (class B: `gix-sec`) |
| `ring` 0.17.14 | FAIL | FAIL (class C: C sources) |

The backend used for the probe:

```rust
#![feature(random)]
use std::random::{Rng, SystemRng};

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let buf = unsafe { core::slice::from_raw_parts_mut(dest, len) };
    SystemRng.fill_bytes(buf);
    Ok(())
}
```

Note the symbol is `__getrandom_v03_custom` for **both** 0.3 and 0.4; `getrandom` 0.4.3's
`backends/custom.rs` still declares the v03 name. `getrandom` 0.2, which `ring` pulls, uses a
different mechanism (the `register_custom_getrandom!` macro), so a fix has to cover two shapes.

**The right fix is probably not the custom hook**, because the hook is a `RUSTFLAGS` setting that
every consumer has to remember and that a workspace cannot express per-dependency. `getrandom` 0.4
carries a `hermit.rs` backend selected by `target_os = "hermit"`, which is exactly the shape this
project's `std` already took from Hermit. An upstream arm, or a patch under `patches/`, is a
decision for whoever picks this up.

##### What was decided, and where the paragraph above was wrong

**`patches/getrandom-nife/` is the answer** (milestone 64, 2026-08-17), and it is the custom hook
after all. The objection above is half right: the flag is a `RUSTFLAGS` setting, and a consumer that
forgets it gets the same `compile_error!`. But **a workspace states it once, in its own
`.cargo/config.toml`**, and per-workspace is the correct granularity anyway, because whether an
entropy source exists is a property of the target rather than of any one dependency. "Cannot express
per-dependency" was true and was not the requirement.

What tipped it was the other side of the ledger. A `[patch.crates-io]` fork needs no flag at the call
site, which is genuinely nicer, and costs a maintained fork of a crate this very note recorded as
**mid-transition across 0.2, 0.3 and 0.4 inside one dependency graph**. §46's rule is that we vendor
where correctness is won by exposure; a hook the upstream crate designed for this case is neither
that nor code we would otherwise write, and it is eleven lines that upstream cannot break without
also breaking Hermit's.

**The upstream arm is still the right long-term fix and is a smaller diff than the hook.** It is a
pull request against `getrandom`, not a change to this tree.

Three things about the recipe, and the third cost an hour:

1. Depend on `getrandom_backend` (a **provisional** name; calef names crates).
2. `rustflags = ["--cfg", "getrandom_backend=\"custom\""]` in the consuming workspace's
   `.cargo/config.toml`.
3. **`use getrandom_backend as _;` in the binary.** An rlib nothing references is not linked, so
   without this the build reaches `rust-lld: error: undefined symbol: __getrandom_v03_custom`. Two of
   the eight probes linked without it because they happened to call `getrandom` on a path the linker
   kept; six did not. Same shape as a `no_std` panic handler: a crate that exists to define a symbol
   has to be pulled in on purpose.

Re-measured with it, on 2026-08-17:

| crate | with `patches/getrandom-nife` |
|---|---|
| `rand` 0.9 | **PASS** |
| `uuid` 1 | **PASS** |
| `gix-object` 0.51 | **PASS** |
| `gix-actor` 0.36 | **PASS** |
| `gix-config` 0.48 | FAIL, class B (`gix-sec`) |
| `gix` 0.74 | FAIL, class B (the `errno` crate now, not `gix-sec`) |
| `zip` 5 | FAIL, class C (`zstd-sys`) |
| `ring` 0.17 | FAIL, `getrandom` **0.2**, whose hook is the `register_custom_getrandom!` macro |

**Four of eight, which is exactly what the 2026-08-04 stub reached**, and the point is the other
four: not one of them still fails on `getrandom`'s dispatch. Class A is closed and what is left
behind it is classes B and C, which are different work.

#### Class B: the crate falls through to `unix` (3 of 11)

`tar` (via `filetime` 0.2.29) and `gix-config`/`gix` (via `gix-sec` 0.12.2) both do a
`cfg_if`-shaped dispatch whose **last arm assumes unix**:

```
error[E0433]: cannot find `unix` in `os`
  --> gix-sec-0.12.2/src/identity.rs:36:26
   |
36 |             use std::os::unix::fs::MetadataExt;
error[E0425]: cannot find function `geteuid` in crate `libc`
```

This is the class that says something about us rather than about them. There is no
`std::os::nife`, `libc` has no `nife` module, and a crate whose platform ladder ends in "else
it is unix" cannot compile here. The three crates that hit it are asking for a **uid** and a
**file mtime set**, neither of which this system has in the form they want.

Contrast `tempfile`, which has an explicit `other.rs` arm and therefore compiles. **The distinction
between the 35 and this class is entirely whether the crate author wrote a fallback**, and nothing
about how hard the crate is.

#### Class C: a C library or C sources (3 of 11)

- `ring` 0.17.14: builds C and assembly, and `cc` cannot find `assert.h` for this target.
  `fatal error: 'assert.h' file not found`.
- `zip` 5.1.1 (via `zstd-sys`): same shape, a C build script.
- `diesel` 2.3.11 with `sqlite`: **compiles** and fails at link,
  `rust-lld: error: unable to find library -lsqlite3`.

These are jobs for the C seam (notes/c-seam.md), not for the `std` PAL. They are also the only class
where "make it build" and "make it work" are the same task.

#### Class D: a socket/fd model that does not exist (1 of 11)

`rocket` 0.5.1 fails in `socket2` 0.6.5 and then `mio` 1.2.2:

```
error: Socket2 doesn't support the compile target
error[E0432]: unresolved import `crate::sys::IoSourceState`
```

`mio` with **no** features compiles (its `sys` module is empty), which is why it appears in the pass
list; the moment anything asks for the reactor it does not. This is the same shape as class B, one
level up: a readiness-based IO model wants file descriptors and a poller, and neither exists here.

## What compiles and still does nothing

`tempfile` 3.27.0 selects its platform like this:

```rust
#[cfg_attr(any(unix, target_os = "redox", target_os = "wasi"), path = "unix.rs")]
#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(not(any(unix, target_os = "redox", target_os = "wasi", windows)), path = "other.rs")]
mod platform;
```

`other.rs` is six functions, and every one of them is:

```rust
Err(io::Error::new(io::ErrorKind::Other, "operation not supported on this platform"))
```

So `tempfile` builds, links, and returns an error from `NamedTempFile::new()`. That matters far
beyond `tempfile`, because **gitoxide's atomicity story runs through it**: `gix-lock`'s commit calls
`self.inner.persist(&resource_path)`, which is `gix-tempfile`, which is `tempfile::persist`, which
on nife is `not_supported()`. A `gix` that built cleanly would fail to write a single ref.

The lesson for milestone 99 and 66 is a sequencing one. **Do not read a passing build as a working
crate**, and do not order the work by what fails to compile: `tempfile` never appears on a build
failure list and is on the critical path for git.

## The prioritised gap list

This is the deliverable milestones 99 and 66 consume. It is **not** the order the milestone 27 table
suggests, because that table counts functions and this counts demand.

The method: for each probe, take `cargo tree -e normal --target aarch64-unknown-nife.json` (normal
edges only, so `cc`, `autocfg`, `vcpkg` and every proc-macro crate are excluded, since those run on
the **host** and can call anything they like), then grep every package's `src/` for call sites of the
std APIs the PAL refuses. The "probes" column is how many of the 50 dependency closures contain at
least one call site.

| rank | gap | PAL today | probes | packages | note |
|---|---|---|---|---|---|
| 1 | `getrandom` backend | **CLOSED** 2026-08-17 | 8 failed outright | `rand`, `uuid`, `ring`, all of `gix` | not a std gap; `patches/getrandom-nife` |
| 2 | `std::os::unix` fallthrough | absent, **declined** | 21 | 34 | mostly benign (behind `cfg(unix)`); fatal in `filetime`, `gix-sec`. See below |
| 3 | `thread::spawn` | `Unsupported`, **a fork** | 20 | 33 | `rayon`, `crossbeam`, `tokio`, `diesel`. See below |
| 4 | `env::var` | **CLOSED** 2026-08-17 | 16 | 24 | `chrono` (TZ), `clap`, `gix`, `figment`; `vars()` used to **panic** |
| 5 | `fs::create_dir(_all)` | `Unsupported` | 11 | 17 | **verb exists** (`MKDIR`) |
| 6 | `available_parallelism` | `Ok(1)` | 11 | 7 | already answers honestly |
| 7 | `fs::read_link` | `Unsupported` | 10 | 10 | no symlinks in the contract |
| 8 | `File::set_len` | **CLOSED** 2026-08-17 | 10 | 9 | `TRUNCATE` existed all along; only the size word was missing |
| 9 | `fs::symlink_metadata` | **already bound** | 9 | 13 | the row was stale: std routes it to `lstat`, which this PAL binds |
| 10 | `std::os::fd` | absent | 9 | 12 | `mio`, `memmap2`, `is-terminal` |
| 11 | `fs::read_dir` | `Unsupported` | 8 | 13 | **verbs exist** (`OPENDIR`, `READDIR`) |
| 12 | `fs::remove_file` | `Unsupported` | 7 | 12 | **verb exists** (`UNLINK`) |
| 13 | `fs::remove_dir(_all)` | `Unsupported` | 7 | 8 | **verb exists** (`RMDIR`) |
| 14 | `fs::hard_link` | `Unsupported` | 7 | 4 | no verb |
| 15 | `Permissions` | `readonly` is `false` | 7 | 8 | authority is a capability, not a mode bit |
| 16 | `env::temp_dir` | no PAL at all | 7 | 8 | needs a namespace answer, not a PAL one |
| 17 | `process::Command` | no PAL at all | 6 | 10 | `gix-command`, `gix-credentials` |
| 18 | `env::current_dir` | no PAL at all | 6 | 5 | same namespace question |
| 19 | `Metadata::modified` | `Unsupported` | 5 | 7 | no verb reports mtime, but §51 gave us a clock |
| 19a | `Path::is_dir` on a directory | was always `false` | (not counted) | | closed with the five above; `create_dir_all` needed it |
| 20 | `fs::set_permissions` | `Unsupported` | 4 | 3 | |
| 21 | `TcpListener` | `Unsupported` | 4 | 11 | no LISTEN verb in the socket contract |
| 22 | `ToSocketAddrs` / DNS | numeric only | 4 | 5 | |
| 23 | `Metadata::created` | `Unsupported` | 3 | 3 | |
| 24 | `set_nonblocking` | `Unsupported` | 3 | 4 | contract is blocking-only |
| 25 | `fs::rename` | `Unsupported` | 2 | 2 | **verb exists** (`RENAME`); undercounted, see BUGS |
| 26 | `fs::copy` | **CLOSED** 2026-08-17 | 2 | 2 | needs no verb: an open, a read/write loop, two closes |
| 27 | `fs::canonicalize` | `Unsupported` | 2 | 1 | |
| 28 | `File::set_times` | `Unsupported` | 2 | 1 | |
| 29 | `File::try_clone` | `Unsupported` | 2 | 1 | a handle is one session's token (§27) |
| 30 | `File::lock`/`try_lock` | `Unsupported` | 2 | 1 | `gix-tempfile` |
| 31 | read/write timeouts | `Unsupported` | 1 | 1 | |
| 32 | `Metadata::accessed` | `Unsupported` | 0 | 0 | **nobody asked for it** |

### The second pass, 2026-08-17: what closed, what was declined, and why

Milestone 64's first pass took the five bindings below. The second worked the ranked list from the
top and stopped where the reason to stop was a decision rather than an effort.

**Closed:** rank 1 (`getrandom`, above), rank 4 (`env`), rank 8 (`File::set_len`), rank 26
(`fs::copy`). Rank 9 turned out to need nothing: `fs::symlink_metadata` routes to `sys::fs::lstat`,
which this PAL has bound since milestone 27, so the row was recording a refusal that was not there.

**Rank 4 is the one worth reading, because it is the sting in a second place.** `env::var` was
recorded as "no PAL at all", which sounded like the harmless kind of gap: `getenv` falling through to
`sys::env::unsupported` answers `None`, and `None` is what a Unix box with the variable unset answers
too. But the same fallback's `env()` is `panic!("not supported on this platform")`, so
**`std::env::vars()` aborted the process**, and so did `Command::envs`, a logger dumping its
configuration, anything that filters the environment rather than asking for one name. Like
`tempfile`, it compiled perfectly. Unlike `tempfile`, the fix was ours.

The backend (`patches/std-nife/overlay/std/src/sys/env/nife.rs`) is a **process-local table, empty at
start**: nothing endows a nife process with variables, `set_var` works because that is what `set_var`
means everywhere, and `vars()` returns an empty iterator, which is the one answer here that is never
a lie. Milestone 47's namespace is where a *seeded* environment would come from, and the shape does
not change when it arrives.

**Declined, and each for a reason rather than for time:**

- **Rank 2, `std::os::unix`.** The three crates behind it want a **uid** (`gix-sec`) and a **file
  mtime set** (`filetime`), and this system has neither in the form they ask for. A `std::os::nife`
  that answered `geteuid()` would be inventing an identity nothing issues, and a `MetadataExt` over
  a contract with no mode bits would be a Unix fiction over a capability refusal, which is exactly
  what the `InvalidFilename`-not-`PermissionDenied` choice in `sys/fs/nife.rs` exists to avoid. §42's
  rule is to declare what you offer; the honest answer here is that these crates cannot build, and
  the note's own observation stands: *the distinction between the passes and this class is entirely
  whether the crate author wrote a fallback.*
- **Rank 3, `thread::spawn`.** Not declined on the merits: it is a **design fork** and the roadmap
  block already says so in its own BUGS. The kernel has everything the spawn needs (retype a TCB,
  CONFIGURE it into this address space, START it); what has never been decided is *what a `std`
  thread is* against the budget model, and a PAL that guessed would ship the answer as an
  implementation detail. It also has no build failures behind it at all: all four of `rayon`,
  `crossbeam-channel`, `tokio` and `ignore` compile and link today.
- **Ranks 7, 10, 14, 15, 20, 23, 29 and 30** (`read_link`, `std::os::fd`, `hard_link`, `Permissions`,
  `set_permissions`, `Metadata::created`, `File::try_clone`, `File::lock`). Each refuses because
  nothing in §27 backs it, and inventing a backing is the failure mode. `try_lock` is the one that
  will hurt: `gix-tempfile` wants it.
- **Rank 19, `Metadata::modified`.** The nearest miss on the list. The FS server keeps an mtime and
  §43 gave us a clock to read it against, so the only missing piece is a **field in `FSTAT`'s
  reply**, which makes it a wire-format change, the expensive and irreversible kind, and not a
  lane's to make. It wants a `DECISIONS` section and calef.
- **Ranks 16, 18 and 27** (`env::temp_dir`, `env::current_dir`, `fs::canonicalize`) and everything
  else that needs to resolve a path against something. These are the `File::open` resolution fork,
  which the roadmap block reserves to be answered jointly with milestone 47's namespace half rather
  than twice. Nothing here routed around it.
- **`remove_dir_all`** keeps the existing refusal rather than gaining a recursion. `readdir` shows a
  PAL *can* hold a capability per level, so the mechanism is not the obstacle; the recorded reasoning
  (the loop belongs where each step can be checked against that level's capability, in `user/src/rm.rs`)
  is a decision this lane had no evidence to overturn.

### Five of these were bindings, not verbs, and milestone 64 bound them

Ranks 5, 11, 12, 13 and 25 (`create_dir`, `read_dir`, `remove_file`, `remove_dir`, `rename`) were
each backed by a verb the FS server **already implemented**: `fs_server/src/bin/fs_server.rs` has
dispatched `MKDIR`, `OPENDIR`, `READDIR`, `UNLINK`, `RMDIR` and `RENAME` since milestones 47 and 48.
Nothing was missing from the contract and nothing was missing from the server; the client side in
`patches/std-nife/overlay/std/src/sys/fs/nife.rs` simply still refused, and its own comments
still said the verbs did not exist.

**All five are bound now**, and the `std_exerciser` demo walks them under a real directory
capability on both ISAs. See notes/std.md for the behaviours (what `read_dir(".")` means with no
global namespace, why the listing is drained rather than streamed, and why `remove_file` refuses a
directory).

**A sixth came with them and is not in the table above**, because the census had no row for it:
`Path::is_dir()` was `false` for every directory that exists, since `OPEN` refuses a directory and
`stat` propagated the refusal. That made `std::fs::create_dir_all` non-idempotent, which is the
call every crate reaches for rather than `create_dir`. It costs no extra message: the `EISDIR` the
server already sends *is* the answer. Nothing in a gap list built from `Unsupported` counts would
have found it, because `stat` never returned `Unsupported`; it returned the wrong thing.

Milestone 47's block had already said this for `rename`, `unlink` and `rmdir` ("now a binding gap
rather than a missing verb"). The measurement added two, `read_dir` and `create_dir`, and put
`create_dir` above all of them by demand.

**A refusal outlived its reason by two milestones and nothing caught it**, which is the finding
worth more than the five functions. A refusal that looks correct reads exactly like a refusal that
is correct, and the comment explaining it is written once and then believed. The thing that found it
was not a review; it was asking fifty crates what they wanted.

**And binding them turned up a bug that only a narrowed capability would have shown.** `OPENDIR` and
`MKDIR` take the rights the caller wants on the child in the request's second word, and the server
answers `EPERM` if the intersection with the parent's rights comes up **short of what was asked**
(§47's monotonicity is the intersection; the refusal is the server telling the truth about it). The
first version of this binding asked for `dir::ALL`, which works perfectly through the full-rights
grant every test uses and fails through every narrowed one. A PAL cannot know what its own
capability carries, so it has to ask for the minimum the operation needs: `ENUMERATE` for a listing,
and nothing at all for `create_dir`, which closes the handle it gets back. Caught by reading the
server rather than by a test, and there is still no test that would catch it (a std program spawned
with a narrowed directory capability does not exist yet; that wants a lane).

## BUGS

- **The runtime half is still not measured, for somebody else's crate.** Everything in the tables
  here is compile and link; no probe has ever been booted, so no line in them is evidence that a
  *crate* works, and `tempfile` is proof that the distinction is real rather than pedantic.
  What the second pass added is runtime evidence for **the PAL surfaces it closed** (`env`,
  `set_len`, `copy` are asserted by `std_exerciser` under a real directory capability on both ISAs),
  which is a different claim. Booting a crates.io crate under a stated endowment remains the missing
  half and the milestone's own acceptance criterion. It needs a crates.io dependency in a program
  this tree builds, which is a §46 decision rather than a lane's.
- **The census counts call sites, not reachable calls.** A `fs::read_dir` inside a `#[cfg(windows)]`
  block counts. The over-count is roughly uniform across rows, so the *ordering* is trustworthy and
  the absolute numbers are not. The precise version (make each `unsupported()` a distinct undefined
  symbol and let the linker report reachability) needs a farm rebuild per probe and was not worth it
  to reorder a list that would not reorder.
- **`fs::rename` is undercounted, badly.** It sits at rank 25 with two probes, and gitoxide renames
  constantly; it just does it through `tempfile::persist` rather than by naming `fs::rename`. Any
  row here can be wrong in the same direction whenever a crate wraps the call. Treat the ranks as a
  starting order, not a budget.
- **Fifty crates is not crates.io.** They were chosen to span categories and to include milestone
  99's and 66's own leaves, which is a deliberate bias toward the things this project is about to
  need. A random sample would look different and would be less useful.
- **Feature flags change the answer.** `flate2` was probed with `rust_backend`; its default C
  backend would land in class C. `mio` was probed with no features and passes; with any it does not.
  `clap` was probed with `std` only. Where a crate has a pure-Rust option, the probe took it, and
  that is a choice this note is making on the reader's behalf.
- **One version of one nightly.** The pinned toolchain of 2026-08-04 and the crate versions resolved
  that day. `getrandom` in particular is mid-transition across 0.2, 0.3 and 0.4 in one dependency
  graph.

## What this says about milestone 99 (gitoxide) and milestone 66 (Vaultwarden)

**Milestone 99's decision holds, and the measurement strengthens it.** The block says gitoxide's
gaps "are this project's own roadmap rather than a compatibility project". That is what was found:
`gix`'s blockers are a `getrandom` backend, `gix-sec`'s uid lookup, and `tempfile`'s persist, plus
the five fs bindings above. Every one of them is either PAL surface this tree wants anyway or a
small patch under `patches/`. Nothing in gitoxide's tree wants `fork`, and nothing wants a C library.

Three things the block does not currently say, which the measurement adds:

1. **`getrandom` is the first task, not an incidental.** Nothing in `gix` builds without it.
2. **`tempfile` is the second**, and it is invisible to a build-failure list. `gix-lock` cannot
   commit a ref until `tempfile::persist` works, which means `fs::rename` matters far more than its
   rank-25 position says.
3. **`gix-command` and `gix-credentials` call `process::Command`**, for which there is no PAL at
   all. Whether the staged `git init`/`commit`/`log` path reaches them is unknown; if it does, it is
   a design fork (this kernel spawns by capability) rather than a porting task, exactly as the block
   says for C git.

**Milestone 66 is further away than milestone 99, and the measurement says by how much.** Rocket
fails at `socket2` before any Vaultwarden code is reached, `diesel` needs `libsqlite3`, and `ring`
needs a C toolchain for this target. Those are three separate subsystems (a readiness IO model, the
C seam at library scale, and a socket contract with `listen`/`accept`), and none of them is a `std`
gap that widening the PAL would close.
