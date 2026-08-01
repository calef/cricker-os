# The RISC-V arch tests: closing a parity gap in the suite, not in the kernel

`kernel/src/arch/aarch64/` carried 21 unit tests. `kernel/src/arch/riscv64/` carried none, across
the same three files (`mmu.rs`, `timer.rs`, `exceptions.rs`). Both ISAs booted the same suite and
both were green, so nothing looked wrong. But the tests that existed on one side and not the other
are exactly the ones about *the things the two ISAs do differently*, which is where a port is most
likely to be subtly wrong. DECISIONS §19 makes parity a gate; the gate was being applied to the
kernel's capabilities and not to the suite that proves them.

This note records what was translated, what has no RISC-V analogue, how each new test was proved
able to fail, and what the exercise found.

**It found three defects.** That is the argument for the whole lane, and it is worth stating before
anything else, because "write the missing tests" reads like bookkeeping until the tests refuse to
pass.

## BUGS: what the missing tests were hiding

### 1. The timer ran at 80 Hz and said it was running at 100 Hz

`timer::tick` re-armed with `sbi_set_timer(now() + interval())`. `now()` is read inside the handler,
after the trap entry and after the SBI `ecall` round trip to OpenSBI, so every period ran long by
however much that cost, and the lateness was never recovered.

This is the same bug aarch64 shipped and then measured, and the aarch64 module header has documented
it since: `CNTV_TVAL_EL0` (relative) gave ~70 Hz against a configured 100 Hz, and `CNTV_CVAL_EL0`
(absolute, on a fixed grid) fixed it. RISC-V had the same defect for a different reason. SBI's
`set_timer` takes an absolute deadline but is **write-only**: there is no register to read the
previous deadline back from, the way `CVAL` can be read back. So the grid has to be kept in
software, and the easy thing to do instead is re-arm from the clock.

Measured, by reverting the fix and running `ticks_arrive_at_the_configured_rate`:

```
timer drift: 20 ticks in 25 periods
```

**80 Hz delivered against 100 Hz configured. One preemption in five, gone**, silently, on every
RISC-V boot since milestone 20. The fix is `DEADLINE[hart]`, an absolute deadline kept per hart and
advanced by exactly one interval, with the same drop-a-tick safety valve aarch64 has for the case
where the next deadline is already in the past.

### 2. `missed_ticks()` was a stub returning 0, defended by a backwards argument

The old comment said a missed tick was not a meaningful idea on this ISA, "since SBI set_timer
re-arms from `now`, so a late handler simply spaces the next tick out rather than dropping a count".
That is true and it is the wrong conclusion: re-arming from `now` is what made the count
*unmeasurable*, not what made it *unnecessary*. The cost of holding a lock across a tick deadline is
just as real here as on aarch64; there was simply no instrument.

With the grid in place the count is real, and `a_long_critical_section_costs_a_tick` is what makes
the price of `IrqSafeMutex` visible on the second ISA.

### 3. `TICKS` was one global counter for a per-hart timer

aarch64's `TICKS` is `[AtomicU64; MAX_CPUS]`, and the comment on it explains why (DECISIONS §11): a
single counter is advanced by *every* core's tick, so "holding a lock stops my ticks" stops being
observable, because masking interrupts masks only the holding core.

RISC-V had one global. Under `-smp 4` the other three harts kept counting into the same word.
Measured, by reverting to the global and running `holding_a_lock_masks_the_timer`: **61 ticks landed
during a critical section that masked this hart's interrupts.** The reasoning aarch64 had written
down was simply never carried across.

None of the three is a capability gap, which is why the parity record (notes/riscv-parity-scope.md)
did not catch them: every one of them is a *quality of implementation* property that only a test
looks at.

## EXAMPLES: translating a property instead of transliterating a test

The rule was: read the aarch64 test to find what property it asserts, then assert that property the
RISC-V way. The mechanisms are different almost everywhere.

| aarch64 mechanism | RISC-V mechanism |
|---|---|
| `TTBR0` (user) + `TTBR1` (kernel), two registers | one `satp`; every process root carries a copy of the kernel's top-level entries |
| four translation levels, 48-bit VA | Sv39: three levels, 39-bit VA, high half is 256 GiB |
| `VBAR_EL1`, a 16-slot table, 2048-byte aligned | `stvec`, one entry point, 4-byte aligned, low 2 bits are the MODE field |
| `tlbi` + `dsb`, hardware broadcast to other cores | `sfence.vma`, local, plus an SBI RFENCE IPI to the other harts |
| generic timer, `CNTV_CVAL_EL0` readable back | SBI TIME `set_timer`, write-only, so the deadline lives in software |
| PXN and UXN: two independent execute-never bits | one `X` bit whose privilege is decided by the `U` bit |
| an architectural device memory type in the descriptor | no such field in base Sv39; an RSW software bit stands in |
| EL1, with `CurrentEL` readable | S-mode, with no way to read the current privilege at all |

