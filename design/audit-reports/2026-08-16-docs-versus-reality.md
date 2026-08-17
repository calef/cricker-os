# Documentation sweep, 2026-08-16: names and numbers the tree moved past

**Kind:** documentation. **Lens:** docs versus reality, scoped by the staleness worklist, read for
names and numbers a reader would act on. **Findings:** fixed 4, minted 1, accepted 2.

The first sweep run under milestone 93's mechanism, and the first report to land in this directory
rather than in `notes/`. It ran in the same lane that built the mechanism, deliberately: a cadence
whose first run has never happened is a plan rather than a mechanism, and everything the procedure in
[notes/documentation-audit.md](../../notes/documentation-audit.md) claims about running one is
claimed because this run did it.

## Scope

`script/audits --worklist` ranks every document in `notes/`, `design/roadmap/` and the repository
root by how many of the files it cites have moved since the document itself was last edited. 87 rank
at all as this report lands, out of 269 in scope; it was higher before the sweep, because editing a
document resets its own clock. The scope taken from the top of that list:

| document | moved/cited | last edited |
|---|---|---|
| `notes/glyphs.md` | 15/16 | 2026-08-03 |
| `notes/framebuffer-contract.md` | 11/12 | 2026-08-03 |
| `notes/compositor.md` | 8/8 | 2026-08-04 |
| `notes/verification.md` | 4/18 | 2026-08-15 |

Plus two whole-tree passes that the first two documents provoked, which is the pattern worth noticing
rather than the finding: **a stale name met in one document is almost never alone.** Both passes are
findings 1 and 2 below.

`notes/shared-page-audit.md` was the highest-ranked document and was **skipped on purpose**. It is a
dated security-audit report, and a report is evidence about a moment rather than a description of the
tree; code moving under it does not make it wrong. The same reasoning excludes `design/decisions/`
from the worklist entirely.

### Two of the four produced no findings, which is a result

`notes/compositor.md` and `notes/verification.md` were read for the same five classes and came back
clean. Every path in `compositor.md`'s "where the pieces are" table resolves, its numbers re-derive,
and its one tense claim (input delivery is still the one place a blocking `CALL` reaches into a
client, DECISIONS §33) is still true. `notes/verification.md` is the file that supplied one of this
milestone's four founding exhibits, "the proof suite runs in a few minutes", and **that claim is
already gone**: the note now carries a dated timing table instead. Somebody fixed it between
2026-08-03 and this sweep, without the sweep existing.

Recording that matters more than it looks. A mechanism that only ever reports findings teaches its
readers that a clean pass means nobody looked.

### What was deliberately not examined

- **Everything outside those four documents and the two whole-tree classes.** 269 documents are in
  the worklist's scope and 87 rank at all. This sweep read four.
- **`design/decisions/` and `DECISIONS.md`.** Out of scope for a lane's edits, and out of the
  worklist's scope. Four stale environment names there are finding 1's handed-off half.
- **Prose asserting the system lacks something it now has.** That is milestone 117's, the stranger
  test, and it needs a reader with no context rather than one with it.
- **Whether any document is *missing*.** Milestones 68, 40 and 91 own that. This audits prose against
  reality; it does not write prose.
- **The correctness of the arguments in the four documents**, as opposed to the facts they rest on.
  `notes/glyphs.md`'s libghostty-vt recommendation was read for stale facts and not re-costed.

## Findings

### 1. FIXED (partly), MINTED (partly): thirteen environment variables in prose that name nothing

Milestone 120 renamed the OS on 2026-08-15 and swept `cricker` out of the identifiers. It did not
sweep the **environment** namespace. Fifteen `CRICKER_*` names survived in prose; thirteen of them
name nothing the tree reads, and each is an instruction a reader would type and get nothing from.

Found by checking one line of `notes/glyphs.md`'s "where the pieces are" table (`CRICKER_KBD`, which
the tree spells `NIFE_KBD`) and then asking whether the class was bigger than the instance.

**Nine fixed here**: `notes/glyphs.md` (`CRICKER_KBD`), `notes/framebuffer-contract.md`
(`CRICKER_GPU`, `CRICKER_GPU_MON`), `notes/cpu-models.md` (`CRICKER_CPU`, four sites),
`notes/swish-language.md` (`CRICKER_SHOW_TRANSCRIPT`), `design/roadmap/81-hvf-leg.md`
(`CRICKER_ACCEL`).

**Two were not stale and were left alone**, which is the part of this finding worth remembering:
`notes/c-seam.md` and `design/decisions/31-foreign-language-seam.md` name `$CRICKER_CC`, and
`script/bootstrap` really does read `$CRICKER_CC`. The documents are right and the *script* is the
unfinished rename. A blind `sed` over `CRICKER_` would have made two correct documents wrong, which
is the mechanism that destroyed a naming refusal in this tree once already.

**Four are in `design/decisions/` and are handed off**, since a lane does not edit them:
`18-pcie-transport.md` (`CRICKER_DISK`), `27-filesystem-service.md` (`CRICKER_KEEP_REDOXFS`),
`45-partition-guid.md` (`CRICKER_DATA`, which is a Rust constant now spelled `NIFE_DATA`),
`53-parity-matrix.md` (`CRICKER_CPU`).

**Minted: the rename's environment-variable remainder.** Three live code sites still carry the old
name and one of them is a user-facing interface, which is why this is a milestone rather than a fix:
`script/bootstrap` reads `$CRICKER_CC` (the escape hatch a person sets, so renaming it is an
interface change), `script/cpu-matrix` reads `$CRICKER_CPU_MODELS`, and `script/qemu-check`'s header
comment describes a `CRICKER_DISK` check. The four `design/decisions/` sites belong in the same
milestone, because they are the same rename and the same person's call.

