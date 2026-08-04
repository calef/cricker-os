# 66. A refusal is a non-zero status, and not the same one an error gets

**Status: DECIDED.**

Decided 2026-08-03, for milestone 67's `&&` and `$?`. `swish` refuses constantly and by design, so
"what status does a refusal produce" is a claim about the capability model rather than a detail of the
shell.

## It is non-zero

`rm secret && echo gone` must not echo. The intent did not happen, and the next command in a sequence
would be operating on a resource that was never granted. A refusal that read as success would make
`&&` unsafe in precisely the situation this shell exists to demonstrate.

## It is not the status an error gets, because the remedy differs

`Refusal::NoSuchCapability`'s own documentation states the distinction better than a summary would:

> not "permission denied" but "there is nothing you hold that could grant this."

That is an **absence**, not a failed check. A program that ran and failed needs a fix. A refusal needs
a **grant**. Collapsing both into an undifferentiated non-zero throws away the one distinction this
shell was built to make visible, and a script that wants to react to "you were not given this" would
have no way to.

## POSIX already drew this line, so most of it is adoption rather than invention

The convention exists and predates us:

| status | meaning |
|---|---|
| **127** | command not found |
| **126** | found but not executable |
| 128+n | killed by signal n |

POSIX reserves the top of the range for **"the shell could not run this"**, separately from "the
program ran and failed". So `Refusal::NoSuchProgram` maps to **127** and that is not a new convention,
it is the existing one. Only the capability refusal needs a value, and **126** is the closest in
spirit: found it, could not run it for you.

This is CLAUDE.md's guard rail applied to a protocol rather than a name. A convention a reader already
knows from outside costs them nothing, and inventing a parallel one costs them everything they knew.

## One value, not a range

Each `Refusal` variant does **not** get its own status. That is speculative generality with no
consumer: a script that needs to know *which* refusal can read the message, and `&&` needs only
zero versus non-zero. One value for "refused for want of authority" plus 127 for "no such program"
covers `&&` and `$?` completely. If a second consumer ever needs finer detail, it will say what shape
it needs.

## This does NOT contradict §65, and the difference is checkable

§65 says a refusal that is not passive cannot be used as a question. Read quickly, that forbids
`cmd || fallback` and `if cmd; then` in this shell, and it does not.

**The kernel's `reclaim_region` refusal is destructive**: it arms §16's kill on every live thread in
the region and then returns `Err`, so asking destroys the answer. **The shell's refusal is pure**:
`plan()` takes a `RunSpec`, a `Holdings` and an `Expansion`, and returns `Result<Endowment, Refusal>`
having done nothing at all. Nothing is spawned, nothing is granted, no state moves.

So probing is legitimate here and was not there, and the rule that separates them is not "refusals are
safe to ask" but **"an operation whose failure path mutates state is not a predicate"**. The shell's
does not, so it is one.

Worth stating explicitly because the two sections are adjacent, both are about refusals, and they
would otherwise read as a contradiction that a future reader has to resolve from first principles.
