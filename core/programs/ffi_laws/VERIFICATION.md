# Native foreign verification

Each law's expected value comes from this chapter, from a published test vector, from SQLite's
documented result codes or from the fixture's own counters. None comes from a run's output.
Every law that runs code runs on both native backends. Laws FL1 to FL7, FL12 to FL18 and FL23
use the fixture package `ffi_laws` in the program tests, with interface `Laws` and handle kind
`Obj`, both in module `laws`. Its adapter `laws.c`, prefix `hfl_`, owns a static pool of 8 one-byte cells, and a
freed cell is the first one reused, so a `make` after a `free` returns the same address. It
defines:

- `make(f) -> Option[foreign.Ptr[Obj]]` and `make_w`, the same declared `waits`: take a cell,
  store in it the count of makes so far plus 1, increment `made`, and return the cell;
- `free(f, take p) -> i32`, declared `waits`, the kind's release: write `freed K` and a line
  feed to file descriptor 2 with `write(2)`, `K` the cell's stored count, return the cell to
  the pool, increment `freed` and return 0;
- `freed(f) -> u64` and `made(f) -> u64`;
- `same(f, p) -> Option[foreign.Ptr[Obj]]`, with `p` a handle in use, declared `waits`: return `p`'s own pointer;
- `mark(f, mut b) -> u64` and `mark_w`, the same declared `waits`: write `0xAB` to each of the
  loan's `len` bytes and return `len`;
- `loan_length(f, b: Bytes) -> u64` returns the length of the immutable loan, and
  `loan_sum(f, b: Bytes) -> u64` returns the sum of its bytes;
- `gate(f, on: u32) -> i32`, on the host stack: sets or clears the gate that FL17 uses;
- `touch(f) -> i32`: write `touched` and a line feed to file descriptor 2 and return 0;
- `hold(f) -> i32`, declared `waits`: increment `held`, then wait on a semaphore; `held(f) ->
  u64`; and `open(f, n: u32) -> i32`, on the host stack, which posts the semaphore `n` times.
  `hold` breaks FFI-7's progress clause on purpose, and only `open`, which needs no helper,
  ends its wait.

The [FFI refusal cases](../../front/cases/ffi/) run with a foreign manifest. The
[SQLite package](../sqlite/) owns the SQLite laws.

