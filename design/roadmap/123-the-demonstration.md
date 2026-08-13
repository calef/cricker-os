# 123. The demonstration: somebody else's software, running narrow, and a gate that keeps it there

**Status: NOT-STARTED.** Minted 2026-08-13 by Chris, on the observation that this is the only claim
in `design/why-now.md` with nothing scheduled against it. §82 states four falsification conditions
and this milestone is what closes the first of them.

**Gate: MILESTONE 121.** The corpus needs at least one program this project did not write, and 121
is the first. The tripwire in element four does not depend on it and could start earlier; the
manifest and the negative control do.

## What is being demonstrated, and why the current evidence does not do it

The six-pager's section 6 orders four claims by how well the evidence supports them. Three are
demonstrated or partly so. The fourth, **that software can run under narrow authority and still be
useful**, is not, and it is the one the thesis rests on.

The objection to answer is fair and should be written down in its strongest form: **this project
wrote the kernel and then wrote the 52 programs that run on it, so of course they fit the model.**
Nothing in the tree currently answers that, because nothing in the tree was written by anyone else.

## The four elements

### Same input, same output, different authority

Run a ported third-party tool here and on Linux over the same corpus and show the results are
**byte-identical**.

This exists to pin the word "useful", because the easy way to make authority look narrow is to
quietly do less. A subset that passes is not a demonstration. Identical output over a corpus nobody
chose for the purpose is hard to fake, and it is checkable by someone who does not trust us.

### The authority manifest, printed beside it

**`caps` already does most of this.** The shell has a `caps` builtin (`grant_plan`'s `Command::Caps`,
routed by `swish`) that reports the capabilities a command line would be granted, and the boot gate
already exercises it on `wc`, `date`, `rm *.txt` and `budgeter --mem 16`.

What is new is pointing it at software we did not write and putting the output next to the run. The
demonstration is not the assertion that this is more secure. It is that **the question has an answer
here and does not have one on the reference platform.** A reader can check a six-line listing; on
Linux the same program holds the user's entire reach and there is no listing to print. That asymmetry
is the argument, and it does not require trusting a benchmark.

### A negative control, at two strengths

Milestone 108's discipline applies: a claim is worth what its falsification is worth, and that
milestone verified its property failed without the change.

**The weak control** is the same command against a directory it was not granted, refused at the
capability layer rather than by a check the program performs on itself.

**The strong control is an attack class that becomes unrepresentable.** Hand an archive extractor a
file containing `../../etc/passwd`. On a conventional system an unhardened extractor writes outside
its target, which is a documented, recurring, exploited class. Here `one_name` refuses `..`, so the
program cannot express the path at all: the same malicious input meets a system where the escape has
no spelling. Same input, different outcome, by construction rather than by vigilance.

That is the most persuasive single artifact available from this whole thesis, and it needs an archive
tool rather than a search tool. Nothing currently schedules one.

### A tripwire, or the demonstration rots

§82 records that nothing measures grant width. The sharper problem is that nothing would **hold** it:
`caps` prints the number and no gate fails when it grows.

A regression gate in the shape of the existing icount bench tripwire, failing CI when a program's
grant widens, converts this from a demonstration into a maintained property. **This element matters
more than the other three**, because §82's stated most-likely failure is ports that quietly
reconstruct ambient authority, and that failure looks exactly like success while it is happening. A
one-time demonstration cannot catch it. A gate can.

## One program is not the claim

A single tool running narrow proves the thing is **possible**. It does not prove it is **typical**,
and the claim in the six-pager is about software rather than about one program.

What settles the argument is a distribution over a small corpus, five or ten real programs, reporting
both numbers this project has committed to:

- **Grant width** (§82): how narrow an authority does each program actually run under.
- **Patch burden** (§84): what fraction ran unmodified, and of the remainder, what fraction of the
  patches would be acceptable upstream.

So the honest shape is that **121 is the proof of concept and the corpus is the proof.** This
milestone is done when the corpus exists and its numbers are recorded, not when one program runs.

## Prior art

**Code to use:** `caps` and the `grant_plan` planner, which already compute what a command line is
granted. The measurement half of this milestone is mostly assembly rather than construction.

**A design to copy:** the icount benchmark tripwire, for the gate's shape. It already solves the hard
part of a regression gate in this tree, which is having a committed baseline and a tolerance that
does not get widened to make a red run green.

**A mistake to avoid:** demonstrating on a corpus chosen because it fits. The programs should be
picked for being ordinary and useful, and the ones that do not port should be **reported rather than
dropped**, because the fraction that fails is the finding.

## BUGS

- **`caps` is plan-time, not process introspection.** It reports what the planner would grant, not
  what a running process holds. If those can diverge, the manifest measures the wrong object, and
  nobody has checked whether they can.
- **"Width" may not be a scalar.** Is six capabilities with narrow rights wider or narrower than two
  with broad ones? A tripwire needs a canonical ordering and there is no obvious one, so the gate may
  end up watching a vector rather than a number.
- **A grant has no canonical serialization.** The tripwire needs a stable textual form of a grant to
  diff against a baseline, and `caps` output was written for a human at a prompt rather than for a
  gate.
- **The strong negative control has no owner.** It needs an archive tool, and the compression work is
  currently a question rather than a milestone.
- **This milestone can be marked done while proving little.** If the corpus is one program, the
  distribution is meaningless and the claim is no better supported than before. The corpus size is the
  thing to hold, and nothing here enforces it.
