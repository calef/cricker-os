# 59. Append is an open mode, so `>>` costs a character and a flag

**Status: DECIDED.**

Milestone 50 finished the prompt's operators: `|`, `<`, `>` and now `>>`, on both ISAs.

**`>>` needed no new machinery at all, and that is the interesting part.** §55 decided that the file
behind a redirect is opened and written by *the shell itself*, because one page cannot serve two
clients. Append inherits that decision whole: it is a flag on the open the shell already performs. No
manifest field, no `spawnproto` bit, no init change, and nothing added to the syscall surface, which
is the test §55 was really making. **An operator that fits in the existing model is evidence the
model was drawn in the right place**; one that had required a new capability would have said §55 was
wrong.

The guest test asserts the appended file is **exactly twice** the length of the truncated one in all
three counts, rather than checking what `echo` prints. That makes it a claim about the operator
instead of a claim about a program's output.

**`2>` is deliberately absent, and it is a design fork rather than missing work.** There is no second
output stream in this system. Adding one is a decision about what a program's error channel *is* in a
capability OS (a second sink capability? a distinguished slot? a convention?), and it belongs in the
model before it belongs in the parser. `notes/pipes.md` carries the analysis.

## BUGS

- **The shell is the only writer, so `>` cannot redirect a program that writes through its own file
  capability.** Nothing does today. The day something does, §55 is the section that has to move, not
  this one.