| law | bug | oracle and setup |
|---|---|---|
| **FL1** a wrong archive digest | a pin checked after linking, or not at all | `ffi_laws`' x86_64 row names a file whose bytes are exactly `abc`, pinned to the SHA-256 of the empty string. `hirsel build` of an entry that calls `native.make` is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu does not match its pin: expected sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855, found sha256 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad` (FIPS 180-2 vectors), and the linker is never started. With the pin changed to `ba78…15ad`, the same build is refused with `… is not an indexed ar archive`, so the digest is checked before the bytes are parsed. |
| **FL2** a missing target row | a missing row passed through as a linker error, or a row required when nothing is reached | With only the x86_64 row, `hirsel build --target aarch64-unknown-linux-gnu` of the FL1 entry, with its real pin, is refused with `FFI-3: ffi_laws@1/native has no archive for target aarch64-unknown-linux-gnu; its archives are for x86_64-unknown-linux-gnu`, before the linker starts. An entry of the same package that never names `native` gets no FFI-3 refusal for aarch64. This is a negative test: it gives no AArch64 coverage. |
| **FL3** double release | a second release that reaches C | A test takes `f: Foreign[Laws]`, makes `p` with `native.make`, calls `native.free(f, p)`, then calls `native.free(f, p)` again inside an `isolate`. The `isolate` returns `Ok(Error(causes))`, with primary cause `Fail("foreign handle released")` at the second `free`, and then `native.freed(f)` returns 1. |
| **FL4** stale use and a racing release | a released handle's pointer passed to C, a copy that escapes the check, or two releases that both reach C | As FL3, but the second call is `native.free(f, q)`, where `q` is a copy of `p` that a resource field held: the same cause, and `freed` returns 1. Then two `send` resources `ObjBox` of the fixture each hold a copy of one fresh handle (the fixture breaks FFI-6's one-field rule on purpose), each moves into its own task, and both tasks release it at once with `close_obj`: one returns 0, the other fails with `Fail("foreign handle released")` (it waited for the first and found it consumed, FFI-6), never `in use`, and `freed` returns 2. |
| **FL5** byte loans preserve length, storage and range | capacity passed as the length, storage that moves while a helper writes it, or a read beyond an immutable slice | `let mut b: Bytes = list.with_capacity(16)`, then `list.fill(mut b, 4, 0)`. `native.mark(f, mut b)` returns 4. Afterwards `list.len(b)` is 4 and every byte of `b` is `0xAB`. Then `list.push(mut b, 0)` admits nothing, because the capacity is still 16. An empty `b` gives `mark` 0, and the adapter checks that `ptr` is not `NULL`. In a separate test, fill an owned `Bytes` list `d` with four zero bytes. `native.mark_w(f, mut d)` returns 4 while another task runs 10⁴ allocations. Afterwards every byte of `d` is `0xAB`. For the immutable range test, fill `c: Bytes` with the twelve values 0 through 11 and pass `list.slice(c, 4, 8)` to `native.loan_length` and `native.loan_sum`. They return 4 and 22, the length and sum of bytes 4 through 7. |
| **FL6** an FFI-reaching test refused a cached pass | a pass key made for a test whose reach holds C | A parameterless test calls `probe(None)`, where `fn probe(f: Option[Foreign[Laws]]) -> u64 { match f { Some(f) => native.freed(f), None => 0 } }`, and passes. Run `hirsel test` twice with one cache: both runs report it executed, and neither reports it reused. A parameterless test beside it that does not call `probe` is reported reused on the second run. The same holds when the only path to `native` is a resource's finalizer, on a fresh and on a warm reach record. |
| **FL7** recording and replay refused before C runs | a live recording half, a recording host or a stored replay that enters native code | `hirsel test --replay` of a test that calls `native.touch` fails with a line ending `refused: FFI-8: a recording build cannot reach foreign code: ffi_laws@1/tests/touches reaches ffi_laws@1/native/touch`. Its stderr holds no `touched`, and no recording file exists. `hirsel tape replay` of an entry that reaches `touch`, given any tape path, exits 2 with `replay: refused: FFI-8: …` and does not open the tape. `hirsel build --recording` is refused the same way. A deploy of that entry's recipe to a recording group exits 2 with `FFI-8: a recording build cannot reach foreign code: …`, the group keeps serving its old image, and the host's code store gains no copy of the recipe; `hirsel deploy --create SPEC` with `recording` enabled exits 2 with `FFI-8: …` and creates neither the recording directory nor any host state. `hirsel tape replay --state` given a host state whose assembly record holds that recipe, and a tape whose routing header names it and whose records after the header are not valid records, exits 2 with `replay: refused: FFI-8: …`, not a tape error. |
| **FL8** SQLite round trip | a loan, `SQLITE_TRANSIENT` or `NULL`/empty mistake | In `sqlite`'s own tests, with grant `Foreign=hirsel.ai/sqlite@1/sqlite/Sqlite`: create `t(b blob)`, insert `x''`, `NULL` and `x'00ff'` with `bind_bytes` and `bind_null`, after which the bound list is cleared and refilled. Selecting in insertion order gives `Blob` with 0 bytes, `Null`, and `Blob` with `00 ff`. |
| **FL9** SQLite release paths | a finalizer that runs after an explicit release, a busy close, or a last release that frees the mutex it reports through | Four tests, each preparing `s` from `select 1` on a fresh connection `db`: (a) `finalize(take s)` then `close(take db)`; (b) `close(take db)` then `finalize(take s)`; (c) `close(take db)` with `s` dropped at scope end; (d) both dropped at scope end, in either order. Every explicit call returns `Ok(())`. In (b), `step(mut s)` between the two calls still returns `Row`, because the connection stays open until its count is zero. With `s` from `select abs(-9223372036854775808)` stepped once, so its last result is an error, `close(take db)` returns `Ok(())` and `finalize(take s)` then returns `Error(SqliteError(code: 1, message: "SQL logic error"))`, `sqlite3_errstr(1)`'s text; each test passes with no cause, so no finalizer met a consumed handle. |
| **FL10** no ambient input | the OS layer, libc or a pragma reading or changing process state | `select random()` returns the same value in two runs of a fresh process. `select date('now')`, `select datetime(0, 'localtime')` and `select datetime(0, 'utc')` each return `NULL`. `attach ':memory:' as x` fails at `prepare` with code 23, and `vacuum` and `vacuum into 'x.db'` fail at `step` with code 23 (`SQLITE_AUTH`). `pragma temp_store_directory = '/tmp'` and `pragma soft_heap_limit = 1` fail with code 23. |
| **FL11** directory confinement (slice 2) | a sidecar, temp file, super-journal or attach outside the grant | In a granted scratch `Dir` whose parent holds a sentinel directory: open `a.db`, run a WAL transaction from two connections, then a rollback-journal transaction. The files created are exactly `a.db`, `a.db-wal` and `a.db-shm` (WAL), and `a.db-journal` only while the journal transaction runs. The parent and the sentinel are unchanged. `open(f, d, "../x")` and `open(f, d, "file:a.db?vfs=unix")` give code 14 before C runs. A hot journal whose super-journal name is `/tmp/evil` fails recovery with `SQLITE_CANTOPEN` and opens no file outside. A `sort` over 10⁶ rows creates no file. Two connections to one leaf under two grants of different directories each see only their own rows. |
| **FL12** an archive whose code is outside its pin | a thin archive, or a member that is not a relocatable object, admitted because its digest matches | `ffi_laws`' x86_64 row names an indexed thin archive made with `ar --thin` from the adapter's `laws.o`, which lies beside it, pinned to the thin archive's true SHA-256. `hirsel build` of the FL1 entry is refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu is a thin archive; its members are not inside the pinned bytes`, and the linker is never started. With the row naming a regular archive whose one member is `laws.o` compiled for `aarch64`, again with its true pin, the refusal is `… member laws.o is not a relocatable object for x86_64-unknown-linux-gnu`. With a regular x86_64 archive whose index lists `hfl_make` but whose member does not define it, the refusal is `… does not define hfl_make, declared at SITE`. |
| **FL13** a recipe-only build of a foreign program | a recipe from which a reader cannot recompute the foreign address, or one that links archives the leaves do not select | `ffi_laws` gets a second row, `aarch64-unknown-linux-gnu`, naming any file with its true pin. `hirsel build --emit recipe --code-store DIR` of the FL1 entry for x86_64 prints R. Delete the package's directory: `hirsel build --recipe R --code-store DIR` succeeds, and the executable's `native.make` call works. Change only the aarch64 row's bytes and pin and publish again: the new R differs from R (FFI-9), and it builds from the code store alone the same way. A recipe made from R's bytes by removing the x86_64 archive's entry from its foreign archives part, with its objects in the store, is refused as `Missing`, and one made by adding the aarch64 archive's entry as `Extra`. |
| **FL14** archive code that runs with no grant | an initializer, an indirect function or an out-of-prefix symbol admitted, so C runs before main or with no call | Each archive below has its true pin, and a parameterless test of `ffi_laws` that never names `native` sits beside the FL1 entry. `laws.c` plus `__attribute__((constructor)) static void hfl_c(void) { write(2, "ctor\n", 5); }` makes the build of the FL1 entry refused with `FFI-3: archive F of ffi_laws@1/native for x86_64-unknown-linux-gnu: member laws.o has section .init_array of type SHT_INIT_ARRAY, which is not an admitted object feature` before linking. With the same archive, `hirsel test` of the package, whose tests are the parameterless one and one that takes `Foreign[Laws]`, is refused at its build with the same text, and its output holds no `ctor`. An archive with a prefixed symbol `hfl_unique` bound as `STB_GNU_UNIQUE` is refused with `… member laws.o has symbol hfl_unique with binding STB_GNU_UNIQUE, which is not an admitted object feature`. An archive defining the global `malloc` is refused with `… member laws.o defines malloc, which does not start with the prefix hfl_`. One defining `hfl_x` as an `ifunc` is refused with `… has indirect function hfl_x, …`. |
| **FL15** a header that disagrees with its archive | a wrapper whose header says `handle` or `u64` where the audited C takes `take` or a loan | `ffi_laws`' header declares `free(f, p: foreign.Ptr[Obj]) -> i32 waits` (no `take`), while the archive's `.hirsel_abi` comes from the true header. `make` still makes `Obj`, so its first use of the kind is refused with FFI-5 because the kind has no release. No archive is admitted. An archive with no `.hirsel_abi` is refused with `… has no ABI text`. A refusal case with two kinds `A` and `B` in one package stores a `foreign.Ptr[A]` in a field of type `foreign.Ptr[B]` and is refused at that field by the type checker. |
| **FL16** a lost or misplaced release | a cancelled close, a failed finalizer or an un-released local that leaks or double-releases | (a) A task with a cancellation already requested calls `close_obj(take o)`, where `o: ObjBox` holds `p` and `close_obj` destructures it and calls `native.free`: `free` is issued and returns, `freed` rises by 1, and the task's next cancellation point fails with `Cancelled`. (b) `ObjBox`'s finalizer variant `drop(o) { fail("x") }`: after the box is dropped, `freed` has risen by 1, by the mechanical release, and the finalizer's cause is reported (FAIL-12). (c) `hirsel run` of an entry that makes three handles with `native.make` into locals and returns: stderr ends with exactly `freed 3`, `freed 2`, `freed 1` on three lines, in descending ordinal order. |
| **FL17** an address reused or handed back | a released pointer that leaves the index only at delivery, refusing a new object made at its address while the release is still in C, or a returned pointer that aliases a live owner | With `gate(f, 1)` set (a host-stack fixture function after which `free` returns its cell to the pool, increments `held` and then waits on the semaphore before returning), a task makes `p` with `make` and calls `free(f, p)`. The test calls `held` until it returns 1, so the release is in C with the cell already free, and then calls `q = make` on the host stack: `q` is accepted though the pool returned `p`'s cell, because `p`'s pointer left the index at issue. `open(f, 1)` ends the release; `made` returns 2 and `freed` 1. With `gate(f, 0)`, `native.same(f, q)` fails with `Fail("foreign pointer already held")`, `made` is unchanged, and `free(f, q)` then returns 0 and `freed` returns 2. A refusal case binds `ffi_laws`' `make` in a second interface's module, naming `Obj` there too, and is refused at the second (FFI-6). |
| **FL18** a full helper pool | four calls holding every helper and starving a fifth, a lost queue entry, or a cancellation in the queue that leaves a handle in flight | Four tasks call `hold`; the test calls `held` on the host stack until it returns 4. In a `hirsel run` entry, a fifth task makes its own `p` with `make` on the host stack, the `K`-th make, and calls `native.same(f, p)`; a sixth calls `make_w`; both wait in the queue. Cancelling the fifth makes it fail with `Cancelled`, whether it was observed at entry or in the queue. Then `open(f, 4)` ends the four holds, every task ends, and the sixth's `make_w` returns a handle with `made` one more than before. The run's stderr then holds `freed K`, written by run-end release, so the cancelled call left `p` live, not in flight. |
| **FL19** a SQLite column read with no row, or an unsafe SQL input | C reading a column with no current row or out of range, a C string reader past the loan, or a second statement prepared silently | `column_i64` right after `prepare` fails with `fail("sqlite: no current row")`; after `step` returns `Done`, the same; with a row of 1 column, `column_i64(mut s, 1)` fails with `fail("sqlite: column index out of range")`; `bind_i64(mut s, 0, 1)` gives `Error(SqliteError(code: 25, message: "sqlite: parameter index out of range"))`. `exec` of the bytes `create table t(x); `, a zero byte and `drop table t` gives `SqliteError(code: 1, message: "sqlite: SQL holds a zero byte")` and `t` does not exist afterwards. `prepare` of `select 1; select 2` gives `Error(SqliteError(code: 1, message: "sqlite: prepare takes one statement"))`, `prepare` of ` -- c` gives `… "sqlite: no statement"`, and `exec` of ` -- c` gives `Ok(())`. |
| **FL20** a SQLite error message at the buffer's edge | a scan past the buffer, a cut inside a character, or a code decoded with the wrong width | In `sqlite`'s own tests through its private `read_error`: a buffer whose bytes 0 to 3 are `01 00 00 00` and bytes 4 to 259 are 256 `a`s decodes to code 1 and a 256-byte message; bytes 0 to 3 `0a 04 00 00` decode to code 1034 (`SQLITE_IOERR_FSYNC`, 10 + 4 · 256) and bytes `ff ff ff ff` to code −1; bytes 4 and 5 `c3 28` decode to `"sqlite: error text is not UTF-8"` and an all-zero payload to `"sqlite: no error text"`. Through the adapter: `prepare` of `select * from ` followed by 240 `a`s and `é` (`c3 a9`) fails with code 1 and the message `no such table: ` and the 240 `a`s, 255 bytes: SQLite's message is 257 bytes, and a cut at 256 would split the `é`. |
| **FL21** SQLite contention from two tasks (`Db` and `Stmt` are `send`) | a bind or column call that blocks a scheduler thread on the connection's mutex, or an error message replaced between the failing call and its read | Two tasks each hold a statement of one connection, one stepping `with recursive c(x) as (select 1 union all select x+1 from c where x < 10⁶) select count(*) from c` and the other binding and stepping `select ?1` 1000 times; both return their expected values (10⁶, and each bound value), and a third task with no SQLite call finishes while they run on a 1-thread scheduler. With `create table u(x unique)` holding 1, a step of `insert into u values (1)` in one task while the other task's statements succeed on the same connection returns `SqliteError(code: 2067, message: "UNIQUE constraint failed: u.x")` (`SQLITE_CONSTRAINT_UNIQUE`, 19 + 8 · 256). |
| **FL22** SQLite in a granted directory under contention (slice 2) | a lock released by a sibling's close, a second writer admitted, a stale `-shm` read, a read-only `Dir` that writes, or a sync error dropped | With `a.db` in a scratch `Dir`: two connections in one process both begin `immediate`; the second gets `SQLITE_BUSY` (5) at once. Closing a third connection to `a.db` while the first holds its write lock leaves the lock held: the second still gets 5. A second process does the same. Opening `a.db` through a read-only grant and running `insert` gives `SQLITE_READONLY` (8), and the directory's bytes are unchanged; with `a.db` in WAL mode, `open` through the read-only grant succeeds and its first `select` fails with `SQLITE_CANTOPEN` (14). A `-shm` file of 32 KiB of `0xFF` left by a killed process is truncated by the next first opener, which reads the database correctly. A hard link `b.db` to `a.db` makes `open(f, d, "a.db")` fail with code 14. |
| **FL23** a raw handle that escapes its owner | a handle reached outside its owner through projection, inference, a generic argument or a capture | Refusal cases in package `ffi_laws`, each refused with FFI-6 at the stated site: `pub resource type Open { f: Foreign[Laws], p: foreign.Ptr[Obj] } drop(o) { let _ = native.free(o.f, o.p) }`, which is not `opaque`, at the type; `let l: List[foreign.Ptr[Obj]] = list.new()`, where the binding supplies the expected type (LANG-11), at the raw handle element type; `fn id[T: plain](x: T) -> T { x }` called as `id(p)`, at the call, for its inferred type argument; and a closure that names `p` in its body, at the capture. Accepted: `fn keep(o: ObjBox) -> List[ObjBox]`, because an owning resource holds no raw handle, and `let q = p` with `q` then unused, after which `freed` is unchanged. |

