# 55. The file behind a `>` is the shell itself, because one page cannot serve two clients

Milestone 50, finished 2026-08-01. `user/src/swish.rs`, `kernel::user::redirection_tests`. See
`notes/pipes.md`.

**A redirected program's output goes to a file the shell writes, not to an adapter process holding
that file.** This was not the plan, and the reason it changed is a property of §27's contract rather
than a preference.

## What forced it

**`fs_proto` shares one page between the FS server and its clients.** A client stages bytes or a
name into that page and *then* calls, so its use of the page **straddles the call boundary**. Two
client *processes* doing that concurrently race, and nothing in the contract orders them: there is no
lock, and a rendezvous on the FS endpoint cannot span a put-then-call pair.

The plan was for the shell to hold a filesystem and ask init to build a `sink.rs` file adapter per
redirection, giving two FS clients. That survives `date > out.txt`, where the shell touches no file
while the adapter writes. **It does not survive `ls > out.txt`**, which is exactly a line where the
shell must read the filesystem *while* the redirection is being written.

There is no ordering fix, and that is the part worth stating precisely: **there is no moment when
both parties are done.** `fs_service::wait_for_caretaker` already records the startup half of this
hazard; this is the steady-state half, and startup ordering cannot solve it.

## The answer

The shell already holds the directory capability, so it is the one process that can write the file
without opening a second session.

- **`>`**: the child keeps the shell's result endpoint in its output slot, which is exactly what an
  unredirected command has, and the shell drains it into a file instead of onto the terminal.
- **`<`**: the shell mints an endpoint, gives the head stage `READ`, and is itself the producer,
  which is the path a builtin producer already took.
- **`ls > out.txt` spawns no process at all.**

## Why this is the right shape and not a retreat

**It costs the milestone nothing**, which is the test.

What a redirected program holds is **unchanged**: one endpoint, `WRITE`, and no way to ask what is
behind it. §51's indifference claim survives intact, because it was always a claim about what the
*writer* holds and never about who implements the far end. And there is no change to
`grant_plan::spawnproto`, to init, or to the kernel.

The honest smaller claim: `>` still grants strictly less than Unix's fd 1, but the sentence is now
"the program holds an endpoint and **the shell** holds the file" rather than "an adapter holds the
file". `Sink::File` and `Source::File` stay in the manifest, because the declaration check still
needs them (`worker 9 > out.txt` is still refused as `NotAByteStream`); they simply need no
capability at the prompt.

**The adapter shape remains correct when the client is not the shell**, which is what `sink_tests`
measures against a real RedoxFS image. Nothing is deleted; one caller stopped needing it.

## The demonstration, because it is better than a constant

```text
$ ls
  globset/  motd  other/  redir/  rmtree/  scratch  sub/
$ ls > out.txt
$ wc < out.txt
  8 8 57
```

**Seven entries listed, eight lines counted**, because `>` truncates before the command runs, so
`ls` sees the file it is writing into. riscv64 reports 10 on its own image. Two different numbers,
each internally consistent, is stronger evidence than a pinned expectation could be.

## BUGS

- ~~**Nothing gates the `--features shell` boot.**~~ **CLOSED 2026-08-02 by `script/shell-check`**,
  which milestone 50 wrote when it finished `>>` (§59). It boots the interactive system on both ISAs,
  types five lines, and reads the answers, and it is the only thing in the tree that runs the real
  `system_initializer`. The entry stands as written otherwise, because its argument is what got it
  built: it bit twice in one session, both times presenting as *a boot that printed nothing*, and
  cost a manual bisect against a live prompt each time. The three causes were a virtual-address
  collision, init's sixteen-slot cspace overflowing, and four stack pages being too few. Milestone
  50 then hit the same shape a third time, which is what made the gate worth its cost rather than
  merely worth wanting.
- **The interactive shell holds the image root unnarrowed.** Defensible for the machine's own
  prompt, and still a default nobody decided on the record.
- **`rm` remains unreachable from the prompt.** The refusal is no longer "you hold no such
  capability"; what is missing is the `fs_subtree_caretaker` init would build per invocation, and
  init deletes its FS endpoint copy after building the shell, so that is the line that changes
  first.
- **Every byte crosses the shell's address space twice and nothing prices it.** No benchmark.
