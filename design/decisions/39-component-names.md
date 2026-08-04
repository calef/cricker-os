# 39. A component is named for what it is, and nothing is named for a daemon

**Decided 2026-07-30 (Chris).** Userspace components take names that describe what they do.
Specifically: **no `-d` suffix**, and no term of art that requires archaeology to parse.

## The argument, which is Chris's

Milestone 39's naming section had already argued that "daemon" is the wrong word here, on technical
rather than aesthetic grounds: a Unix daemon is defined by what it detaches from (no controlling
terminal, inherited ambient authority, a pid file, started by a privileged init), and every one of
those is something this OS deliberately does not have. `netd` holds five explicit capabilities, cannot
name its own callers, is supervised, and can be reaped by something that lacks the authority to build
it. It is about as far from a daemon as a long-running process gets.

I then argued to keep the `-d` names anyway, weighing churn against benefit. Chris's response is the
better argument and settles it: **if we are not going to use "daemon", we should not name things `d`
for daemon.** A name is a claim, made before a reader sees a line of code, and this one is false. It
is the same defect as a stale comment, which this project spends real effort correcting; a name is
just a comment that every reader is guaranteed to read.

## The second half: jargon is the same failure

`termd` was to become `linedisc`, the correct Unix term of art. Chris did not recognise the phrase and
asked what a line discipline is, **and he built this system.** That is decisive evidence about the
name, not about him: `linedisc` imports vocabulary from exactly the system whose model we rejected,
which is the `-d` failure wearing a different hat. It became `line_editor`, which someone who has never
read a tty manual understands immediately and which is accurate about the visible behaviour.

The crate `crates/linedisc` renames too, rather than being kept as the implementer's term of art. If
the phrase is jargon to the system's author, it is jargon in the crate as well.

## The rule going forward

- Name a component for **what it is** (`net_stack`, `compositor`, `display`, `line_editor`), not for what
  Unix would have called it.
- **Never `-d`.** Not `netd`, not a future `logd` or `authd`.
- Prefer a word a reader can parse without prior Unix exposure. `blk`, `spawner`, `console`, `input`,
  `shell`, `painter`, `window` were already right, and were always the majority of the tree; the four
  `-d` names were the outliers, not the convention.
- Milestone 39's vocabulary is now the tree's: a **component** is the shippable unit, a **service** is
  what it offers, a **contract** is the wire protocol. "Server" stays a fine role word inside a
  component. "Daemon" appears nowhere.

The rename itself is milestone 46, deliberately its own mechanical commit, which also carries the
naming conventions and the checks for the ones a machine can check. That pairing is on purpose: this
rule and the three inconsistencies found alongside it (crate-name word separation, four spellings of
"the wire contract", and a `feature/`-versus-`feat/` branch-prefix duplicate) are each the kind that
decays without enforcement, and the checker is what makes a convention survive the first inconvenient
moment. The part that cannot be checked, "name it for what it is", stays prose because it needs
judgement.

**Built 2026-07-30.** The rename landed as one commit; the conventions are
[notes/naming.md](../../notes/naming.md), and `script/lint`'s `naming conventions` block checks four of
them. The unfalsifiable-looking half turns out to have a demonstration after all: run the `-d` check
against `main` before the rename and it names exactly `compd`, `gpud`, `netd`, `termd`. What no check
reaches is the jargon argument above, because `linedisc` passes every one of them. A person not
recognising a word is still the only test for that, which is the honest limit of this section.
