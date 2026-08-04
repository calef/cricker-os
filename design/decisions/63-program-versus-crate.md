# 63. The line between a program and its crate is "does this need a capability"

Milestone 70 lifted `swish`'s logic into `crates/swish` and had to decide, function by function, what
went. The rule that fell out is the one to reuse: **logic that needs no capability goes in the crate;
anything that moves or exercises authority stays in the program.**

That is not a restatement of "pure versus impure". Matching a glob against a directory listing is
pure, but *reading* the directory needs a capability, so `expand` takes the directory read as a
callback and the matching moves out. The seam is authority, not IO, and in a capability system those
are the same question asked precisely.

Applied, it moved `route`, the pattern and expansion decisions, and every sentence the prompt prints,
with 33 host tests behind them. It left `builtin`, `dispatch_one`, `run`, `spawn`, `pipeline` and the
file sinks, all of which are capability movement. **The test is not whether a function is testable in
principle but whether the crate would have to be handed authority to test it.**

`coremark`, `line_editor`, `compositor` and now `swish` are each a crate and a program sharing a
name, which says exactly this: the crate is that program's logic, and the program is its authority.
