# Tearing down an address space

There are two ways to give back the page-table frames an address space accumulated, and
which one is right depends entirely on **whether the whole space is going away or just part
of it.** cricker-os needs only the first, and that is worth understanding rather than
assuming.

## The two strategies

**Walk-and-reclaim.** On each `unmap`, after clearing the L3 leaf, walk back *up* the tree:
any table that just became empty gets freed and its parent's pointer cleared, stopping at the
first table a sibling still needs. This is incremental and precise. It is also more work per
call: a tree-emptiness check at each level, and a per-leaf TLB invalidation.

**Record-all-frames.** The address space keeps a list of *every* frame the mapper ever handed
out, leaves and intermediate tables alike. To tear it down, free the whole list and throw away
the root. No walk. No `unmap`. One TLB/ASID flush covers everything at the end.

## Why cricker-os uses record-all-frames

Because **an address space dies all at once.** A process exits, and its entire `TTBR0` world
is gone. When you are freeing *everything*, incremental reclamation buys you nothing: you are
not keeping any of it, so there is no emptiness to track and no order to respect. You just free
the set.

`user::AddressSpace` (`kernel/src/user.rs`) is exactly this: a `root: Frame` and a
`frames: Vec<Frame>` that records what the mapper allocated. Its `Drop` frees the lot. The test
`a_dead_user_thread_frees_its_whole_address_space` asserts that four user address spaces come
and go with **zero** net frames leaked.

Record-all-frames is strictly cheaper here than walk-and-reclaim: O(frames), no tree traversal,
no per-leaf TLB dance. So a reclaiming `unmap` was considered and deliberately not built. There
is nothing for it to do that this doesn't already do better.

## The opposite case, in the same kernel

Kernel thread stacks do the reverse, and on purpose. A dead thread's stack VA range is
**reused** by the next thread, so `KernelStack`'s teardown frees the leaf mappings but
**keeps** the intermediate tables. Reclaiming them would just force the next thread to
reallocate them. The test `a_finished_thread_is_reaped_and_its_memory_returned` pins this: a
second batch of eight threads must cost exactly zero frames.

So the two in-kernel teardown behaviors are not inconsistent, they are two answers to two
questions:

| Situation | Keep tables? | Mechanism |
|---|---|---|
| A thread stack area, reused by the next thread | **Yes** | free leaves, keep tables |
| A whole user address space, gone for good | free everything | record-all-frames |

## When walk-and-reclaim *would* be the right tool

**Partial unmap of a live address space**: a running process that unmaps a region it will not
reuse (Unix `munmap`). There you cannot free-everything (the space lives on) and you cannot
keep-everything (the tables would accumulate), so you must reclaim precisely the tables that
region emptied. We have no `munmap` and no plan for one, which is exactly why the reclaiming
primitive is unneeded.

## The meta-lesson

`paging::unmap` carried a `TODO (milestone 7): or every process exit leaks its page tables` for
a long time. It was true of the *primitive* in isolation and false of the *kernel*, which had
already solved teardown a better way. A later reader (a code survey, then us) read the TODO as a
live bug and nearly "fixed" an unused method into existence.

**A TODO that outlives the decision that resolved it becomes misinformation.** The fix was to
correct the comment, not to add code. See DECISIONS.md §4 on not building the abstraction before
the requirement.

---

*Add to this file as new teardown concepts come up.*