## Package-context examples

These examples use their named package and manifest. The native and SQLite
fixture packages supply the declarations and grants.

```hirsel
// native.hirsel, module native of package sqlite, marked foreign
pub fn open_memory(f: Foreign[sqlite.Sqlite], mut e: Bytes) -> Option[foreign.Ptr[sqlite.DbHandle]] waits
pub fn exec(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> i32 waits
pub fn close(f: Foreign[sqlite.Sqlite], take db: foreign.Ptr[sqlite.DbHandle]) -> i32 waits
```

```hirsel
// native.hirsel, marked foreign
pub fn version(f: Foreign[sqlite.Sqlite]) -> i32 { 3 }          // refused: FFI-2, a foreign function has no body
```

```hirsel
// in package app, which depends on sqlite
fn v(f: Foreign[sqlite.Sqlite]) -> i32 { sqlite.native.version(f) }   // refused: FFI-2, native is private to sqlite
```

```hirsel
// in module sqlite of package sqlite
fn later(f: Foreign[Sqlite]) -> fn(Foreign[Sqlite]) -> i32 { native.version }   // refused: FFI-2, a foreign function is only called
```

```hirsel
pub fn main(out: Stdout, db: Foreign[sqlite.Sqlite]) -> Result[(), sqlite.SqliteError] waits {
  let mut c = sqlite.open_memory(db)?
  sqlite.exec(mut c, "create table t (x integer)")?
  sqlite.close(take c)
}
```

