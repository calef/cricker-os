# 141. A palette worth looking at, and a gate that lets it be one

**Status: NOT-STARTED.** Minted 2026-08-19 by calef, on seeing that the terminal's colours were
chosen as a test instrument: *"can we have an option at some point to make it pretty and not just
good for tests?"*

**Gate: NONE.** The first piece is a check nobody has written, and it needs no decision.

**In brief.** The sixteen-colour palette in `crates/video_terminal` was picked so that a corrupted
pixel is a detectably wrong colour rather than a different legal one. That is a good reason and it
made the screen ugly. This milestone gets both, and the order matters: **the gate comes first, and
then the palette is free.**

## The finding that makes this cheap

**The palette does not deliver the property it is ugly for.** Its own comment says the entries "have
all three channels distinct in most entries", and measured on 2026-08-19: **no entry has three
distinct channel values**, and **eight pairs are related by a channel permutation**. Entry 1 is
`0xcd0000` and entry 2 is `0x00cd00`, so swapping the red and green channels turns red into green,
which is a legal palette colour and passes every check.

So the tradeoff everyone assumed, pretty against testable, was never being paid for. The screen is
ugly and the swap it guards against is undetected.

## Why this is a property rather than a palette

**The test's requirement is a property of the set, not a specific list of colours**, and that is the
whole reason this milestone is possible:

1. **Every entry has three distinct channel values.** Then a swapped channel changes the colour.
2. **No two entries are permutations of each other.** Then a swap cannot land on another legal
   colour.
3. **No entry is saturated at `0xff` in a channel that another entry saturates**, which is what the
   present palette actually buys and should keep: a dropped shift or a saturating write lands
   off-palette.

**Any palette satisfying those three is as good a test instrument as this one and better than it**,
because this one fails the first two. And those constraints leave enormous room: they rule out pure
primaries and near-duplicates, and they permit essentially every considered terminal palette a
person would recognise.

## The order

1. **Write the check.** Three assertions over `PALETTE`, host-tested, in the crate. It fails today,
   which is the point: **watch it fail before making it pass**, per this tree's standard.
2. **Choose a palette that passes.** calef's call, because it is a thing a reader meets and because
   the whole request is aesthetic. The check tells him which candidates are admissible; it does not
   choose.
3. **Only then consider an "option".** His word was *option*, which may mean a nicer default or may
   mean a configurable palette. A configurable one is a different and larger thing: the palette is
   currently a `const` three parties agree on, and making it runtime state means the kernel test and
   the host scanout check have to learn which palette is active. **Do not build that without asking
   which he meant.**

## BUGS

- **The scanout check's own reasoning is tied to 128x64** ("pure primaries on a 128x64 surface make
  a pretty screen and a bad test") and milestone 29 is moving the surface to 800x600. The argument
  plausibly survives unchanged, and nobody has re-read it at the new size.
- **This block assumes the three properties above are the right ones.** They are this tree's
  reconstruction of what the original comment was reaching for, not a specification anybody wrote
  down. A fourth failure mode nobody has named would not be caught by them.
- **A palette that passes the gate can still be ugly**, and no gate can fix that. The check makes an
  attractive palette *admissible*; it does not make one appear.
