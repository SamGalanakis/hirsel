# hirsel-core-rt

Memory operations for FORM v1, independent of the old runtime. Logical charges
are supplied by the emitter using `hirsel-core-layout::charge`. Allocation
refusal returns the plain C `Capacity` record. Physical host exhaustion is a
run stop, as specified by FAIL-15.

The C declarations are in `include/hirsel_core_rt.h`. Pointers and recipes come
from our emitters. They must obey the ownership, layout and threading contracts
documented on the Rust entry points. No runtime validation of compiler output
is performed. Task/provider code must serialize cross-task funding operations
in its recorded event order; the funding forest itself is synchronized.

Bookkeeping precedes each own payload. Counted payloads start with the FORM
u64 count at offset zero and user data at offset eight. Native padding and
bookkeeping never contribute to logical charges.
