# 33. A compositor: one screen, mutually distrusting clients

**Status: BUILT.**

**In brief.** **Built (2026-07-29), both ISAs**, rung two of the display ladder: `compositor` multiplexing one screen among three clients, each holding a capability to its own surface; software composition honouring a damage rectangle; input routed by capability over the terminal contract's `OP_BYTES`; enumeration and screenshots as read-only mappings rather than verbs. No new syscall and no new method. notes/compositor.md, DECISIONS §33

**Why it matters.** **the canonical multiplexer of one device among distrusting clients**, and the thesis at its sharpest: a client is *proved* unable to reach its neighbour's pixels even when handed the exact address of them, and the compositor holds no authorization code because the authority is a mapping rather than a message. It also found the kernel's one missing primitive (no wait-any), recorded as a fork
