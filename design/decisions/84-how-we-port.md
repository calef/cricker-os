# 84. How we port: prefer software that has already dropped ambient authority

**Status: DECIDED.** calef, 2026-08-13: *"Porting to rust is wrong if we port ambient authority along
the way... I'd prefer to avoid reconstructing every application. That doesn't build a community."*

§82 names the risk in its qualifications: cheap porting makes bad porting cheap, and an ecosystem of
ports that quietly reconstruct ambient authority leaves the box holding nothing. This turns that
warning into a rule with an order of preference and a number to watch.

## The failure this prevents

A program written for a Unix assumes it may open an absolute path, walk upward, and find its
configuration wherever the convention says it lives. Rewriting that program in Rust removes the
memory-safety class and **changes nothing about its authority model.** If the port succeeds and the
assumptions come with it, the result is a memory-safe program that still wants the whole disk, and
the capability system underneath it is decoration.

So a port is not judged by whether it runs. It is judged by **how narrow a grant it runs under**, and
that has to be true of the port rather than promised by the platform.

## What the Rust rewrite community does and does not give us

There is a large, active effort rewriting C tools in Rust: coreutils, and the ecosystem of
replacements like `ripgrep`, `fd`, `eza` and their neighbours. It is genuinely valuable and it is
**not aligned with this thesis**, because those projects optimise for *compatibility*. A faithful
Rust `ls` is faithful about walking a global filesystem too.

Stated plainly so nobody has to rediscover it: **that community gives us the memory-safety half for
free and the authority half not at all.**

## The community that is aligned, and it is the same design as ours

**`cap-std`** (Bytecode Alliance) is a capability-oriented version of the Rust standard library. Code
takes a `Dir` to open files from, declaring its intent to open only what is underneath, and `Dir`
refuses `..`, symlinks and absolute paths that would escape. Its filesystem API deliberately follows
**WASI's** sandboxing model, and under WASI it becomes a *thinner* layer than `libstd`, because there
are no absolute paths to handle.

**Milestone 47 gave this system absolute paths, and the analogy survives it** (2026-08-18). A `/`
here names the root of the caller's *own* namespace rather than a global one, so it is a position
inside the capability the program already holds; two shells in two subtrees resolve the same token
to two different files. That is the case `cap-std` refuses because it cannot express it, and the
reason it can be expressed here is that the resolver is client-side and a grant records a position.
So the sentence above stays true of WASI and is no longer true of us, which is the better direction
for the comparison to fail in.

That is this system's model, arrived at independently and for other reasons. `Dir` is a directory
capability. `one_name` in `patches/std-nife` is what `Dir` enforces. A program already written
against `cap-std`, or already targeting WASI preopens, **has had the de-ambienting work done by
somebody else**, funded by somebody else, for reasons that have nothing to do with us.

That is the work to leverage, and it is a different corpus from the Rust rewrites above.

## The order of preference

1. **Software already written against `cap-std` or WASI preopens.** It holds handles rather than
   naming paths, so the port is an integration rather than a redesign.
2. **A faithful Rust implementation plus an upstreamable patch** that makes it accept a directory
   handle instead of discovering paths. The patch has to be defensible *on Linux*, because a patch
   only we want is a patch we carry forever.
3. **Ideas rather than implementation**, where the design is ambient at its core. `ps`, `top`, `sudo`
   and a system-wide package manager cannot be de-ambiented, because enumerating and acting on
   everything **is** what they are. Take the interface, write the thing.
4. **Reconstruction, as a last resort and rarely.** Every instance spends the community argument.

## The measure

§82 names grant width. This adds the second number, and together they are the pair that decides
whether the thesis is holding:

- **Grant width.** How narrow an authority does the ported program actually run under? This measures
  whether confinement is real.
- **Patch burden.** What fraction of ported software runs unmodified, and of the remainder, what
  fraction of the patches are accepted upstream? This measures whether a community is possible.

**A patch we carry forever is the tax Capsicum's authors documented**, paid per application and never
retired, and carrying many of them is how a project becomes a fork of the world with one maintainer.
CloudABI is what that looks like at the end: a technically sound capability runtime that died because
its ecosystem needed adapting and the adapting had one owner.

calef's constraint is therefore an engineering constraint rather than a preference.
**Reconstruction does not build a community**, and a demonstrator with no community is a demonstrator
nobody continues.

## BUGS

- **`cap-std` cannot run here today, so preference 1 currently has nothing to prefer.** The gap is
  narrower than an earlier draft of this entry claimed, and the narrower version is worth stating
  exactly. **Descent is built**: `fs_proto`'s `OPENDIR` resolves one name under a directory handle,
  requires `DESCEND`, and attenuates so no descendant exceeds its ancestor, and `rm`, `swish` and both
  `fs_*_caretaker` programs walk with it. What is missing is one layer up: the `std` PAL calls
  `OPENDIR` only inside `read_dir`, against `ROOT`, and drops the handle, so there is no directory
  object for `cap-std`'s `Dir` to bind to. Milestone 122 is that binding, and until it lands this
  preference is policy about work that cannot start.
- **The `cap-primitives` backend is unmeasured in the way that matters.** Nobody here has read it, so
  whether its Unix and Windows split offers a seam a third backend can use, or whether that needs
  upstream work, is unknown. A backend would also be a **carried patch** until this system is
  something Bytecode Alliance has reason to care about, which is precisely the tax this section says
  to count.
- **This is not a WASI runtime and nothing here promises WASI compatibility.** The alignment claimed
  above is of *design shape*, not of ABI, and reading it as "we can run WASI binaries" would be a
  serious misunderstanding.
- **Nothing measures patch burden.** As with §82's grant width, the number that would settle the
  argument has no tooling behind it, so the claim that ports stay close to upstream is an intention.
- **The tiers are judgment and no corpus has been classified.** Which real programs fall in 1, 2, 3
  or 4 is unmeasured, and the honest answer today is that we do not know the shape of the
  distribution.
