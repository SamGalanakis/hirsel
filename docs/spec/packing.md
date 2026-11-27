# In-process code loading

*Specified 2026-11-27 (lane packing-p1-loader-3253, from the ratified in-process packing
proposal). Rules PACK-1 to PACK-10. This is slice P1, the loader core. Each rule says what is
built; the rest is specified, not built. The hot call ABI, the typed export adapter, `Held`
and the language surface of `core.code` over these rules are slice P2 and are not specified
here.*

A **pack host** is an OS process that runs many Places. It installs compiled Hirsel code
while it runs, calls it, replaces it and unloads it. This chapter defines the artifact it
installs (`.hirsel-mod`), the service that produces the artifact (`Compiler`), the memory
the code lives in (the code slab), the handle that keeps code alive (`Code[F]`) and the
tables that let a fault or a profile name loaded code.

Nothing here changes what a program means ([PROG-6](programs.md#prog-6-one-meaning)). A
Place that is loaded and a Place that is linked as a standalone executable
([EXEC-4](exec.md#exec-4-the-image-is-the-pinned-bytes)) are the same program.

**What isolates Places in one process.** Hirsel's checker, its bounds and overflow checks,
the stack watermark ([FAIL-17](failure.md)), byte and step bounds
([MEM-19](budgets.md#mem-19-limits), [MEM-30](budgets.md#mem-30-step-bounds)) and the closed
import mask (PACK-7). Places in one pool are not isolated from each other against
speculative side channels. Secrets never live in a shared pool, and Places that need
confidentiality from each other run in separate OS processes. A hardware fault in loaded
code stops the pool and quarantines the image (PACK-9).

## PACK-1: Trust

1. **A digest is identity, never authority.** The digest of a `.hirsel-mod` proves which
   bytes it is. It grants nothing. A pack host installs a module only when it holds an
   **admission** for it: a binding `module key -> module digest` (PACK-2 defines the key)
   that this host produced itself, from a reply of the `Compiler` service it started or was
   configured with. A host never accepts an admission made on another machine. Another host
   or a bucket may supply module bytes by digest; the local admission decides whether those
   bytes are installed.
2. **Module bytes are outside input.** A module arrives from a content store or a socket.
   The reader checks every length, offset, count, kind and identifier before the loader
   uses it, and refuses a module that fails a check (PACK-10). These checks guard against a
   damaged or foreign file. They are not a second check of the compiler.
3. **The compiler is trusted and the source is not.** The front end and checker are the
   security boundary for a program. The `Compiler` caps (PACK-3) bound what a hostile
   source can cost. They do not contain a faulty compiler.
4. **No linker in the serving host.** A pack host contains no `ld.lld`, no `dlopen` of
   Place code, no ELF or Mach-O reader and no compiler. It contains the `.hirsel-mod`
   reader and the loader.

## PACK-2: `.hirsel-mod` v1

A `.hirsel-mod` is one whole release image for one target: every function the release
reaches that the host image does not already hold, laid out and resolved as far as it can
be without knowing a load address. It is a cache artifact. It can always be rebuilt from
the release's CoreIR, and it is never an interchange format. A reader accepts version 1
only.

All integers are little-endian. The file holds no address. Every reference is an offset
into a section, an index into a table, or an identifier from a closed list.

### Header (232 bytes)

| offset | bytes | field | meaning |
|---:|---:|---|---|
| 0 | 8 | `magic` | `HIRMOD` `00` `01` |
| 8 | 4 | `target` | index of the target in the toolchain's target list |
| 12 | 1 | `backend` | `0` Cranelift, `1` LLVM |
| 13 | 3 | zero | |
| 16 | 8 | `policy` | policy bits, below |
| 24 | 32 | `abi` | runtime ABI digest: the runtime layouts and the import row table |
| 56 | 32 | `compiler` | compiler identity |
| 88 | 32 | `coreir` | digest of the release's CoreIR closure |
| 120 | 4 | `text_align` | power of two, at least 64 |
| 124 | 4 | `data_align` | power of two, at least 64 |
| 128 | 8 × 13 | section table | `(offset: u32, length: u32)` for each section below, in order |

Sections follow the header in table order with no gaps other than zero padding to 8 bytes,
and the last ends the file. Unknown policy bits, a nonzero reserved byte, an overlap, a gap
or trailing bytes refuse the module.

**Policy bits.** Bit 0: the code carries tape recording instrumentation. Bit 1: the code
masks indices against speculative reads. Every other bit is zero in version 1. A pool
installs a module only when its policy bits equal the pool's.

**Module key.** `key = SHA-256("hirsel.mod-key.v1", target, backend, policy, abi, compiler,
coreir)`. Two modules with one key are the same code. The backend is part of the key, so a
Cranelift image and an LLVM image of one release are different generations, and moving a
Place between them is an explicit `activate` (PACK-6).

**Module digest.** `SHA-256("hirsel.mod.v1", file bytes)`.

### Sections

| # | section | loaded | contents |
|---:|---|---|---|
| 0 | `text` | text band | machine code; frame pointers always kept |
| 1 | `data` | data band | literals, function and task descriptors, drop recipes, replay indexes, site pool words |
| 2 | `imports` | bound | import records |
| 3 | `text_fixups` | applied | fixup records whose site is in `text` |
| 4 | `data_fixups` | applied | fixup records whose site is in `data` |
| 5 | `entries` | resident | entry records |
| 6 | `spans` | no | function spans: `(start: u32, length: u32)`, sorted, disjoint |
| 7 | `cfi` | no | call frame information for final text offsets (PACK-9) |
| 8 | `sites` | no | the release's source position table |
| 9 | `names` | no | one name per span, for profiles and reports |
| 10 to 12 | reserved | | length zero in version 1 |

Loaded data is read-only once published. Version 1 has no writable data: a build that
needs it (coverage counters) is not loadable and the `Compiler` refuses it as `Unsupported`.

**Site words** are the release-local ordinals a static link of the same closure gives
([PROG-10](programs.md#prog-10-code-addresses)). The `Compiler` writes them as plain
integers. The loader never changes one, so `core.site_word` returns the same value loaded
or linked.

### Imports

An import record is 40 bytes: `kind: u32`, `row: u32`, `instance: [u8; 32]`.

| kind | names | binds to |
|---:|---|---|
| 0 | a runtime row: `row` indexes the closed row table of the module's `abi`; `instance` is zero | the row's address in the host |
| 1 | a host-image instance: `instance` is its instance digest; `row` is zero | the host image's copy of that instance |

Import *i* owns the 8-byte **slot** at `data[8 * i]`: the first `8 * imports` bytes of
`data` are zero in the file and the loader writes each bound address there. Code reaches an
import only by loading its slot. Text never holds a direct branch out of its own module, so
a module's text bytes do not depend on where the host image or the slab is mapped, and no
target needs a reach zone or a veneer.

The row table is closed. Each row has an identifier, a symbol, and one **class**:

| class | rows |
|---|---|
| `pure` | compute on their arguments only: byte copy and compare, hashing, checksums, signature verification |
| `metered` | change the calling task's own memory, steps, failure or cleanup state |
| `task` | tasks, units, channels, cancellation |
| `provider` | take a capability and reach the outside through a provider: files, network, clocks, randomness, process state, `provide.*` |
| `foreign` | foreign calls |
| `exec` | process creation |
| `launch` | launch, tape and test rows a standalone executable uses |

### Fixups

A fixup record is 16 bytes: `site: u32`, `kind: u32`, `addend: i64`. `site` is an offset
into the section the record's table names. Records are sorted by `site`, and patched fields
do not overlap. `T` is the address the loader gave `text[0]`, `D` the address of `data[0]`
and `P` the address of the site.

| kind | where | target | patched field |
|---:|---|---|---|
| 1 `Abs64Text` | data | any | 64-bit `T + addend` |
| 2 `Abs64Data` | data | any | 64-bit `D + addend` |
| 3 `Rel32Data` | text | x86-64 | 32-bit `D + addend - P` |
| 4 `PageData` | text | aarch64 | `adrp` immediate: page of `D + addend` minus page of `P` |
| 5 `Low12AddData` | text | aarch64 | `add` immediate: low 12 bits of `D + addend` |
| 6 `Low12LoadData` | text | aarch64 | scaled load or store immediate: low 12 bits of `D + addend`, divided by the access size the instruction encodes |

That is the whole set. Calls and branches inside the module are resolved by the `Compiler`
and need no fixup. The loader refuses any other kind, a kind on the wrong target or in the
wrong table, a site whose field leaves its section, a `T + addend` or `D + addend` that
leaves its section (one past the end is allowed), a value that does not fit its field, a
`Low12LoadData` value that is not a multiple of its access size, and a site whose
instruction is not the form its kind names.

### Entries

An entry record is 80 bytes: `identity: [u8; 32]`, `signature: [u8; 32]`, `text: u32`,
`descriptor: u32`, `flags: u32`, zero `u32`. `identity` is the function's instance digest
([PROG-5](programs.md#prog-5-code-identity)), `signature` the digest of its lowered
signature, `text` its code offset, `descriptor` the `data` offset of its PROG-9 descriptor
or `0xFFFFFFFF`, and `flags` bit 0 is set when the entry waits. Entries are sorted by
`identity`, which is unique. `text` must be the start of a span.

## PACK-3: The `Compiler` service

Compilation always runs outside the pack host, in a **worker process** the service starts
for that compile. `hirsel host`<!-- doc-ref: planned --> supervises a local service; a
fleet worker may be configured with a shared builder on the same machine instead. A
program sees one `Compiler` capability either way.

A request names a checked release closure, a target, a backend and policy bits. A reply is
a module (its bytes, digest and key) or one refusal:

| refusal | when | cached |
|---|---|---|
| `Rejected(diagnostics)` | the source does not check | by request digest |
| `Unsupported(reason)` | the release cannot be a module: it reaches a `foreign` or `exec` row, needs writable data, or needs an import outside the row table | by request digest |
| `Limit(Input)` | the request exceeds `input_bytes` | no |
| `Limit(Cpu)` | the worker used more than `cpu_seconds` of processor time | by request digest and caps |
| `Limit(Memory)` | the worker needed more than `memory_bytes` | by request digest and caps |
| `Limit(Wall)` | the worker ran longer than `wall_ms` | no |
| `Limit(Output)` | the module exceeds `output_bytes` | by request digest and caps |
| `Busy` | the service's bounded queue is full | no |
| `Unavailable` | the worker or the service ended for any other reason | no |

**Caps.** A service has five caps, all required, with these defaults under `hirsel host`:
`cpu_seconds = 10`, `memory_bytes = 512 MiB`, `wall_ms = 20 000`, `input_bytes = 16 MiB`,
`output_bytes = 64 MiB`. Each worker gets the full caps; the service runs at most
`workers` of them at once (default 2) and queues at most `queue` more (default 64).

- **Processor.** The kernel's per-process CPU limit (`RLIMIT_CPU`). The worker ends at the
  cap.
- **Memory.** Linux: the worker's private writable address space is limited to its size at
  start plus `memory_bytes` (`RLIMIT_DATA`), and a refused allocation ends the worker with
  the reserved exit status 77. macOS: the supervisor samples the worker's physical
  footprint every 10 ms and kills it above the cap.
- **Wall.** The supervisor kills the worker at the deadline.
- **Core dumps** are off in the worker (`RLIMIT_CORE = 0`).

A refusal never ends, stalls or grows the pack host: the host waits on a socket with the
wall deadline and holds no worker memory. Requests for one key are single-flight: later
requests wait for the first reply. A worker's reply is accepted only if it is a well-formed
module whose key equals the request's.

The service writes an admission (PACK-1) for each module it returns, into the host's own
admission table.

## PACK-4: The code slab on Linux

Linux x86-64 and aarch64 publish code by changing page protection, never through a second
writable mapping. No page is writable and executable at once, and no file descriptor or
alias can write live code.

1. **Bands.** A slab reserves two adjacent anonymous `PROT_NONE` ranges, a **text band**
   then a **data band**, each at most 1 GiB, so every text-to-data distance fits every
   fixup field. A reservation costs address space only.
2. **Cohorts.** A **cohort** is a page-aligned run in the text band and one in the data
   band. An open cohort is mapped `PROT_READ | PROT_WRITE`, not executable, and nothing can
   call into it. Installs are bump-packed into it, each image at the module's alignment
   (64 bytes or more, so no two images share a cache line).
3. **Batch.** A batch is the installs that land in one open cohort. It closes 250 µs after
   its first install, at once when an install asks for an immediate flush, or when the
   cohort is full.
4. **Publish.** In order:
   1. copy `text` and `data`, write import slots, apply and range-check fixups;
   2. fill every text byte outside an image with the trap pattern (x86-64 `0xCC`; aarch64
      `brk #0`);
   3. one `mprotect` of the text run to `PROT_READ | PROT_EXEC` and one of the data run to
      `PROT_READ`;
   4. aarch64: instruction cache maintenance over the text run (`dc cvau`, `ic ivau`,
      `isb`);
   5. one `membarrier(MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE)`;
   6. add the batch's images to the PC table (PACK-9);
   7. release the images to their callers.

   No address in the cohort leaves the loader before step 7. A slab whose process cannot
   register for that `membarrier` command is not created (`Unsupported`), and its host runs
   Places in separate processes.
5. **Recycle.** A cohort is dead when every image in it has count zero (PACK-6). The
   loader removes its rows from the PC table, waits until no hazard slot names the old
   table (PACK-9), then maps both runs `PROT_NONE` and returns their pages
   (`MADV_DONTNEED`). A stale jump into a dead cohort faults. The range may later open as
   a new cohort; step 2 above runs before it is published again. Only whole dead cohorts
   are recycled: a live page is never written.
6. **`mseal`** is never applied to a slab.

## PACK-5: The code slab on Apple targets

- **macOS arm64.** The text band is one anonymous `MAP_JIT` reservation. Only the pack host
  binary carries the hardened-runtime JIT entitlement; standalone executables are
  unchanged. One dedicated OS **loader thread**, which never runs a task or a fiber, does
  every write: it opens its per-thread write window, performs PACK-4 steps 1 and 2, closes
  the window, calls `sys_icache_invalidate` over the range, then performs steps 6 and 7.
  The window is per thread, so no worker ever sees the band writable. The data band is
  ordinary anonymous memory published with one `mprotect`. Recycling follows PACK-4.
- **iOS, and any host that denies dynamic code.** There is no slab. Every image is
  **prelinked**: compiled ahead of time, signed with the application, and registered in the
  host image's table by module key. `load` of a key in that table returns its `Code[F]`;
  any other key is refused `NotPrelinked`. No evaluator and no bytecode exist on these
  hosts.

Callers cannot tell the three apart: all of them give the `Code[F]` of PACK-6.

## PACK-6: `Code[F]`

An **image** is one installed module. `Code[F]` is a counted handle to one entry of one
image, where `F` is the entry's callable type. The loader gives a `Code[F]` only for an
entry whose `signature` digest equals the digest of `F`.

1. **Interning.** A loader holds one image per module digest. Loading a digest that is
   already installed returns a handle to the same image and installs nothing, so a thousand
   Places from one release share one copy of its code, and returning to an earlier release
   that is still installed compiles and installs nothing.
2. **Counts.** An image is alive while any `Code` names it. A slot's current code holds one
   count. A unit holds one from its spawn until its end is published. A worker's table
   holds one per row. A call takes no count: it borrows a handle its caller already owns.
3. **Slots and `activate`.** A **slot** holds `(version, Code)`. `activate(expected, code)`
   takes the slot's lock; if the slot's version is not `expected` it fails with
   `Stale(current)`; otherwise it stores `code`, adds one to the version and returns the
   new version. That store is the linearisation point. `activate` never waits for a call, a
   worker or a unit that still uses the old code.
4. **Worker tables.** Each worker owns a private table of 64 rows `(slot, version, Code)`,
   evicting the least recently used. To dispatch, a worker reads the slot's version (a
   plain load); when its row has that version it borrows the row's `Code` with no atomic
   operation and no write to shared memory. Otherwise it takes the slot's lock, clones the
   current `Code` into the row and drops the old one. A worker drops rows whose slot has
   moved on before it parks and whenever it sees the pool's update counter change.
5. **Retired code.** An image that no slot names is **retired**. A pool has a
   **retired-code byte budget** (default 64 MiB). Bytes count against it from the moment
   an image is retired until its cohort is recycled. An `activate` that would retire an
   image past the budget fails with `Capacity` and changes nothing. There is no limit on
   how many generations are alive or for how long.
6. **Installed code.** A pool has an **installed-code byte budget**. A load that would
   exceed it fails with `Capacity`.
7. **Unload.** When an image's count reaches zero it leaves the interning table at once,
   and its cohort is recycled when the cohort is dead (PACK-4).

## PACK-7: The import mask

A loader is created with a **mask**: the set of import classes (PACK-2) its pool admits. A
module with an import whose class is outside the mask is refused at load with
`Import(row)`, before any byte is copied. The mask of a pure-Hirsel pool is
`pure | metered | task`. The classes `foreign`, `exec` and `launch` are in no pack host's
mask: a release that reaches them runs in its own process.

The mask is checked on the import table, and PACK-2 leaves code no other way to reach the
host, so a module cannot call a row its mask excludes.

A kind 1 import binds only when the host image holds that instance digest under the
module's `abi` and policy; otherwise the load is refused `Unbound(instance)`.

## PACK-8: Fetching and sharing

- A module is stored and fetched by its digest. A host that lacks one asks its content
  store for exactly that digest.
- Images are interned per whole release (PACK-6), always.
- `core` and `std` instances known when the host is built are prelinked into the host
  image and bound as kind 1 imports. They cost nothing to load and are shared between pool
  processes by the page cache.
- Sharing below a whole release (a package or partition group across different releases)
  is not in version 1.

## PACK-9: Frame information and the PC table

1. **Frame pointers** are kept in all loaded code.
2. **CFI is not resident.** A module's `cfi` section holds DWARF call frame information
   whose code addresses are offsets into `text`, with one CIE per module and one FDE per
   span. It stays in the content store. The loader does not read it and registers nothing
   with a system unwinder (`__register_frame` is never called).
3. **The PC table** maps a code address to the image that owns it. It is the only resident
   record per image outside the image itself: one 24-byte row `(start: u32, length: u32,
   digest: [u8; 16])`, where `start` is an offset in the text band and `digest` is the
   first 16 bytes of the module digest. The text band is divided into 2 MiB chunks; each
   chunk has one immutable sorted array of the rows that overlap it, reached through a
   directory indexed by chunk. A publish or a recycle builds new arrays for the chunks it
   touches and swaps their directory entries.
4. **Readers** are signal handlers: the profile sampler and the fault reporter. A reader
   stores the array pointer in its thread's **hazard slot**, re-reads the directory entry
   to confirm it, searches, and clears the slot. It allocates nothing and takes no lock. A
   replaced array is freed when no hazard slot holds it.
5. **Samples.** The sampler records, for the interrupted PC, each frame-pointer return
   address and the return-address candidate (the word at the stack pointer on x86-64, the
   link register on aarch64), the pair `(digest, text offset)` the table gives at capture.
   A consumer thread fetches `spans`, `names` and `cfi` by digest and uses the CFI to
   decide whether the PC was in a prologue or epilogue, where the candidate is the caller.
6. **Faults.** A hardware fault whose PC the table resolves is a fault in loaded code. The
   pool stops, the report names the module digest, text offset and span name, and the
   digest is **quarantined**: this host refuses to load it again (`Quarantined`).
7. **Causes** never point into text. A cause that outlives its image keeps the image's
   site and literal data alive, not its code (slice P2 defines the owner word).

## PACK-10: Refusals

`load` returns a `Code[F]` or exactly one of these. No refusal installs anything, and none
ends the host.

| refusal | when |
|---|---|
| `NotAdmitted` | the host holds no admission binding this module's key to its digest |
| `Malformed(what)` | the bytes are not a well-formed version 1 module |
| `Mismatch(field)` | `target`, `abi` or `policy` differs from the loader's |
| `Import(row)` | an import's class is outside the mask |
| `Unbound(instance)` | the host image lacks a kind 1 import |
| `Fixup(index)` | a fixup is refused by PACK-2 |
| `NoEntry` | the module has no entry with the requested identity and signature |
| `Capacity` | the installed-code budget or the band is exhausted |
| `Quarantined` | the digest faulted on this host |
| `NotPrelinked` | the host has no slab and its image lacks the key |
| `Unsupported` | the host cannot publish code (PACK-4) |

## Gates

| gate | target |
|---|---|
| G1c | `activate` at most 10 µs p99 with 16 workers dispatching; no shared-line write per call |
| G3 | load of a cached 8 KiB module with 200 fixups at most 50 µs p50 and 250 µs p99; compile and load of an unseen 300-line release through Cranelift at most 30 ms p50 |
| G10 | hostile compiles: no pack host exit |
| G11 | hostile runtime: no pack host exit other than the PACK-9 fault stop; dead cohorts fault |
| G12 | a million load and unload rounds return resident memory to within 2% |
