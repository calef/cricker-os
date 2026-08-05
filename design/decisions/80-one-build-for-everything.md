# 80. One build for the kernel and everything that runs on it

**Status: DECIDED.** Raised by Chris on 2026-08-05 ("at what point is it working against us that all
the software gets built and tested with every change?"), measured the same day, and settled as: **keep
one build, and let the split fall out of running software this project did not write.**

Recorded because the question is a good one that will be asked again, and the answer depends on
numbers that will change.

## The measurement that settles it today

| area | lines |
|---|---|
| `crates/` | 48,809 |
| `kernel/` | 39,901 |
| **`user/`, all 54 programs** | **17,991** |
| `xtask/` | 4,735 |

**The programs are 17% of the tree**, and the expensive check does not touch them: `verify` runs
`cargo kani` over `crates/` only and costs 28 to 36 minutes, while `build + test` compiles everything
including all 54 programs in about three minutes.

So splitting the programs out would attack **the smallest slice of the cheapest check**. The
bottleneck is one prover (milestone 119), and no amount of build separation moves it.

## What one build buys, which is the part worth defending

`crates/abi` is shared, so **the compiler enforces that programs and the kernel agree about the
syscall surface.** That is rung one of the ladder: the wrong state is unrepresentable. Split the
build and that agreement falls to rung three or four, two artifacts asserting a shared layout with
nothing checking.

This tree already contains exactly one instance of that failure and documents it as a hazard.
`c_seam` states the address-space layout twice, once in Rust and once in `user/c/c_seam.c`, and rule
7 exists because of it. **A split build manufactures more of them**, and the failure mode is a
program writing to the wrong page, arbitrarily far from the edit.

## The threshold is ownership, not size

The question is not how many programs there are. It is **who wrote them.** The moment this project
runs a program it did not write, the build splits whether anyone plans it or not, because the source
is not here.

That moment is already on the roadmap: milestone 64 (enough `std` to run somebody else's crate),
milestone 99 (git, via gitoxide) and milestone 66 (Vaultwarden). What they need is what a split build
needs anyway, and the sequencing follows from that rather than from a size:

- a **versioned** syscall ABI, rather than one the compiler happens to agree on today;
- a **sysroot** to build against out of tree.

In-tree programs can stay in-tree indefinitely, because they are cheap. **Build the ABI-and-sysroot
story because milestone 64's ranked gap list demands it, and the split falls out.** Do not split
first to save three minutes.

## The coupling that does hurt today, and it is not the build

It is runtime. Every program in the initrd is a live process during tests, and that has already
caused failures twice: milestone 107's lane found the aarch64 boot **out of frames**, with ten
`net_stack` regions held for the whole boot because nothing reaps a net server, and milestone 67's
lane found `script/test` failing intermittently because **"a scripted-shell witness is not free"**.

**The pressure is not that we build too much; it is that we boot too much**, and nothing reclaims
what a finished service held. Milestone 107 already named that as the binding constraint on the whole
network line, and it is the lane worth spending on this pressure.

## What would change the answer

Stated so the next person can check rather than re-argue:

- **`user/` growing past roughly the size of `crates/`**, which would make the three-minute build a
  real number. It is 17,991 lines against 48,809 today.
- **A single program large enough to dominate a rebuild.** gitoxide or Vaultwarden could do this
  alone, which is another reason the ownership threshold and the size threshold arrive together.
- **`build + test` becoming the long pole.** It is 3 minutes against `verify`'s 28 to 36. If milestone
  119's sharding succeeds, this is the number to re-check, because the bottleneck moving is exactly
  when this decision deserves re-opening.
