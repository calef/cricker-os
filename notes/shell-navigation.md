# Navigating a system with no global namespace (milestone 47's commands)

`cd`, `pwd`, `ls`, `mkdir`, `rm`, on a machine where there is no `/` to root anything in and no
ambient authority to reach with. The keystone that made these possible is [the directory
capability](dir-capability.md), which you should read first: the six-rung rights ladder, `OPENDIR`
handing back authority rather than bytes, and `fs_subtree_caretaker` as the subtree caretaker. That
note calls this part "the easy part once this exists", which was true of four of the five verbs.

The governing constraint is Chris's, and it is not a technical one: *"I hate Windows/DOS specifically
because they went differently than virtually every other OS I've used."* Gratuitous divergence taxes
every user forever. So the bar is not "is this more capability-pure", it is **"does the model
actually force this"**, and three divergences clear it. The rest of Unix's surface survives.

The pure half is `crates/grant_plan/src/nav.rs` (host-tested in milliseconds); the requests are
`user/src/swish.rs`; the wire verb `rm` needed is `fs_proto::fs::UNLINK` with `Server::unlink` behind
it; the guest proof is `kernel/src/user/shell_navigation_tests.rs` and the host half is
`xtask::shell_navigation_landed`.

## They are builtins, and that retires a worry rather than dodging one

Same category as `caps`: they spawn nothing, need no grant, and confer no new authority, because the
shell is **reading and rebinding a capability it already holds**. A working directory, in capability
terms, is a directory capability used as the default base for resolving names, and held by the shell
that is entirely legitimate, the same as its untyped budget.

The worry this retires was raised while designing `ls`: a listing *program* would have to be handed
the power to read everything it lists, which is the over-granting this whole milestone exists to
refuse. It is not a program. Nothing is granted, so nothing is over-granted.

What is bad about Unix's cwd is three specific things, and `cd` is none of them:

1. **Children inherit it silently**, so every process gets a starting point nobody granted it.
2. **Relative paths resolve implicitly**, so a program's reach depends on invisible state.
3. **`..` walks out**, so the cwd bounds nothing.

## The three earned divergences

- **No absolute paths.** There is no namespace to root one in. The refusal is that the name **cannot
  be expressed**, not that a permission was checked, which is why the `std` PAL answers
  `InvalidFilename` rather than `PermissionDenied` and why `grant_plan::Refusal::NoAbsolutePath` is its
  own variant rather than a shape complaint. Plan 9 kept the syntax by making `/` the root of *your*
  namespace; that is the roadmap's recommendation for later, and until it is built the syntax is
  refused rather than quietly reinterpreted as relative.
- **`..` stops at your root**, and it is worth being precise about *why*, because "we check for `..`"
  would be a worse answer. The shell holds a **stack of the directory capabilities it descended
  through**, one per level. `..` pops one. At the root there is nothing to pop, so nothing is sent
  and there is no check to get wrong. The FS server would refuse the name anyway (`..` is not a
  component `check_component` accepts), so the two mechanisms agree without either relying on the
  other. Chroot's shape, arrived at from the other direction.
- **`pwd` is relative to your root**, because naming anything above it implies a namespace that does
  not exist.

