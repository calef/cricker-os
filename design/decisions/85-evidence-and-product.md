# 85. What we port is evidence and must not be ours; what we ship is product and must be

**Status: DECIDED.** Chris, 2026-08-13, after proposing that we take Ubuntu's most-installed package
list, work down it from most to least, and implement our own versions. The proposal is right for one
of the two things it was aimed at and destroys the other, and the two look identical from a distance,
which is why this section exists.

## The proposal, and what is right about it

Take an external ranking of what software people actually have installed, and work down it.

**The ordering is the valuable part.** Milestone 123 names its own worst failure as "demonstrating on
a corpus chosen because it fits", and an install-count ranking is immune to that in a way no list
assembled here could be. Nobody in this project chose it, it cannot be quietly reshaped when a
program turns out to be inconvenient, and "we are through the top N" is a progress statement an
outsider understands without preamble.

That much is kept, for both halves below.

## Why reimplementation destroys the evidence

Milestone 123 exists to answer exactly one objection, and it is a fair one: **this project wrote the
kernel and then wrote the 52 programs that run on it, so of course they fit the model.**

If the corpus is our own reimplementation of `ls`, `grep` and `tar`, then after all that work the
objection is untouched. It is still our software, written by people who know the model, and it will
hold a narrow grant because we made it hold one. The grant width we measured would be a measurement
of our own intentions rather than of anything about the world.

**The corpus is evidence only because we did not write it.** Reimplementing it converts the strongest
available evidence into none, at considerable cost.

It is also §84 inverted. That section puts reconstruction at tier four, "last resort and rarely", and
records Chris's own reason: reconstructing every application does not build a community.

## The split

The same ranking serves both halves, differently.

**For evidence (milestone 123), the ranking selects which third-party programs to port.** The corpus
stays software this project did not write. The list is filtered, *before it is consulted*, to leaf
applications: things a person invokes, rather than libraries, package management or init. What is
ported is measured for grant width (§82) and patch burden (§84), and what fails to port is reported
rather than dropped.

**For product (the distribution), the ranking is a build order for our own implementations.** A
distribution cannot ship as a pile of other people's patches, a capability-native program written
from scratch is better software than a patched one, and this is §84's tier three doing its job: take
the ideas, write the thing. `design/what-a-distribution-packages.md` owns that tier.

**The two must not be confused in the numbers.** A program may legitimately appear in both, ported
first as evidence and later rewritten as product, and when that happens the measurements belong to
the ported one. A grant width taken from our own implementation is not evidence of anything.

## Two cautions about the list itself

**The head of the ranking is the least suitable part.** It is dominated by libraries, the package
manager, init and language runtimes, none of which are applications and several of which should not
exist here at all: a package manager writes system-wide and runs maintainer scripts with broad
authority, which is tier three outright. A literal walk from the top spends its first stretch on the
wrong material. The filter above is what prevents that, which is why it is stated in advance.

Worth knowing too that popularity data separates *installed* from *recently used*, and the used
number is the more informative of the two for this purpose, because a package present as a transitive
dependency says nothing about whether anyone runs it.

**And the list encodes Unix's architecture.** It is a census of how one system decomposed the
problem, including the parts that exist *because* authority is ambient: `sudo` exists because there
is an ambient root to escalate to, `ps` because enumerating every process is something anyone may do.
A faithful walk down it rebuilds that shape one program at a time, each individually confined,
arriving at a system whose structure still assumes the thing this project removed. That is §82's
stated most-likely failure arriving dressed as a roadmap.

**So the ranking selects candidates; it does not decide what a system needs.** A program near the top
that exists only to manage ambient authority is evidence that the old system needed it, not that this
one does.

## BUGS

- **The filter is judgment and can be gamed.** "Leaf applications" has no mechanical definition here,
  and a filter applied after seeing which programs port cleanly is cherry-picking with extra steps.
  It has to be written down before the list is consulted, and this section does not enforce that.
- **The popularity data is a biased sample.** It reflects users of one distribution family who opted
  in to reporting, which is not the same population as "software people run".
- **No stopping rule for the product half.** Milestone 123 sizes the evidence corpus at five to ten
  programs. Nothing sizes the userland, and "work down the list" is unbounded by construction.
- **Nothing yet decides what a capability-native replacement should look like** when the original
  exists only to manage ambient authority. The honest answer for `sudo` and `ps` may be that they
  have no successor here, and this section does not settle it.
