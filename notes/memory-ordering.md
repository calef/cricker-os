# Memory ordering, and the fences with no partner

Milestone 116. CLAUDE.md's fourth rule is *assume weak memory ordering*, and this is the inventory
that says where in the tree that rule is actually being relied on, who is relying on it, and which
sites were trusting something that is not there.

**A release fence with no matching acquire orders nothing while reading as though it does.** That is
worse than an absent fence, because the fence is the comment: a reader who meets
`fence(Ordering::Release)` stops asking the question. The mistake was found twice on 2026-08-04 by
two methods that share nothing, milestone 80's loom harness on the clock page's seqlock and
milestone 43's audit on the compositor, and no gate in the tree could see either one.

This note does three things. It counts the population honestly. It adjudicates every site into a
bug, a soundness argument, or dead code. And it answers, with a measurement rather than an opinion,
whether pairing can be checked mechanically.

## The count, and how it was taken

From the merged tree, outside test code:

| | |
|---|---|
| `Ordering::*` occurrences, total | **504** |
| ... in test code | 198 |
| ... outside test code | **306** |
| ... of those, `Relaxed` | 243 |
| ... of those, carrying acquire or release semantics | **63** (27 `Acquire`, 22 `Release`, 14 `SeqCst`) |
| `fence` call sites outside test code | **11** |
| `compiler_fence` call sites outside test code | **1** |
| Files holding at least one non-test site | 45 |

**Two things about that count are worth more than the number.**

The first is that a line-oriented grep gets it wrong, which CLAUDE.md already warns about from the
`#[path]` module count. `rustfmt` splits a long `compare_exchange` across five lines, so the ordering
argument and the atomic it belongs to are rarely on the same line. The scan behind this table blanks
out comments and string literals, then walks backwards from each `Ordering::` to the open paren of
the call that contains it, so a multi-line call is attributed to the right atomic. Checking the
pattern against the real shapes first is the part that matters: prose in this tree says "Release" and
"Acquire" constantly (`kernel/src/sync.rs`'s header, `fs_server`'s "Release a handle") and a naive
grep counts every one of them.

The second is that the test-code exclusion is where the first version of this scan was wrong, and it
was wrong in the direction that hides things. The kernel's tests are separate files pulled in with
`#[cfg(test)] mod disk_tests;`, which has no brace, so a scan that looked for the next `{` after the
attribute swallowed the following 400 lines of live kernel and quietly classified a real fence in
`kernel/src/user.rs` as test code. An attribute followed by a semicolon before any brace marks a
whole file, not a region. Both forms exist in this tree and a scan has to know the difference.

## Where this kernel's ordering actually lives, which is mostly not here

**63 sites is a small number for an SMP kernel on two weakly ordered ISAs, and the reason is
DECISIONS §9.** Almost everything shared is behind a ranked interrupt-safe lock, and
`IrqSafeMutex` wraps `spin::Mutex`, which locks with `compare_exchange(false, true, Acquire,
Relaxed)` and unlocks with `store(false, Release)`. So the overwhelming majority of this kernel's
happens-before edges are supplied by an acquire/release pair **inside a dependency**, where nothing
in this tree names them and no grep over this tree can find them.

That fact runs through every adjudication below, so it is worth stating as a rule:

> **An ordering edge in nife comes from one of four places, and only the first is greppable.**
>
> 1. **An explicit fence or an ordered atomic**, in this tree. 63 sites plus 12 fences.
> 2. **The `SCHED` lock**, taken by every `SEND`, `RECV`, `CALL` and `REPLY`. A blocking IPC
>    rendezvous therefore orders everything the sender wrote before it against everything the
>    receiver reads after it, at no cost and with nothing written down.
> 3. **A spawn.** A page written before the process that reads it existed is ordered by the act of
>    creating that process.
> 4. **Nothing at all**, because the writer and the reader are the same core with interrupts masked.

Point 2 is the one that changes the shape of this inventory. DECISIONS §10 says control by message
and bulk by shared page, and in practice **every shared-page publish in this tree except one is
immediately followed by a blocking `CALL` or a `REPLY` to the process that reads it.** So the fences
guarding those publishes are, with one exception, redundant rather than load-bearing. They are not
wrong and they are not being removed; the point is that a reader cannot tell which kind a given
fence is, and until this milestone nothing said.