One divergence that is *not* earned and was not taken: `cd` with no operand. Unix goes to `$HOME`;
there is no environment here (that is out of this lane's scope), so bare `cd` goes to **your root**,
which is the one distinguished place a shell has and is exactly what it was granted.

## The cwd stops at the process boundary

This is the rule most likely to be got wrong, and the place it is made true is `grant_plan::FileGrant`.

`wc report.txt` resolves the name against the shell's position **at the moment the grant is made**.
`plan_against` walks any leading path once, there, and records where it landed *as a value*:

```rust
pub struct FileGrant<'a> {
    pub dir: nav::Cwd,   // where the name was resolved, fixed at plan time
    pub name: &'a [u8],  // always a single component
    pub writable: bool,
}
```

The child receives a capability to that one file. It has no cwd, inherits nothing, and cannot
re-resolve anything: **the convenience is the shell's, the authority is explicit.** A child that
could re-resolve a name later would have ambient authority smuggled in through a string.

That `dir` is a value and not a pointer to the shell's position is what makes it structural rather
than a promise, and there is a host test that moves the shell afterwards and checks the grant did not
follow. `a_name_is_resolved_against_the_shells_position_at_grant_time` is the whole rule in one test.

A consequence worth naming: **the resolver lives in the client**, which is the roadmap's
recommendation over putting a path walker in the server. The FS server still only ever sees a single
component relative to a capability presented to it, so §27's rule is intact, and a path is N
`OPENDIR`s from the shell rather than one walk in a server.

## `rm` separates two things Unix conflated, and one of them is not offered

- **Unlink (this is what `rm` does).** The *name* goes away. A handle already open on the file keeps
  reading, and the bytes are still there. Atomic replace and the temp-file idiom both depend on it.
- **Revoke.** The object dies and every capability to it goes stale. **Not offered**, and the shell
  says so rather than letting one word mean either.

Why revoke is absent is a fact about the contract rather than a preference: it would mean
invalidating handles the FS server minted for clients **it cannot enumerate**. The handle table is
per *server* (that is [dir-capability.md](dir-capability.md)'s structural finding), and the server
does not track which client holds which handle. §13 revokes frames and §16 revokes objects because
the kernel owns those tables; nothing owns this one that way yet. `crates/slots`' generational names
would make a revoked handle fail safely once something does.

### Unlink was not free, and the first version of it was a revoke

RedoxFS frees a node the instant its last link goes, so `remove_node` plus a handle table gave a verb
that removed the *object*: the read that followed an unlink returned `ENOENT` from a deallocated
node. The engine has the right mechanism (`on_open_node`/`on_close_node` and a release list, which is
Unix's deferred delete), and nothing was using it, because our handle table is ours.

So `Server::open_file_at` and `Server::create_file_at` now register the node as open and
`Server::close` deregisters it, and **that pairing is what makes this verb an unlink**. The sabotage
check is direct: delete either registration and
`unlink_takes_the_name_and_leaves_the_object_for_whoever_holds_it` goes red. It caught something the
first time it was run, too: with only the `open` registration deleted the test stayed green, because
it exercised only the `create` path, so it now goes through both doors.

The cost is stated where it is paid: **a handle nobody closes pins the node for the life of the
server**, exactly as a leaked fd does on Unix, and this table outlives its clients.

### `rm` refuses a directory, and there is no `rmdir`

`UNLINK` answers `EISDIR` for a directory, which is POSIX's own answer. There is no `RMDIR` verb on
this contract, so **`mkdir` can make a directory that nothing here can take away.** That is a real
asymmetry and it is declared rather than papered over: a single verb that removed whatever it found
would be how one word takes a subtree away from someone who typed it by accident, and adding `RMDIR`
was a contract change this lane did not need to make its own builtins work. It is the obvious next
thing, with `rm -r` behind it, and both belong to whoever picks up recursive removal.

## Two shells, two roots, and neither can name the other's files

The headline, and it is proven with the **real shell binary**: `user/src/swish.rs` grew a role that
reads a script instead of a keyboard, holding a `fs_subtree_caretaker`'s narrowed endpoint where the
interactive one holds a terminal. So what the guest test exercises is what the prompt exercises, not
a reimplementation of it, and the thing being confined is a shell.

```text
  /            motd  scratch  sub/  other/
  /sub         inner  deeper/       <- shell A is rooted here
  /other       secret               <- shell B is rooted here
```

**Neither shell is told which subtree it holds.** Each tries to open `inner` and `secret` and reports
which it reached, so the property is read off the *pair* of reports rather than claimed by either.
Shell A reaches `inner` and not `secret`; shell B the reverse; and the same crossing holds for what
their listings contain, because a listing is a rendering of authority and a stranger in one is an
escape even though nothing was opened.

Not by policy. The FS server can reach both directories on any request it likes, and each caretaker
one hop up holds the whole image root. What stops each shell is that **no capability reaching the
other subtree exists in its cspace.**

The falsification is cheap and was run: point the second shell at `sub` as well, and
`two_shells_with_different_roots_cannot_name_each_others_files` fails with "it opened a file that
exists only in the OTHER shell's root".

The two runs are **sequential**, and that is a fact about the harness rather than about the model:
all three processes in a caretaker chain share one page with the FS server, so two live clients
would scribble over each other's requests. Being alive at the same instant would prove no more than
this does; they are separate processes with separate roots either way.

### And from outside the guest

`xtask::shell_navigation_landed` reads the image the run left behind with the pinned engine, for both
subtrees. In each: the name the shell kept is there, the directory it made is there, and **the name
it removed is not**. The pair is what makes it non-vacuous, since a shell that created nothing
satisfies "what it removed is gone" perfectly. And no name of either shell's making is in the image
root, which is the upward escape.

## What the shell had to be told, because nothing on the wire will say

**There is no verb that reports what a handle carries.** That matters here rather than being a
curiosity: `OPENDIR` refuses with `EPERM` when the intersection is smaller than the request
(deliberately, per §42's rule against silent degradation), so a shell that asked for `dir::ALL` from
a narrower capability could not `cd` at all. It has to ask for exactly what it holds.

So the shell is **told** its rights at spawn, in the same `fs_proto::grant::spec` word the
caretaker's own grant rides in. That works and it is a gap: a program handed a directory capability
by someone else has no way to ask what it can do with it, and its options are to be told out of band
or to probe by attempting things. Reported rather than fixed, because widening the contract to suit
a builtin is the wrong direction, and because "what does this capability carry" is a question `caps`
should be able to answer for every capability rather than a flag on one verb.

## BUGS

- **No `RMDIR`, so `mkdir` makes something no verb here removes.** Argued above.
- **No revocation**, and the reason is the per-server handle table. Argued above.
- **The interactive prompt still holds no directory**, so at a real keyboard every one of these five
  answers "this shell holds no directory capability; there is nothing here to name". That sentence is
  **true rather than a placeholder** (it is a fact about that shell's cspace, §27's amendment; the
  interactive boot has held a directory since milestone 50), and
  it is the same state the per-file grant has been in since milestone 31. The builtins are gated in
  the navigating role on both ISAs; what is missing is a boot that wires an FS service into the
  interactive system, which is a wiring change and not a change here.
- **The shell tracks 8 levels and 16-byte components.** Both are `grant_plan::nav`'s constants: the path
  stack is an array because this program has no allocator, and 16 bytes is `fs_proto::grant::MAX_NAME`
  (a name that could be `cd`'d into but not granted would be a place you can reach and cannot talk
  about). Deeper is `TooDeep`, refused rather than truncated, because a truncated path names a
  different directory.
- **`ls` reads a directory in rounds of 256 bytes** and re-reads it per round, so a name added or
  removed mid-listing can be seen twice or missed. That is `READDIR`'s own caveat
  ([dir-capability.md](dir-capability.md)), inherited rather than added.
- **The navigating shell needs two extra stack pages**, and the number is a measurement. With the one
  page `run` maps it overflowed by **192 bytes**, which presented as a data abort on its own `sp` and
  then as the 60-second lost-wakeup watchdog, because the kernel test was still waiting for a report
  from a process that had died. A shell carries a path stack, a parsed path and a listing buffer by
  value; 4 KiB is genuinely tight. Same discipline as the FS server's stack (§27): sized by what it
  did, not by what looked generous.

## EXAMPLES

At a prompt with a directory capability (the navigating role's script, spelled as you would type it):

```sh
$ pwd
  /
$ cd ..
  you are at your root; there is nothing above it to name
$ cd /other
  no absolute path: there is no namespace here to root one in, so that name cannot be expressed
$ ls
  deeper/
  inner
$ cd deeper
$ pwd
  /deeper
$ cd ..
$ mkdir logs
$ rm logs
  that is a directory
```

Unlink, and the thing that makes it an unlink:

```rust
// fs_server/src/lib.rs, and the test beside it
let h = srv.create_file_at(sub, "doomed").unwrap();
srv.write(h, 0, b"the bytes outlive the name").unwrap();
srv.unlink(sub, "doomed").unwrap();
assert!(srv.open_file_at(sub, "doomed").is_err());   // the name is gone
srv.read(h, 0, &mut buf).unwrap();                   // and the object is not
```

Resolve at grant time, from the shell's side:

```rust
// grant_plan: the grant records WHERE it was resolved, as a value
let g = plan_against(&run, prog, m, holds)?.file.unwrap();
// g.dir is the directory as it stood when the line was typed; a later `cd` cannot change it,
// and the child holds a capability to g.name in g.dir and no way to name anything else.
```

Run it:

```sh
script/test                  # both ISAs, plus the post-run host check on the image
cargo test -p grant_plan          # the cwd, the clamp, and resolve-at-grant-time
cargo test --manifest-path fs_server/Cargo.toml   # unlink, and that it is not a revoke
```
