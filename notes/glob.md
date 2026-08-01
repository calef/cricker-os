# The glob matcher

`crates/glob`, milestone 47's navigation-and-naming lane. A pattern and a name go in, a `bool` comes
out. No IO, no syscalls, no directory, no allocation, no capability, and no idea what a file is.

This note is the working reference: what the crate does, the four scope decisions it settled and why,
the algorithm and the reason it cannot be made to hang, and where the proof stops and enumeration
starts.

## Why the matcher is a lane on its own

Milestone 47's finding about globbing is that the interesting question is **not how to match**. That
is a solved problem with fifty years of prior art and no thesis in it. The interesting question is
**what a match grants**.

`rm *.txt` with five hundred hits has to convey exactly those five hundred files and nothing else.
The roadmap's four candidate answers and its verdict:

| answer | verdict |
|---|---|
| five hundred file capabilities | honest, and it exhausts capability slots |
| the directory plus a name list | cheap, and it over-grants catastrophically |
| make `rm` a builtin | dodges the question, and costs `rm` as a program |
| **a directory capability attenuated to a name set** | the principled one |

`fwarden` today serves a namespace of exactly one name; globbing generalizes it to a **set**. Same
caretaker, same `fs_proto` above and below, nothing new in the kernel. That is a wiring job, it needs
the directory-capability verb, and it is a different lane.

What falls out is this crate: a total function on two byte strings, which is exactly the part that
can be machine-checked. The property the wiring lane wants from it is worth stating, because it is
the whole demonstration: **the expansion you see is the grant.** `echo *.txt` prints literally the
authority `rm *.txt` would transfer, because the matched set *is* the namespace the warden will
serve. Unix cannot make that claim, since `rm`'s authority never came from the command line at all.
That only means anything if the matcher is one matcher, used in both places, that cannot be talked
into a different answer.

DECISIONS §46 rule 1 is why it is written rather than depended on: it is on the verification path,
and you cannot restructure someone else's crate to make a model checker tractable.

## The surface

```rust
pub enum Dot { Special, Ordinary }

pub fn matches(pattern: &[u8], name: &[u8]) -> bool;
pub fn matches_with(pattern: &[u8], name: &[u8], dot: Dot) -> bool;
pub fn has_magic(pattern: &[u8]) -> bool;
pub fn literal<'a>(pattern: &[u8], out: &'a mut [u8]) -> Option<&'a [u8]>;
pub fn match_steps(pattern: &[u8], name: &[u8], dot: Dot) -> (bool, usize);
pub const fn cost_bound(pattern_len: usize, name_len: usize) -> usize;
```

Six functions, and each is there for a caller that exists:

- **`matches`** is the matcher.
- **`has_magic`** and **`literal`** are the two questions the shell asks of a word *before* it plans
  a grant: is this a pattern at all, and if not, what name is it with the escapes stripped? A word
  with no magic names one file and wants a single-name warden; a word with magic is an enumeration
  and wants a warden over the matched set. `literal` exists so escape-stripping is written once: `a\*b`
  is the file called `a*b`, and getting that wrong means asking the filesystem for a name nobody has.
- **`match_steps`** and **`cost_bound`** are the anti-blowup claim made checkable. See below.

Everything is **bytes**. A name here is a byte string (`fs_proto::grant::MAX_NAME` is sixteen of
them), so a matcher that decoded UTF-8 would have to decide what to do with a name that is not valid
UTF-8, which is a question the filesystem never asks. `?` therefore matches one byte, which is half
of a two-byte UTF-8 character. Same as `fnmatch(3)` in the C locale.