**Converted to a gate**, per the sweep procedure's part 3: `script/lint`'s `==> environment names in
prose` block. It derives liveness from the tree (a `NIFE_`/`CRICKER_` name is live if some
non-markdown file reads or assigns it) rather than spelling `NIFE_` into a rule, which is exactly why
it passes `CRICKER_CC` and fails `CRICKER_KBD`. Verified against all nine distinct stale names
(`CRICKER_CPU` alone accounted for five sites): it flags every one, and passes both live ones,
`CRICKER_CC` and `CRICKER_CPU_MODELS`. **This class cannot rot again.**

### 2. FIXED: nine backticked paths that resolve to nothing, across six notes

Three renames the notes never followed:

| the note said | the tree says | renamed |
|---|---|---|
| `script/stack-frames` (`notes/frames.md`) | `script/stack-frame-check` | |
| `user/src/suptree.rs` (`notes/trusted-init.md`, `notes/live-replacement.md`) | `crates/supervision_proto` | 2026-07-31, by rule 7 |
| `user/src/virtio.rs` (`notes/fs-server.md` ×3, `notes/dma.md` ×2, `notes/net.md`) | `crates/virtio` | |

**The fix for `notes/fs-server.md` was itself wrong on the first pass**, and it is recorded here
because it is the sweep's own failure mode: `user/src/virtio.rs::run_blk_server` was corrected to
`user/src/blk.rs::run_blk_server`, and that is still not true. The function lives in
`crates/virtio`; `user/src/blk.rs` dispatches to it. Caught by checking the replacement the same way
the original was checked. **Verify the new name, not only the old one.**

### 3. FIXED: `notes/glyphs.md` on the size of `crates/video_terminal`

"It is about 1,500 lines including its tests and its keymap" carries an argument (keeping the Rust
engine as the reference implementation a foreign one is checked against). Measured: 1,752 lines
across `lib.rs`, `keymap.rs` and `script.rs`.

**Deliberately not given a milestone 125 marker**, and the reason is a boundary between the two
mechanisms rather than laziness. A hedged magnitude is the right shape for this claim: it moves on
every test anyone adds, and a marker would fail the build for a document that is still making its
point correctly. Re-measured and re-hedged, with the sweep named in the sentence so the next reader
knows when it was last true. **Gate a number that must be exact; hedge and re-measure a number that
is an order of magnitude.**

### 4. FIXED: `script/lint`'s own justification for not checking backticked paths

The comment above the markdown-link check said a survey found **8 unresolvable backticked paths over
95 markdown files** and that "a checker there is 100% false positives". Re-measured: **31 over 379**,
and it is no longer 100%, since three of the 31 were finding 2 above.

The conclusion survives in weakened form and the comment now says so: the reason the check does not
exist is the false-positive *rate*, not a claim of perfection, and separating deliberate history from
rot needs a reader. This is the most instructive finding of the four, because the rotted claim was
**the recorded reason a gate does not exist**. A stale justification is worse than a stale fact: it
keeps a decision alive after its evidence has gone.

### 5. ACCEPTED: twenty-eight unresolvable backticked paths that are deliberate history

The 28 that remain after finding 2, recorded rather than fixed, with the reason where a reader meets
it (`script/lint`'s comment). `crates/linedisc` is preserved on purpose by DECISIONS §39;
`crates/heap`, `crates/slab` and
`kernel/src/heap.rs` are accurate prose about what milestone 14 removed; every old spelling inside
`design/roadmap/73` and `design/roadmap/63` is in the block *about* renaming those files;
`script/sanitize` and `script/brief` are names milestone 115 rejected. **History we deliberately keep
is not rot**, and a gate that could not tell the difference would be fixed by deleting the gate.

### 6. ACCEPTED: `design/roadmap/39-repository-structure.md` proposed work that half happened

Its "cheap first move" proposes lifting `virtio`, `net_transport`, `socket_proto` and `suptree` into
`runtime/` crates. Three of the four are crates already, lifted by rule 7 rather than by this
proposal and with no `runtime/` directory involved. The block is `RECORDED` with a decision pending,
so a reader costing it would over-count what remains.

Accepted rather than fixed: the argument stands as it was made, and rewriting another milestone's
proposal is that milestone's call. A dated correction was added beneath it saying what the paragraph
looks like against the tree today, which is the "recorded-accepted, where a reader meets the claim"
disposition doing its job.

**This is the planned-as-built direction of claim rot**, the one the milestone block warned about,
and it is the only instance this sweep found. That is a sample of one, not evidence that the
direction is rare.

## What the mechanism learned about itself

Three things, recorded because the second sweep should not rediscover them. All three are in
`notes/documentation-audit.md`'s `BUGS`.

1. **A stale name met in one document is almost never alone.** Both whole-tree passes were provoked
   by a single line, and both found a class. The step that turns a typo fix into a mechanism is
   asking whether the class is bigger than the instance, and it should be step two of every sweep.
2. **The path survey missed `path::symbol` citations and bare filenames.** It matched
   `` `user/src/virtio.rs` `` and missed `` `user/src/virtio.rs::write_block` `` on the next line.
   Four of the nine path corrections were found only afterwards, by grepping for the name just
   fixed: three were `path::symbol` and one was a bare `` `suptree.rs` ``, which the detector
   deliberately does not resolve because a basename is ambiguous across the tree.
3. **The most valuable finding was a rotted justification, not a rotted fact.** Finding 4 was a
   measurement that had grown by a factor of four under a comment explaining why a gate was
   unnecessary. Sweeps should read the reasons gates give for not existing.
