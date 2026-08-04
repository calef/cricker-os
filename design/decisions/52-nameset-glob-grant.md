# 52. A set of names is a namespace, and that is how a glob is granted

**Status: DECIDED.**

Milestone 47's globbing lane. `crates/fs_proto`'s `nameset` and `grant` modules,
`user/src/fs_nameset_caretaker.rs`, `kernel::user::SetGrant`, `grant_plan`'s expander. See
`notes/glob-grant.md`.

`rm old.txt` grants the directory holding one name. **`rm *.txt` grants that directory attenuated to
the names the pattern matched**, served by a caretaker that refuses everything else.

## The over-grant this closes, and the one it does not

Without this, expanding a glob in the shell and passing the results as arguments hands the child
authority over the whole directory, and the pattern is then only a filter the child could ignore.
The attenuation makes the pattern *structural*: the confined program cannot name a file the pattern
did not match, because the caretaker serving it has never heard of one.

Stated honestly, because §42's habit applies to authority too: **this is closed for a pattern
operand and remains open for a literal one.** A single name still travels through
`fs_subtree_caretaker`, which grants the directory. That is the current state, not a design
position.

## A set does not fit in registers, so the grant becomes a frame

A name rides in two `START` argument words. A set cannot, at any plausible size. So the set is
encoded into **a frame of its own, mapped read-only** into the caretaker, which copies it to a local
before doing anything else.

This is the honest place for `ARG_MAX` to reappear, and it reappears as a different thing.
`fs_proto::nameset::MAX_NAMES = 8` is not a buffer limit, it is **the size of a capability**: how much
authority one grant can carry. Unix's `ARG_MAX` is a limit on how much *data* you can pass, which is
why it produces `xargs`; ours limits how much *authority* you can package, which is a bound worth
having rather than one to engineer around. The shell refuses an over-long expansion at the prompt
(`grant_plan::Refusal::TooManyNames`), so an over-long set arriving in the kernel means the wiring built a
grant no command line could have expressed, and it panics.

## The zero-length name means "the operand is your namespace"

`fs_proto::grant::WHOLE_NAMESPACE` is a name of length zero. A program started with it learns its
operands by **enumerating the capability it holds**, which reveals exactly what the command line
already printed and nothing more.

The set frame needs none of the ordering care the shared page needs, and the reason is structural
rather than careful coding: it is written **before the caretaker is spawned**, into a frame nothing
else has ever held, so there is no reader to race.

## One consequence worth keeping

A set namespace is **fixed**, so a sweep over it is one listing with no rounds. The recursive walk in
`rm -r` must re-read from cursor 0 because removing a name shifts a real directory's entries; re-reading
a set would instead hand the loop names this run has already taken away. The difference is a fact
about the two namespaces, not an optimization in one of them.

## BUGS

- **Eight names.** A glob matching a ninth is refused at the prompt rather than truncated, which is
  the right failure, but it is a low ceiling for a real directory. Raising it is a frame-size
  question, not a design question.
- **A set is granted, never revoked.** Like every grant here, the caretaker holds it until it exits.