**The exception is the clock page**, which is the only cross-address-space protocol with no
rendezvous underneath it at all: a process reads the wall clock out of a shared mapping with no
syscall. That is exactly where milestone 80's loom harness found a real bug, and it is not a
coincidence.

## The fences, all twelve, adjudicated

Each of these now carries a `PAIR:` comment at the site naming where its other half is. The gate in
`script/lint` requires it; see below.

| Site | Side | Partner | Adjudication |
|---|---|---|---|
| `crates/clock_proto` `read` | acquire | the writer's `W_SEQ` release store in `publish` | **Sound as far as it goes, and the writer's opening fence is milestone 80's bug.** The only protocol where both halves must be in this tree |
| `crates/cred_proto` `wipe` | neither | none, and none wanted | **Sound.** A `compiler_fence` emits no instruction and orders nothing between cores. Now says so |
| `kernel/src/arch/aarch64/exceptions.rs` `last_user_fault` | acquire | `USER_FAULTS.fetch_add(1, Release)` in `user_fault` | **Sound, and the model for the tree.** Both halves present, both load-bearing, both explained at the site before this milestone |
| `kernel/src/arch/riscv64/exceptions.rs` `last_user_fault` | acquire | the same pair on the other ISA | **Sound.** Parity holds |
| `kernel/src/user.rs` `term_print` | release | none; the `ipc_call` below it is the edge | **Sound, redundant.** The terminal is blocked in `recv_cap` |
| `kernel/src/user/keyboard_service.rs` `take_typed` | acquire | `ring_publish`'s fence in `user/src/kbd.rs` | **Sound.** The reader milestone 43 named as getting it right |
| `kernel/src/user/compositor_service.rs` `type_bytes` | release | `drain_input` in `user/src/compositor.rs` | **Sound, redundant** (the doorbell `CALL` follows). **A fourth writer the audit's count of three missed**; see below |
| `user/src/kbd.rs` `ring_publish` | release | two readers, one fenced and one not | **Sound, redundant.** `call(DOORBELL, ...)` follows immediately |
| `user/src/window.rs` `commit` | release | `serve_frame` in `user/src/compositor.rs` | **The one that is load-bearing.** See below |
| `user/src/display_terminal.rs` `present`, first | release | the display driver's `barrier()`, or `serve_frame` | **Sound, redundant** on the display path |
| `user/src/display_terminal.rs` `present`, second | release | `serve_frame` | **Sound**, by the reply this process is about to send |
| `user/src/compositor.rs` `flush` | release | `barrier()` in `user/src/display.rs` | **Sound.** The `CALL` orders the driver's read; the fence covers the driver-to-device leg |

### The one publish the rendezvous does not cover

Milestone 43's audit called finding 7 reachable "on real weakly ordered hardware", which is true and
which this inventory can now make specific. The path is not the obvious one.

The compositor's doorbell is **shared**, and `serve_frame` rescans *every* client's control page on a
`COMMIT` from *anyone*. Only the caller is blocked. So when window A commits, the compositor reads
window B's sequence and B's four damage fields while B is running, possibly mid-`commit` on another
core. B's release fence orders B's own stores correctly; nothing orders the compositor's loads, so it
may take the new sequence beside the previous frame's rectangle. There is no rendezvous between B and
the compositor to fall back on.

Every other producer in that subsystem is covered by its own `CALL` or by the reply it is about to
send, including the keystroke path in `display_terminal`, where `ring` is deliberately false and it
looks at first reading as though nothing follows. Something does: the compositor is blocked in a
`CALL` *to* the terminal, and the terminal's reply is the edge.

So milestone 43's two acquire fences are the right fix, and the reason they are needed is the shared
doorbell rather than the fence asymmetry that led to them.

### And the audit's count was three, where the tree has four

