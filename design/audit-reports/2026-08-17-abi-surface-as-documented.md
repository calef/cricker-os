# Documentation sweep, 2026-08-17: the ABI surface as documented

**Kind:** documentation. **Lens:** the ABI surface as documented, read from the wire outward: every
constant two programs must agree on, checked against what the prose says the surface is.
**Findings:** fixed 5, minted 1, accepted 2.

## Why this lens

`script/audits` fired on the strongest trigger the table has, and it is an event rather than a
threshold: **ABI constants 43 -> 50, and any change fires** (§74, event triggers first). The lens
follows the trigger. Documentation describing a syscall surface that grew seven constants yesterday
is the documentation most likely to be lying today.

It is also the lens the rotation asked for. `design/audit-reports/README.md`'s "Running one" lists
the lenses not yet taken as supply chain, userspace confinement, and **the syscall surface itself**,
and the previous sweep's "what was deliberately not examined" does not mention the ABI at all.

**The seven, re-derived rather than taken on trust**, by diffing the `pub const NAME: u64` surface of
`crates/abi` against the commit that landed the last report (`e4d76715`):

```
SURVEY  ENUMERATE  DONE  READY  RUNNING  BLOCKED  DEAD
```

All seven are milestone 126's, `endpoint::SURVEY` plus `rights::ENUMERATE` plus the five `survey`
state codes. **This lane was briefed that the seven also included `fs_proto::fs::STATFS` and
milestone 40's manual-index constants, and they are not in it**: the baseline counts `crates/abi`
only, and both of those live in other crates (and are not `u64`). Recorded because it changed where
the sharpest read was: the trigger is entirely about **supervision, enumeration, and a new right**,
not about the filesystem protocol. Both of the constants named in error were checked anyway and are
in the findings below or in the clean list.

## Scope

`script/audits --worklist` was taken and then set aside for this sweep, deliberately, and that is
itself a finding about the mechanism (see "What the mechanism learned"). It ranks by how much cited
code has moved, and **not one of the top twenty documents was an ABI document.** The lens came from
the trigger instead, and the scope was chosen by asking which files describe the surface that moved:

| document | why it is in scope |
|---|---|
| `crates/abi/src/lib.rs` | the boundary artifact itself, and the thing the trigger counted |
| `kernel/src/syscall.rs` | the kernel's half of the same boundary |
| `notes/capabilities.md` | the note that explains the surface to a newcomer |
| `notes/capability-lifecycle.md` | the note that explains rights, which is what `ENUMERATE` changed |
| `crates/capability/src/lib.rs` | where `Rights` is defined and where its argument is made |

Plus one whole-tree class each for the two stale counts, which is the step the first sweep named as
the one that turns a typo fix into a mechanism: **ask whether the class is bigger than the
instance.** It was, both times.

### What came back clean, which is a result

- **`notes/abi.md` is correct and current.** It says "Four syscall numbers, and that is the whole
  width of the trap" and its table lists `SYS_CAP_DELETE`. `notes/README.md` agrees, at "one `svc`
  and four syscall numbers". **This is the opposite of the direction anyone expects**: the browsable
  prose a newcomer reads was right, and the doc comment inside the boundary artifact was three weeks
  stale. A sweep that had audited only `notes/` would have found nothing and reported the ABI clean.
- **`crates/capability`'s `Rights` is documented well and `ALL` was widened correctly.**
  `Rights::ALL` is `0b1111`, and its comment already warns that a constant defined without widening
  that mask is silently dropped at every delegation. Milestone 126 got this right. There was a real
  correctness question here and the answer was no.
- **`fs_proto`'s `STATFS` left no rot**, because the crate makes it impossible: `verb::FIRST`/`LAST`
  bound the contract, `TABLE` is `[Verb; LAST - FIRST + 1]`, and adding an opcode without a row is a
  compile error. That is rung one of the ladder and it is why op 18 needed no documentation chase.
- **`endpoint::SURVEY`'s own documentation is unusually complete**, including the refusal semantics,
  the cursor contract and the snapshot caveat. Nothing to correct.
- **No riscv64 parity gap on either finding.** The "three calls" comment existed only on the aarch64
  `svc` arm; `kernel/src/arch/riscv64/exceptions.rs` never stated a count.

### What was deliberately not examined

- **Everything that is not the syscall boundary.** This is a narrow sweep by design. 269 documents
  are in the worklist's scope; this read five files and two whole-tree classes.