```hirsel
// in the wrapper: copy a column's bytes out through an exclusive loan
fn column(f: Foreign[Sqlite], s: foreign.Ptr[StmtHandle], i: u32, mut out: Bytes) -> i32 waits {
  let mut e: Bytes = error_buffer()
  let n = native.column_len(f, s, i, mut e)       // -1 on failure, the error in e
  if n < 0 { return 1 }
  let start = list.len(out)
  list.fill(mut out, int.narrow(n), 0)
  native.column_copy(f, s, i, mut out, start, mut e)   // C writes bytes start .. start + n
}
```

```hirsel
// native.hirsel, marked foreign
pub fn name(f: Foreign[sqlite.Sqlite], s: String) -> i32       // refused: FFI-5, String is not a foreign type
```

```hirsel
// in module sqlite of package sqlite
pub opaque type DbHandle {}
pub opaque resource type Db: send { f: Foreign[Sqlite], p: foreign.Ptr[DbHandle] }
  drop(d) { let _ = native.close(d.f, d.p) }
```

```hirsel
// in module sqlite of package sqlite
pub type Leak { p: foreign.Ptr[DbHandle] }                        // refused: FFI-6, a handle field outside a resource
```

```hirsel
// in module sqlite of package sqlite
pub fn raw(d: Db) -> foreign.Ptr[DbHandle] { d.p }                // refused: FFI-6, a handle in a pub signature
```

