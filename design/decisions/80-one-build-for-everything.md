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

### That 17% is a thin number, and the honest one is larger

Chris pushed on it the same day: what fraction of the crates serve user programs? Measured from the
dependency graph rather than by inspection, **30 of 43 crates are reachable from `user`** and 33 from
`kernel`. None is reachable from neither.

| | crates | lines | share of `crates/` |
|---|---|---|---|
| user only | 10 | 11,325 | 23% |
| kernel only | 13 | 12,782 | 26% |
| **shared** | **20** | **24,702** | **51%** |

Attributed generously, userland is `user/`'s 17,991 plus its exclusive crates, **29,316 lines against
the kernel's 52,683**, so roughly **36% of the owned tree rather than 17%**. The smaller figure counts
only `user/src/`, which is the thin IO shell of programs whose logic was deliberately lifted into
crates so it could be host-tested and Kani-reachable. **This project's own method is what makes the
naive measurement misleading**, and the correction belongs here rather than in a conversation.

**It does not change the conclusion, and the reason is the 51%.** A build split has to cut somewhere,
and the natural line, "userland builds separately", runs straight through 20 crates both sides use,
`abi` among them. That is not a seam; it is the thing holding the syscall agreement together at rung
one. The split would run *across* the shared code rather than *between* the two halves.

The user-only list is instructive about what would be orphaned: `supervision_proto`,
`system_initializer`, `swap_proto`, `virtio`, `c_seam`. Those are the spawn path, the init path and
the driver transport. **Userland-only by reachability, load-bearing by function.**

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
milestone 99 (`git` on cricker-os) and milestone 66 (Vaultwarden). What they need is what a split build
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

- **Userland growing past the kernel.** Compare like with like: 29,316 against 52,683 today, counting
  each side's exclusive crates. `user/src/` alone against all of `crates/` is the wrong comparison and
  was the first one taken here.
- **The shared 51% shrinking.** If the 20 crates both sides use ever became a small fraction, a real
  seam would exist where today there is none. That is the structural signal, and a better one than
  any line count.

### The trend this architecture predicts, and the number that would show it

Chris, 2026-08-05: a microkernel should see the kernel plateau while userland grows, so the ratio
drifts toward less kernel as functionality is added. That is right, and it is close to the
definition: the kernel holds mechanisms and userspace holds policy, so new capability is new
userspace.

**Measured against the wrong number it will look false for a while**, which is why the number is
recorded here. `kernel/` is 39,901 lines, and it decomposes:

| | lines |
|---|---|
| `arch/`, two ISAs | 7,854 (19%) |
| test files | 10,907 (27%) |
| remainder | 21,140, of which 7,887 (37%) are comments |

So the kernel's actual logic is roughly **13,000 lines**, which is in seL4's neighbourhood and a
plausible plateau size. The comment density is deliberate (CLAUDE.md).

**The confound is parity, and it is a multiplier on the kernel specifically.** Rule 1 puts all
architecture-specific code under `kernel/src/arch/` and §19 makes parity a gate, so **x86_64, which is
declared and not started, will add a third expression of the same mechanisms**. Several thousand
kernel lines representing no new capability at all. The ratio can therefore drift toward *more*
kernel while the microkernel thesis stays exactly intact.

**So the number to watch is non-arch, non-test kernel logic.** That is the thing that should plateau.
Arch lines are a parity tax, not kernel growth, and counting them into the trend makes the
architecture look like it is failing when it is doing what it claims.

The kernel is also not finished growing: milestone 106's deadline wait, milestone 108's frame
capabilities for drivers, and above all **reclaiming what a finished service held** (milestone 107's
binding constraint, and a §16 fork about what a capability *is* when its holder dies) are all
mechanisms, so they belong in the kernel *by* the thesis rather than despite it.

Where the prediction is clearest is what comes after: milestone 55's SMB3 server, 66's Vaultwarden,
99's git, 40's documentation service, 65's secrets service. Every one is pure userspace, and 55 alone
is described as probably the largest single piece of work in the project. **None of them adds a kernel
line**, and that is the claim the architecture is making.
- **A single program large enough to dominate a rebuild.** gitoxide or Vaultwarden could do this
  alone, which is another reason the ownership threshold and the size threshold arrive together.
- **`build + test` becoming the long pole.** It is 3 minutes against `verify`'s 28 to 36. If milestone
  119's sharding succeeds, this is the number to re-check, because the bottleneck moving is exactly
  when this decision deserves re-opening.
