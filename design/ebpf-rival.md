# The rival worth understanding, not building

eBPF is the strongest competing answer to the question this whole architecture asks: safe kernel
extension through *verification* rather than *isolation*, with no IPC cost. Worth reading as the
other fork. It does not undercut the thesis so much as relocate the cost: the eBPF verifier is itself
a large, subtle, repeatedly-CVE'd component, so "the verifier is the TCB" is its version of the
problem, not an escape from it. No milestone; a reading item.
