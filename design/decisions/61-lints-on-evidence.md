# 61. A lint is adopted on evidence from this tree, not on its description

**Status: DECIDED.**

Milestone 68 turned on eight candidate lints and kept five. The three that lost were not bad lints;
they are all defensible defaults that many Rust projects run. They were wrong **here**, and nothing
short of running them over this tree and reading the output would have shown it.

`cast_possible_truncation` is the clearest case. The argument for it in a kernel is strong: address
arithmetic narrowed by an implicit `as` produces a plausible wrong address rather than a compile
error. Then you run it and 199 of 497 hits are `u64`/`i64` to `usize`, flagged because a 32-bit
pointer target would truncate. §19 names aarch64, riscv64 and x86_64; all three are 64-bit, and there
is no way to tell clippy that `usize` is 64 bits for us. A gate whose output is more than half
inapplicable trains a reader to skim, and a skimmed gate is not a gate.

`items_after_statements` failed differently, and the way it failed is the more interesting one. All
43 of its hits are the same shape:

```rust
// `aarch64_cpu` gives CNTKCTL_EL1 no named fields, so set the bit by hand: EL0VCTEN is bit 1.
const EL0VCTEN: u64 = 1 << 1;
CNTKCTL_EL1.set(CNTKCTL_EL1.get() | EL0VCTEN);
```

The constant sits beside its use, under the paragraph explaining it. Obeying the lint hoists all 43
to the tops of their functions, separating each from its explanation and piling unrelated constants
where a reader looks for the function's first action. The lint is enforcing a general rule against a
**specific convention this project chose on purpose** (CLAUDE.md: keep the constraint next to the
code it constrains), and the convention is better.

`format_code_in_doc_comments` is the same lesson inside code rather than around it. It collapsed a
deliberately aligned column of trailing comments in `crates/gpt`'s module example, destroying the
call-to-destination mapping the example existed to show. A doc example is written to be read as much
as run, so its alignment is authored meaning; a formatter cannot tell that from incidental spacing.

`doc_markdown` is the counter-example that keeps this from being an argument against linting. It
produced 416 hits, of which roughly half wanted backticks around `RedoxFS`, `PCIe`, `OpenSBI` and
`AArch64`. Those are proper nouns, and rendering them as code tells a reader "this is an identifier
you could type" about the name of a project. The other half were real: `TTBR0_EL1`, `BTreeMap`,
`FRAME_SIZE`, `crates/measured_boot`. The fix was not to drop the lint but to configure it, because
the split was legible once the hits were read. **Configure when the false positives share a cause,
drop when they do not.**

## The rule

A lint goes in `[workspace.lints]` only after it has been run over this tree and its hits read. What
gets recorded is the number, not the intention: `Cargo.toml` and `rustfmt.toml` each carry the count
that killed the lints they exclude, so the next person to propose one finds the measurement instead
of re-running it.

The corollary matters more than the rule. Nothing goes in that table "to see what it finds", because
`script/lint` runs `-D warnings`: adding a lint is a commitment to fix every existing violation
first. That is the same ratchet discipline §38 applies to dead code, and it is why the two lints
milestone 68 could not finish (`undocumented_unsafe_blocks`, 228 sites; `missing_docs`) are absent
from the table rather than present with a pile of `#[allow]`s underneath them.

## BUGS

`script/decisions --check` cannot see a section in the WRONG PLACE, only a missing number. §61
itself landed below the `## Reading` closer when it was written, so the headings ran §59, §60,
then the closer, then §61, and the gate reported "numbering clean" throughout because it tests for gaps
(`set(range(1, max+1)) - set(seen)`) and never for order. Corrected by hand on 2026-08-03. This is
the same well-formed-but-wrong blind spot CLAUDE.md already records for `§N` citations, which is why
it is written down here rather than only fixed: the check that would catch it is a comparison of
heading order against numeric order, and it does not exist yet.

`missing_docs` was absent for the reason above at this decision's writing (2026-08-03): item
coverage was 67-94% and adopting it meant an open-ended commitment. **Superseded by §107
(2026-08-22)**: three burn-down passes shrank the worklist from 32 crates to 7, changing the cost
enough that calef took it as a considered exception to this section's own rule, recorded explicitly
there rather than silently reversing it here.

`undocumented_unsafe_blocks` WAS the standing example here and is now a gate: all 205 undocumented
blocks were read and commented, and the lint is in `[workspace.lints]`. The episode that produced
this section's rule is worth keeping anyway, because it is about safety comments rather than about
lints. An attempt to GENERATE the comments stamped "the node pointers come from `Box`ed locals" onto
an `unsafe impl Node`, whose safety condition is that `next`/`set_next` address the same field. It
was reverted.

**A safety comment is an assertion, not a formality.** Its entire value is that the next reader can
rely on it instead of re-deriving the argument, so one that is plausible and wrong is worse than an
absent one: absence prompts a check, and a confident falsehood prevents it. The workable test for
whether a batch of sites may share one sentence is not "does this code look similar" but **"is the
sentence checkable at each site"**. It was, at 58 byte-identical panic-handler traps and at 73
`invoke` calls whose contract places no obligation on the caller at all. It was not, in a test module
where the pointers differ in what they are and when they are queued.
