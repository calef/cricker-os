# 45. A nife partition is `EC5CC08B-D749-4434-AC38-A274C50385BA`, and that never changes

**Status: DECIDED.**

*Exercised by milestone 120 (2026-08-15): the OS renamed from cricker-os to nife, and this GUID did not move, exactly as this decision's title promised. Images written before the rename remain nife partitions by the only identity that counts.*

Milestone 57 needed a GPT partition **type** GUID for a nife data partition (a RedoxFS volume,
§34). There is no registry to apply to and no upstream value to adopt: RedoxFS ships none, and Redox
itself does not define one. So one was generated, version 4, on 2026-07-30:

```
EC5CC08B-D749-4434-AC38-A274C50385BA      gpt::guid::types::CRICKER_DATA
```

**It is random on purpose.** A type GUID's entire job is to not collide with anybody else's, and the
only mechanism for that without a registry is 122 bits of randomness. A memorable value spelling
something in hex would be a worse GUID for exactly the reason it would be a nicer string.

**It never changes, and that is the decision rather than the number.** A disk written by one release
and read by another has only this integer to agree on. More pointedly, it is load-bearing for the
recovery story milestone 57 exists to make credible: *the board is dead, can I get my data?* The
answer is "plug the drive into a Mac or a Linux box and run the host tool", and the first step of
that is `sgdisk -p` showing a partition whose type you recognise. A type GUID that drifted between
releases would make a backup readable only by the software that wrote it, which is the definition of
not a backup.

Two consequences worth writing down:

- **Changing it is a format break**, on the same footing as changing the on-disk filesystem layout,
  and would need a migration rather than a version bump.
- **A `--typecode` on a future `mkpart` must accept an arbitrary GUID**, not a table of ours. A
  partitioning tool that could only write nife partitions would be useless for the actual job
  (setting up a drive that also carries an EFI system partition and a Linux filesystem).

`crates/gpt` names ten other type GUIDs beside it, and every one of them was read back out of
`sgdisk` on the machine rather than typed from memory. Four are pinned by the committed fixtures. See
notes/gpt.md.
