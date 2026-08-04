# 56. The filesystem contract describes its own verbs, so a caretaker is written once

**Status: DECIDED.**

Milestone 61, 2026-08-01. `fs_proto::verb::{Operand, Verb, TABLE, of}`, and the dispatch in all
three caretakers. See `notes/dir-capability.md` and `notes/glob-grant.md`.

**Each verb declares its own shape, and a caretaker's dispatch is a lookup rather than a `match`.**
A row carries the opcode, its `Operand` (`None`, `Name`, `Payload`, `Rename`), whether it carries a
second word, whether it mints a handle, and the rights it requires.

## What this is actually fixing

Not duplication. **The failure mode.** Before this, a caretaker was a hand-written `match` over the
verb, so a verb added to the contract was simply *absent* from a caretaker and the capability quietly
was not there. That is exactly how milestone 57's extended attributes reached none of the three, and
nothing failed when it happened.

A verb with no row is now a **build error** (`const assert!` on the table's length, plus opcode
order). The absent case became the loud case, which is the whole deliverable.

It closed a second instance nobody had noticed: `fs_nameset_caretaker` now filters on
`takes_name()`, so a new name-taking verb is filtered **by construction**. Before, one added to the
contract would have arrived **unfiltered**, which is the same shape as the xattr gap and would have
been an authority hole rather than a missing feature.

## `needs_all` and `needs_any` are two fields because rights are not one question

`Rights::allows` is "all of". `OPEN` needs *read* **or** write, and folding that into an all-of check
would refuse capabilities the FS server itself accepts. Two fields, because a single one cannot
express both without lying about one of them.

## `Operand::Name` versus `Operand::Payload` is the load-bearing distinction

An **attribute** name is a `Payload`, not a `Name`. So the four extended-attribute verbs pass the
name-set filter **without being matched against it**, which is correct: the set designates files, and
an attribute name is not a file. Getting it wrong in either direction is a real bug. As `Name`, a
program could not read its own file's attributes. As `Name` on something that *is* a file operand,
the filter would be bypassed and the set would stop meaning anything.

## The table shares dispatch and never attenuation

**This is the constraint the milestone was rewritten around**, and it survived contact.

The milestone was first drafted as "one caretaker parameterized by how the namespace is described",
which is refuted in `swarden.rs`'s own header under a section answering that exact question.
`fs_subtree_caretaker` performs **no checks at all** by design: one `OPENDIR` at startup, the server
intersects the granted rights and mints a restricted handle, and everything after is reached through
it. A name filter would trade that program's single strongest property for a switch.

So the table is dispatch only. `fs_subtree_caretaker` consults **no policy rows**, and still performs
no checks, because a lookup that picks a length or a zero cannot refuse anything.

`fs_file_caretaker` keeps its own per-verb `POLICY` (`Local`, `Forward`, `Refused(errno)`), because
its refusals carry meaning a boolean would destroy: `CREATE` answers `ENOTDIR` and not `EACCES`,
since a file capability **is not a directory**, so the request does not mean anything rather than
meaning something refused. A shared "allowed / not allowed" column would have flattened that.

## BUGS

- **`fs_file_caretaker` answers `EBADF` to every directory verb except `CREATE`.** Writing the rows
  down is what exposed it: all seven fell through one `_ =>` arm shared with "you named a handle I
  never minted", so two different statements came out as one word. `ENOTDIR` is very likely right for
  all of them by exactly the argument `CREATE` makes. **Behaviour was deliberately preserved**,
  because changing it changes the wire, and it is recorded here and in `notes/grant-expression.md`
  rather than fixed quietly.
- **A wrong row is wrong in three programs at once.** The mitigation is that the table is pure data
  in a host-testable crate, so five host tests reach it directly, which a `no_std` `match` could not.