- **`design/decisions/` and `DECISIONS.md`.** Out of scope for a lane's edits. One finding there is
  handed off below.
- **The `fs_proto` wire protocol beyond `STATFS`**, the `survey` state codes against the scheduler's
  actual states, and every `*_proto` crate other than `fs_proto`. A protocol-by-protocol read is a
  lane of its own and is proposed as one.
- **Whether the ABI's prose is *good*, as opposed to true.** `abi`'s own header records that the
  crate's name is unrecorded; that is milestone 115's business, not this sweep's.
- **Prose asserting the system lacks something it now has.** Milestone 117, the stranger test.
- **Anything requiring the emulator.** No claim here was checked by booting; every one was checked
  against source.

## Findings

### 1. FIXED, and CONVERTED: the syscall surface is four calls, and five places said three

`SYS_CAP_DELETE` landed **2026-07-24** with milestone 19d. The ABI crate's front-page heading has not
changed since **2026-07-14**. So the boundary artifact understated its own width for 24 days, and the
claim had spread to five sites:

| site | said |
|---|---|
| `crates/abi/src/lib.rs:7` | "# The surface is three calls, and that is deliberate", and a `text` block omitting `cap_delete` |
| `crates/abi/src/lib.rs` (`Error::BadSyscall`) | "The syscall number is not one of the three" |
| `kernel/src/syscall.rs:1` | "The syscall boundary. **Three calls.**", same omission in its block |
| `kernel/src/arch/aarch64/exceptions.rs:370` | "It is three calls. See syscall.rs." |
| `notes/capabilities.md:226` | "## The surface is three calls" |

**`Error::BadSyscall`'s is the one that is not arguable.** The others can be read as a slogan about
shape; that one describes what the kernel does with a register value, and passing 3 is valid. An
error-code description in the ABI crate misdescribing the ABI is the sharpest form this class takes.

All five fixed. The rewrite keeps the argument rather than just incrementing the number, because the
argument survives intact and is better for it: **three of the four calls are authority over
yourself, and the fourth is everything else.** That is what the crate's own `SYS_CAP_DELETE` comment
already said ("like `exit` and `yield` it is a bare syscall"), so the corrected header is more
coherent than the original, not merely more current.

