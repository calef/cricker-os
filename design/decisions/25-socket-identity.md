# 25. Socket identity: a socket id in phase one, minted endpoints as the tracked later step

**Status: DECIDED.**

**Decided 2026-07-28 (Chris), resolving the milestone 30 piece-3 fork (notes/net.md).** A process
holds one `Stack` endpoint capability; opening a connection yields a **socket id**, a small
integer carried in the message words, with the per-connection **shared frame** as the real granted
resource. Chosen because milestone 27's `std::net` PAL wants a file-descriptor-like handle, and
because a minted kernel endpoint per TCP connection spends a bounded kernel object per socket
(the endpoint budget is finite, as the 27+28 merge demonstrated).

**The purer capability story is deferred, not rejected, and Chris's direction is explicit: come
back for it.** Minted-endpoint-per-socket makes a socket an unforgeable, individually delegatable,
individually revocable object. **Triggers to build:** (1) a socket needs to be delegated to a
third process (the id is meaningless outside the holder of the stack cap, which is a feature until
it is a limit); (2) milestone 23's hot-swap work wants per-connection revocation during a net
server swap. The contract keeps the shared frame as the per-connection resource precisely so this
migration changes the handle, not the data plane.
