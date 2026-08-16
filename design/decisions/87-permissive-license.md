# 87. MIT OR Apache-2.0, and why the GPL's lesson does not transfer

**Status: DECIDED.** calef, 2026-08-15, ratifying as a recorded decision what had lived since the
first commit as a Cargo.toml convention. Raised from a conversation about why Linux beat the
BSDs, which is the argument this entry exists to answer: the tree is permissively licensed, and
the best-known story in free software says copyleft is why the biggest free kernel won.

## The choice

Dual **MIT OR Apache-2.0**, the Rust ecosystem's convention, declared once in the workspace
metadata and inherited by every member crate. MIT for maximal simplicity and reach; Apache-2.0
beside it for the explicit patent grant the bare MIT license lacks, which is doing real work for
an operating system that may someday court hardware vendors.

## Why the GPL's lesson is not being ignored

The lesson of Linux versus the BSDs is routinely compressed to "copyleft wins," and compressed
that far it is wrong. What the history supports: **the license that minimizes contributor
friction in its own era wins.** In 1998 that was the GPL, because vendors would not fund a
commons a competitor could capture, and copyleft was the guarantee that let IBM and Red Hat
invest. The sign has since flipped. LLVM overtook GCC substantially because permissive licensing
let Apple and Google build on it without legal friction; Rust, Kubernetes, and nearly every
ecosystem winner since 2010 is MIT or Apache; and this project's own reference set agrees (Redox
is MIT, Zircon is BSD-style, and seL4's GPLv2 kernel is widely cited as adoption friction in the
embedded space it targets). A Rust kernel under GPL would sit awkwardly inside the
permissive-normed crate culture its own components live in. A newcomer arriving from Rust meets
exactly the license they expect, which is the third principle applied to legal text.

## Why nife's win condition differs from Linux's

Linux needed a commons no vendor could capture, because the commons was the product. nife is a
demonstrator (§14): the claims are that a verified capability microkernel runs real workloads,
and that one architect plus agents can build one. If a vendor someday forks nife into a product
and ships it, that is the BSD "absorbed by Apple" ending, and for this project's thesis it is
closer to victory than defeat: a customer ran it. The assets hardest to freeload, the proof
maintenance, the velocity, and the documentation culture, stay wherever the method lives.

## The costs, accepted with eyes open

- **The Sun scenario is allowed.** A fork can take improvements out of the commons and
  contribute nothing back. Accepted because there is no commons to drain yet, and the
  mitigation is the one that always worked: be the upstream worth staying close to.
- **This is the most irreversible decision on the books.** Relicensing requires the consent of
  every contributor whose work survives in the tree, so the practical cost of changing course
  grows with every outside contribution. That is exactly the "who else has already acted on
  this" category, and it is why this entry exists: the reasoning should be findable when
  someone proposes revisiting it.

## What was not chosen

- **GPLv2/v3**: the friction argument above, plus kernel-specific evidence (seL4) that copyleft
  costs adoption in exactly the embedded and vendor contexts a microkernel courts.
- **MPL-2.0 and other file-level copyleft**: a middle ground with the legal-review burden of
  copyleft and the drain-resistance of neither camp; nothing in the reference set uses it.
- **A single license rather than the dual grant**: MIT alone lacks the patent grant; Apache
  alone is incompatible with GPLv2 consumers and heavier than small-crate reuse wants. The dual
  grant is the ecosystem's solved problem, and inventing differently here would cost readers
  the recognition the naming tenet already values.
