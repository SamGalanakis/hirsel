# Generic StaticFn mut calls

LANG-13 makes these calls follow direct-call modes. For each permutation of mut,
take and view, the handler changes Tx.writes from 1 to 3, and returns 8 or the
two-byte String: 38 or 32. Both String allocations are dropped before return.
The cross rows pass app's descriptors through lib's generic typed adapter.
The rigid mut T rows install 4+3=7 or the three-byte String. Two disjoint mutable
fields become 4 and 6, hence 46.

Each callee fails when its local Explode finalizer divides by zero; the primary cause uses that resource cell, leaving the task cell untouched (FAIL-16). Each caught failure has exactly one cause. Cell's finalizer fails with a second
cause if its last installed value is wrong: 5 before a write and during a failing
assignment RHS, 9 after a write. This checks T2.5/T3.1 without giving isolate a
mutable outer capture. Causes owns the failed resource cell until dropped; the reserved isolate cell is refunded because the task cell was not used (FAIL-16). Dropping Causes and the state resource gives used 0.

The two probe rows are runner-spec-1020 p04 and p08, preserved as source. Their main returns Ok if the transaction updated its mutable Tx once, and the generic dispatch delivered true, respectively. Wrappers return 0 for Ok, and 100+Exit.status for Error, all used 0. The original programs print no output.