**There is no error case, and no `Result`.** A `[` with no closing `]` is a literal `[`; a trailing
`\` is a literal `\`. That is bash and `fnmatch(3)`, and the reason to match them goes past
compatibility: a pattern is untrusted input, and the alternative to "a malformed pattern means
itself" is an error path in the grant planner for every typo. `ls [foo` should list the file called
`[foo` if there is one.

## Four scope decisions

### `**` is out, permanently, and it is not a matching feature

Recursive descent (`**/*.rs`) does not belong in this crate and will not arrive later. Two reasons,
and the second is the real one.

**It needs a path separator, and milestone 47 has not settled path syntax.** The roadmap is still
weighing Plan 9's answer (absolute paths that are personal, resolved in the client's `user_rt`
against a table of prefix-to-directory-capability) against resolution in the FS server. Baking `/`
into the matcher now would be one lane guessing at another lane's decision.

**`**` is a traversal feature wearing a matching costume.** `*` says "consider these bytes"; `**`
says "and also descend into that directory, and the one below it". Descending means opening a
subdirectory, which in this system means *holding a capability for it*. That is enumeration and
granting. Putting `**` inside a string matcher hides an authority question inside a pure function,
which is the exact mistake this OS exists to not make.

So the crate matches **one name**, a single path component, which is what `fs_proto` actually
carries. When path syntax is settled, recursive descent lands as a traversal layer *above* this
crate: walk the directory capabilities you hold, call `matches` per component. `**` belongs there,
because that is where the authority to descend is.

**The honest cost:** nothing here treats `/` as special, so handing `matches` a whole path lets `*`
match across separators. That is a caller error, not a mode, and the type system cannot catch it
while a name is `&[u8]`. Written down instead, in the crate docs where a reader meets the function.

### zsh's glob qualifiers are out, and the reason is authority

`*(.)` for regular files, `*(om[1])` for the newest, `*(Lm+1)` for over a megabyte. The best thing
in zsh's glob engine, and none of it is here. The roadmap said to settle this **before** building the
matcher around them; settled, out, and the matcher is not built around them.

The reason is not scope discipline. A qualifier needs type, mtime and size **per candidate**, so one
`enumerate` becomes N `FSTAT` calls and needs a **read right beyond enumerate**. That makes `echo
*(.)`, which reads like a display, an operation requiring strictly more authority than listing the
directory. In a capability system that is a change to what the command *is*, not a feature flag.

If qualifiers ever arrive, they arrive as a separate, visible step over an already-enumerated set,
with the extra right named at the point it is taken, and the matcher stays a function of two byte
strings.

### A leading dot is special by default, because the default should grant less

`Dot::Special`, what `matches` uses: a name beginning with `.` is matched only by a pattern beginning
with a **literal** `.`. So `*` does not match `.config`, `?` does not match `.`, and `[.]config` does
not match `.config` either. That last one is glibc's `FNM_PERIOD` rule exactly, and the reason a
bracket expression does not count is that the rule exists to stop a *wildcard* reaching a dotfile,
and a class is a wildcard however few members it has.

This is a policy rather than a matching fact, which is why fnmatch makes it a flag and why
`Dot::Ordinary` exists. It is the *default* here because of what a match means in this system: `rm *`
hands a child authority over everything it matched, so the default is the reading that grants less.
A user who wants the dotfiles asks for them, and gets the larger grant deliberately.

### POSIX character classes are out, and the syntax does something else instead

`[[:alpha:]]` is not a character class here. The inner `[:` and `:]` are not syntax, so it parses as
the class `[[:alpha:]` (members `[`, `:`, `a`, `l`, `p`, `h`) followed by a **literal `]`**, and it
matches the two-byte names `[]`, `:]`, `a]`, `l]`, `p]`, `h]`. Locale-dependent classes have no
meaning on a system with no locale, but a quiet wrong answer is worse than a missing feature, so this
is in the crate docs and pinned by a test.

## The algorithm, and why it cannot be made to hang

A pattern is untrusted input. Somebody types it, or a program produces it, so "a pathological pattern
makes the shell stop responding" is a denial of service rather than a performance note.

The classic way to earn one is the recursive matcher everybody writes first. `a*a*a*a*b` against a
long run of `a` with no `b` makes it try every way of splitting the run between the stars, which is
exponential. This is the same shape as catastrophic regex backtracking and it has taken down real
services.

**This matcher keeps exactly one backtrack point: the most recent `*`.** On a mismatch it retries
with that `*` absorbing one more byte. A later `*` overwrites the saved one, which is sound because
anything an earlier `*` could have absorbed a later one can absorb instead. Two facts follow, and
together they are the whole argument:

- The retry position **advances monotonically and never resets**, so there are at most `name.len() +
  1` backtracks.
- Between two backtracks, `i + j` (name position plus pattern position) strictly increases, so at
  most `name.len() + pattern.len() + 1` iterations happen.

The work is therefore polynomial in the two lengths, with **no term that grows with the number of
`*`s**. That last part is the entire difference between this and the naive matcher.

The bound is not left as prose. `cost_bound(pattern_len, name_len)` computes it from the two lengths
alone, before any matching happens, saturating rather than wrapping (a wrapped bound would be a small
number, and every `steps <= bound` assertion would then be checking nothing). `match_steps` reports
what a match actually cost, counting every pattern byte examined including the ones a bracket
expression is scanned over and the ones re-examined after a backtrack. A caller that wants to refuse
an expensive pattern before running it can ask `cost_bound` and never start.

The one place per-step cost is not constant is a bracket expression, which has to be scanned. An
**unterminated** `[` is the worst case: it rescans to the end of the pattern every time, and a
pattern of 400 `[` against a name of 400 `[` costs 80,200 steps. Still polynomial, and there is a
test that says so.

## What is proved, and where a solver was the wrong tool

Six Kani harnesses, run by `script/verify`, and the split with the host tests is deliberate.

| harness | what it quantifies over |
|---|---|
| `matching_is_total` | every pattern of 0..=3 arbitrary bytes against every name of 0..=3 arbitrary bytes, both `Dot` settings: no panic, no overflow, no out-of-bounds index, and the loop terminates |
| `the_cost_never_exceeds_the_bound` | the same domain: `steps <= cost_bound(pattern_len, name_len)`, which is the *accounting* rather than the blowup (see below) |
| `no_magic_means_the_pattern_is_its_own_only_match` | the same domain: if `has_magic` is false, the pattern matches a name **iff** that name is `literal(pattern)` |
| `star_matches_anything_and_question_matches_one_byte` | every name of 0..=3 arbitrary bytes: `*` and `**` always match, `?` matches iff the name is one byte, `??` iff two, `*?*` iff non-empty |
| `the_dot_rule_only_touches_names_that_start_with_a_dot` | the same domain: `Special` is never more permissive than `Ordinary`, it is permissive only for a pattern opening with a literal `.`, and for a name not starting with `.` the two settings are **identical** |
| `a_negated_class_is_the_exact_complement_of_the_class` | every two-byte class body and every byte: `[xy]` and `[!xy]` partition the byte space. Also the harness that reaches the four- and five-byte class forms the three-byte domain cannot |

**Three bytes, measured rather than chosen, and the road there is the interesting part.** Kani
unrolls loops, so the match loop enters the formula once per possible iteration and the cost grows
with the name length rather than staying flat. Four bytes each was tried and abandoned twice, at 3 GB
and past ten minutes without finishing. What three bytes buys is every byte *value*, 2^48
pattern/name pairs per harness, reaching a closed class, an escape, a trailing backslash, an
unterminated bracket and two stars. What it does not buy is quantification over length.

Two things had to change to get there, and both are DECISIONS §46 rule 1 in practice: restructure the
code, do not weaken the claim.

- **The bracket expression is scanned once, not twice.** The first version found the closing `]` in
  one loop and tested membership in another, and Kani unrolls both, nested inside the match loop it
  is already unrolling. Merging them into a single pass that decides membership as it goes removed a
  whole loop from the unrolling. It is also less work at runtime, which is the usual shape of these.
- **The unwind bounds are measured, not guessed.** Every outer iteration adds at least one to the
  step count, so the worst step count over a domain is a safe upper bound for the iteration count
  over it. `the_worst_case_over_the_proof_domain_is_what_the_unwind_bounds_are_set_from` enumerates
  the harnesses' own domain and pins the answer: **10 steps at three bytes, 17 at four**. The unwind
  is 11. Guessing 20 instead cost nothing in correctness and a great deal in solver time, because an
  unwind bound too high grows the formula for iterations that cannot happen.

**The honest cost, since a gate people skip is worse than none.** The six harnesses take about **ten
minutes** of solver time, which makes this the largest single entry in `script/verify` after
`calendar`'s seven. Two thirds of it is the two harnesses that quantify over a symbolic-length
pattern **and** a symbolic-length name (199s and 186s); the other four are 92, 57, 53 and 1. The
lever that worked was cutting the dot rule's name bound from three bytes to two, which is sound
because that rule is a predicate on the name's first byte. The lever that did not work, measured and
recorded so nobody re-tries it, was restating a restriction as `kani::assume` instead of an early
`return`: 181s became 186s, which is noise.

**Kani found a real defect in a harness, which is worth recording because it was not in the code.**
The negation-complement property failed in 42 seconds with a counterexample, and the counterexample
was right: with the class body fully symbolic, `[!y]` is not "the class of `!` and `y`", it is
*already* a negated class, so `[!!y]` is its complement rather than its double. The assumption that
excludes `!` and `^` at the head of the body is about what the two spellings *are*, not a weakening
of the claim.

**The length-independent claims are the host tests' job, and that is the `ntp_proto` lesson applied**
(see [ntp.md](ntp.md)): a model checker is the tool for domains too big to enumerate, not a better
tool for domains that are not. Two places it decided the design here:

- **Equivalence with exhaustive search.** The property that would settle "is the greedy
  single-backtrack loop actually correct" is "it agrees with a naive matcher that tries everything".
  A solver is bad at that, because the reference is recursive and has to be unwound. Enumeration is
  perfect at it: every pattern of length 0..=5 over an alphabet holding one representative of each
  syntactic role (`a`, `b`, `*`, `?`, `[`, `]`, `!`, `-`, `\`) against every name of length 0..=3
  over `a`, `b`, `-`. 66,430 patterns times 40 names, 2,657,200 comparisons, 0.3 seconds, and it is
  *complete* over that domain rather than bounded.

  The alphabet is the part that took thought. It is not "some characters"; it is one representative
  of each thing the decoder can do, so the enumeration reaches unterminated brackets, `]`-first
  classes, `[a-]`, `[!ab]`, escaped metacharacters and trailing backslashes without anyone having
  thought to write them down. Length five rather than four because five is the shortest pattern that
  reaches a full range (`[a-b]`) and a full negated class (`[!ab]`).

  The reference shares `decode` with the real matcher **on purpose**. What the
  cross-check compares is therefore the search strategy and nothing else: if the two disagree, the
  greedy loop is wrong rather than the syntax being read two ways.

- **The blowup itself.** `cost_bound(3, 3)` is 285, and no matcher at all is slow enough to exceed
  that on three bytes, so the Kani harness is not where exponential backtracking would be caught.
  What it catches is an error in the *accounting*, which is what would otherwise let the big test
  pass while measuring the wrong thing. The blowup evidence is the host test: it runs
  `a*a*a*a*a*a*a*a*a*a*b` against 100,000 bytes of `a` and asserts the measured step count is under
  the bound computed before the match started. The naive reference in the same file would not finish
  that input before the heat death of anything. Overclaiming this in the harness comment and then
  working out the arithmetic is how the distinction got written down.

Both directions of the cross-check are pinned as non-vacuous: 15,956 of the 2,657,200 pairs match,
asserted, because an agreement test between two matchers that both said "no" to everything would pass.

## Corners, and where implementations differ

The unit tests exist for these. Every one is a place a real glob implementation has been wrong.

| input | answer | why |
|---|---|---|
| `[foo` | literal `[foo` | no closing `]`, so it was never a bracket expression |
| `[]]` | matches `]` | `]` immediately after `[` (or `[!`) is a member, the only way to have one without an escape |
| `[a-]` | matches `a` and `-` | a `-` with nothing after it inside the body is a member, not a range |
| `[-a]` | matches `-` and `a` | same rule, other end |
| `[z-a]` | matches nothing | a reversed range is empty. POSIX says undefined; glibc says empty, which is the answer that cannot surprise anyone into a larger grant |
| `[\]]` | matches `]` | strict POSIX bracket expressions have no escapes; glibc's fnmatch and bash both honour one, and matching them is worth more than matching the standard nobody implements |
| `a\` | matches `a\` | a trailing backslash has nothing to escape and is itself |
| `` (empty) | matches only the empty name | |
| `[[:alpha:]]` | matches `a]`, `:]`, ... | no POSIX classes; see above |

## What is not in the crate

- No filesystem access of any kind, no enumeration, no `fs_proto`, no `capsh`. It does not depend on
  anything, inside the tree or out.
- No allocation. `literal` writes into a caller buffer and returns `None` rather than truncating,
  because a truncated filename is a different file and this is the path that decides what gets
  granted.
- No `**`, no qualifiers, no POSIX classes, no locale, no case folding, no path separator.
- **No grant.** The `fwarden`-attenuated-to-a-name-set work, the shell's expansion, `capsh::plan`
  seeing the expanded set rather than the pattern, and `xargs` for the capability-limit version of
  `ARG_MAX`: all of that is milestone 47's other lanes. This crate is the part with no authority in
  it, which is why it could be finished and proved on its own.