`notes/capabilities.md` was handled differently, because its claim sits inside a section titled
"# Milestone 7d: the syscall surface". **Three was what 7d landed, and history is not rot** (the
first sweep's finding 5). The heading was retitled to say so and a dated paragraph added for what the
surface is today, so the number stays true as history and a reader cannot leave with a wrong
present-tense belief.

**Left as history, deliberately:** `crates/abi/src/lib.rs:56` ("milestone 7d's first three
syscalls", correct), `design/roadmap/07-user-mode.md:20` and `notes/README.md:180` (both "**7d**:
three syscalls", correct as changelog entries).

### 2. FIXED: `abi::rights` had no documentation, and `objtype`'s claimed to be the rights bits

The three lines documenting the rights module have been attached to `objtype` since **milestone
19a**, which inserted that module directly beneath them. Two consequences, and the second is worse
than the first: the module that defines the rights bits had **no module documentation at all**, and
`objtype`'s rustdoc opened by telling the reader it was the rights bits before contradicting itself
in the next sentence.

Found by reading the module the trigger pointed at. **`rights` is exactly where `ENUMERATE` landed
yesterday**, so the ABI's newest constant went into the one module in the crate with no doc comment,
and neither the author nor the reviewer would have seen anything missing, because rustdoc showed
prose about rights right above it, on the wrong item.

Fixed by splitting them back apart, and the restored text now carries the count and the `ALL`-mask
hazard. This is a documentation bug that a structural gate cannot see by construction: the file
parses, every line is a valid doc comment, and the only thing wrong is which item it is attached to.

### 3. FIXED: `notes/capability-lifecycle.md` said rights are three bits

"`Rights` are three bits: `READ`, `WRITE`, `GRANT`." `ENUMERATE` made that four on 2026-08-17, and
this line said three the next day. This is the note a reader goes to for how authority narrows, which
is precisely what a new right changes, so it is the highest-consequence instance of the class.

The same note's "Two independent narrowings" table 25 lines below enumerated the same set and was
also two-thirds of the way there. A row was added for the question `ENUMERATE` introduces (*what may
the holder learn?*) and the heading no longer counts the rows, because the count is the part that
rots and the independence is the part that matters.

### 4. FIXED: two tests claimed to pin the rights wire format and pinned three of four bits

`abi`'s `rights_are_distinct_single_bits` and `capability`'s `rights_bits_are_the_wire_format` both
assert `[READ, WRITE, GRANT] == [1, 2, 4]`, and both carry doc comments saying that **each** right
must be a distinct single bit because the values are load-bearing on both sides of the boundary. The
newest right was the one nothing held down. Both now cover `ENUMERATE`.

`capability`'s got one assertion more than the audit strictly required, and it is the useful one:
`Rights::ALL` is now checked to be exactly the union of the four named bits, spelled out by hand.
`ALL`'s own comment warns that a constant missing from that mask is **silently dropped at every
delegation**, and nothing checked it. Milestone 126 got it right; the next one now cannot get it
wrong quietly.

**A test that enumerates is a claim about a set, and it rots exactly like prose does.** That is worth
stating as its own lesson, because a sweep looking only at `.md` files would never have opened either.

### 5. FIXED: `notes/interleaving.md` had both of its harness numbers wrong

"Sixteen harnesses across three crates." Measured: **19 across 4**. The note already contradicted
itself, its timing table three hundred lines down saying "all four crates" and naming
`canary_gate`'s three.

**This one arrived through the conversion rather than through the lens, and that is why it is here.**
`notes/counted-claims.md` cited this exact sentence as the claim a `<!--count:NAME-->` marker could
never reach, because milestone 125's marker read digits and nothing else. Teaching it words meant
this sentence became markable; marking it meant measuring it; measuring it found it wrong twice.
Both numbers now carry markers.

### 6. MINTED: milestone 120's rename remainder reaches the wire formats

The last sweep minted a milestone for the rename's **environment-variable** remainder. The same
rename has an unfinished remainder in the namespace that matters most, and this lane found it while
enumerating constants two programs agree on:

| constant | value | documented? |
|---|---|---|
| `crates/nifefs/src/lib.rs` `MAGIC` | `CRKR0002` | **yes, correctly**, in the crate's own on-disk layout block |
| `crates/manual/src/index.rs` `MAGIC` | `CRKRMAN1` | **no prose anywhere names it** |

**This is a mint rather than a fix for two reasons, and neither is effort.** A magic number is the
irreversible category AGENTS.md names: an on-disk format two programs agree on, where the code is a
morning and the un-shipping is not. And it is a naming decision, which is calef's.

The documentation half is the honest part of the finding. `nifefs` is the `CRICKER_CC` shape the last
sweep recorded: **the document is right and the code is what is unfinished**, so there is nothing to
fix in prose. `crates/manual`'s index format is not described in prose at all, which is a
documentation *gap* and therefore milestone 40's rather than a sweep's; it is named here only because
a reader who goes looking for the format will meet the magic first.

Proposed severity: low, and the reason to do it at all is that a format renamed later costs an image
migration where a format renamed now costs a constant. Every `nifefs` image regenerates from its
crate today.

The remaining `CRK*` strings (`CRK47-*`, `CRK37-*`, `CRK57`, `CRKWRIT1`) were checked and are **test
fixture payloads**, not formats. Renaming them would be churn with no reader, and they are listed
here so the next person does not have to re-derive that.

### 7. ACCEPTED: `design/decisions/16-object-revocation.md` carries the stale count

Its heading reads "Two new methods on the Untyped object (the surface stays three syscalls)". A lane
does not edit `design/decisions/`, so this is handed to the integrator. It is also the mildest
instance: a dated record of an argument, whose point (methods were added, not syscalls) is still
exactly right, and whose parenthetical was true when written. Recorded rather than urgent.

`notes/object-revocation.md:179` says the same thing in a note this lane could have edited, and it
was **left alone on purpose**: it is milestone 16's narrative, the parenthetical's argument is
correct, and the sweep had already added a forward pointer in `notes/capabilities.md`, which is where
a reader looking for the surface actually lands. Fixing every historical mention of a superseded
count is how a sweep turns into a rename.

### 8. ACCEPTED: `design/roadmap/61-caretakers.md`'s verb count is right today by luck

"`subtree` and `nameset` are identical at 18 verbs." The fs contract carries 18 verbs today
(`OPEN`..`STATFS`), so the sentence reads true, and it was written when the contract had 17. It is
accepted rather than fixed because this lane could not establish, without reading both caretakers
properly, whether the claim is about the whole contract or about the subset each proxies, and **a
number corrected to a different wrong number is worse than one left alone**. It wants the
protocol-surface lane proposed below, which will have both files open anyway. Recorded here so the
next reader knows the sentence has not been verified rather than assuming a clean pass covered it.

## The class converted into a check

Per the procedure's part 3. **The class is a prose claim about the size of an ABI set, spelled as an
English word**, and the conversion is not a new gate: it is milestone 125's marker, widened.

`notes/counted-claims.md`'s `BUGS` already recorded the blind spot in as many words: *"A count
spelled in words is invisible. `notes/interleaving.md` says 'sixteen harnesses across three crates'
and no marker can help it. Digits or nothing."* Every claim this sweep found was spelled as a word,
which is not a coincidence: **a small count in prose about a design gets written as a word, and
design prose is where the expensive claims live.** So the recorded limitation was not cosmetic, it
was aimed at exactly the class an ABI lens produces.

What landed in `script/lint`'s `==> counted claims` block:

- **The marker reads a cardinal spelled as a word**, up to `twenty` plus the round tens and
  `hundred`. No composition: `twenty-one` fails loudly rather than passing, and the note says why.
- **Four new registry entries.** `syscalls` and `rights-bits` are the two ABI sets this sweep found
  stale prose about; `loom-harnesses` and `loom-crates` are the pair the note asked for by name.
- **Markers now blank each other** before the number search, so a count name containing a numeral
  cannot donate it to the next claim on the same line. Nothing in the tree needed that yet.
- **Eight markers placed**, four of them in `.rs` doc comments, which is new for this mechanism and
  is recorded as a limitation because the fenced-block exemption is implemented for `.md` only.

The gate went from 4 counts to 8 and from 12 markers to 20. **It was verified by breaking it**: with
the ABI header put back to "three calls" it fails with `crates/abi/src/lib.rs:7: says 3, the tree
says 4`, naming the file, the claim and the question the derivation answers.

Why this rather than a bespoke ABI check: the elegant option is the one with fewer moving parts, and
one registry with a wider regex beats a second gate that counts the same kind of thing. It also
means the next word-spelled count anywhere in the tree is markable, not just this one's.

**The ratchet is unchanged, which is the property that made this safe.** Only a *marked* number is
ever read, so admitting words cannot make the gate fire on an ordinary sentence that says "the three
of them". A gate that had tried to find unmarked word-counts would have been all false positives; the
first sweep's finding 4 is about exactly that failure.

## What the mechanism learned about itself

Three things, recorded so the third sweep does not rediscover them. The first is the one worth acting
on.

1. **The staleness worklist could not have found this sweep's findings, and a lens taken from the
   trigger could.** Not one ABI document appeared in the worklist's top twenty. The reason is
   structural rather than a bug: the worklist ranks a document by how much of the code it *cites* has
   moved, and `crates/abi/src/lib.rs` is not a document that cites code, it **is** the code. Its doc
   comments are the most-depended-on prose in the tree and they are invisible to the heuristic
   entirely, along with every other doc comment. The procedure currently presents the worklist as
   "the lens question's starting answer"; on this evidence it is the starting answer for a sweep of
   `notes/`, and **the trigger is the better starting answer when the trigger is an event**. Both
   sweeps so far confirm it from opposite directions: the first was scoped by the worklist and found
   name and number rot in notes, this one ignored it and found rot in the boundary artifact.

2. **The most valuable finding was again not a wrong fact.** The first sweep's lesson was that a
   rotted *justification* beats a rotted fact. This sweep's equivalent is finding 2: a correct doc
   comment attached to the wrong item, which no gate can see and which makes the reader's experience
   worse than a missing comment would, because `objtype` actively asserts something false about
   itself. **Read what a doc comment is attached to, not only what it says.**

3. **Code is documentation, and a sweep that reads only `.md` will keep missing the sharpest
   claims.** Five of this sweep's six fixes are in `.rs` files, including two test comments that made
   a claim their assertions no longer honoured. `notes/documentation-audit.md`'s procedure says
   "documents"; the corpus that matters is prose, wherever it lives.

All three are added to `notes/documentation-audit.md`'s `BUGS`.
