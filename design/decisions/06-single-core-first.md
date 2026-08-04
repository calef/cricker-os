# 6. SMP: single-core, refactor when it hurts

**Status: SUPERSEDED BY 11.** (§11 is the scheduler rewrite this section named as its accepted cost.)

Boot CPU 0 only. Globals and a big lock are fine for now.

We explicitly considered shaping per-CPU data structures up front as cheap insurance,
and declined. Feeling the pain that created per-CPU structures is itself a legitimate
way to learn why they exist. Cost: a scheduler rewrite later. Accepted knowingly.