```hirsel
pub opaque type Sqlite {}
pub opaque type DbHandle {}
pub opaque type StmtHandle {}
pub type SqliteError { code: i32, message: String }
pub type Step = Row | Done
pub opaque resource type Db: send { f: Foreign[Sqlite], p: foreign.Ptr[DbHandle] }
  drop(d) { let _ = native.close(d.f, d.p) }
pub opaque resource type Stmt: send { f: Foreign[Sqlite], p: foreign.Ptr[StmtHandle] }
  drop(s) { let _ = native.finalize(s.f, s.p) }
pub fn open_memory(f: Foreign[Sqlite]) -> Result[Db, SqliteError] waits
pub fn exec(mut db: Db, sql: String) -> Result[(), SqliteError] waits
pub fn prepare(mut db: Db, sql: String) -> Result[Stmt, SqliteError] waits
pub fn bind_i64(mut s: Stmt, i: u32, v: i64) -> Result[(), SqliteError] waits
pub fn bind_f64(mut s: Stmt, i: u32, v: f64) -> Result[(), SqliteError] waits
pub fn bind_bytes(mut s: Stmt, i: u32, v: Bytes) -> Result[(), SqliteError] waits
pub fn bind_text(mut s: Stmt, i: u32, v: String) -> Result[(), SqliteError] waits
pub fn bind_null(mut s: Stmt, i: u32) -> Result[(), SqliteError] waits
pub fn step(mut s: Stmt) -> Result[Step, SqliteError] waits
pub fn column_kind(mut s: Stmt, i: u32) -> Kind waits
pub fn column_i64(mut s: Stmt, i: u32) -> i64 waits
pub fn column_f64(mut s: Stmt, i: u32) -> f64 waits
pub fn column_bytes(mut s: Stmt, i: u32, mut out: Bytes) -> Result[(), SqliteError] waits
pub fn reset(mut s: Stmt) -> Result[(), SqliteError] waits
pub fn finalize(take s: Stmt) -> Result[(), SqliteError] waits
pub fn close(take db: Db) -> Result[(), SqliteError] waits
pub type Kind = Integer | Float | Text | Blob | Null
```

