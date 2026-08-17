# 94. What may live in a userspace library, and what must be per-binary

**Status: DECIDED.**

**Decided 2026-08-17 (calef), from milestone 130's finding rather than from a proposal.** The lane
that unified the panic handler found a recorded constraint half of which had gone stale, kept the
sound half, and asked whether the correction belonged here. It does, for a reason narrower than the
finding: **the constraint is one every future userspace crate meets, and it was discoverable only by
reading `user_rt`'s header.**

## The claim that was half wrong

`crates/user_rt` has recorded since 19f.6 that a `#[panic_handler]` is not an item in it, because a
panic handler is **per-final-binary**: exactly one may exist in a linked program, so an item in the
library would force it on every program that links the crate and collide with any program that wants
its own (`hello` does). **That half is correct and stays.**

Beside it sat the sentence that went stale: *"each binary keeps its own one-line handler; it is
trivial."* By the time anyone counted, the handler was **fifteen lines with two `unsafe` blocks and
two `// SAFETY:` comments**, the trap instruction was inlined at **48 sites in 7 variants**, and 58
handlers existed across `user/`, `crates/` and `fs_server/`.

**The constraint was right and the inference from it was not.** A *handler* cannot live in a library.
The *trap* always could.

## The rule

**Ask what the language forces to be per-binary, and lift everything else.** A property that attaches
to the final link (`#[panic_handler]`, `#[global_allocator]`, `_start`) genuinely cannot be an item in
a shared crate. The mechanism it is built out of usually can, and the two are separable:

- `user_rt::trap()` holds the instruction, which is ordinary code.
- `user_rt::panic_handler!()` expands to the handler in the binary, which is where the language
  requires it to be.

The linking property survives untouched, and `user_rt`'s claim to be "the one place in userspace that
names the two ABIs" becomes true, where 48 files had been falsifying it.

**The tell that the inference is wrong rather than the constraint**: a per-binary item whose body is
copied verbatim into every binary. If the body is identical everywhere, it is not per-binary; only its
*declaration* is. Copying it is asserting the same invariant N times by hand, which §61 already says
a `// SAFETY:` comment must never be: it is an assertion, not a formality, and 88 hand-written
assertions of one invariant is 87 chances to write it differently.

**And one of them was different.** `terminal_sink_caretaker` called `exit()` and never trapped, so a
panic there reported `EVENT_EXIT` where every other program reports `EVENT_FAULT` (§26): **a
supervisor would have been told a panicking program finished cleanly.** Latent rather than live only
because that adapter is built with `fault: None`; it would have become real the day someone endowed
that spawn site. That is the cost of the copy, found by counting the copies.

## What this does not license

**It is not "lift everything into `user_rt`".** Device helpers stay in the drivers that own them: a
UART `putc` and echo logic are not runtime, they are the program, and the crate's header says so.
The test is whether the thing is *runtime* — something every program needs in the same form — not
whether it is *duplicated*. Two programs sharing an accident is not a library.

**And it does not make a macro the default answer.** `panic_handler!()` is a macro because the
language requires an item in the binary and a macro is the only way to place one from a library. Where
no such requirement exists, a function is the answer and a macro is machinery.

## BUGS

- **Nothing gates this.** A future crate can hand-roll a trap again, and `script/lint` will not
  notice. The 48 sites were found by a lane that went looking, and the same lane could have found
  them a month later. A grep for the trap instruction outside `user_rt` is a plausible check and is
  not built.
- **The rule names three per-binary properties and there may be more.** `#[panic_handler]`,
  `#[global_allocator]` and `_start` are the ones this tree has met. A fourth arriving does not make
  this section wrong, but it will not be listed here.
