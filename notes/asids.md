# ASIDs: tagged address spaces

*(Milestone 15. The mechanism behind `crates/asid` and the disappearance of the context-switch
TLB flush.)*

## The problem

The TLB caches translations. Two processes both have a `0x40_0000`, mapping different frames.
Until this milestone the TLB could not tell them apart, because every user mapping was *global*:
its entries matched under any address space. The only safe switch was `tlbi vmalle1is`, discard
every EL1 translation on every core, the kernel's included, and then re-walk everything from
cold. Correct, and the code said so honestly, and slow in exactly the way that matters on the
hot path of a microkernel: switches are what an IPC-heavy system does all day.

## The mechanism, in three parts

1. **The `nG` bit** (descriptor bit 11, "not-global"): a TLB entry made from an `nG` mapping is
   tagged with the ASID that was live during the walk, and matches lookups only under that same
   ASID. Every *user* constructor in `paging` now sets it. Kernel mappings stay global on
   purpose: the high half is identical for everyone, and tagging it would waste TLB entries
   re-caching the same kernel translation per process.
2. **The ASID rides in `TTBR0_EL1`'s top bits**, written together with the table root as one
   composed value (`mmu::ttbr0_value`). Installing an address space *is* installing its tag.
3. **Each address space owns one ASID for life** (`crates/asid`): allocated at creation, freed
   at teardown after `tlbi aside1is` has destroyed every entry wearing it. That flush-then-free
   order is the whole reuse contract, stated at the drop site.

The switch itself now flushes nothing. The old space's entries stop *matching* instead of being
destroyed; come back to that process later and its translations may still be warm. Invalidation
survives at exactly two targeted points: revocation flushes by VA across all ASIDs (a shared
page leaving every space), and teardown flushes by ASID (a space leaving the world).

## Why allocation is a bitmap, not Linux's generation scheme

The roadmap sketched "generation/rollover", the Linux design: processes outnumber ASIDs there,
so exhaustion bumps a generation, mass-flushes once, and reassigns lazily. Milestone 14 changed
the arithmetic under that sketch: concurrent address spaces are bounded (`MAX_SPACES` = 160,
enforced by the revocation registry), below even the smallest hardware ASID space (8-bit: 256).
The exhaustion case the generation machinery guards is unreachable here, and machinery whose
hard path can never be exercised honestly is machinery that rots. So: one bitmap, ASID 0 born
reserved for the kernel (the "nobody is home" reserved table runs as ASID 0 forever), 255
numbers for at most 160 spaces. If `MAX_SPACES` ever outgrows 255, the first answer is 16-bit
ASIDs (one TCR bit plus an ID-register check), not a new algorithm.

## What is proved, and what is witnessed

Three Kani harnesses in `crates/asid` (`script/verify`), the frontier crate
notes/verification.md predicted:

| Harness | Property |
|---|---|
| `the_kernel_asid_is_never_allocated` | no reachable state hands out ASID 0: a user space can never share the kernel's tag |
| `two_live_asids_are_distinct` | two live allocations never alias, from any state (sharing a tag would let the TLB serve one process the other's memory) |
| `free_releases_exactly_its_own_asid` | free clears its own bit and nothing else |

The flush-before-reuse half of the contract is kernel-side and runs on hardware, so it is
*witnessed* rather than proved: `asid_tagging_keeps_address_spaces_apart_without_flushes` maps
the same VA to different bytes in two spaces, switches between them with no flush, and demands
each space read its own byte. If `nG` were missing, the tag were not in TTBR0, or two spaces
shared an ASID, that test reads the wrong byte.
