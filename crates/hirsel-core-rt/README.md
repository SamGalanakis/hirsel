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

`//:hirsel-core-rt-archives` builds `libhirsel_core_rt.a` for x86-64 and AArch64,
using this crate's normal Cargo dependencies. Each target also has an `x86_64`
or `aarch64` subtarget. This namespace has no old-runtime feature variants or
old-language startup training. The emitters supply resumable mechanical drop
glue through the ABI module.

`MEASUREMENTS.md` records the instruction samples and missed targets. Reproduce
them with `python3 crates/hirsel-core-rt/measure.py --out <new-directory>` from
the repo root. `--abi-only` instead compiles and runs `tests/abi.c` against the
published header and a static archive. Ordinary Rust tests run through Buck.
