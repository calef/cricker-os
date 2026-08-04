# 50. Namespace composition (`bind`), not stored paths

**Decided 2026-07-31 (Chris).** Milestone 47 raised the question as an open fork after `mv`, `rm`
and `ln` were worked through; this settles the mechanism and the name together, because choosing the
mechanism is what made the name available.

**We do not get symbolic links. We get `bind`**: a process composes its own namespace, and thereafter
a name resolves where it attached it. Plan 9's answer, and Plan 9 reached it from the premises this
project already adopted: per-process namespaces, no global root, resolution against what you hold.

## Why, in one line

**A stored path is somebody else's decision embedded in your lookups.** Whoever created the entry
decided that this name redirects, and every later reader inherits that decision. It cannot *escalate*
(§48's resolution rules bound it to what the holder already reaches), but it is still another
party's data steering your resolution. **With `bind`, the composition is yours**, and nobody can plant
anything in a view you assembled. For a system whose whole claim is that authority comes from what you
were handed, that is not a small difference.

## The naming search is evidence, not an anecdote

Twenty-eight-plus candidates were worked before this decision, and **the search terminated without a
winner**: the last dozen produced no new failure modes at all. The families sorted cleanly:

- **Mirror** (`mirror`, `reflection`, `erised`, `matsuyama`, `speculum`, `glass`, `scryer`): the
  *right* property, viewer-dependence, and every word already spent by computing: `mirror` is a
  replica, `reflection` is runtime introspection, `echo` is a shell builtin we ship, `parallel` is
  concurrency. **Physical-optics vocabulary has been comprehensively borrowed**, so the one metaphor
  that fits has no available word.
- **Window** (`aperture`, `pane`, `casement`, `oculus`, `fenestra`): available words, *wrong*
  property: an opening shows the same thing to everyone who looks through it.
- **Concealment** (`costume`, `disguise`, `veneer`, `front`, `mask`, `curtain`, `screen`, `patina`,
  `whitewash`): wrong property *and* they imply an underlying thing being covered, when there is
  nothing underneath.
- **Road** (`route`, `alley`, `way`, `parkway`, `tread`): a road leads somewhere fixed regardless of
  who walks it.
- **`alias`** was semantically closest and collides twice, once with zsh and once with macOS's
  object-tracking Finder alias, which is its inverse. **`harmonic`** cleared every test and failed on
  the *direction of causation*: a harmonic is determined by its fundamental, where our resolution is
  determined by the namespace. **`deictic`** (linguistics: an expression whose referent depends on the
  context of utterance) is the only candidate that described the property without importing a false
  relationship, and its cost was obscurity.

**The asymmetry decided it.** The construct resisted naming for fifty years because it is a poor fit
for any familiar relationship. `bind` needed no search at all: the mechanism already has a name, in
Plan 9 and in Linux's `mount --bind`, and using it *claims* "this is that", which is true. Inventing a
synonym would have failed §39 in a novel way: asserting novelty where there is none. **One of these
is a well-trodden idea; the other is a thing nobody has managed to name.**

## What this costs, and the escape hatch if it bites

A bind lives in a namespace and dies with it. **A stored path is on the disk**, and that matters in
exactly one case: faithfully representing *someone else's* filesystem.

**Whether milestone 55 needs that is unverified and should be checked before it decides anything.**
Time Machine writes a **sparse bundle** (a set of band files), so a Mac's own symlinks live *inside*
that image and the server sees opaque blocks. If that holds, the fidelity argument never arises.

If interop does demand it, the answer is **an inert stored path: store the bytes, return the bytes,
never interpret them.** Every hard question here (what namespace it resolves against, whether `..`
clamps, whether `rm -r` descends through it, what to call it) comes from *resolving*. Drop resolution
and the cluster collapses, including the naming problem, because an uninterpreted stored path is data
with a type tag rather than a construct.

## Not built

This decides the mechanism, not the implementation. `bind` needs per-process namespace machinery
beyond milestone 47's per-shell roots: a mount table per process and resolution through it. That is
real work and it is the direction 47 already leans, but nothing here ships it.
