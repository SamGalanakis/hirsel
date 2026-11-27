# In-process code loading

*Specified 2026-11-27 (lane packing-p1-loader-3253, from the ratified in-process packing
proposal). Rules PACK-1 to PACK-10 are slice P1, the loader core. Rules from PACK-11 are
slice P2 (lane packing-p2-hot-abi-3281), the hot call ABI. PACK-9 item 7 and PACK-14 through PACK-17
specify the P2b language and runner cutover. Their typed adapter, cause owner, Held and runner bindings are not yet built.*

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
| 24 | 32 | `abi` | runtime ABI digest: `SHA-256("hirsel.mod-abi.v1", revision, row table)`, where the revision is a number that changes with any runtime layout or calling rule loaded code depends on |
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

Both digests use the repository's framed hash: the domain string, then each part, every one
preceded by its length. Integers in the key are little-endian (`target` and `backend` as in
the header, `policy` as `u64`).

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
| 9 | `names` | no | one name per span, in span order: one `u32` end offset per span, then the names' UTF-8 bytes end to end |
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

A fixup record is 16 bytes: `site: u32`, `kind: u8`, `bias: u8`, zero `u16`,
`addend: i64`. `site` is an offset into the section the record's table names. Records are
sorted by `site`, and patched fields do not overlap. `bias` is the distance from the field
to the end of its instruction, 4 to 8, for `Rel32Data`, and zero for every other kind; it
keeps `addend` a plain offset into the target section. `T` is the address the loader gave `text[0]`, `D` the address of `data[0]`
and `P` the address of the site.

| kind | where | target | patched field |
|---:|---|---|---|
| 1 `Abs64Text` | data | any | 64-bit `T + addend` |
| 2 `Abs64Data` | data | any | 64-bit `D + addend` |
| 3 `Rel32Data` | text | x86-64 | 32-bit `D + addend - (P + bias)` |
| 4 `PageData` | text | aarch64 | `adrp` immediate: page of `D + addend` minus page of `P` |
| 5 `Low12AddData` | text | aarch64 | `add` immediate: low 12 bits of `D + addend` |
| 6 `Low12LoadData` | text | aarch64 | scaled load or store immediate: low 12 bits of `D + addend`, divided by the access size the instruction encodes |

That is the whole set. Calls and branches inside the module are resolved by the `Compiler`
and need no fixup. A reference the code makes through a global-offset entry reads a
**pointer slot**: 8 bytes of `data` after the import slots that an `Abs64Text` or
`Abs64Data` fixup fills. On x86-64 a `mov` from such an entry becomes a `lea` of the target
instead and needs no slot. The loader refuses any other kind, a kind on the wrong target or in the
wrong table, a site whose field or `bias` leaves its section, a `T + addend` or `D + addend` that
leaves its section (one past the end is allowed), a value that does not fit its field, a
`Low12LoadData` value that is not a multiple of its access size, and a site whose
instruction is not the form its kind names.

### Entries

