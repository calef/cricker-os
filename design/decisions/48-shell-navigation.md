# 48. Navigation is the shell rebinding what it holds, and every shell has its own root

**Built 2026-07-31** (milestone 47). Concept note: notes/shell-navigation.md. Rests on §47's
directory-capability keystone.

`cd`, `pwd`, `ls`, `mkdir` and `rm` are **shell builtins, not programs**: the same category as
`caps`. They spawn nothing, need no grant, and confer no authority, because the shell is reading and
rebinding what it already holds. That also retires a worry raised while designing `ls`: a listing
*program* would be over-granted, holding the power to read everything it lists. It is not a program.

## The headline, and it is demonstrated rather than argued

**Two shells holding different subtrees cannot name each other's files**, not by policy, but because
no capability reaching them exists. `two_shells_with_different_roots_cannot_name_each_others_files`
runs the **real shell binary** in a scripted role on both ISAs. Neither shell is told which subtree it
holds; each tries `sub/inner` and `other/secret` and reports what it reached, so the property is read
off the *pair* rather than asserted by either. Falsified first: pointing the second shell at `sub`
fails with "it opened a file that exists only in the OTHER shell's root". A host check then reads the
post-run image for both subtrees from outside the guest entirely.

## `..` is clamped by not having anything to send

The shell holds a **stack of the directory capabilities it descended through**, one per level. `..`
pops one; at the root there is nothing to pop, so **no request is made at all**. The FS server would
also refuse `..` as a component, so the two mechanisms agree without either depending on the other,
and the clamp is not a string check that could be spelled around. A path is validated against a copy
of the position before anything is sent, so `cd ../../..` from one level down is refused whole and
moves nothing.

## The cwd stops at the process boundary, and the grant is a value

`plan_against` walks a leading path **once, at the prompt**, against the shell's position *now*, and
records where it landed in `FileGrant { dir: nav::Cwd, … }`. `dir` is a **value, not a pointer at the
shell**, so a later `cd` cannot change what an already-planned grant means (a host test moves the
shell afterwards and checks the grant did not follow). What stops a child re-resolving is that it
receives a capability to one file, no directory and no cwd: **there is nothing for a string to be
resolved against.** The convenience is the shell's; the authority is explicit. A child able to
re-resolve a name later would be ambient authority smuggled in through a convenience feature.

§27 stays intact: the resolver lives in the client, so the server still only ever sees one component
relative to a capability presented to it.

## `rm` is an unlink, and the first version was accidentally a revoke

Milestone 47 required these be distinguished: **unlink** removes a name while existing holders keep
reading (the atomic-replace and temp-file idioms depend on it); **revoke** kills the object and every
capability goes stale.

The first implementation was a revoke, and the machine said so: RedoxFS frees a node the instant its
last link goes, so a read after unlink got `ENOENT` from a deallocated node. **The engine already had
Unix's deferred delete (`on_open_node`/`on_close_node` and a release list), and nothing was using
it.** Registering on open and deregistering on close is what makes the verb an unlink; deleting either
half turns the test red.

The test also caught a gap in itself: with only the `open` registration removed it stayed green,
because it exercised the `create` path. It now goes through both doors. **A test that passes when half
the mechanism is deleted is the failure that matters.**

**Revoke is not offered**, for a structural reason rather than a scheduling one: it would mean
invalidating handles the FS server minted for clients it cannot enumerate, because the handle table is
per *server* (§47).

## Gaps reported rather than papered over

- **No `RMDIR`.** `mkdir` can make a directory no verb removes (`rm` answers `EISDIR`). Declined
  deliberately: **a verb that removes whatever it finds is how one word takes a subtree away.** It is
  the obvious next step, with `rm -r` behind it.
- **No verb reports what rights a handle carries**, so a shell must ask for exactly what it holds
  (`OPENDIR` refuses when the intersection is smaller than the request). The shell is told its rights
  at spawn; a program handed a directory by someone else can only be told out of band or probe.
- **The interactive prompt still holds no directory**, so at a keyboard all five say so truthfully.
  Wiring an FS service into the interactive boot is a wiring change, not a change here.
- Two shells run **sequentially**: the three processes in a caretaker chain share one page with the
  FS server, so two live clients would clobber each other's requests.