`user/src/window.rs`, `user/src/display_terminal.rs` and `user/src/kbd.rs` are the three the audit
named. The fourth is `kernel/src/user/compositor_service.rs`'s `type_bytes`, the kernel playing the
input driver, which publishes into the same ring with the same fence and the same comment. The audit
looked at that file and named its *reader* (`keyboard_service`'s `take_typed`) as the one that gets
it right, so the writer beside it went past.

**The fix is unaffected**, because `drain_input` is the single reader of both ring producers, and one
acquire fence there covers them. Recorded because the count is wrong in the audit and a later reader
would try to reconcile three against four.

## The 63 ordered atomics, by protocol

| Protocol | Where | Adjudication |
|---|---|---|
| Clock page seqlock | `crates/clock_proto` | Writer's opening fence missing. **Milestone 80's finding**; fixed on its branch, loom-checked |
| Work-steal request slot | `kernel/src/sched.rs`, `crates/steal_request` | Release CAS to claim, acquire swap to take. **Loom-checked** by milestone 80, six harnesses |
| User fault record | `kernel/src/arch/*/exceptions.rs` | Relaxed record, release counter, acquire fence on the read. Correct, on both ISAs |
| Boot roster | `kernel/src/smp.rs` | `ROSTER`, `DESCRIBED`, `ONLINE`, `ONLINE_MASK`: relaxed arrays under a release flag, acquire flag then relaxed arrays. Textbook array publication, single-shot at boot |
| IRQ routing table | `kernel/src/sched.rs` `IRQ_ROUTES` | Release store, acquire load, same array. Paired |
| One-shot service wiring | `fs_service`, `entropy_service`, `disk_service`, `credential_service` | **Four instances of one correct idiom**: relaxed fields, then a release flag; readers acquire the flag, then read the fields relaxed |
| Spin locks | `crates/user_rt`, `patches/std-nife` (3), `fs_server/src/bin/second_mount.rs` | Acquire CAS, release store. **The one shape that cannot be one-sided**, because the lock is both halves |
| Benchmark start barrier | `kernel/src/bench.rs` `TP_GO` | Release store, acquire spin. Paired. The `SeqCst` reset is over-strong and has no reader yet, so it orders nothing and costs nothing |
| Secret wipe | `crates/cred_proto` | `compiler_fence`, no cross-core meaning, no partner wanted |

### The two sites that are sound for a reason that was not written down

**`HWID` in `kernel/src/smp.rs` is read with `Acquire` and written with `Relaxed`.** An acquire whose
release does not exist, which is this milestone's bug class in mirror image. It is sound: nothing is
published behind `HWID`, the publication flag for the whole roster is `ROSTER`, and a caller reaches
a valid slot index only by having read `ROSTER` with an acquire first. The acquire on `HWID` is
decorative. The tell is that the same array is read with `Relaxed` thirty lines further down, so the
file already disagrees with itself about whether that ordering means anything. **Left as it is**,
because this milestone changes no ordering that is not shown wrong, and an unnecessary `Acquire`
removed is still an ordering change with no argument behind it.

**`user/src/compositor.rs`'s `publish` had a comment that was wrong**, and this is the one place the
inventory corrected the record rather than the code. It writes six control-page fields with plain
`write_volatile` and then writes `MAGIC` last, under a comment saying "the store order is what makes
the check mean anything". It is not. `write_volatile` guarantees that the access happens and
guarantees no ordering at all; on aarch64 the interconnect may make `MAGIC` visible before the fields
above it. What makes the check sound is that `publish` runs once, before the serve loop, so no
client's `HELLO` can be answered until every page is written. **Both client programs say exactly that
at their end and this end did not**, which is the same one-sided shape as a fence: the argument
existed and lived somewhere the reader was not. The comment now carries it, and says what would have
to change if `publish` were ever called again while clients run.

### The window list, which no reader has yet

`publish` writes `wlist::COUNT`, then `FOCUSED`, then the per-window `RECORDS`. If `COUNT` were ever
used as the publication flag for the records it counts, that order is backwards. **It is not used
that way today, because nothing in the tree reads `RECORDS` at all**: `user/src/window.rs` reads
`COUNT` and `FOCUSED` and reports them, and the kernel reads `FOCUSED`. Recorded here rather than
fixed, because the first reader of `RECORDS` is the change that makes it matter and that reader
should meet this paragraph.

### Relaxed, and the one question worth asking of it

The 243 `Relaxed` sites were not adjudicated individually; the brief scoped this milestone to the
sites that carry ordering. One sub-question was worth asking anyway, because a relaxed
compare-exchange used as a claim is the classic form of this bug: **fifteen compare-exchange sites
outside test code, and at twelve of them the `Relaxed` is the failure ordering**, which is
conventional and right. The three that are relaxed on success publish nothing behind the value:
`kernel/src/sched.rs`'s thread-budget reservation, `kernel/src/pci.rs`'s BAR cursor, and the
interrupt-routing lottery in `kernel/src/arch/*/irq.rs`, where the loser of the race reads the
winner's answer out of the compare-exchange's own failure return. In all three the value **is** the
whole protocol.

## Can pairing be checked mechanically? Measured, not argued

**Broadly, no.** The obvious check is per variable: for every atomic touched with `Acquire`, require
a `Release` somewhere on the same atomic. It was built and run against the tree before this note was
written, and here is what it did on a population of 63:

**Seven flags, six of them correct code.** It flagged all five spin locks, because a lock pairs a
compare-exchange with a store and the receiver expression (`self.0.locked`, `REG.locked`) is not a
stable identifier. It flagged the `USER_FAULTS` protocol on both ISAs, which is the *best* pair in
the tree, because its acquire side is a `fence` and not a load on that variable. It missed `HWID`,
the one genuine finding, because `HWID.get(id)?.load(...)` does not textually name the same thing
`HWID[i].store(...)` does. A check that fires on the exemplary case and stays quiet on the defective
one is not a weak check, it is an anti-check, and shipping it would have taught every future reader
to add an allow-list entry without looking.

That is the measurement. The reason underneath it is structural and does not improve with effort:

- **The partner is frequently not an ordering primitive at all.** For most of this tree it is a
  blocking IPC rendezvous whose happens-before edge lives in `spin::Mutex`, in a dependency. No AST
  in this repository contains it.
- **The two halves are in different programs**, often different crates, sometimes different languages
  (`user/c/c_seam.c`). Cross-binary dataflow.
- **The defect is an absence.** Finding 7 was a reader with no fence anywhere. There is no token to
  match on, which is why a grep-shaped tool cannot find the exact bug this milestone is named for.

**Narrowly, yes, and it is worth having.** Milestone 112 faced the same question about SAFETY
comments and shipped a narrow check that is true plus a recorded limitation, rather than a broad one
that only looks like a gate. Same posture here:

> **Every `fence` outside test code must carry a `PAIR:` comment naming where its other half is.**

`script/lint` enforces it, over the contiguous comment block immediately above the fence, which is
where `SAFETY:` already lives. Twelve sites today.

**It checks the bookkeeping, not the ordering, and that limitation is the design.** What it buys is
the forcing function. An author made to write down where the partner lives has to go and look for it,
and looking is precisely the step neither of the two authors on 2026-08-04 took: both wrote a fence,
both wrote a true comment about what their own side does, and neither opened the other file. Writing
`PAIR: serve_frame in user/src/compositor.rs` requires opening `serve_frame`, at which point the
absence is visible. Four of the twelve markers in this tree changed what their author believed while
being written.

**Fences only, not all 63 ordered atomics**, and the line is principled rather than a budget. A
`Release` store names its variable, so its partner is one grep away and a reader can find it unaided.
`fence()` names nothing at all. It is the one construct in the language whose meaning is entirely
about code somewhere else, which makes it the site where the partner has to be written by hand.

## What actually wants a loom harness

`script/interleaving-check` (milestone 80) is the only tool in the tree that can *decide* a specific
protocol, so the useful output of an inventory is a queue for it rather than a gate. In priority
order, with the reason each earns a harness:

1. **The compositor's control page.** The one protocol above with a real reader that is not covered
   by a rendezvous, and the one where a reachable bug was found by reading. It needs the reader and
   two concurrent writers, which is the shape loom is good at. Blocked on nothing except that the
   logic lives in `user/src/compositor.rs`, a `no_std` binary, so `serve_frame`'s page reads have to
   come out into `crates/compositor` first. That is rule 7 pushing in the direction it always pushes.
2. **The input ring**, `crates/compositor`'s `ring`. Two producers (`user/src/kbd.rs` and the kernel)
   and two consumers (`drain_input` and `take_typed`) over one head/tail contract, with only one of
   the four consumers' sides fenced before milestone 43. A four-party contract with an asymmetry in
   it is worth a model even when the answer is "sound".
3. **The one-shot wiring flag**, the idiom repeated in four services. A single harness over one
   extracted helper would cover all four and would turn "the same four lines, four times" into a
   checked property instead of a pattern someone recognises.

Not worth one: the spin locks (the lock is both halves), the boot roster (single-shot, single
writer), `TP_GO` (nothing published behind it), and anything under `arch/`, where rule 1 keeps the
code and lifting it is a larger question than a harness.

## EXAMPLES

Run the gate:

```
$ script/lint
...
==> every fence names its counterpart
fences: 12 outside test code, each naming its counterpart
```

See it fail, which is worth doing once before trusting it. Delete the `PAIR:` line above any fence:

```
$ script/lint
lint: fences with no PAIR: comment naming the other half:
  user/src/kbd.rs:194: core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
```

Take the count yourself, without trusting the table above. The multi-line trap is real, so compare
the two:

```
$ git grep -c 'Ordering::' -- '*.rs' | awk -F: '{n+=$2} END {print n}'      # lines, not sites
$ git grep -o 'Ordering::[A-Za-z]*' -- '*.rs' | wc -l                       # sites
```

Write a marker for a new fence. The form is where, not why, because the "why" is already the comment
above it:

```rust
// The bytes must be visible before the tail that advertises them.
//
// PAIR: `take_typed` in kernel/src/user/keyboard_service.rs has the matching fence.
core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
```

If there is no matching fence, say what the edge actually is and name it:

```rust
// PAIR: no acquire fence, and none is needed. The terminal is blocked in `recv_cap` and the
// `ipc_call` below is what wakes it, so the kernel's release of the `SCHED` lock and the
// terminal's acquire of it are the pair.
```

## BUGS

- **The gate checks that a comment exists, not that it is true.** A `PAIR:` marker naming a function
  that does not exist, or naming the wrong one, passes. It is bookkeeping with a forcing function
  attached, and the paragraph above says so for the same reason this one does: the failure mode of a
  gate people over-trust is worse than the failure mode of no gate.
- **The 243 `Relaxed` sites are not adjudicated.** Only the compare-exchanges were checked, on the
  grounds that a relaxed claim is the classic form. A relaxed store used as a publication flag with
  data behind it would be the same bug as this milestone's and is not covered here.
- **Nothing here is a proof about aarch64 or riscv64.** Every soundness argument in this note is a
  C11-model argument about happens-before, and so is loom's. `notes/interleaving.md` (milestone 80)
  says the same thing three times and it is worth repeating a fourth: a failure either tool reports
  is real, a clean result is not a proof about the silicon.
- **The rendezvous argument rests on `spin::Mutex`, which is a dependency.** If `IrqSafeMutex` were
  ever reimplemented, or if `spin` weakened its orderings, roughly half the adjudications in this
  note would need rereading and nothing would fail first. This is the largest unpinned assumption
  here.
- **The 63-site figure is a count of *syntax*, not of protocols.** Nine protocols cover all of it,
  and a tenth could be added tomorrow in `Relaxed` and never appear in this table.
- **Two of the three findings this note describes are fixed elsewhere.** The clock seqlock's writer
  fence is milestone 80's, and the compositor's two acquire fences are milestone 43's. This milestone
  changed no ordering at all, which was its scope note and is also the honest result: the inventory
  found one wrong comment and one decorative `Acquire`, and no new bug.
- **`crates/user_rt`'s spin lock and the interrupt-routing lottery cannot be modelled today**, for
  the reasons milestone 80 recorded: `user_rt` is aarch64 inline `asm!` and does not compile for the
  host, and the lottery lives under `arch/`.

---

*See also `notes/locking.md` for the ranked-lock discipline that makes this population small,
`notes/deadlock.md` for the other half of that discipline, `notes/interleaving.md` (milestone 80) for
the loom harnesses that can decide a protocol, `notes/shared-page-audit.md` (milestone 43) for the
audit whose finding 7 is half of this milestone's motivation, and `notes/compositor.md` for the
subsystem most of the fences are in.*
