# 104. The rich-text font is DejaVu Sans Mono, and the palette is Solarized

**Status: DECIDED.** calef, 2026-08-20, answering the two questions milestone 142 said blocked it:
*"DejaVu Sans Mono and Solarized."*

**What it unblocks.** Milestone 142's increment three (the glyph atlas) and increment six (the
palette). Increments one and two never needed either and can start whenever a lane is free.

## The font: DejaVu Sans Mono

**It is Menlo's immediate ancestor, not a lookalike**, and that is why it answers the original ask.
calef's opening proposal was Menlo Regular 11. Menlo is Apple's, `All Rights Reserved`, and its SLA
grants use-while-running-macOS with no redistribution. But Menlo's own `name` table, read on this
machine, says verbatim:

> Menlo is based upon the Open Source font Bitstream Vera and the public domain font Deja Vu.

Manufacturer `Bitstream`, designer `Jim Lyles`, vendor URL still pointing at GNOME. **DejaVu is the
last point in that chain where the outlines were given away.** So this is not a compromise on the
ask; it is the ask, at the point it is licensed.

**And it does not reserve its own name**, which is the property §100 was bitten by. The DejaVu
licence is Bitstream Vera plus public-domain changes, reserving "Bitstream", "Vera",
"Tavmjong Bah" and "Arev", **not "DejaVu"**. Source Code Pro reserves "Source" and would have
inherited §100's objection whole; JetBrains Mono declares no reserved name either but is not
Menlo's ancestor. So a glyph can be fixed later without a rename, which §100 records as a live cost
for this project rather than a formality.

**The known cost, stated rather than discovered**: DejaVu has been dormant since 2016 and has no
variable font. Neither matters for a fixed-cell monospace atlas rendered at build time, and both
would matter for rung three.

## The palette: Solarized

**Canonical Solarized, and calef narrowed it himself.** His opening proposal was *"Solarized Dark
Higher Contrast"*. Milestone 142's lane found that variant **is not Schoonover's and is not an
iTerm2 built-in**: it traces to a 2011 gist, all sixteen values differ from canonical, and it
discards Solarized's structural choice of putting the greys in the bright half of the ANSI table.
Shown that, he answered "Solarized", and confirmed when asked: **"To clarify, Solarized Dark."** So
it is canonical Solarized Dark, chosen over the variant he opened with **after** learning what that
variant actually was. That ordering is the point of recording it: the finding changed the answer,
which is what a survey is for.

**It needs a one-unit nudge to pass milestone 141's gate, and that is worth knowing in advance.**
141 defines three properties a palette must satisfy for a corrupted pixel to be a detectably wrong
colour. Canonical Solarized passes properties 2 and 3 and **fails property 1 on exactly one entry**,
`#93a1a1`, whose channels are not all distinct. A one-unit change to one channel fixes it and is
invisible to any eye. Solarized also has **no channel at `0xff` anywhere**, where the palette it
replaces has twelve, so it is a better test instrument than the thing that was chosen to be one.

**The question that nudge raises, and it is calef's when it arrives**: a palette is sixteen numbers
and a name. Changing one number by one unit almost certainly makes it "Solarized" still in every
sense a person cares about, and this tree has just spent a decision (§100) on the difference between
a look and an artefact. **Record the nudge where a reader meets the palette**, so nobody later
believes the constant is Schoonover's untouched and reasons from that.

## Why these two are recorded together

They are one aesthetic decision with two halves, taken in one sentence, and separating them would
imply a reader could adopt one without the other. **They also share a shape worth noticing**: in
each case the thing calef named was the right thing to want and the wrong thing to ship, and the
answer was the nearest artefact that is actually ours to use. Menlo to DejaVu is a licence chain;
Higher Contrast to canonical is a provenance chain.

## BUGS

- **Neither half is measured against the other.** Nobody has rendered DejaVu Sans Mono in Solarized
  at 1280x720 and looked at it. The specimen harness (`cargo run -p bitfont --example specimen`)
  renders a bitmap font and cannot show an anti-aliased atlas, so the first time this combination is
  seen will be on a screen. That is the ordinary order for this milestone and not a defect, but a
  reader should not take this section as evidence that the pair looks good together.
- **The nudge is unimplemented and unlocated.** It belongs in milestone 141's gate work or 142's
  increment six, and until one of them lands, the palette named here does not pass the check named
  here.
- **DejaVu's licence is not on `deny.toml`'s allow-list**, which matters only if a font ever arrives
  as a crate rather than as bytes in `vendor/`. §100 recorded the same gap; this decision makes it
  bigger, because a transcribed atlas is hundreds of kilobytes under an obliging licence where the
  current font is a kilobyte of nothing.