```hirsel
// native.hirsel, module native of package sqlite, marked foreign
pub fn open_memory(f: Foreign[sqlite.Sqlite], mut e: Bytes) -> Option[foreign.Ptr[sqlite.DbHandle]] waits
pub fn exec(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> i32 waits
pub fn prepare(f: Foreign[sqlite.Sqlite], db: foreign.Ptr[sqlite.DbHandle], sql: Bytes, mut e: Bytes) -> Option[foreign.Ptr[sqlite.StmtHandle]] waits
pub fn column_count(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle]) -> u32 waits
pub fn bind_i64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: i64, mut e: Bytes) -> i32 waits
pub fn bind_f64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: f64, mut e: Bytes) -> i32 waits
pub fn bind_blob(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: Bytes, mut e: Bytes) -> i32 waits
pub fn bind_text(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, v: Bytes, mut e: Bytes) -> i32 waits
pub fn bind_null(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut e: Bytes) -> i32 waits
pub fn step(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], mut e: Bytes) -> i32 waits
pub fn column_kind(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> i32 waits
pub fn column_i64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> i64 waits
pub fn column_f64(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32) -> f64 waits
pub fn column_len(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut e: Bytes) -> i64 waits
pub fn column_copy(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], i: u32, mut out: Bytes, start: u64, mut e: Bytes) -> i32 waits
pub fn reset(f: Foreign[sqlite.Sqlite], s: foreign.Ptr[sqlite.StmtHandle], mut e: Bytes) -> i32 waits
pub fn finalize(f: Foreign[sqlite.Sqlite], take s: foreign.Ptr[sqlite.StmtHandle]) -> i32 waits
pub fn close(f: Foreign[sqlite.Sqlite], take db: foreign.Ptr[sqlite.DbHandle]) -> i32 waits
pub fn errstr(f: Foreign[sqlite.Sqlite], code: i32, mut e: Bytes)
```
