# Miri over the host crates

Milestone 79. `script/miri` runs `cargo miri test` over the host-testable workspace members, the
same crate selection as `script/test`'s host leg. Weekly in CI (`.github/workflows/miri.yml`) plus
on demand; not part of `script/test` or `script/gates`.

## What Miri checks that nothing else here does

Miri is an interpreter for Rust's mid-level IR that enforces the language's dynamic rules while the
tests run: aliasing (no `&mut` that aliases anything live), pointer provenance (a pointer is only
good for the allocation it was derived from, and an integer-minted pointer only for allocations
whose provenance was exposed), uninitialized reads, use-after-free, out-of-bounds, invalid values,
and leaks at process exit.

The rest of the tree's analysis surface cannot see that class. Kani proves the properties it is
asked about; the fuzzers see crashes and hangs; clippy sees shapes; the type system stops at every
`unsafe` block. There are 224 `unsafe` occurrences under `crates/`, concentrated in `ipc`,
`user_heap`, `intrusive`, `virtio`, and `paging`, and an aliasing bug in one of them passes every
existing gate while being real UB on every target. Miri is the one tool here whose whole job is
that class, and the first full run proved the point: it found two genuine UB defects in
`user_heap`, both invisible to 60 passing native tests.

The complement matters too. Miri only judges the paths the tests execute: it is a dynamic checker,
not a proof, so an `unsafe` block no test reaches is as invisible to it as it was before. Coverage
(`script/coverage`, the 80% floor) is what keeps "the paths the tests execute" an honest
approximation of "the paths".

## The first full run (2026-08-03, nightly-2026-08-03)

Every host-testable workspace member (`--workspace` minus the three bare-metal crates), 41 packages.
After triage, the front door (`script/miri`, one `--workspace` invocation, 95 test binaries) runs
green in 27 minutes of wall time on the dev machine with a warm compile cache.
The sequential per-crate triage sweep took about 31 minutes, of which
five packages are 79% (`xtask` 473 s, `cred` 452 s, `measured_boot` 334 s, `coremark` 190 s, `gpt`
127 s); 30 of the 41 finish in under 10 s. The interpreter tax measured about three orders of
magnitude where it was visible (`ntp_proto`'s 10^9-value sweep: 0.6 s native, a projected day-plus
interpreted; `calendar`'s 315,000 round trips: about a second native, still running at 11 minutes
when it was killed and sampled instead).

### Findings, each with its verdict

**1. `user_heap::insert_free`: write through an invalidated borrow. Real UB, fixed.**
The free-list insertion walked the list through a raw `link` pointer derived from `&mut self.head`,
then took `&mut self.head` a *second* time for the predecessor check. A fresh `&mut` is a fresh
unique borrow: it invalidated the tag `link` carried, and the head-insertion store through `link`
was a write through a dead borrow. Every compiler we ran emitted the store anyway, which is why 60
native tests passed; it was still UB, licensed to break on any toolchain bump, in the allocator
under every userspace program. Fix: take the head link once and compare pointers instead of
re-borrowing. This is the exact class the milestone was run for, found on its first pass.

**2. `user_heap`: integer-minted pointers with no exposed provenance. Real UB, fixed.**
The allocator's arithmetic is integer arithmetic (`alloc` mints a tail block at `aligned + size`;
`insert_free` recovers a predecessor node from a link address), and a coalesced block can span two
separately donated regions, so no single donated pointer could carry provenance for it even in
principle. Rust's rule for that shape is expose-and-reclaim, and the code was using `addr()`, which
deliberately does not expose. The first `alloc` to carve a block coalesced across a donation
boundary was a write with no exposed tag. Fix: `insert_free` now calls `expose_provenance()` on
every incoming range. Same instruction at runtime; the difference is that the optimizer is told the
escape exists.

**3. `paging` test harnesses: leaked frames. Test bug, fixed.**
`domain.rs`'s synthetic-frame pool and `tests/mapping.rs`'s pretend frame allocator both leaked
their 4 KiB tables on purpose ("a test process is about to exit anyway"), 16 and dozens of reports
respectively. Deliberate or not, a suite that fails the leak check on purpose teaches everyone to
ignore the gate. Both harnesses now free what they allocate through a `Drop` guard per test,
declared first so it drops after the mapper that reads the tables.

