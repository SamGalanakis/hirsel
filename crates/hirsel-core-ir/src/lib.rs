//! CoreIR: the lowered form the one-pass front end emits and both native emitters
//! read. A [`Module`] is the whole emitter input apart from layout recipes
//! (`hirsel-core-layout`, FORM v1), the target and the optimisation policy.
//!
//! # Versions
//!
//! **FORM v0 (frozen):** neutral identities (`hirsel-core-ids`); the CFG (blocks with
//! parameters, edges with arguments, `Jump`/`Branch`/`Switch`/`Return`); values,
//! slots and addresses; the scalar, compare, convert, storage and call
//! instructions; signature, instance and function rows; constants and statics;
//! item-relative source anchors; the text form ([`text`]); the calling convention
//! and instance rules below.
//!
//! **FORM v1a (frozen; header `core v1`):** declared types with layout and drop
//! recipes (`Module::types`, `hirsel-core-layout`, charge policy `charge-v1`);
//! own allocation, growth (`alloc .. like`), `free` and `copy` (moves, replace
//! with install-before-drop, growth); `drop` by recipe, with initialization bits
//! kept by `UpdateMask`; resources as blocks with a cause cell and a finalizer
//! grant; Rc/Arc nodes (`alloc counted`, `share`, `release`, `try_unwrap`);
//! failure successors (`fail bN` on the instructions that can fail, `raise`,
//! `propagate`); `never` signatures, whose calls end their block before
//! `unreachable`, which also follows a read of a binder of type `Never`; logical admission and refund with bound scopes (`limit_enter`,
//! `limit_exit`); task custody through runtime calls (`rt.spawn`, `rt.join`,
//! `rt.cancel`, `rt.check_cancel`, `rt.channel_new`, `rt.channel_sender`,
//! `rt.send`, `rt.try_send`, `rt.receive`). Rule texts: failure.md, budgets.md and
//! tasks.md (FAIL-2..14, MEM-14..16, MEM-19..28, CONC-3..9).
//!
//! **FORM v1b (frozen at spec revision 4):**
//! - Variants: `case` reads a variant place's case number (`u32`), `set_case`
//!   writes it, and `case c f` address steps reach a case's payload. Own, resource,
//!   stable and scoped variants; `Option` and `Result` are ordinary variant rows of
//!   the type table, one per instantiation. The tag's place and width stay private
//!   (MEM-13): a producer never names the tag bytes. A variant with one case that
//!   holds data may keep its other cases in spare values of that payload (a niche,
//!   `hirsel_core_layout::Encoding::Niche`): then `set_case` of the data case writes
//!   nothing, since its payload makes the case, and `set_case` of another case writes
//!   that case's spare word, which overlaps the payload. So a producer writes the
//!   payload of the data case completely, and sets an empty case only once the
//!   payload is moved out or was never written.
//! - Inline arrays `Inline[T, n]` (SEM-9): field type `array n T`, `n` elements at
//!   `T`'s native stride, no block and no charge of their own; the `element v`
//!   step fails with `Bounds` at its `address` when `v` is not below `n`; cleanup
//!   goes from index 0 up.
//! - Regions (`region_open`, `region_close`, `alloc .. in`). A region handle is a
//!   value and never a field (MEM-1).
//! - Stable and scoped records, compact records and active-size compact variants
//!   (MEM-9..11), links (MEM-10), `Text` (`W` count, then bytes) and `FixedSpan`
//!   (`W` count, then elements at a fixed stride) (MEM-12), with the charges
//!   `charge::standalone` and `charge::span_builder` (MEM-24). Span builders are
//!   ordinary code over these: `alloc .. in`, `copy`.
//! - Callable environments: an `own fn` is a code pointer and an environment
//!   pointer (logically 16). The environment block is one word naming its drop
//!   glue, then the captures in the order the body first names them; it is
//!   charged `charge::closure_env` of the captures' sizes (the glue word is never
//!   charged, MEM-24) and its glue cleans the captures in reverse, frees the block
//!   and refunds it. A captureless `own fn` has a null environment. Captures hold
//!   no resource, so this cleanup never fails and runs no finalizer.
//! - Providers (CAP-5): a provider operation is a closed runtime row (below,
//!   "Providers"), a cancellation point when it `waits` (so it can fail),
//!   admitting nothing. `HostFile` is a mechanical resource that is not a
//!   block: a `u64` provider handle (field type `hostfile`, 8 bytes, logically 8),
//!   with no cause cell, no grant and no charge. Its drop step calls the runtime
//!   entry `hirsel_core_hostfile_drop(context, handle)`, which issues the
//!   provider's close through the replay point, ends any lock, ignores the
//!   outcome, admits nothing and cannot fail (core.md, MEM-24, FAIL-13); it is
//!   not a cancellation point and adds no status. It is cleaned where its field
//!   falls in the recipe's order. It never reaches counted cleanup: it is not
//!   `shareable`, and a counted payload holds no resource (MEM-16).
//! - Cleanup of `List`, `String` and `Map` storage, the `Map` entry slot, and the
//!   step order a counted release resumes by, dynamic sequences and maps
//!   included, are stated in `hirsel-core-layout` (`DropRecipe`, `map_entry`).
//!
//! **FORM v1c (slot kernels; header `core v1`):** `mark` and a core finalizer on
//! an own row, below ("Slot kernels"). Nothing else changes, and no v1a or v1b row
//! changes meaning.
//!
//! Brands, `SlotOwner` and `uses` lists are not in FORM v1 at all: a pool handle
//! is plain data checked at run time.
//!
//! # Slot kernels (FORM v1c)
//!
//! core's collections that hold uninhabited slots (`Deque`, a heap's sift, a
//! stable sort's merge) are core code over one private carrier, `Slots[T]`
//! `{raw: List[T]}`, whose `List` length word is the count of live slots. Three
//! rows exist only for them; well-formedness refuses each outside a function
//! that is an instance of a core@1 definition.
//! - **`mark a, live|empty : t`** says the slot at `a`, a place of `t`, now holds
//!   a value (`live`, after a put) or none (`empty`, after a take). A move is a
//!   `copy` with its source marked `empty` and its destination `live`; the blocks
//!   may differ. It has no run-time effect: engines emit nothing for it, so a
//!   body's code is the same with its marks removed. The evaluator keeps it as a
//!   liveness shadow and stops the run on a take (`empty`) of a slot marked empty,
//!   a put (`live`) over a slot marked live, a `read` of, or `copy` from, bytes of
//!   a slot marked empty, and a `free` (or cleanup's free) of a block with a slot
//!   marked live. A `drop` of a marked slot, and `List` cleanup of one, is a take
//!   too: the slot must be marked live and is then marked empty, so cleanup that
//!   reaches a slot a kernel emptied stops the run. A slot never marked may be
//!   either (storage a `List` hands over is all live and unmarked; a new block
//!   needs no registration). A zero-size slot has no storage to identify it and is
//!   not shadowed.
//! - **`unmark a, live|empty : t`** is the inverse: the slot leaves the shadow and
//!   is ordinary storage again, in the state the kernel names. A kernel that hands
//!   its storage back as an ordinary `List` (a sort's scratch returned to its
//!   owner) unmarks each slot it marked, `live` over the prefix `0..len` and
//!   `empty` past it, so later `List` operations see plain storage. The evaluator
//!   stops the run on `unmark live` of a slot marked empty (a hole handed back as
//!   a value) and `unmark empty` of one marked live (a value left behind); a slot
//!   never marked passes. Engines emit nothing for it, as for `mark`.
//! - **A finalizer on an own row** (`type tN = own "Deque[T]" drop fM { .. }`,
//!   `DropRecipe::finalizer`) is called with the value's address, as `(ptr) ->
//!   ()`, before its fields are cleaned (FAIL-8), under the current limit with no
//!   grant and no cause cell. It only moves slots, to restore its fields' own
//!   invariant (a `Deque` moves its live slots into the prefix `0..len` of its
//!   `List`, front first), and cannot fail: wf refuses a finalizer body from which
//!   a failure is reachable ([`failure::Failures`]: a fallible instruction, `raise`
//!   or `propagate`, an indirect call, a failing runtime call, or a call of such a
//!   function or of one whose body the module does not hold). Arithmetic its
//!   invariants bound is written wrapping. A module that only names the row holds no
//!   body to check; the one that defines it does. The fields' cleanup then drops the values, resuming
//!   past a failed drop as it always does (FAIL-12). In counted cleanup such a
//!   value is one step (its finalizer and then its fields, whole); a counted
//!   payload holds no resource, so it cannot fail there.
//! - **A clone body on an own row** (`type tN = own "Deque[T]" clone fM { .. }`,
//!   `Shape::clone`, MEM-17) is `(ptr from, ptr to) -> (u64 requested, u64
//!   available)`. A derived clone of any value holding the row copies its bytes and
//!   then calls it instead of copying the row's fields: on `(0, 0)` `to` holds an
//!   independent copy of `from`; otherwise `to` is as the byte copy left it, owns
//!   nothing and is not cleaned, and the pair is the refused request. No engine
//!   reads the field; the front derives the calls.
//!
//! # Compact data (FORM v1b, spec 6ef88514)
//!
//! Compact values are region bytes. FORM adds four primitives for them and
//! nothing else: a `byte vN` step (an unaligned byte offset), `pointer aN` (an
//! address as a `ptr`), `trim vB in vR entry vE from vP charge vN` (lowers a region block's charge and
//! refunds the difference to its record) and `rt.dest_id` (a run-unique `u64`,
//! cannot fail). `alloc`'s size is a value, so a blob of `n` bytes is one alloc.
//! Writers, readers, `clone_in`, `seq.index_in`, `text.as_string`, `writer.push`
//! and `emit` are ordinary producer code over these, `copy`, `read` and `store`.
//! - **Blob**: a region block of a 16-byte engine header (used bytes at 0,
//!   written at finish; W in bytes at 8) then the data, charged round8(n) and
//!   never the header (MEM-24). Data offsets below are from the block pointer.
//! - **Locator**: a view of, or stable reference to, compact data is
//!   `{blob ptr, at}` (`FieldTy::Locator(target)`, 16 bytes, align 8, logically
//!   16, whatever the target); `Stable` of a compact type is refused. The row
//!   names what it locates as a compact field type, `locator text`, `locator
//!   fixed e`, `locator seqspan e`, `locator indexedspan e` or `locator tN` of a
//!   compact `tN`, and layout refuses any other target. That target is the only
//!   recipe hash, equality and order read; nothing is parsed from a name. A
//!   standalone `Text` or span is `{at: locator X}`, a `SeqIndex` is a stable
//!   reference (8 bytes native, logically 16) to one region block {`locator
//!   seqspan e` @0, count u64 @16, u64 offsets @24, W-free from the span's data
//!   start} (MEM-12; it has no `key` or `ordered` role, so nothing hashes or
//!   compares it), and a compact stable type held in a non-compact holder is
//!   `locator tN`. A template row `Locator($i)` or
//!   `View($i)` is materialized by substitution alone; the layout never depends
//!   on the target. An `IndexedSpan` reads and iterates as a `SeqSpan` with the
//!   same locator and W; its trailer (count W-wide offsets from the first element
//!   byte) follows the elements, outside its own length but inside the holder's
//!   extent. It is never a writer, a `Child` or a field encoding.
//! - A region alloc can return an optional third u64 result, the opaque payer-entry
//!   token captured at admission. Only the destination writer passes it back to
//!   trim; its numeric value is engine bookkeeping and is not program data.
//!   `alloc plain` has Own's semantics but promises no outgoing mechanical drop;
//!   general `alloc own` and counted nodes prepay a resumable native frame.
//! - **Build**: a record `{blob, region, entry, charge, cursor, end = 16 + n, w, id = rt.dest_id}`; a
//!   writer frame `{record, start, count, last}`; a `Child` `{id, at}`. Every
//!   write checks, in order: the identity of each link it stores, in field order
//!   (`Bounds`); then fit and the W cap (`Overflow`, an earlier fit failure
//!   first); then writes, each header, scalar or field checked before its bytes.
//!   An appended element's returned `Child` is checked at receipt (id and at are
//!   the frame's, else `Bounds`) before the count advances. Closing an
//!   `IndexedSpan` checks the whole trailer fits before storing any entry.
//!   `build_stream_in` refuses with `Overflow` when n exceeds the W cap, then
//!   admits round8(n). Finish stores used = cursor - 16 and W; `finish_trim`
//!   then `trim`s the blob with its region, captured entry and previous charge to
//!   round8(used) (MEM-23). Streaming roots are stable
//!   streamable types only.
//! - **Packed** element types (layout's predicate): numbers, bool, char, `Text`,
//!   compact types, non-empty plain records and tuples of packed fields (encoded
//!   field after field), `FixedSpan` and sequences of packed types. `()`, Never,
//!   callables, capabilities, links and `Inline` of anything else are refused.
//! - **`clone_in`** of a streamable value copies exactly its bytes at its own W
//!   into a new blob, as a new root, charged round8(length); any other compact
//!   value copies its whole blob. `copy_closed_in` does not exist.
//!   `seq.index_in` allocates round8(24 + 8 count) (`charge::seq_index`).
//! - `text.as_string(t)` is a `View[String]` (Views): the carrier
//!   `{blob + at + W, count, count}` written into the caller's `dest`; `emit` of a
//!   `String` view or a `Text` into a `Text` is one fit check and one `copy`. An
//!   unmarked `T` parameter of a compact `T` also takes a `View[T]` (OWN-7): the
//!   producer passes the locator.
//! - **A view of a plain type** follows the general rule (Views). A view of a
//!   plain packed element decodes it field after field into a native place (a compact
//!   field becomes its locator); a view of a compact element is its locator.
//!   Iterating a span advances by the element's packed size, constant for a plain
//!   element and `compact_extent` at the holder's W otherwise.
//! - **A `FixedSpan` element MEM-9 does not fix** (MEM-12: private, at a stride of
//!   the logical size) is encoded in its logical layout, which fills that stride
//!   exactly: a record or tuple field after field, each field so encoded; a plain
//!   variant its case index (declaration order) as a little-endian u64, then the
//!   active case's fields after field, zero up to 8 plus the largest payload; an
//!   `Inline[T, n]` its elements in turn. For scalar records this is MEM-9's
//!   encoding, so a type has one encoding. `FixedSpan[Small(u8) | Large(u64)]`
//!   strides 16: `Small(7)` is `0u64, 7, 0 x7`; `Large(x)` is `1u64, x`.
//!
//! # Views (FORM v1b)
//!
//! A `View[T]` is carried exactly as a `T` is carried: it is `T`'s own bytes, a
//! non-owning copy that nobody cleans (OWN-8). A view of a scalar is the scalar.
//! Any other view is a place of `T`: passed as the address of a place holding
//! those bytes (the owner itself or a frame copy) and returned by writing them
//! into the caller's `dest`, like any non-scalar result. One rule, whatever made
//! the view: viewed storage is frozen while the view lives, so the copy cannot be
//! told apart from the owner.
//! - **`View[String]`, `View[Bytes]`, `View[List[T]]`** are therefore a carrier
//!   `{data, length, capacity}`. A view of a whole value copies the owner's
//!   capacity, so `list.capacity` and `string.into_bytes` read the truth through
//!   an unmarked parameter (OWN-7); it frees nothing because nobody cleans a view.
//!   A view of a range that a function makes is a new carrier whose capacity is
//!   its length (core.md "Slices": no view grants spare capacity), written into
//!   that function's `dest`: `list.slice(xs, lo, hi)` and `string.slice` write
//!   `{data + lo * stride, hi - lo, hi - lo}` after the bounds check
//!   `lo <= hi <= length` (`Bounds`); `text.as_string(t)` writes
//!   `{blob + at + W, count, count}`. A literal passed to a view parameter is its
//!   carrier in a frame slot (Strings and literals).
//! - **Kernels that only retype** are the carrier bytes themselves: the views
//!   `string.as_bytes` and `string.view_string` copy the 24 bytes into `dest` (or
//!   pass the same address on), and the moves `string.of_bytes` and
//!   `string.to_bytes` hand the same carrier over. No other instruction.
//! - **Cost**: a call passes one pointer; reading an element is two loads (the
//!   data pointer from the place, then the element), as for an owned `T`; a
//!   returned view copies `T`'s size (24 bytes for a carrier). `Option[View[T]]`
//!   has `Option[T]`'s layout and no cleanup.
//! - **A view field** of a scoped type is `FieldTy::View(T's field type)`: `T`'s
//!   size, alignment and logical size, cleaned by nobody, refused outside scoped
//!   holders (core.md: scoped values are never stored in own, stable or resource
//!   fields or collections). A view of a view-typed field is those same bytes
//!   again. A view of compact data is its locator (Compact data), so
//!   `FieldTy::View(Inline(C))` of a compact `C` is laid out as a `Locator`.
//! - **Inside a generic template** the rule is the same: a view of `$i` is a row
//!   `View[$i]` that materialization lays out by substitution alone, as the
//!   argument's own row with no cleanup (a `Locator` for a compact argument). At
//!   `T = String`, `View[T]` and `View[List[u8]]` are the 24-byte carrier,
//!   identical to the concrete row, so a template and concrete code agree on every
//!   `Option[View[T]]` that crosses between them. No per-instance kind flag is
//!   needed: a view is never a pointer word.
//!
//! # Strings and literals (FORM v1b)
//!
//! A `List`, `String` or `Map` carrier `{data, length, capacity}` owns storage
//! exactly when its capacity is not 0, except a `List` of a zero-sized element,
//! which never does (below). Cleanup, the counted release's last step and growth
//! test the capacity, never the data pointer: a capacity-0 carrier frees nothing,
//! and its first growth is an `alloc own` against the current limit (MEM-26),
//! never `alloc .. like`.
//! A dynamic ordinary/counted or region allocation may carry `overflow vN`, a
//! bool saying its exact logical charge exceeds u64. It refuses admission even
//! with u64::MAX available, reports that limit's actual room, and follows the
//! same finalizer-grant failure path as any refusal. Omission means false.
//! - **Only a `String` is ever static** (core.md "Literals"). An own `List`,
//!   `Bytes` or `Map` with elements always owns its storage: capacity 0 means
//!   length 0, because a `List` has in-place element writes. So a carrier of
//!   capacity 0 and length > 0 is typed `String`, or a view (`string.as_bytes`
//!   of a static `String` is `View[Bytes]` `{static dN, length, 0}`: nothing
//!   writes through a view), never an own `List`, `Bytes` or `Map`.
//! - **A string literal** is `{static dN, byte length, 0}`: one `static` read
//!   and two constants, stored through `byte` steps over the carrier. It admits
//!   nothing, charges nothing and cannot fail. `""` may use a null pointer.
//!   Passed to a view parameter it is that carrier in a frame slot, with nothing
//!   to clean: a capacity-0 `String` has no storage and plain elements, so a
//!   producer may omit its `drop`.
//! - **A byte-string literal** (`b".."`) is `Result[Bytes, Capacity]`: it admits
//!   `round8(length)` as `List[u8]` storage (`alloc own` against the current
//!   limit) and copies its bytes out of `static dN` into `{data, length,
//!   length}`; a refusal is the `Capacity` value. `b""` is `Ok({0, 0, 0})`.
//! - A static `String`'s length exceeds its capacity, so every room test is
//!   `length + n > capacity` (after the overflow check), never `capacity - length`.
//!   Growth copies `length` bytes out of the static data and frees nothing. Static
//!   data is read-only: no `store` or `copy` targets it (the evaluator stops on
//!   one). `string.into_bytes` and `clone` of a capacity-0 `String` copy into
//!   newly admitted storage of capacity equal to the length (core.md, MEM-24);
//!   `string.as_bytes` reports capacity 0.
//! - **The private kernel `string.to_bytes`** is zero instructions, so it
//!   requires a `String` that owns its storage or is empty (capacity not 0, or
//!   length 0); it never turns a static carrier into an own `Bytes`. Callers meet
//!   it by growing first (`push_char`, `push_u64` reserve), by testing the
//!   capacity (`into_bytes` copies a static `s`), or, for an operation that only
//!   shortens (`truncate`, `clear`), through the private `shorten(mut s, n)`: one
//!   store of the length word, data and capacity unchanged.
//! - **A `List` of a zero-sized element** (SEM-4: a record whose fields are all
//!   zero-sized, or an array of none or of such; `hirsel_core_layout::storage_free`)
//!   is `{0, length, 2^64 - 1}` from `list.new` on: it has no element storage.
//!   Creating, reserving, growing and cloning it emit no `alloc` and no charge
//!   (its logical charge is 0); clone copies the carrier. Only the length is
//!   bounded: a `reserve` past `2^64 - 1` elements or a push at that length is
//!   `Capacity` with `requested` `2^64 - 1`, made by an `alloc own` of that
//!   charge with `overflow` true against the current limit, which always refuses
//!   and reports that limit's room as `available` (MEM-22). Its cleanup is none,
//!   decided from the layout (its drop glue never reads the carrier), so the null
//!   data pointer under capacity `2^64 - 1` is never freed. An element access is
//!   zero bytes: a `copy` of 0 bytes touches no storage, so its addresses (the
//!   null data plus an offset) need not point at any.
//! - **Order** of `String`, `Text` (through its `text.as_string` view), `Bytes`
//!   and `List[u8]` is `rt.compare_bytes(a, a_len, b, b_len) -> i8` (-1, 0, 1;
//!   unsigned bytes, a proper prefix first, SEM-7): `==` is `eq r, 0`, `<` is
//!   `lt r, 0`. Other sequences, tuples, records and variants compare by producer
//!   code over their components, calling it for each text component. Comparison
//!   admits nothing.
//! - **Hash, equality and order of a located value** are the rows the helper for
//!   its locator's target emits (sample `stable-keys.core`). Each reads W from
//!   the blob header (at 8) and the value's count, W wide, at `blob + at`:
//!   - `Text`: its bytes follow the count; order and equality are
//!     `rt.compare_bytes`, the hash word `rt.hash_bytes(bytes, count)`, exactly
//!     those of a `String` with the same bytes, at any W.
//!   - `FixedSpan[e]`: elements follow the count at e's packed size;
//!     `SeqSpan[e]` and `IndexedSpan[e]` (read as a `SeqSpan`, trailer ignored):
//!     count, byte length, then elements, each advanced by its
//!     `compact_extent` at the holder's W (`Text`: W + count). Each element is
//!     decoded into a native place (a compact element to its locator) and its
//!     own helper called. The hash folds the elements as `List[e]` does, with
//!     no count mixed in; equality is equal counts, then element by element;
//!     order is lexicographic over the shorter count, the first difference
//!     deciding, then a proper prefix first (SEM-7).
//!   - a compact record or variant: fields in declaration order (a variant's
//!     declaration ordinal first, then its payload), as for its plain
//!     counterpart.
//!
//! # Providers (FORM v1b)
//!
//! A provider or run-time C entry is reached only through a closed runtime row
//! ([`RtOp`]); FORM has no call of a C symbol by name. The provider rows are
//! `rt.args_len`, `rt.args_get`, `rt.host_*` and `rt.stdout_write` /
//! `rt.stderr_write` ([`HostOp`], one row per core operation; signatures at
//! [`RtOp::signature`]). Their operands are scalars and pointers only: the
//! capability, `Args` or `HostFile` handle, and each `String` or `Bytes` as a
//! pointer to its carrier `{data, length, capacity}` (for a view, the address
//! of the place holding its carrier, Views). Every `host`, `stdout` and `stderr`
//! row returns the outcome `value: u64, error: u8, code: u32, written: u64`,
//! from which the producer builds the `Result`; the rows that `wait` add the
//! failure status (Native ABI) and fail with `Cancelled` when cancellation is pending, `host_mkdtemp` also with
//! `Bounds` when the name has fewer than 32 spare bytes. `host_wall_ms` and
//! `host_mono_ns` return the `u64` alone and cannot fail; neither can the `args`
//! rows. The run time reads and writes the carrier: `host_read` fills spare
//! capacity and `host_mkdtemp` appends the directory's name, each storing the new
//! length; neither changes the data pointer or the capacity (no provider
//! operation admits). `rt.args_get` returns a run-owned `String` carrier of
//! capacity 0, or null past the end; the producer copies its 24 bytes into the
//! payload of `Some`, a `View[String]` valid until the run ends.
//!
//! Engines map the rows to the run time's entries, the context first:
//! - `rt.args_len` → `uint64_t hirsel_core_args_len(const HirselCoreContext *,
//!   uint64_t args)`.
//! - `rt.args_get` → `const HirselCoreBuffer *hirsel_core_args_get(const
//!   HirselCoreContext *, uint64_t args, uint64_t index)`.
//! - every `HostOp` → `HirselCoreHostOutcome hirsel_core_host_op(HirselCoreContext
//!   *, uint8_t operation, uint64_t handle, HirselCoreBuffer *operand, uint8_t
//!   mode, uint64_t site)`, with [`HostOp::code`], the row's handle, its carrier
//!   pointer (null when it has none) and `mode` (0 when it has none).
//!   `HirselCoreHostOutcome` is the row's results then the status, laid out as
//!   the Native ABI's result struct; a row that cannot fail ignores the status
//!   and keeps `value`. The entry resolves the provider: `open` and the
//!   directory and stream operations by the capability handle, the file
//!   operations by the `HostFile`'s retained provider.
//!
//! # Maps (FORM v1b)
//!
//! `map.new` is the empty carrier `{0, 0, 0}` and `map.len` reads the live count.
//! `get`, `insert`, `set` and `remove` are ordinary code in core's `map` module over
//! private bodiless operations, each a sequence over the `map_entry` recipe: an
//! entry's hash, state, key and value by slot; the lookup index (a power of two of
//! u64 words after the entries, each a slot + 1 or 0, probed linearly); appending
//! an entry; swapping a value; removing an entry (state 0, the key cleaned, the
//! value moved out); and growth (admit, copy the live entries in order, free the
//! old storage when its capacity is not 0). Cleanup is the recipe's: live entries in
//! insertion order, each key before its value.
//!
//! A key's hash is glue built from `K`'s recipe: from `h = 0` it folds
//! [`hash_mix`] `(h, w) = rotl((h ^ w) × 0x9E3779B97F4A7C15, 26)` over each scalar
//! component zero-extended to `u64`, in field order (a variant's tag first), and
//! over `rt.hash_bytes(data, len)` for each `String`, `Bytes` or `Text` component,
//! then returns `h ^ (h >> 32)`. `rt.hash_bytes` ([`hash_bytes`]) starts at
//! `h = len` and folds the little-endian 8-byte words, the last zero-padded. The
//! function is fixed, not seeded: the producer hashes static map literals with
//! [`hash_bytes`] and the run time must agree. Iteration is insertion order
//! (SEM-5), so no hash is observable. Engines map `rt.hash_bytes` to `uint64_t
//! hirsel_core_hash_bytes(const uint8_t *, uint64_t)`, with no context or status.
//!
//! # Identities
//!
//! `rt.dest_id` is the run's one counter of fresh `u64` identities: a
//! destination's (MEM-4) and every `Unique` (`unique.new()` is `call rt.dest_id ()`).
//!
//! # Causes (FORM v1b)
//!
//! A `Causes` value's one handle is its head cause cell (null when empty); the cells
//! are the run time's. `for c in cs` walks them, primary cause first:
//! `rt.cause_value(cell) -> ptr` is the cell's `Cause` and `rt.cause_next(cell) -> ptr`
//! the next cell, null after the last. Neither takes the context, admits, waits or
//! fails; engines map them to `hirsel_core_cause_value` and `hirsel_core_cause_next`.
//! The `Cause` is in core's layout (32 bytes): `Fail`'s `String` carrier at 0 and the
//! `Site` (u64) at 24. `CauseKind` is that carrier's capacity niche (FORM #75): kind
//! `k` (declaration order: Overflow 0, Bounds 1, Division 2, Unwrap 3, Fail 4,
//! Cancelled 5, Capacity 6) other than `Fail` is the capacity 2^63 + k. `View[Cause]`
//! is that pointer, valid while the `Causes` lives. A `Fail` cause owns its message;
//! cleaning the `Causes` frees it when its capacity is not 0.
//!
//! # Capabilities and the entry (FORM v1b)
//!
//! A capability value (`Dir`, `Net`, `Clock`, `Random`, `Stdout`, `Stderr`,
//! `Stdin`, `Env`, `Process`, `Tasks`) and `Args` is a `u64` provider handle
//! (field type `cap`, logically 8). It is plain (CAP-2): copying it copies the
//! handle, and nothing in the program cleans it. The provider makes every handle
//! (entry grants, attenuation) and keeps what it designates in host memory,
//! outside every limit, until the run ends; provider operations take the handle.
//! `Args` designates the run's argument table, read by its runtime operations.
//!
//! `main`'s row is the context, its result destination, then one `u64` per
//! parameter in declaration order. The process entry passes the configuration's
//! `grants[i]` for parameter `i` (`Args` included); the driver maps each parameter
//! type to the configuration's grant and refuses a run whose configuration does
//! not grant one before calling `hirsel_core_run` (CAP-4). After `main` returns and
//! its cleanup is done, the run ends its grants: the provider closes what they
//! designate. That is not program cleanup: it admits nothing, runs no finalizer
//! and cannot fail, and no copy of a handle is live by then.
//!
//! # Failure (FORM v1a)
//!
//! An instruction that can fail ([`Inst::can_fail`]) may name a failure successor:
//! a block without parameters, entered with the cause already appended to the
//! task's chain. Without one, the failure leaves the function at once, so a
//! producer names a successor exactly where owners are live. A failure path ends
//! in `propagate` (or `raise`, which appends a new cause first) and never reaches a
//! `return`: nothing catches a failure (FAIL-14). Propagation reserves nothing;
//! the chain lives in one cell per task and one per resource (FAIL-10). Cleanup on
//! a failure path drops the live owners in the order the normal exit would. A
//! failing finalizer appends its cause, its resource's fields are still cleaned and
//! its block freed (FAIL-12); a pending result is discarded. Mechanical cleanup
//! (fields, boxes, counted releases, handles) never fails (FAIL-13). `Capacity` is
//! a failure only inside a finalizer, against its grant (MEM-27); everywhere else a
//! refused admission is a value: `alloc` yields null and the room left in the
//! limit it tried (MEM-22 `available`). [`failure::Failures`] says which functions
//! of a module may fail; wf and the LLVM emitter share it.
//!
//! # Storage and admission (FORM v1a)
//!
//! Every charge is a `charge-v1` logical size computed by the producer and passed
//! to `alloc`; `free` refunds the same amount to the limit the block's record now
//! leads to (MEM-20, MEM-23). `limit_enter b` admits `b` and makes a limit of `b`
//! current; `limit_exit` returns its unused part and forwards its record to the
//! parent, so blocks made inside stay charged (MEM-25). `alloc .. like v` admits
//! against the limit `v` was charged to (MEM-26), except inside a finalizer, where
//! it admits from the current limit (MEM-27): the grant, or a bound scope entered
//! within the finalizer, which is then current (MEM-19). A refusal is the failure
//! `Capacity` only when the refusing limit is the grant itself; a bound scope's
//! refusal, inside a finalizer too, is a value (MEM-25), and `limit_enter`'s always
//! is. A resource value is a pointer to its block, charged
//! storage + cause cell + grant; its finalizer runs under a limit of its grant,
//! whose used part is handed over at the end. A whole destructure moves the fields
//! out with `copy` and `free`s the block, refunding that complete charge (MEM-23). A counted node is a u64 count at
//! offset 0 and the payload at offset 8; `deref` of an rc or arc field names the
//! payload. A counted payload holds no resource, so a release runs no user code.
//! Every counted row names its kind: `alloc counted rc|arc` and `share`,
//! `release` and `try_unwrap` with `rc|arc`, so each row alone says whether its
//! count operations are atomic. An `arc` node is made and counted atomically, and
//! each of these rows is an event in the run's order (MEM-15); `rc` never crosses
//! a task (Rc is not `send`). The producer takes the kind from the core type the
//! source operation is on, `rc.*` or `Rc[T]` gives `rc`, `arc.*` or `Arc[T]`
//! gives `arc`, never from bounds of `T`; a row whose kind differs from its node's
//! allocation is malformed (the evaluator stops on it).
//!
//! # Runtime calls (FORM v1a)
//!
//! `Callee::Runtime` names an engine operation with a fixed signature row
//! ([`RtOp::signature`]); the call site's signature must be that row. Task and
//! channel charges are admitted whole at `rt.spawn` and `rt.channel_new` (queue,
//! result slot, record, primary cell and the child's limit), so send, receive and
//! join admit nothing. A refused spawn returns null with `x` still the caller's.
//! `rt.join` writes the result, or returns a `Causes` that owns the child's used
//! cells until it is dropped (`JoinError = Failed(Causes) | Cancelled(Causes)`).
//!
//! # Native ABI (FORM v1a)
//!
//! **Every function row** uses one convention, the same for direct calls, indirect
//! calls through a code pointer (`fnref`) and `extern` rows (a FORM function
//! defined in another module, linked by its instance digest; never a C symbol):
//! - The task context pointer comes first, then the destination pointer when
//!   `dest` is present, then the parameters as in FORM v0. The context carries the
//!   current limit stack and the cause chain.
//! - The function returns, in the target's C convention, a struct of its result
//!   scalars **followed by a `u8` failure status**: 0 when it returned, 1 when it
//!   failed, its cause already appended to the task's chain in the context. No
//!   other value occurs. With no results the return is a plain C `u8`; with one
//!   result it is `{ r0, u8 }` (two registers on x86-64 SysV and AArch64).
//!   A `never` function returns only the status, which is always 1.
//! - After the call the caller tests the status once: zero continues with the
//!   results, nonzero enters the call's failure successor, or propagates (returns
//!   status 1 itself) when there is none. Results are undefined when it is 1.
//!
//! Every function row has the status, whether or not its body can fail: a caller
//! reads only the callee's signature (LANG-8) and an instance is compiled and
//! cached alone, so whether a body can fail is not known at the call. A function
//! whose body cannot fail returns status 0 by a constant; an emitter that has the
//! callee's body in hand (inlining, or a callee in the same unit with no failing
//! exit) may drop the test. `main` is entered by the engine the same way.
//!
//! **Runtime rows** (`Callee::Runtime`) are the runtime's own declared C entry
//! points, with the row's parameters and results. Only the rows that can fail
//! ([`RtOp::fails`]: `join`, `check_cancel`, `channel_new`, `send`, `receive`)
//! append the `u8` status, and only those may name a failure successor. The
//! runtime entries behind instructions (`alloc`, `free`, `limit_enter`, region
//! operations, `trim`) are not calls in FORM and report refusals as values.
//! `rt.dest_id` is a runtime row with no parameters and one `u64`, no status;
//! `rt.compare_bytes` likewise takes no context and has no status.
//!
//! Layouts are the recipes': resources and boxes are pointers, counted nodes are
//! count then payload.
//!
//! # Values, slots and addresses
//!
//! A value is defined once, by a block parameter or an instruction result, and is
//! a scalar ([`Scalar`]) or a pointer. A slot is a typed piece of frame memory with
//! no initialization discipline beyond what its producer guarantees: `StorageLive`
//! begins it, `StorageDead` ends it, reads and stores in between name addresses.
//! An address is a base (a slot, a pointer value or another address) and steps;
//! `Inst::Address` evaluates it and runs its bounds checks, and later reads and
//! stores use it. Uses need not be dominated by definitions in any sense stronger
//! than the program's own control flow: emitters do not re-check.
//!
//! # Calling convention (FORM v0)
//!
//! A function with signature `(p0, ..., pn) -> (r0, ..., rm) dest t` is called
//! with, in order: a pointer to the destination when `dest` is present; then each
//! parameter, a scalar or pointer by value, or a by-address parameter (`&t`) as a
//! pointer to a place of that type the caller owns for the duration of the call.
//! Results come back in registers in the target's C convention for a struct of the
//! result scalars (`m <= 1`: the plain C return). `bool` travels as a byte 0/1,
//! `char` as a u32 scalar value. Link names are `h` followed by the hex digest of the
//! instance key ([`InstanceKey::digest`]). The context parameter and the failure
//! status are the FORM v1a additions to this convention (Native ABI, above).
//!
//! # Loans
//!
//! Every by-address parameter states how its place is lent for the call
//! ([`Loan`]), written `&mut t`, `&view t` or `&take t`. The extent is the whole
//! call, from entry to every exit, failure cleanup included. These are facts the
//! checker already proved (OWN-7..11); an emitter may rely on them and may ignore
//! them:
//! - `mut`: no other path reaches the place during the call;
//! - `view`: nothing writes the place during the call;
//! - `take`: the callee consumes the place, by reading it (into its own slot, as a
//!   front-lowered callee does at entry) or by dropping or moving out of it in
//!   place. Neither the callee nor anything else writes the place during the
//!   call. A copyable value may therefore be lent as `take` from a place other
//!   arguments still view.
//!
//! What `noalias` on a `take` parameter claims, exactly: during the call no
//! write reaches that memory through any other pointer, and the callee itself
//! never writes the caller's place (it writes only its own slot). That holds even
//! when a `view` of the same place is passed alongside, because neither is
//! written. `take` does not say the place is readonly in LLVM's sense (a drop in
//! place releases what it owns), and it does not say the pointer is not captured.
//!
//! A `view` is not invariant beyond the call, and a `mut` place changes. Places
//! behind the parameter (a carrier's buffer, a counted header, a queue, a region
//! record, a provider handle) carry no loan. The destination is a fresh place the
//! caller provides; it is exclusive like `mut`. wf refuses a body that writes its
//! own `view` or `take` parameter's place (store, copy to, set case, lending it as
//! `mut` or as a destination), a drop of its own `view` place, and a call whose
//! `mut` place or destination provably overlaps another argument's place (the
//! same root, one projection a prefix of the other). The test evaluator checks
//! the rest on every call: a `mut` place or the destination overlaps no other
//! argument's place, and no write touches a `view` or `take` place while its call
//! runs. It runs only entries that take no host arguments: the front corpora's
//! cases and evaluator-backend laws, not native programs. On the native side,
//! running each program with an emitter's loan-derived facts on and off and
//! comparing the outputs is the safety net.
//!
//! # Roles
//!
//! [`roles::roles`] gives some addresses of a body a storage role. It derives
//! them from the form itself; nothing stores them. Two accesses (`read`, `store`,
//! either side of a `copy`) through addresses of different roles touch disjoint
//! bytes, throughout one activation. An emitter may rely on that, and may ignore
//! it. An address without a role makes no claim.
//! - A **carrier type** is an own record whose one field is a `seq` of a scalar:
//!   `String`, `Bytes`, `List` of a scalar. Its buffer holds scalars, never a
//!   carrier.
//! - `Carrier(root)`: bytes of the carrier held in a slot of a carrier type, or
//!   lent by a `mut` or `take` parameter of one (Loans). The address reaches it
//!   through `pointer` values, `field 0` and constant `byte` steps, and stays
//!   within its 24 bytes. A parameter's entry value has its role only while no
//!   edge targets the entry block. `view` parameters get none, because two views
//!   of one place may be passed together.
//! - `Buffer`: storage reached through a pointer read from a carrier role's data
//!   word, then `index` or `byte` steps. Every buffer of a body has this one
//!   role, because a move hands a buffer from one carrier to another.
//!
//! This is what the language already guarantees. Slots are distinct. A `mut` or
//! `take` place overlaps no other argument (Loans) and lies outside the callee's
//! frame. A buffer is separately owned storage (OWN-3), static bytes or a view of
//! either, and it holds scalars. Counted headers, map storage, queues, region
//! records and provider handles have no role. LLVM can take one alias domain per
//! function and one scope per role; its inliner clones them. The test evaluator
//! derives the same roles on every call, and it stops the run when two roles of
//! one activation touch the same byte.
//!
//! # Instances
//!
//! A function row names an instance: a definition plus the canonical text of each
//! closed type argument and of each implementation chosen for a constraint. Sizes
//! are not part of the key: they are a function of the types and the target, and
//! sharing code between same-layout instances is an emitter's identical-code
//! folding, not identity. The driver collects the instances a program reaches and
//! caches them by digest; emitters materialise the bodies they are given. A
//! program whose instantiation keeps growing (`f[T]` calling `f[List[T]]`) is
//! refused by the driver as a compiler resource refusal (type-argument nesting or
//! instance count past the compiler's resource limit), never as a language error
//! and never by a depth rule in the language.
//!
//! An instance's signature is the closed signature of its substituted types (FORM
//! #64): a parameter of a type argument is carried the way that type is carried in a
//! closed function. Each type row publishes how ([`Carried`], text `value s`,
//! `value ptr`, `nothing`, or nothing written for by address), written by the
//! producer from the same rule its closed signatures use, so an importer or
//! materialiser reads the row and never classifies a body: a scalar `u64`, the
//! one-field record `W { x: u64 }` and the tuple `(u64,)` all have the body
//! `{ 0: u64 }`, and only the first is `value u64`. By value: every scalar type
//! (integers, `bool`, `char`, `f32`, `f64`) as itself; a plain function, `Region` and
//! `Writer` as `ptr`; a capability, `Args`, `Unique` and `Site` as a `u64` word; a
//! `View[T]` as `T`; and an own or stable variant whose bytes are one word (FORM #78)
//! as that word. A variant is one word when all its cases are fieldless (the tag, `u8`,
//! `u16` or `u32`), or when one case holds one field, every other case is fieldless and
//! the field leaves spare values for them: a `bool` (`u8`), a `char` (`u32`), a
//! one-field record of either, or such a variant, so `Option[char]` is `value u32` with
//! `None` as 0x110000 and `Option[Option[bool]]` is `value u8`. `()`, `Never` and the
//! error type carry nothing (the parameter is dropped). Everything else, tuples,
//! arrays, records, other variants, handles to runtime data (strings, lists, maps,
//! boxes, tasks, channels, host files, causes) and closures, is passed by address and
//! returned through `dest`. A body holds a word variant in a place as any variant; the
//! boundary loads and stores its word. Layout refuses a row carried unlike its body:
//! on a record `value s` needs exactly one field of scalar `s`, `value ptr` one `fn` or
//! `borrow` field, `nothing` no field; on a variant `value s` needs the word rule's `s`
//! and a layout whose tag fills the word. `samples/scalar-rows.core` shows less[u64] by
//! value beside less[W] by address.

#[cfg(test)]
hirsel_guard::install!();

/// A test evaluator for the FORM v0 subset, for this crate's samples and its producers'
/// behaviour cases.
#[cfg(any(test, feature = "eval"))]
pub mod eval;
pub mod failure;
mod ir;
pub mod roles;
pub mod text;
#[cfg(any(test, feature = "wf"))]
pub mod wf;

pub use ir::*;

#[cfg(test)]
mod tests;
