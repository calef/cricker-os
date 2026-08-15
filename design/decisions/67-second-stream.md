# 67. A program's second stream is a declaration, not a number

**Status: DECIDED.**

**Decided 2026-08-03 (calef), from notes/pipes.md's open fork.** `2>` gets built on option (c):
a program that has diagnostics **declares a second output in its manifest** (`OutputSpec` grows
the position), the shell plans a second endpoint only for programs that declare one, and `2>`
binds to the declared output. Aimed at a program that declares none, it is a truthful refusal, the
same statement `caps` already makes: the command line can only name what the manifest offers.

The alternatives, refused with reasons. A **numbered-slot convention** (Unix's fd 2 transplanted)
imports the ambient-agreement disease the note diagnoses: nothing here is ambient, and a number
everyone must agree on forever is the mechanism this system exists to not need. A **second opcode
on the one endpoint** (a diagnostic tag in the sink frames) is cheap but preserves "one channel,
two kinds of thing" as a tag, sends diagnostics down a pipe into `wc` by default, and dissolves
nothing.

The consequence the choice buys: separation arrives per program, as each declares (`date` first,
whose clockless complaint into a redirected file is the motivating loss). The consequence it
costs: `2>` works only on declaring programs, which is this shell's grammar being itself.
