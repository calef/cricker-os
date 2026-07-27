# Prior art and reuse

Where to look before building, what is worth taking, and the rule that decides it. Written
after the question "cricker-os looks a lot like Redox; can we reuse their components?" The
answer generalizes past Redox, so this note is the standing survey checklist for every
milestone design, not a one-off comparison.

## The rule: the reuse boundary is the TCB boundary

**Inside the TCB, always build.** Two reasons, both structural rather than cautious:

1. **Verification.** Anything in the TCB must be provable, and nobody writes code to be
   proved by accident. Foreign kernel code imported into the TCB is unverified surface that
   undercuts DECISIONS §14 directly. The proofs we have (caps, IPC, MMU invariants; see
   notes/verification.md) exist because the code was shaped for them.
2. **The thesis.** The demonstrator's claim is about *this* capability core. A kernel that
   is partly someone else's demonstrates less, whatever its quality.

**Pure-logic crates: prefer a maintained no_std crate, unless building it is the point.**
The §7 crates we wrote by hand (`dtb`, `elf`, `paging`, `heap`, `caps`, ...) were the point;
they are also now proved or provable, so they stay. For peripheral plumbing not yet built
(a PCI capability walker, a GPT parser, a font rasterizer), the calculus flips: a
well-maintained kernel-agnostic crate beats a bespoke one we then have to trust anyway.

**Userspace components: actively prefer porting.** This is not a compromise of the
demonstrator; it can be evidence *for* it. Milestone 23's ambition is vendor-shippable
components confined by the kernel. A ported foreign component running confined behind the
capability contract demonstrates exactly that: the kernel safely runs code we did not
write. Userspace reuse feeds the thesis.

**Two guards, regardless of layer.** No reuse may widen the syscall surface (that is a
design fork, per CLAUDE.md), and no reuse may smuggle in POSIX assumptions (open-by-path,
fork/exec, ambient authority). A component that cannot live behind explicit capabilities
is not a candidate, however good its code.

## Redox specifically

The similarity is shallower than it looks. Redox is Unix-shaped: a scheme namespace
("everything is a URL"), POSIX via relibc, a syscall ABI modeled on Linux's, a kernel with
a heap. cricker-os is seL4-shaped: capabilities, endpoint-only naming, explicit spawn
endowment, a kernel that cannot allocate (milestone 14). Their kernel is not a donor, even
of fragments. Their userspace is where the portable assets live:

| Component | Fit | When |
|---|---|---|
| **RedoxFS** (`redoxfs` crate) | Best single candidate. A real CoW filesystem that also compiles for Linux/FUSE, so it is only loosely coupled to Redox syscalls. If a persistent read-write FS server ever earns a milestone (crickerfs is an archive format, not that), porting redoxfs behind a capability-confined server beats writing one. | Post-19 |
| **relibc** | Long shot; the only credible one if 19's "Linux-compat later" fork is taken. It has a platform layer targeting Linux and Redox; a third backend against our native ABI (notes/abi.md) is conceivable. The catch: it assumes fork/exec-ish process semantics that explicit-endowment `Spawn` deliberately does not provide. Note it, do not plan on it. | 19 compat fork, if taken |
| **Userspace drivers** (NVMe, e1000, xHCI, ps2) | Reference implementations, not code. Written against Redox's scheme and IRQ model, but the register-level logic is the hard-won part and it reads well. | 16, 24 (real hardware) |
| **ion, Orbital, pkgutils** | No. ion needs POSIX and we have a shell; the rest is out of scope. | never |

Licensing is a non-issue: Redox is MIT, compatible with our MIT/Apache-2.0 dual license.

## The wider survey list

Redox is not the richest donor. For crate-level plumbing, the kernel-agnostic no_std
ecosystem is closer to our shape, because those crates were built to be embedded in someone
else's kernel:

- **rCore / Arceos ecosystem** (`virtio-drivers`, and friends). Kernel-agnostic virtio,
  written for exactly our situation. Our virtio is built, but this is the model of what a
  reusable donor crate looks like.
- **Tock** (`tock-registers`). Type-safe MMIO register definitions; the pattern is worth
  copying even where the crate is not taken.
- **Hubris** (Oxide). Not a donor of code (its all-static task model is very different) but
  a donor of designs: their idol IPC interface definitions and their debugger story
  (humility) are prior art for milestone 23's component contracts.
- **seL4 ecosystem.** The design donor throughout (CDT, Reply caps, CAmkES/Microkit for
  component composition, CapDL for trusted init in milestone 22). Code reuse is nil (C,
  and their proofs are theirs), but every capability-model fork should check what seL4 did
  first. Already the habit; see the roadmap's Prior-art sections.
- **Fuchsia.** The closest general-purpose capability OS. Design prior art for milestone 23
  (capability routing, component manifests) and for the "what would growing up look like"
  question in notes/why-not-general-purpose.md.

## Calls on the record

- **`pci` crate (2026-07-27): built, over `pci_types`.** Made during the overnight PCIe run
  without the survey pass this note requires; recorded the next day in DECISIONS §18 with the
  honest split: the host-testable closure shape and the witness tests favored building, the
  rule favored reuse, and the omission of the pass is the actual lesson. Kept.

- **`crickerfs` (2026-07-28): kept, over `tar-no-std`/ustar.** Weighed retroactively on
  Chris's question, with the swap costed out. The format predates this note (a learning-era
  artifact), but the keep decision is a fresh application of the rule: the kernel parses the
  initrd itself, so the parser sits **inside the TCB**, which is exactly where the rule says
  build; 263 lines we wrote and test beat someone else's header arithmetic on boot input.
  The swap's real benefits (standard tooling, `tar tvf` inspection, ~260 lines retired) are
  ergonomic, its cost is churn across the kernel boot path, four userspace binaries, and
  every proof-of-real-bytes assertion (six sites assert the `cricker-` magic; tar has no
  magic at offset 0). Revisit at **milestone 22** (trusted init): the initrd format must
  change there anyway to carry measurements or signatures, and adopting ustar as the
  container is nearly free inside a redesign that is happening regardless. Inspectability,
  the one benefit worth having now, is purchasable as a ~30-line `xtask initrd-ls` without
  touching the format.

## The convention

Every milestone design block in design/roadmap.md carries a **"Prior art"** section. The
convention, from here on: that section covers *reuse* too, answering three questions before
building: is there **code to use**, a **design to copy**, or a **mistake to avoid**, in the
ecosystems above? The build-vs-reuse call gets recorded with its reason, the same as any
decision. When the answer is "build", say why; when it is "port", the two guards above
still apply.