**4. A `glob` sweep failure in the triage logs: an artifact, not a finding.** One intermediate run
recorded `greedy_agrees_with_exhaustive_search_over_every_short_pattern` failing with
`checked = 43,720`; that run caught the file mid-edit, with the Miri stride applied but the
completeness pin not yet updated. The finished edit passes under Miri and natively.

**Also seen, deliberately left:** integer-to-pointer cast *warnings* in `paging`'s tests, where
`phys_to_ptr` is an identity cast by design (the tests' documented trick is that host addresses
stand in for physical ones); they are warnings about a pattern the harness is honest about, not
errors.

## "Miri-clean" means the sampled paths

An interpreter runs roughly three orders of magnitude slower than the silicon. The exhaustive
suites cannot run under it as-is, and each one gates itself down under `cfg(miri)` with the reason
written next to the test:

| Site | Native | Under Miri |
|---|---|---|
| `ntp_proto` `every_nanosecond_survives_the_round_trip` | all 10^9 nanoseconds, 0.6 s | strided sample (stride 999,983, prime) plus the edges |
| `calendar` `format_and_parse_round_trip_across_the_range` | ~315,000 round trips | ~300, stride widened 1000x |
| `glob` `greedy_agrees_with_exhaustive_search_over_every_short_pattern` | all 2,657,200 pattern/name pairs | every 61st pattern, 43,720 pairs, the completeness pin adjusted to the exact sample |
| `glob` `the_worst_case_over_the_proof_domain_is_what_the_unwind_bounds_are_set_from` | ~1.8M runs, pins the exact argmax | skipped: a sample that misses the argmax fails against correct code |
| `glob` pathological/quadratic bound tests | 100,000- and 2,000-byte names | 2,000 and 200; both assertions still run |
| `gpt` header and entry-array corruption sweeps (`real_disks.rs`; two live, one already `#[ignore]`) | 260k+ parses, each re-CRCing 16 KiB | skipped; the clean-fixture tests walk the same paths |
| `gpt` small-table sweep (`table.rs`) | 261,120 parses | skipped, same reason |
| `cred` store tests via `cheap()` | Argon2id at m=256 KiB, t=2 | Argon2's floor (m=8 KiB, t=1); same paths, fewer blocks. The known-answer vector tests keep their published costs |
| `cred` `an_unknown_identity_costs_what_a_known_one_costs` | 50 timed KDF runs | skipped: a wall-clock ratio under an interpreter measures Miri, not the KDF |

So a green `script/miri` certifies the memory rules on every path the sampled suite executes, and
does not restate the exhaustive claims; those stay native-only, in `script/test`. The skipped `gpt`
sweeps lose nothing Miri-specific: what they add natively is completeness of the CRC argument,
which is not a memory property.

Two test surfaces stay out entirely, deliberately. `tools/redoxfs_host` and `fs_server` are their
own workspaces whose runtime is spent inside the vendored RedoxFS engine, and a finding in vendored
code lands in the vendor pin, not in a crate this tree can fix (vendor/README.md). They keep their
native gates in `script/test`.

## Running it

```
script/miri              # everything, what the weekly workflow runs
script/miri -p gpt       # one crate
script/miri -p glob -- greedy   # any cargo-miri-test args pass through
```

The miri component rides the pinned toolchain: `script/bootstrap` installs it, and `script/miri`
adds it itself on a machine bootstrapped before milestone 79.

## BUGS

- Miri judges only executed paths. An `unsafe` block without a test is not "Miri-clean", it is
  unvisited. The coverage floor is the guard on that gap, and it is a floor, not totality.
- `-Zmiri-strict-provenance` is not on, and for `user_heap` it never can be: the allocator's
  cross-donation coalescing is inherently expose-and-reclaim (finding 2 above). The roadmap block
  named strict provenance as a later ratchet; if it is ever tried, it needs a per-crate carve-out.
- The weekly cadence means a regression can live on `main` for up to a week before CI sees it.
  `script/miri -p <crate>` before pushing `unsafe` changes is a habit, not a gate; nothing
  enforces it.
- Miri interprets the host target (aarch64-apple-darwin locally, aarch64 Linux in CI). Endianness
  and pointer width match the kernel targets today, so nothing is lost, but a finding that depends
  on target-specific layout would be reported against the host's.
- The samples under `cfg(miri)` are hand-maintained twins of the native domains. The `glob`
  completeness pin (43,720) is the honest version of that risk: it is asserted, so a drifted
  sample fails loudly, but most of the other samples have no such pin and would shrink silently.
