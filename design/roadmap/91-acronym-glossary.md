# 91. A glossary, and every acronym linked to it

**Status: NOT-STARTED.** Raised 2026-08-03 by Chris, from the reader's chair: navigating the
acronyms is the hardest part of understanding these docs. That is the naming tenet's own concern
one level down; CLAUDE.md says names are what make this OS legible to humans and to LLMs, and an
acronym is a name whose claim is hidden until the reader already knows it.

**Gate: DECISION.** The glossary's own name and location are provisional and Chris's to settle. The
block adds a scheduling constraint of its own: this touches nearly every documentation file, so it
should start only when no lane holds unmerged notes/ edits.

**Measured, so the size is a number:** the markdown tree (notes/, design/, the root files)
carries ~835 distinct all-caps tokens. The top of the list is the real problem: IPC appears 251
times, DMA 231, EL0 181, IOMMU 180, TCB 123, none of them expanded anywhere a reader can reach
from the use. The naive count also includes things that are *not* acronyms (rights constants like
WRITE, the status vocabulary like BUILT, plain emphasis like NOT), which is the finding that
shapes the enforcement below: the true glossary is likely a low hundred entries, and the gate
needs a recorded line between prose and code.

**The deliverable, in three parts:**

1. **The glossary itself**: one entry per acronym, with the expansion, one or two sentences of
   what the term means *in this tree* (TCB here is the trusted computing base, not a thread
   control block, and that collision alone justifies the document; where both senses exist, the
   entry says so), and a link to the note that owns the concept. Entries are anchors, so every
   link lands on the definition, not the top of a long file.
2. **Every prose use links to its entry.** Every use, not first-use-per-file, and the rationale
   is Chris's stated pain: readers land mid-file, from a search, a cross-reference, or a code
   comment's pointer, and a first-use convention only serves the reader who started at the top.
   The line that keeps this sane: **backticked tokens are code identifiers and are exempt**
   (`WRITE` the right, `BUILT` the status, register names); bare all-caps tokens in prose are
   acronyms and link. Tokens that are neither (emphasis, non-acronym capitals) go in a recorded
   exemption list next to the gate, each with a reason.
3. **A gate, or it drifts**: script/lint learns to fail on a bare all-caps prose token that is
   neither glossary-linked nor in the exemption list, so a new acronym cannot arrive undefined.
   The gate's own blind spot, recorded now: it cannot check that a link points at the *right*
   entry, the same limit the citation checks already record.

## Scope note

The markdown tree first; rustdoc comments and kernel code comments are out of scope for this
milestone (comments cite notes by design, so the glossary serves them transitively; linking
inside rustdoc is a different mechanism and a follow-on decision). Sequencing: this touches
nearly every documentation file, so it lands as mechanical passes (glossary first, then linking
in reviewable batches) and should start only when no lane holds unmerged notes/ edits, for the
same conflict reason the roadmap split cited. Milestone 40 (documentation as a system service)
inherits the glossary as a first-class installable page when it happens; cross-reference it
there. The glossary's own name and location are provisional and Chris's to settle.