Three of those differences change what a test can honestly claim, and each is written into the test's
own doc comment rather than left for a reader to discover:

- **`kernel_text_is_executable_and_not_writable`** keeps aarch64's `!is_user_executable` assertion,
  but it is **not carrying weight on Sv39**. One `X` bit plus the `U` bit means `Sv39::leaf_flags`
  reports kernel-exec or user-exec and never both, so given kernel-exec the other cannot fail. It
  stays because the property is what the kernel cares about and the format under it may change
  (Svpbmt, or a future format with separate bits).

- **`the_uart_is_mapped_as_device_memory`** is a weaker claim here. On aarch64 the device type is an
  architectural PTE field, and getting it wrong lets the CPU speculatively read MMIO, which for a
  UART FIFO register *consumes the byte*. Base Sv39 has no such field, so `paging::Sv39` carries the
  flag in an RSW software bit and QEMU's `virt` derives the real memory type from the physical
  address. The test asserts the kernel's bookkeeping is right, not that the hardware was told. It
  still catches the mistake worth catching (mapping the UART with `Flags::kernel_data()`), which is
  what the mutation below confirms.

- **`a_low_address_does_not_translate_when_no_process_is_running`** is a *stronger* claim here, and
  the test addresses had to be chosen carefully to keep it one. `Mapper::translate` returns `None`
  for anything outside its half **before walking a single entry**, so a test address above 2^38
  would pass without reading any page table and prove nothing. All three addresses are inside Sv39's
  low half on purpose.

### The one property that could not be translated honestly

aarch64's `asid_tagging_keeps_address_spaces_apart_without_flushes` proves two things: distinct
spaces get distinct ASIDs, **and** switching between them flushes nothing, so their TLB entries
coexist.

The second half is not true on RISC-V today, because `write_satp` follows every `csrw satp` with a
bare `sfence.vma`, which discards the whole TLB. So the ASID is composed into `satp` and then made
irrelevant on the very next instruction. An isolation test written here would pass with the ASID
tagging removed entirely, which makes it a test that cannot fail for its stated reason.

What shipped instead is `the_satp_carries_the_address_spaces_asid`, which proves the half that is
real: distinct nonzero ASIDs, placed at bits 59:44 where the hardware reads them, without disturbing
the MODE field above or the root PPN below (they are packed with no slack, so a shift that is off by
four lands in one or the other).

**Follow-up, not done here:** dropping the unconditional `sfence.vma` from `write_satp` would make
the aarch64 property true on RISC-V and would remove a full TLB flush from every context switch,
which is a real cost the context-switch benchmark is currently paying. It is a change to the
switching model rather than a test fix (it needs the ASID recycling and rollover story checked
against `crates/asid`), so it is a decision to take deliberately, not a side effect of a test lane.

## What has no RISC-V analogue

Three tests were considered and one of them genuinely does not exist here.

- **`el1_runs_on_sp_el1` has no analogue.** At EL1, `sp` means `SP_EL1` or `SP_EL0` depending on
  `SPSel`, so the kernel and a user trap frame can be made to share one stack pointer register by
  accident. RISC-V has one `sp` per hart and no `SPSel`; the question cannot be asked. The
  *adjacent* RISC-V hazard is real but different (the kernel must recover its own `tp` and stack
  after a U-mode trap, through `sscratch` and the per-hart `TrapStash`), and it already has its
  witnesses in `arch::percpu_matches_hart` and the SMP suite.

