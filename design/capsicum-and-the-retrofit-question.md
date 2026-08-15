# Capsicum, and the retrofit question

FreeBSD's Capsicum is the most important prior art this project has, because it is the strongest
existing answer to **"why build a new OS for capabilities instead of adding them to one that
exists?"** That question will be asked, and this is the material for answering it honestly.

Recorded in `design/` because it frames an argument rather than documenting something we built.

## What Capsicum is

Watson, Anderson, Laurie and Kennaway, *Capsicum: practical capabilities for UNIX*, USENIX Security
2010. Shipped in FreeBSD since 9.0. The mechanism:

- **`cap_enter()`** puts a process into **capability mode**, which removes access to *global
  namespaces*: no `open()` by absolute path, no PID namespace, no sysctl by name, no `/dev`.
- **File descriptors become capabilities.** `cap_rights_limit()` attenuates one: this fd may read
  but not write, may not seek, may not `fstat`, with companions for ioctls and fcntls.
- **`openat()` against a directory fd becomes the only way to reach a file**, which makes a directory
  fd a directory capability.

Converted applications include `tcpdump`, `dhclient`, `kdump`, `rwhod`, and Chromium's FreeBSD
sandbox.

## Why it matters most: it converged on our model from the opposite direction

`openat()` plus a directory descriptor **is** milestone 47's directory-capability model. Capsicum
arrived there while retrofitting a forty-year-old Unix; we arrived there building from the first
instruction. **Agreement between two projects with opposite constraints is worth far more than
agreement between two that share premises**, and it should be cited whenever the directory-capability
design needs defending.

The second convergence is sharper. Capability mode broke `getaddrinfo()`, because DNS resolution
needs `/etc/resolv.conf` and there is no path to open it with. FreeBSD's answer is **`libcasper`**:
a helper process that retains the wider authority and serves a narrow interface to the sandboxed
program. That is **exactly the caretaker pattern**: `fs_file_caretaker` holding a directory
capability and exporting one file, `c_confiner` holding a region and confining a C component. Two
projects, opposite directions, same answer.

## The argument cuts both ways, and that is the useful part

Capsicum is **the best argument against building nife**: you can have capabilities without a
new operating system, on a production OS, today, running real software.

It is also **the best evidence for building it**, and the evidence is the Capsicum authors' own. Their
experience reports document that converting applications is laborious, because Unix APIs assume
ambient authority *everywhere*: `getaddrinfo` is the canonical case but far from the only one. Every
converted program needs a helper service, an audit, and a reorganisation into "acquire authority, then
drop it".

So the honest framing, and the one to use when asked:

> Capsicum proves the model works on real software. It also proves that retrofitting it costs a
> per-application conversion effort, forever, because the surrounding API assumes the thing the model
> removes. The interesting question a fresh system answers is what the API looks like when ambient
> authority was never available to assume, and that is what we are measuring.

## The cautionary data point: CloudABI

Ed Schouten's CloudABI went further: a POSIX-*like* runtime with **no** ambient authority at all,
where a process starts with exactly the descriptors it was given. Closest thing to our model that has
ever shipped as a general runtime.

**It is deprecated and effectively dead.** Not because the model failed technically, but because it
needed software recompiled against it, the ecosystem stayed small, and maintenance did not survive its
maintainer moving on.

That is a real caution rather than a footnote, and it belongs on the record next to the validation:
**a pure capability system has an adoption problem**, and the deeper the purity the worse it gets. It
is also precisely why DECISIONS §14 frames this project as a **demonstrator** rather than something
seeking adoption. CloudABI is what happens when a pure capability runtime is judged as a product; we
should not accidentally start making product claims for one.

## Jails are a different mechanism, and the distinction is worth keeping precise

FreeBSD jails (2000, thirteen years before Docker) partition the namespace: a jailed process sees a
subset of the filesystem, its own network stack, its own process table. But **within** a jail,
authority is ambient: root in a jail is root over everything in the jail.

That is isolation by **partitioning**, not by **designation**. It is genuinely useful and it is
categorically not this. "FreeBSD already has containers" is a common way to misread the comparison,
and the answer is that containers bound *how much* ambient authority a process has, while capabilities
remove the ambience.

## What Capsicum does better than us, stated plainly

It sandboxes real, useful programs on a production operating system that people run in anger. We
confine `worker`, `budgeter`, `heeder`, `spinner`, a C component, and a filesystem we vendored.

That gap is the honest one, and closing part of it is what milestones 53 to 55 are for: a Time Machine
backup server is a real workload with a real user, and it is the first thing here that could be
compared with a Capsicum-sandboxed daemon on its merits rather than on architecture diagrams.

## See also

- DECISIONS §14 (the project's direction) for why adoption is not the goal.
- DECISIONS §31 (the foreign-language seam) and the `fs_file_caretaker` / `c_confiner` sources for
  the caretaker pattern `libcasper` independently arrived at.
- Milestone 47's `PATH` and absolute-paths sections, where the "no global namespace to search" result
  is the same one `cap_enter()` produces by removal.