An entry record is 80 bytes: `identity: [u8; 32]`, `signature: [u8; 32]`, `text: u32`,
`descriptor: u32`, `flags: u32`, zero `u32`. `identity` is the function's instance digest
([PROG-5](programs.md#prog-5-code-identity)), `signature` the digest of its lowered
signature, `text` its code offset, `descriptor` the `data` offset of its PROG-9 descriptor
(past the import slots) or `0xFFFFFFFF`, and `flags` bit 0 is set when the entry waits. Entries are sorted by
`identity`, which is unique. `text` must be the start of a span. A module's entries are
the roots its request names. `hirsel build --emit module` names one, the package's entry.

## PACK-3: The `Compiler` service

Compilation always runs outside the pack host, in a **worker process** the service starts
for that compile. `hirsel host`<!-- doc-ref: planned --> supervises a local service; a
fleet worker may be configured with a shared builder on the same machine instead. A
program sees one `Compiler` capability either way.

A request carries one package's source files (path and bytes), a target, a backend and
policy bits. The worker is the `hirsel` tool: `hirsel build --emit module` checks the
source, compiles the units its entry reaches, and links them into one module, keeping the
sections the entry reaches. A reply is a module (its bytes, digest and key) or one refusal:

| refusal | when | cached |
|---|---|---|
| `Rejected(diagnostics)` | the source does not check: the worker's exit status is 1 | by request digest |
| `Unsupported(reason)` | the worker's exit status is 2 and it names the reason the release cannot be a module: it reaches a `foreign` or `exec` row, needs writable or thread-local data, needs an import outside the row table, or asks for policy bits or a target the `Compiler` does not build | by request digest |
| `Limit(Input)` | the request exceeds `input_bytes` | no |
| `Limit(Cpu)` | the worker used more than `cpu_seconds` of processor time | by request digest and caps |
| `Limit(Memory)` | the worker needed more than `memory_bytes` | by request digest and caps |
| `Limit(Wall)` | the worker ran longer than `wall_ms` | no |
| `Limit(Output)` | the module exceeds `output_bytes` | by request digest and caps |
| `Busy` | the service's bounded queue is full | no |
| `Unavailable` | the worker or the service ended for any other reason, a tool failure with exit status 2 among them | no |

**Caps.** A service has five caps, all required, with these defaults under `hirsel host`:
`cpu_seconds = 10`, `memory_bytes = 512 MiB`, `wall_ms = 20 000`, `input_bytes = 16 MiB`,
`output_bytes = 64 MiB`. Each worker gets the full caps; the service runs at most
`workers` of them at once (default 2) and queues at most `queue` more (default 64).

- **Processor.** The kernel's per-process CPU limit (`RLIMIT_CPU`). The worker ends at the
  cap.
- **Memory.** The worker's startup memory guard holds `memory_bytes`
  (`HIRSEL_MEMORY_LIMIT`): on Linux the soft `RLIMIT_DATA`, on macOS the allocator's capped
  arena. A refused allocation ends the worker with exit status 71.
- **Output.** The kernel's file size limit (`RLIMIT_FSIZE`) is `output_bytes + 1`, and the
  supervisor refuses a module longer than `output_bytes`.
- **Wall.** The supervisor kills the worker's process group at the deadline.
- **Core dumps** are off in the worker (`RLIMIT_CORE = 0`).

A refusal never ends, stalls or grows the pack host: the host waits on the worker's pipe until
the wall deadline and holds no worker memory. Requests for one key are single-flight: later
requests wait for the first reply. A worker's reply is accepted only if it is a well-formed
module whose target, backend, policy bits and `abi` are the request's and this host's.

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
5. **Retire and recycle.** An image is dead when its count is zero (PACK-6). The thread
   that drops the last count only hands the image to the loader: it takes no lock,
   allocates nothing and makes no system call, so a dispatching worker can be that
   thread. The loader **reclaims** the dead images it was handed at the start of each
   load and whenever the pack host calls `reclaim`. For each one it:
   1. removes the image from the interning table, unless a newer install of the same
      digest is there;
   2. removes the image's row from the PC table in every chunk the image spans and waits
      until no hazard slot names an old array (PACK-9);
   3. maps each text page on which the image was the last live one `PROT_NONE` and
      returns it (`MADV_DONTNEED`). A stale jump into such a page faults. An image that
      dies before its cohort is published has its text filled with traps instead, and
      its wholly dead pages are closed when the cohort is published.
   A text page that a live image shares is never written on Linux. So the part of a
   dead image on a page it shares with a live image of its cohort stays mapped and
   executable, and a stale jump into it runs its old code instead of faulting, until the
   last image on that page dies. A page the host refuses to close is counted
   (`unrevoked`) and stays out of use. A dead image's bytes count as installed until its
   cohort is recycled.

   A cohort is dead when every image in it is dead. The loader then maps both its runs
   `PROT_NONE`, returns their pages, and returns the ranges to the slab. The range may
   later open as a new cohort; step 2 above runs before it is published again.
6. **`mseal`** is never applied to a slab.

## PACK-5: The code slab on Apple targets

- **macOS arm64.** Both bands are one anonymous `MAP_JIT` reservation, because a `MAP_JIT`
  mapping cannot be placed beside another; the data band is never mapped executable. Only the pack host
  binary carries the hardened-runtime JIT entitlement; standalone executables are
  unchanged. One dedicated OS **loader thread**, which never runs a task or a fiber, does
  every write to the text band: it opens its per-thread write window, writes, and closes
  the window; the installing thread waits for it. The publisher then calls
  `sys_icache_invalidate` over the run and performs steps 6 and 7. The window is per
  thread, so no worker ever sees the band writable; a text run that is being filled is
  executable to them, and no address in it has left the loader. The data band is
  published with one `mprotect`. Recycling follows PACK-4, except that returned pages may keep their bytes
  (`MADV_FREE`), so a range is filled with the trap pattern when it is next mapped.
  Reclaiming a dead image follows PACK-4 step 5, and the loader thread also fills the
  part of the dead image on a page a live image shares with the trap pattern, then
  invalidates the instruction cache over it; a stale jump into it traps. A
  process that has no per-thread write window (a virtual machine guest, for one) or whose
  `MAP_JIT` reservation is denied has no slab (`Unsupported`).
- **iOS, and any host that denies dynamic code.** There is no slab. Every image is
  **prelinked**: compiled ahead of time, signed with the application, and registered in the
  host image's table by module key. `load` of a key in that table returns its `Code[F]`;
  any other key is refused `NotPrelinked`. No evaluator and no bytecode exist on these
  hosts. A table row holds the module key, the module digest, the text address and each
  entry's identity, signature and address. A prelinked image is interned, counted and
  quarantined like an installed one, uses no budget and has no PC table row. A loader
  whose band length is zero has no slab on any host and gives prelinked images only.

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
   image past the budget fails with `Capacity` and changes nothing. The bytes of a
   dead cohort return at the loader's next reclaim (PACK-4 step 5), so a host that
   activates without loading calls `reclaim` before it treats `Capacity` as final.
   There is no limit on how many generations are alive or for how long.
6. **Installed code.** A pool has an **installed-code byte budget**. A load that would
   exceed it fails with `Capacity`.
7. **Unload.** When an image's count reaches zero it is handed to the loader, which
   retires it at its next reclaim and recycles its cohort when the cohort is dead
   (PACK-4 step 5). Until that reclaim, a load of the same digest installs a new image.

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
   directory indexed by chunk. A publish, a reclaim of a dead image or a recycle builds
   new arrays for the chunks it touches and swaps their directory entries.
4. **Readers** are signal handlers: the profile sampler and the fault reporter. A reader
   claims a free **hazard slot** of the table's 64 by storing the array pointer in it,
   re-reads the directory entry to confirm it, searches, and clears the slot. It allocates
   nothing, takes no lock and reads no thread-local state, so a handler that interrupts
   another reader on its own thread claims a second slot. A replaced array is freed when
   no hazard slot holds it. An image has one row from its publish until the loader
   reclaims it after its death (PACK-4 step 5).
5. **Samples.** The sampler records, for the interrupted PC, each frame-pointer return
   address and the return-address candidate (the word at the stack pointer on x86-64, the
   link register on aarch64), the pair `(digest, text offset)` the table gives at capture.
   A consumer thread fetches `spans`, `names` and `cfi` by digest and uses the CFI to
   decide whether the PC was in a prologue or epilogue, where the candidate is the caller.

   The runtime's sampler writes the pairs as one `CodeFrames` record (kind 14) directly
   after the sample's `CpuSample` record, and only when the table gave at least one. Its
   words are the thread id, then four words for each address an image owns: the address's
   index in the sample, its text offset, and the digest prefix as two little-endian words.
6. **Faults.** A hardware fault whose PC is in the text band, taken while a task of an
   execution runs, is a fault in loaded code. Its execution stops with the run stop `Code`
   ([FAIL-15](failure.md#fail-15-run-stops)): the pool stops and the process goes on. The
   report names the module digest, text offset and span name, and the digest is
   **quarantined**: this host refuses to load it again (`Quarantined`). The handler
   resolves the PC and records the first such fault on the execution; the quarantine and
   the span name, which is read from the module by digest, are the reporter's work outside
   the handler, after the pool has stopped. The reporter goes by the recorded digest and
   never resolves the PC again: by then the image may be dead and its address another
   image's.

   - **Signals.** A process that has a loader takes `SIGSEGV`, `SIGBUS`, `SIGILL`,
     `SIGTRAP` and `SIGFPE` through the runtime's one fault handler, under the rules of
     [EMB-3](embed.md#emb-3-embedded-launch): a signal that is not a fault in loaded code
     goes to the action the handler replaced. The PC of an x86-64 breakpoint trap is the
     address of the trap byte, one less than the address the kernel reports.
   - **A stale address.** A PC in the text band that no row owns is in the text of a dead
     image (PACK-4 step 5). The execution stops with `Code` all the same, the report names
     no image and nothing is quarantined: the fault is its caller's, which kept an address
     past its `Code[F]`.
   - **A stack fault first.** A fault on a task stack's guard or growth pages keeps its
     own meaning (`Stack`, `HostMemory`, growth) wherever its PC is.
   - **The join.** The runtime does not link the loader. A pack host registers its
     loader's text band and PC lookup with the runtime once per process, and takes the
     row addresses its loader binds imports to from its own image. A row the image lacks
     (the law and measurement rows) has no address, and a module that imports one is
     refused as `Import(row)`.
7. **Causes (L2, specified).** On supported 64-bit targets the prepaid native cell
   starts with the public 32-byte Cause (message/kind carrier at 0, site word at 24),
   the next-cell pointer at 32 and one aligned 64-bit counted data-owner word at 40.
   Funding and reservation state follow at 48. The public value, iterator prefix and
   MEM-24 logical 32-byte charge stay fixed. The changed native layout takes a new
   PACK-2 ABI revision.

   The word is zero on reservation. Zero in a recorded cell means all referenced
   literal/source data has process lifetime or is owned by the moved message itself.
   Otherwise it points to a native owner for the exact R/NX literal storage and source
   directory for this cause. The mutable counts live outside the read-only bytes.
   The directory owns the release-local site map and the source bytes needed to render
   it (or their immutable content-store pin). It has no edge to text, an image handle,
   executable glue, a slab cohort or a PC-table row. Install establishes the data
   owners before publishing an entry. They can survive independent recycling of RX.

   Recording takes the site owner from the code supplying the ordinal (the caller for
   an inherited site), while that generation is pinned. Move a heap message and its
   payer unchanged. For a literal, retain its own backing, including when a caller's
   literal fails in a different release. With a single origin the word holds one count
   on its data owner. With two different unloadable origins it holds one counted
   reference to a pair (site owner first, literal owner second), each owning one count.
   The fixed two-reference record is provided with the cell reservation, so even this
   case admits and allocates nothing on failure. A host site or literal uses its
   immortal backing in the corresponding role. Never infer either origin from the
   currently active slot or from the ordinal alone.

   Splicing, catching, joining or moving a chain transfers its cells intact. Iteration
   borrows the cell. On destruction clean the message, release the owner (both counts
   for a pair), then settle the original cell funding. Recycling clears the word.
   Detached copies of message bytes must own their backing. There is no truncation,
   extra logical charge or lookup of live code on any of these paths
   ([FAIL-10](failure.md#fail-10-cause-chain)). Every RX byte becomes eligible for
   ordinary PACK-6 reclaim as soon as its last executable owner is gone, even while a
   chain survives. Formatting uses the recorded owner and ordinal. `core.site_word`
   and tapes contain the unchanged release-local word, never an address, owner tag or
   load-order prefix.

## PACK-10: Refusals

`load` returns a `Code[F]` or exactly one of these. No refusal installs anything, and none
ends the host.

| refusal | when |
|---|---|
| `NotAdmitted` | the host holds no admission binding this module's key to its digest |
| `Malformed(what)` | the bytes are not a well-formed version 1 module |
| `Mismatch(field)` | `target`, `abi` or `policy` differs from the loader's |
| `Import(row)` | an import's class is outside the mask, or the host image lacks the row |
| `Unbound(instance)` | the host image lacks a kind 1 import |
| `Fixup(index)` | a fixup is refused by PACK-2; the text table's records count first |
| `NoEntry` | the module has no entry with the requested identity and signature |
| `Capacity` | the installed-code budget or the band is exhausted |
| `Quarantined` | the digest faulted on this host |
| `NotPrelinked` | the host has no slab and its image lacks the key |
| `Unsupported` | the host cannot publish code (PACK-4), or a module's alignment is above a page |

## PACK-11: The context line

Every native function takes its task's context as a hidden first argument. Its first 64
bytes are one cache line, and a context starts on a 64-byte boundary. Loaded code and
linked code read these words at the same offsets:

| offset | bytes | word | meaning |
|---:|---:|---|---|
| 0 | 8 | `payer` | the slot of the byte budget that pays now ([MEM-19](budgets.md#mem-19-limits)) |
| 8 | 8 | `events` | the task's event gate; zero when nothing observes the task |
| 16 | 8 | `steps` | the signed remaining step window ([MEM-30](budgets.md#mem-30-step-bounds)) |
| 24 | 8 | `stack_limit` | the lowest stack address an activation may enter with ([FAIL-17](failure.md)) |
| 32 | 16 | `fast_salt` | the two map salt words |
| 48 | 8 | `heap_heads` | the task heap's size-class free-list heads; null until code pops blocks inline |
| 56 | 8 | `execution` | the record of the execution this task runs for (PACK-13) |

The prepared map hash key follows at 64 and the strong key at 96. A change to any of these
offsets changes the runtime ABI revision (PACK-2), so a module built for another layout is
refused `Mismatch(abi)`.

A step account's identity is a number that is never zero and never used twice in a
process. A task takes identities from the process in ranges of 64 and takes its first
range when it first opens an account, so opening a `bound steps` writes no memory another
worker reads.

## PACK-12: Fused scope rows

A contained call wraps one call in a byte bound, a step bound and an isolate. Each of the
three has an enter row and a leave row in the runtime, so the plain lowering makes six
runtime calls around the call. An engine may make fewer by running several of these rows
in one runtime call. This changes what the engine emits, never what the program means:
every bound and isolate is entered and left exactly as [MEM-19](budgets.md#mem-19-limits),
[MEM-30](budgets.md#mem-30-step-bounds) and [FAIL-16](failure.md) say, in the same order,
with the same refusals and the same results.

**Fused enter.** When a function enters a byte bound and then a step bound, or a step bound
and then a byte bound, the first enter's row may make both enters. This is allowed only
when:

1. the second enter runs exactly when the first is admitted, and on no other path;
2. every instruction between the two only reads or writes values and frame memory: none
   can fail, charge or refund bytes, take a step, or call; and
3. the second bound's size is a constant or a value computed before the first enter.

The runtime makes the first enter, and the second only when the first is admitted. A
refused second enter leaves the first entered, and the function then leaves the first by
its own path, as in the plain lowering.

**Fused leave.** When a function leaves two or more of these scopes one after another, the
first leave's row may make all of them, innermost first. This is allowed only when every
instruction between the leaves meets condition 2 above and control reaches each later leave
from the one before it without a branch. A later leave row that is also reached another
way (from a refusal or a caught failure, where the earlier leaves did not run) still makes
its own leave on that path.

**What is fused today.** A function that writes `bound bytes { bound steps { isolate(f) } }`
or `bound steps { bound bytes { isolate(f) } }` enters both bounds in one call and, when the
body returns, leaves the isolate and both bounds in one call: three runtime calls around
the body instead of six. The isolate's enter stays a call of its own, because the step that
meters the call to the isolate lies between the bounds' enters and the isolate's. A bound
that encloses an isolate with no second bound keeps its two enters and makes its two leaves
in one call. A step bound written inside the isolate's body lies in another function and
is not fused with the scopes outside it. The paths that leave after a refusal or a failure
make one call per scope.

**Engine limits.** An engine never fuses a row it lowers without a runtime call (a byte
bound with an empty body). The LLVM engine pays the steps of some loops and regions ahead
and refunds the unused ones on the way out; inside such a loop or region it starts no fused
group at a byte bound's row, so a refund always reaches the step bound that was current
when the steps were paid.

**Runtime rows.** `hirsel_core_contain_enter(context, order, bytes, steps, site)` returns
the number of enters made (0, 1 or 2) and the byte bound's `available` result; `order` 0 is
bytes then steps and 1 is steps then bytes. `hirsel_core_contain_leave(context, layers)`
leaves the scopes that `layers` names, innermost first, four bits each from the low end: 1
a byte bound, 2 a step bound, 3 an isolate. Both rows are in class `Metered` of the import
table (PACK-2).

## PACK-13: Executions and run stops

An **execution** is one run of a program's entry: a process's own run, or one generation
of an embedded runtime ([EMB-3](embed.md#emb-3-embedded-launch)). The runtime keeps one
record for each. The record holds the entry the execution runs, the configuration every
unit of the execution starts from, the worst exit status its units have returned, and its
end: the first run stop recorded on it, or the host's stop request. None of these is a
process-wide value, so two executions in one process do not read each other's entry,
configuration, status or end.

Every task's context names its execution's record at offset 56 (PACK-11). A unit belongs
to the execution that created it, and a helper unit to the execution of the unit that
spawned it. The record outlives every unit and task of its execution.

**A run stop ends its own execution.** A run stop
([FAIL-15](failure.md#fail-15-run-stops)) is written to the record of the execution whose
task raised it, and ends that execution's tasks. A task that reaches a step check after
its execution has ended leaves without running further. An embedded execution's stop is
reported to its embedder and the process goes on. A process's own run exits with status
102, as FAIL-15 says.

A pack host's workers serve one execution: the runtime has one worker pool in a process.
Loaded code called through `Code[F]` (PACK-6) therefore runs in its caller's execution,
and a run stop raised while it runs ends its caller too. `isolate` does not contain a run
stop ([FAIL-16](failure.md)).

**Which rows can stop.** Loaded code reaches the runtime only through the import rows
(PACK-2). A row of class `pure` or `metered` needs no capability, so a module that holds
no grant can still call it. Of the stops such a row can reach, none may be one its caller
can bring about by its arguments alone. The stops these rows reach, and why each is not
the caller's to cause, are:

| stop | reached from | why code without a grant cannot cause it |
|---|---|---|
| `PageSize` | the first use of the task heap in the process | the host's page size is read once and does not depend on any argument |
| `StepIds` | opening a step account | identities are taken in ranges of 64 from a 64-bit counter; passing it takes 2^58 range refills |
| `CountOverflow` | a share of a counted value | each holder is a live value of at least one word, so 2^64 holders do not fit in memory |
| `Stack` | a heap or region call that finds less than 16 KiB of stack left | user activations are refused at the allowance ([FAIL-17](failure.md)), and a 16 MiB task stack keeps at least 512 KiB of engine reserve below the largest frame the ceiling admits |
| `HostMemory` | the task heap, a region refill, the glue list, `argon2id`, and mounting another task's stack at a checkpoint | the bytes were admitted by a limit ([MEM-21](budgets.md#mem-21-admission)); the host refuses them only when it was given less memory than the root limit promises |

`HostMemory` is the one entry that depends on the host: on a host whose memory is
provisioned for the root limit it does not occur, and on an overcommitted host a module
that allocates up to the bytes it was admitted can meet it. A pack host that loads code it
does not trust provisions its memory for its root limit.

A row of these two classes also reaches the stops that belong to its caller's own state:
a checkpoint inside a row may run the scheduler, which serves the execution's other tasks
and its units' connections. Those stops need a task, a unit or a socket, which only a row
of class `task` or `provider` creates.

A row of any class that can reach a new stop is listed here with its reason, or is changed
to return a failure. `crates/hirsel-core-rt/audit-stops.py` prints, for each row, the
functions that raise a stop and that the row reaches in the built runtime.

## PACK-14: Loading and calling an entry

PACK-14 through PACK-17 and the owner in PACK-9.7 specify P2b. The language bindings,
typed adapter and runner cutover are unbuilt. The Rust loader already implements the
counted image, versioned slot and worker cache (PACK-6). The compiled-module driver
currently calls a program entry with a fixed signature. The loader check is
`activate_swaps_at_the_expected_version_within_the_retired_budget` in
`crates/hirsel-code/src/tests.rs`. The compiled caller and adapter are separate gates below.

**Binding.** `code.load[F](loader, key, identity)` returns `Result[Code[F], LoadError]`.
`loader` is a host-issued capability for one pool and import policy. Key and entry
identity are the two PACK-2 digests. The checker requires a closed function type at the
use, including modes, effects, result anchors and concrete type arguments. It derives
the signature itself. Source code cannot supply an expected signature digest, mint an
admission, derive a Loader from a digest, or turn a code address into a callable.
The host resolves the key through its local PACK-1 binding, obtains the exact bytes by
digest, checks the PACK-2 header, imports and fixups, and matches the requested entry
before publishing a new image. An intern hit still requires the binding, policy,
quarantine and entry checks. A host without a slab uses the PACK-5 signed table as its
local binding. The result on refusal is the applicable PACK-10 error. An absent identity,
wrong signature (including a `waits` mismatch), or equal machine layout with a different
source type gives `NoEntry`. Refusal publishes no handle, slot change or new image.

The typed export's PACK-2 signature is the framed hash under
`hirsel.pack-export.v1` of the closed PROG-9 callable type text, the PACK-16 hashes of
each closed parameter referent and result (in order), and the canonical CoreIR lowered
adapter signature digest, as separate parts. Anchors and region binders use PROG-9
positional spelling. For a scoped constructor, hash its compiled type row with its
binders so named, even though its value cannot enter a Held. The entry's waits flag
must agree. The ABI revision distinguishes these adapters from the program-root ABI.
An image records the identities of all requested exports as PACK-2 entries.

**Using a handle.** `c(args)`, for `c: Code[F]`, invokes exactly that entry, with the
call checks, argument order, loans and authority of a call of type `F`. A handle can
be moved and explicitly shared within its pool. Both handles then keep the same image.
It has no wire codec or address-based key. Possession permits invocation. It supplies
none of the capabilities the entry takes. PACK-7 checks the transitive imports,
including host-instance dependencies, against the pool policy. Supplying a grant to
a call never widens that mask. Loading and changing a slot require explicit authority.
A Call borrows its handle through the last return and failure cleanup. It cannot yield
a free-standing `F` that outlives the handle.

`code.bind(f)` names a closed top-level function in the calling image under the
inference rules of [PROG-9](programs.md#prog-9-code-descriptors) and produces its
`Code[F]` under that image's existing authority. Statically linked entries have
process-lifetime backing. `code.identity(c)` and `code.revision(c)` expose the same
PROG-9 bytes and optional revision as the named instance, with views anchored in `c`.
A PROG-9 `StaticFn` remains the plain descriptor for statically bound code. Its
identity equality and its role in codec derivation stay as specified. It cannot
denote an entry looked up by key, nor can an unloadable descriptor or bare callable
escape its image's lifetime, including inside another value. Runner registration and
execution use `Code` for both linked and loaded entries. They have one dispatch path.

**Selecting a generation.** `code.slot[F](loader)` creates an empty version-0 slot.
`code.activate(slot, expected, c)` requires write authority to that slot and a handle
of its pool and type. Pool identity is checked before mutation. A foreign pool gives
`WrongPool`. Under the slot lock, a stale expected version gives `Stale(current)`.
A version that would wrap or a retirement exceeding the PACK-6 budget gives
`Capacity`. Success installs the handle and publishes the incremented version at one
linearisation point, even on replacement with the same handle. A refusal preserves
the old entry, version and counts. Dispatch permission alone cannot activate.

`code.dispatch(slot, args…)` returns the entry result in `Ok`, or `Error(Empty)`
without calling user code for an empty slot. Dispatch selects the entry by PACK-6.
A cache hit borrows the matching worker-owned generation with a version load and no shared-line write or refcount increment. A miss takes entry and version
together under the lock. A call overlapping activation may finish on its selected
generation. A later dispatch cannot combine old code with new metadata. The borrowed
row cannot be evicted or refreshed out from under a nested call or a suspended frame.
If the activation leaves worker custody, its continuation takes an owning pin before
the worker can release the row. Every return address, pending cleanup and resumed call
is covered until its last use. A Unit owns its generation from spawn through end
publication. Activation waits for none of them.

## PACK-15: The typed export adapter

Call, Dispatch and Unit use one generated adapter per closed entry. Both native engines
lower the same CoreIR contract. A synchronous Call takes its caller's task, fiber and
PACK-11 context. Dispatch binds the selected Place's execution to worker stock. A
`waits` entry runs only where waiting is allowed, with Unit custody for a parked Place.

1. The adapter takes the current context, a caller-owned, aligned uninitialized
   result place, then the lowered arguments of `F` in order. Its status reports normal
   return or Hirsel failure. A zero-sized result needs no writable bytes. No codec,
   heap argument block or second stack is part of this calling convention.
2. A `view` place is the caller's actual address, CoreIR `Param::Address` with
   `Loan::View`. Its owner stays live and its exposed storage frozen through the last
   use, including cleanup. Overlapping views are legal. A `mut` place is exclusive
   under OWN-9. The result destination is disjoint from live input loans. No address
   may escape beyond its declared anchors. A returned view may borrow caller storage
   only under the result type's anchor contract. A parked activation retains every
   required owner or the borrow is refused by the checker.
3. Value parameters observe normal copy and move semantics, including when passed
   indirectly. A callee that modifies a by-value plain argument and then fails leaves
   the caller's original value intact. A transferred owner has exactly one cleanup
   obligation at each point, including before frame admission. A caught failure
   preserves writes already made through legal mutable loans and effects on capabilities.
4. Construct directly in the result place. The caller may read, move or drop it only
   after normal status. Until then the producer owns initialized components, including
   a pending result if exit cleanup fails. It cleans each exactly once under FAIL-8/9.
   On failure the destination holds no live value. For assignment, stage the result
   before replacing a still-live destination. Returning image-backed data, code or
   cleanup obligations transfers their backing owners before the call's pin ends.
5. Argument evaluation, frame refusal, byte admission, payer handover, step debits and
   refunds, cause order and isolate results match a linked call under the same build
   and bounds (MEM-19/30, FAIL-16/17). Dispatch bookkeeping contributes no language
   step or byte charge. A run stop follows PACK-13. A hardware code fault follows
   PACK-9.6. Neither returns a language failure status to an isolate.

## PACK-16: Held values across replacement

`code.hold(take value)` moves one closed, owned-lifetime value into an opaque `Held`.
Its logical contents are `(type_closure_hash, payload)`. It is local to its pool and
target ABI. The checker rejects a scoped value, a region-backed value or an embedded
uncounted reference into unloadable code or data. Erasure cannot add `send`, sharing,
copy rights or authority. Unqualified Held is task-local. Its transferable form,
`Held[send]`, requires a statically proved `T: send` when constructed.
The representation preserves the payload's alignment, live fields and original payer.

**The 32 bytes.** Let `cT` be the *full* [PROG-10](programs.md#prog-10-code-addresses) type
closure hash. For a type row this is the repository's framed SHA-256
`H("hirsel.core-closure.v1", c)` with one part, the frozen closure wire encoding.
Start at the closed root (row zero), number rows on first visit through fields, held
parts, hook type arguments and hook signatures, and encode their shapes and hooks with
these local references. Reuse a visited index on recursion. For a scalar or unit
use the PROG-10 scalar/unit constant rule: the same domain and one part, its
canonical name in UTF-8.

The Held hash is `H("hirsel.held-type.v1", S(T), cT, graph)` with three framed parts.
`S` is the UTF-8 PROG-9 type spelling (aliases expanded, nominal package major
included). The last part retains source distinctions erased by lowering, for example
the parameter type of a callback nested in a record. Starting at T, visit its closed
type arguments, callable parameters and result, and declared variant payloads or
record fields (including private ones) in their written order, recursively, expanding
each distinct S only once. Within a callable, binders and anchors have PROG-9's
positional spelling. Emit a SEM-10 `u32` node count, then nodes sorted bytewise by S.
A node is its SEM-10 length-prefixed S text followed by four lists in the visit order
above (one result or none). Each list is a `u32` count and the children's S texts.
Native row bytes already bind field names, offsets determined by the ABI, tags and
hook identities. Using names for graph edges makes recursive backreferences finite
and independent of discovery order. No image address, load sequence or backend enters the hash.
[SEM-10](semantics.md#sem-10-codecs) supplies the storage fingerprint when a program
chooses to encode its state. Wire compatibility alone cannot authorize a native move:
a `via` type can store the same bytes under different native layouts.

**Ownership.** A plain self-contained payload needs no image pin and can be copied
when its type is known. Erased owners move. A payload with drop glue retains one
strong originating `Code` owner together with the cleanup entry. An embedded owner
retains its own dependencies. A payload with a literal reference retains its R/NX
backing. Plain self-contained data contributes zero executable owners after retirement.
For glue, physical unload waits for the last owner. Destruction invokes the original
glue under the normal cleanup and funding rules, even after replacement. The pin
survives failure, derived-field cleanup and any resumable finalizer through completion.

`code.take_as[T](take h)` checks the requested closed type witness, all 32 hash bytes
and the pool ABI before touching the payload. On success it moves out the value once
and transfers any backing obligations through the typed value's lifetime. Rebinding to
an equal type never silently substitutes the new generation's finalizer. On disagreement
it returns `Error((TypeMismatch, h))`, with the original value, payer and pins intact.
No payload read, transmute, drop, partial consumption, codec invocation or slot update
occurs on that path. Programs that choose a conversion use their own codec while the old
type is still available.

## PACK-17: Runner installed image and Place instance

An `InstalledImage` belongs to one admitted native realization. The installer resolves
the export entry points, identity/revision bytes, typed call and wire adapters, schema
descriptors, route keys, binding and argument-check tables, and registration order once.
Store schema *descriptions* belong here. Opening a particular store, migration, handle
selection and writer fencing belong to its Place. Validate duplicate keys, bounds and
bindings before publishing the complete immutable table. Use image-relative entry
indices inside it and one external image owner, so entries cannot keep their own image
alive in a cycle. The worker borrows the table and entry under the same generation pin.

A registration builder may exist during installation. No serving task replays the
registration function or keeps its own map of owning adapter closures. The installer
separates a capture into immutable checked image input or a typed instance argument.
Only release-determined immutable data can enter a shared row. Execution-dependent
selection, grant mint, store or caller handles, budgets and mutable environments come
from the bound instance or current invocation. An explicit per-launch registration
budget still receives its existing admission/refusal outcome even on an intern hit.
Compute, granted, transact, query and fenced-attempt entries retain their registration
order, identities, schemas, authority checks, store replay and bounds. Local typed
calls use PACK-15 directly. A wire or journal boundary still runs its declared codec.

Each dormant synchronous Place holds its checkout/identity, selected image slot and
version, and the references needed to bind its own execution, limits and grants. It
owns no resident registry copy, worker context, fiber stack or private dispatch maps.
Binding checks the checkout and selects the slot once for the invocation. Activation
publishes one coherent installation. In-flight calls, Units and Held owners complete
under their original generation. H1–H4 and L1–L7 qualify the loaded runner, including
grant disposal, metered bulk operations, stack refusal, capped compilation, lifetime,
cause stability, stop isolation, accounting and replay. PACK-13's current single-pool
limitation leaves independent sibling stop qualification open.

G8c counts the resident delta for 50,000 dormant sync/dispatch Places above the same
warmed pool with the shared image installed. Count slot arrays, indexes, instance-owned
allocations, execution/grant backing, allocator slack and committed worker growth.
Exclude only the shared image paid once. The target is at most 512 B per Place and
25 MiB total on both 4 KiB Linux and 16 KiB Apple Silicon. The 64-byte hot slot in the
design is part of that total. G8a/b cover parked Units separately.

## Gates

| gate | target |
|---|---|
| G1a | compiled uncontained loaded call versus matched static direct call: at most 5 extra instructions and 2 ns |
| G1c | bound-slot dispatch to first instruction at most 100 ns p50; `activate` at most 10 µs p99 with 16 workers dispatching; no shared-line write per call |
| G3 | load of a cached 8 KiB module with 200 fixups at most 50 µs p50 and 250 µs p99; compile and load of an unseen 300-line release through Cranelift at most 30 ms p50 |
| G8c | 50,000 dormant sync/dispatch Places, at most 512 B/Place and 25 MiB all-in above the shared image (PACK-17) |
| G10 | hostile compiles: no pack host exit |
| G11 | hostile runtime: no pack host exit other than the PACK-9 fault stop; dead cohorts fault |
| G12 | a million load and unload rounds return resident memory to within 2% |