- **`running_at_el1` has no direct analogue, and needs none.** RISC-V deliberately gives S-mode no
  way to read its own privilege level: there is no `CurrentEL`. But
  `breakpoint_is_caught_and_execution_resumes` proves it anyway, and its doc comment says so. The
  breakpoint arm of the dispatcher is guarded by `!from_user` (`sstatus.SPP == 1`), so `BRK_COUNT`
  cannot move unless the trap came from S-mode; and the trap could not have reached our handler at
  all from M-mode, where `mtvec` (OpenSBI's) owns it. A count that went up is a machine executing in
  S-mode. `main.rs` carried a comment promising this analogue "arrives with the RISC-V boot path";
  the boot path arrived at milestone 20 and the comment outlived it, so it now points here.

- **`asid_tagging_keeps_address_spaces_apart_without_flushes` is half-translatable**, covered above.

That is the whole residue: **one test with no analogue, one property that is half-true and says so.**
The gap was never a scoping decision, and it does not need one now.

## How each test was proved able to fail

A test that cannot fail is worse than no test, because it reads as coverage. Every one of the 22 was
run against a deliberately broken kernel and confirmed red, one at a time (a failing assertion ends
the run, so mutations cannot be batched). The mutations are kept in the lane's scratch driver rather
than in the tree; the table is the record.

**Nineteen were proved by breaking the code they check.** Three could not be, and the reason is the
same on both ISAs.

| test | mutation | what it printed |
|---|---|---|
| `mmu_is_enabled` | `is_enabled` reads bits 59:56 instead of `satp.MODE` at 63:60 | `satp.MODE reads as Bare` |
| `a_low_address_does_not_translate...` | map one page in the kernel root's low half, as a surviving identity map would | `0x1000 translates through the live satp` |
| `the_guard_page_is_a_hole` | `map_everything` maps the guard page (and `verify`'s new assertion dropped) | `the guard page IS mapped` |
| `kernel_text_is_executable_and_not_writable` | `Sv39::attrs` always sets `W` | `.text is WRITABLE: W^X is broken` |
| `kernel_rodata_is_read_only_and_not_executable` | `attrs` always sets `W`; and separately, always sets `X` | `.rodata is writable` / `.rodata is executable` |
| `the_stack_is_writable_and_not_executable` | `attrs` always sets `X` | `the stack is EXECUTABLE` |
| `the_uart_is_mapped_as_device_memory` | UART mapped with `Flags::kernel_data()` | `the UART is not device memory` |
| `an_allocated_frame_is_reachable_through_the_mmu` | `attrs` always sets `X` | `RAM is executable` |
| `unmap_invalidates_the_tlb` | the local `sfence.vma` removed from `flush_tlb` | read back `0xaaaa...` through a VA remapped to the frame holding `0xbbbb...` |
| `the_kernel_mapper_refuses_to_overwrite` | `map_page` unmaps first instead of refusing | `left: Ok(())` |
| `the_satp_carries_the_address_spaces_asid` | `ttbr0_value` drops the ASID term | `two live spaces share an ASID` (both 0) |
| `the_timer_is_ticking` | `tick` does not count | `no timer interrupt in three tick periods` |
| `ticks_arrive_at_the_configured_rate` | the original relative re-arm restored | `20 ticks in 25 periods` |
| `the_handler_keeps_up_when_no_lock_is_held` | the handler spins for two intervals | missed 20 |
| `a_long_critical_section_costs_a_tick` | the original relative re-arm restored (no missed accounting) | `did NOT lose a tick` |
| `uptime_advances_monotonically` | `uptime_ms` divides by `TIMEBASE_HZ` instead of `TIMEBASE_HZ / 1000` | `uptime went backwards or stalled: 0 -> 0` |
| `holding_a_lock_masks_the_timer` | `TICKS` back to one global counter | 61 ticks landed inside the critical section |
| `breakpoint_is_caught_and_execution_resumes` | the dispatcher handles the breakpoint but does not record it | `the handler didn't run, but we resumed anyway?` |
| `registers_survive_a_trap` | the dispatcher zeroes `frame.x[18]`, as a wrong trap.s offset would | `the trap frame scrambled a register` |

**The three that are true by construction on a machine that booted**, and are marked as such:

| test | why the property cannot be broken | how the assertion was proved live |
|---|---|---|
| `the_kernel_lives_in_the_high_half` | the kernel is *linked* high; a low-linked kernel is a different port, not a mutation | raised the comparison bar 128 GiB; it read the real PC (`0xffffffc080279d62`) and failed |
| `the_direct_map_reaches_physical_memory` | the kernel reaches every page table through the direct map, so any break kills the boot | expected `pa + 4096`; it read the real translation and failed |
| `stvec_points_at_our_trap_entry` | a wrong `stvec` means the first trap never returns | expected `trap_entry + 4`; it read the real CSR and failed |

The same three are by-construction on aarch64 for the same reasons. They are worth keeping for the
same reason aarch64 keeps them: they cost nothing, and the day one of them *can* fail is the day
someone changed the linker script or the boot path, which is exactly when you want the assertion
sitting there. Saying which tests are in this class is the honest part; dropping them would only
hide it.

`mmu_is_enabled` sits between the two groups and its doc comment says so: the machine cannot reach
the assertion without paging, so the *property* is boot-implied, but the accessor's field extraction
is real and breakable, which is what the mutation exercised.

## Counts

Measured on this lane's branch, before and after. The two suite totals span the whole tree, so a
concurrent lane adding tests moves them; the arch-directory counts are this change's own.

| | before | after |
|---|---|---|
| `arch/aarch64` unit tests | 21 | 21 |
| `arch/riscv64` unit tests | 0 | 22 |
| riscv64 kernel suite | 127 | 149 |
| aarch64 kernel suite | 181 | 181 |

Twenty-two rather than twenty-one: the twelve MMU, six timer and three exceptions twins, plus
`the_satp_carries_the_address_spaces_asid`, which is the salvageable half of an aarch64 test that
lives in `kernel::user::tests`.

## What was still aarch64-only, and what porting it took (DONE, milestone 19, 2026-07-31)

The plan below was written when `kernel::user::tests` was ~30 tests that did not run on RISC-V. It
has been executed, and it held up, so it is kept as written with the outcome noted at each step.
The full record of what changed and what is still gated is in notes/riscv-parity-scope.md.

The module comment blamed the tests: every one drove a **hand-written aarch64 program** through
`exec`, and several read aarch64 fault registers (`ESR`, `FAR`) directly. That was two separable
problems, and only the first was large.

1. **The programs.** The `user_program!` macro assembled aarch64 machine code inline (`hello`,
   `outlaw`, `spin`, `forged_elf`, and friends, from milestone 7a). Each would need a RISC-V
   twin: not a translation of 37 instructions, but 37 *programs*, each hand-assembled.

   **But it should not be done that way.** `riscv_virtio_tests` already showed the alternative: load
   a real ELF from the initrd and drive that. The programs are tiny and their behaviours are
   ordinary (return, syscall twice, read a forbidden address, spin forever, die on purpose), so they
   are `user/` binaries or entry roles of one binary, built by the existing toolchain for both
   targets. That turns "hand-write riscv machine code" into "add roles to a test binary", and it
   deletes the aarch64 hand-assembly on the way rather than duplicating it.

   **Outcome:** exactly this, and cheaper than sized. All five hand-assembled programs (three
   aarch64, two RISC-V) are gone, along with `exec`, the one-page raw-machine-code loader they
   needed, plus three duplicate copies of a nine-instruction stub the supervision tests already kept
   a portable pair of. The replacements are one new binary (`user/src/outlaw.rs`, two roles) and the
   `spinner` milestone 24 had already built. The trick that made one program serve two ISAs was
   passing the forbidden **address** in a register instead of baking it into the code.

2. **The fault-register assertions.** Roughly a third of the tests assert on `ESR`/`FAR` (that a
   fault was a *permission* fault and not a translation fault, that `FAR` named the exact address).
   Those assertions are the *point* of those tests, and they are genuinely arch-specific. The
   portable shape is a small arch-level accessor pair, something like "the last user fault's kind
   and address", implemented from `ESR`/`FAR` on aarch64 and from `scause`/`stval` on RISC-V. RISC-V
   already records the same facts (`USER_FAULTS`, and `user_fault` prints `sepc` and `stval`); it
   just has no readable last-fault record for a test to inspect.

   **Outcome:** `arch::UserFault`, and one correction to the sizing above. RISC-V records the
   *address* but genuinely cannot report the *kind*: `scause` has one code per access kind and no
   field says why the walk refused, so permission-versus-translation is not a fact this ISA hands
   over at all. The classifier walks the page tables the hardware just walked to derive it, which is
   an inference rather than a measurement, and the code says so in a `BUGS` note. Two compositor
   assertions that had been gated off RISC-V for want of a last-fault address came along for free.

**The third thing, which the plan did not anticipate.** The module was blocked on a stale comment as
much as on machine code. `hello` carries the milestone 7-19 role catalogue and xtask called it
"aarch64-wired"; three quarters of that sentence had been false for some time, and the last quarter
was six syscalls hand-rolled in aarch64 `asm!` that `user_rt` had had portable versions of since
19f.6. Deleting the duplicates was the whole port for roughly twenty of the tests. Sizing a job from
what the comments say it needs is how an afternoon's work stays undone for a year.

Sizing it against this project's own units (notes/riscv-parity-scope.md's S/M/L): **M**, one to two
sessions, split into commits that are each independently useful. That was right.

Worth saying plainly, and it was true then: the *portable* coverage in that module was already green
on both ISAs. What RISC-V was missing was the userspace-boundary assertions, not the capability model.

## See also

- notes/riscv-port.md, the port itself and the `arch/` contract both ISAs implement.
- notes/riscv-parity-scope.md, the capability-level parity record (workstreams A-E, all done).
- notes/page-tables.md, why device memory typing matters, and what base Sv39 does not give us.
- notes/locking.md and DECISIONS §9, the deadlock `holding_a_lock_masks_the_timer` exists to
  prevent.
