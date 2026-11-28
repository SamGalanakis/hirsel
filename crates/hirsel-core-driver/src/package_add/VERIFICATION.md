# Package verification

These are the independent oracles for the [package specification](../../../../docs/spec/packages.md).
The [package-directory harness](../test_run_native.rs) and [package edit laws](laws.rs)
run the CLI over temporary directories. Fixture archives are packed with a fixture store F of their own. Every command of
a law runs with `--package-store` naming that law's own store, empty when the law starts unless
the law says otherwise, so no law's outcome depends on what an earlier command left in a
store. Each law also has its own user cache (`XDG_CACHE_HOME`), so its own pass cache and
check records, and its own code store, all empty when it starts and kept across that law's
steps, so a step's `reused` or `executed` can only come from an earlier step of the same law.

A workspace member that a law names only by its directory `m` has the manifest
`name = "local/m"`, `version = "1.0.0"` and `charge = "charge-v1"`, so its identity is
`local/m@1`, and a row binding it is `m = { package = "local/m", major = 1, path = "<dir>" }`.
Published fixtures have scoped names under `fixture.test`, which is a valid host and is never
contacted.

**Known roots.** Two one-file packages, `jx` and `jy`. `jx`'s manifest is:

```hcfg
pkg_v1.Package(
    name: "fixture.test/json",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("json.hirsel", "json")],
    dependencies: [],
)
```

In `jx`, `json.hirsel` is the 33 bytes `pub fn version() -> u64 { 1u64 }` (32 characters)
followed by a line feed. `jy` has `version = "1.0.1"`, and its `1u64` is `2u64`. `jx`'s `C`
is these 88 bytes, grouped by field:

```text
03                                  version 3
11 666978747572652e746573742f6a736f6e
                                    name "fixture.test/json"
01 00 00 00                         version 1.0.0, no prerelease
09 6368617267652d7631               charge "charge-v1"
00                                  no bindings
01                                  one file
0b 6a736f6e2e68697273656c           path "json.hirsel"
01 04 6a736f6e                      module path: one component, "json"
21 70756220666e2076657273696f6e2829202d3e20753634207b2031753634207d0a
                                    the 33 (0x21) file bytes
00                                  no foreign modules
```

The roots are

- X = `sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7` for `jx`, and
- Y = `sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433` for `jy`.

Here X is SHA-256 over the 8-byte little-endian 22, `hirsel.library-root.v1`, the 8-byte
little-endian 88 and the 88 bytes. `hirsel pack jx --output jx.hpkg` prints
`fixture.test/json@1 1.0.0 <X>` and `hirsel pack jy --output jy.hpkg` prints
`fixture.test/json@1 1.0.1 <Y>`. This law catches any encoding drift: a wrong order, a missing
frame, a version left out of `C`, a hash over the archive instead of over `C`, or a shortened
root.

**Known roots with foreign modules.** Package `fixture.test/demo@1` has no bindings and these files, each
ending in one line feed: `api.hirsel` (module `api`) is `pub opaque type A {}` and
`pub opaque type B {}` on two lines (42 bytes), `aa.hirsel` (module `aa`) is
`pub fn ping(f: Foreign[api.A]) -> u64` (38 bytes) and `b.hirsel` (module `b`) is
`pub fn pong(f: Foreign[api.B]) -> u64` (38 bytes). Its manifest adds

```hcfg
foreign: Some([
    ("aa", Native(
        symbols: "ha_",
        archives: [
            ArchiveFile(
                target: "x86_64-unknown-linux-gnu",
                file: "native/aa-x86.a",
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            ArchiveFile(
                target: "aarch64-unknown-linux-gnu",
                file: "native/aa-arm.a",
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
        ],
    )),
    ("b", Native(
        symbols: "hb_",
        archives: [
            ArchiveFile(
                target: "x86_64-unknown-linux-gnu",
                file: "native/b.a",
                sha256: "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d",
            ),
        ],
    )),
]),
```

where `native/aa-x86.a` is the ASCII bytes `abc`, `native/aa-arm.a` is empty and
`native/b.a` is the one byte `b`, each pinned by its FIPS 180-2 SHA-256. Its manifest has `name: "fixture.test/demo"` and `version = "1.0.0"`. Its `C` is 440 bytes,
and its item 6 is, grouped by field:

```text
02                                  two foreign modules
01 02 6161                          module path aa
03 68615f                           prefix "ha_"
00                                  native
02                                  two rows
19 616172636836342d756e6b6e6f776e2d6c696e75782d676e75    target aarch64-unknown-linux-gnu
0f 6e61746976652f61612d61726d2e61   file "native/aa-arm.a"
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855    pin
00                                  no bytes
18 7838365f36342d756e6b6e6f776e2d6c696e75782d676e75      target x86_64-unknown-linux-gnu
0f 6e61746976652f61612d7838362e61   file "native/aa-x86.a"
ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad    pin
03 616263                           bytes "abc"
01 01 62                            module path b
03 68625f                           prefix "hb_"
00                                  native
01                                  one row
18 7838365f36342d756e6b6e6f776e2d6c696e75782d676e75      target x86_64-unknown-linux-gnu
0a 6e61746976652f622e61             file "native/b.a"
3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d    pin
01 62                               bytes "b"
```

Its root is D = `sha256:fc4c9e66e3bb3dcee82a324ed4f2def822c8882b835f3b5177d4fe8fef897ff0`,
and `hirsel pack demo --output demo.hpkg` prints `fixture.test/demo@1 1.0.0 <D>`. Module `aa` precedes `b` by
component bytes although its frame `01 02 61 61` is bytewise greater than `01 01 62`, and the
manifest's row order does not matter. These are wire vectors: the archive bytes are not `ar`
archives, and only FFI-3 at link time asks that they be. The decoder refuses three variants,
each reported as `hirsel: fixture.test/demo@1: archive:demo.hpkg is not a package archive: <failure>` when
it is the head of an archive `app` binds by `{ major = 1, root = "<D>", archive = "demo.hpkg" }`,
because decoding comes before the root check (section 7):

- the same `C` with the two module entries swapped: `foreign modules out of order at aa`, whose
  root, `sha256:79ed8428993d879c742ebf264780eb9cdf68d61fa4410ea7b7256d19ae64b43b`, a lenient
  decoder would accept as a second package with D's meaning;
- the same `C` with `aa`'s two rows swapped: `archive rows out of order at aarch64-unknown-linux-gnu`;
- the same `C` with the byte of `native/b.a` changed to `c` and its pin kept:
  `archive "native/b.a" of b for x86_64-unknown-linux-gnu does not match its pin`.

So do two binding refusals on `jx`'s `C` with its no-bindings byte `00` replaced: by
`02 02 7531 11 666978747572652e746573742f7574696c 01 01 <X> 02 7532 11
666978747572652e746573742f7574696c 01 01 <Y>` (aliases `u1` and `u2` of
`fixture.test/util@1`, each valid alone): `bindings of fixture.test/util@1 differ`; and by
`01 03 737464 03 737464 01 01 <X>` (a published `std@1` under alias `std`): `bad binding
std = std@1`. The law catches a module order taken over framed bytes, a decoder that checks only
what its own encoder sorts, and one that skips the pin of a row for another target.

**L1. Diamond refusal.** The workspace root manifest has `members = ["app", "a", "b"]` and
`vendor/` holds `jx.hpkg` and `jy.hpkg` from above. `a` binds
`json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }` and `b` binds the same with
`<Y>` and `jy.hpkg`. `app` binds `a = { package = "local/a", major = 1, path = "../a" }` and
`b = { package = "local/b", major = 1, path = "../b" }`. `hirsel check app` exits 2, writes no lock, and its stderr
is exactly:

```text
hirsel: fixture.test/json@1 is bound to more than one root:
  local/app@1 -> local/a@1 -> fixture.test/json@1 = sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 exact
  local/app@1 -> local/b@1 -> fixture.test/json@1 = sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433 exact
no single root satisfies every edge; the exact edges of local/a@1, local/b@1 must name one root
```

`local/app@1` is the only top member, and `a` sorts before `b`. Two exact edges accept only
their own roots, so no selection could settle them and the refusal does not offer
`hirsel update`<!-- doc-ref: planned -->. With `b` changed to bind `<X>` and
`jx.hpkg`, the check passes and the lock has exactly one `fixture.test/json@1` row, with root X. That is the
coalesce. With `b` binding `json` major 2 to the root of a `fixture.test/json@2` fixture, the lock has a
`fixture.test/json@1` row and a `fixture.test/json@2` row, and the check passes: majors stay distinct.

**L1b. A floor is settled by a selection, never by a build.** `vendor/wfloor.hpkg` is built by
the harness's reference encoder, because `pack` never writes a floor: its head is `wrap` (the
L6 package) with its binding written as a floor on `jx`, these 143 bytes, whose root is
`sha256:0a8784b0a67e8789914b019e1d8ae502540cd71fb3bd63d7cea77ff7d6084c7a`:

```text
03 11 666978747572652e746573742f77726170 01 00 00 00 09 6368617267652d7631
01 04 6a736f6e 11 666978747572652e746573742f6a736f6e 01
02 9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 00 00
01 0b 777261702e68697273656c 01 04 77726170
1d 70756220666e206f6e652829202d3e20753634207b2031753634207d0a 00
```

that is, alias `json`, `fixture.test/json` major 1, tag 2, tested root X, bound version
1.0.0; its other entry is `jx`'s `C`. A single-package workspace `app` binds
`wrap = { package = "fixture.test/wrap", major = 1, root = "<that root>", archive = "vendor/wfloor.hpkg" }`
and `json = { package = "fixture.test/json", major = 1, root = "<Y>", archive = "vendor/jy.hpkg" }`.
`hirsel check app` exits 2, writes no lock, and its stderr is exactly:

```text
hirsel: fixture.test/json@1 is bound to more than one root:
  local/app@1 -> fixture.test/json@1 = sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433 exact
  local/app@1 -> fixture.test/wrap@1 -> fixture.test/json@1 = sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 floor 1.0.0
to choose one root for every edge, run: hirsel update fixture.test/json@1
```

Then `hirsel update fixture.test/json@1`<!-- doc-ref: planned --> selects Y, the only candidate
both edges accept (`jy` is 1.0.1, at least the floor), adds to `app/hirsel.hcfg` the table
`selections` with the one row
`"fixture.test/json@1" = { root = "<Y>", archive = "vendor/jy.hpkg" }`, prints
`fixture.test/wrap@1: published against fixture.test/json 1.0.0, built with 1.0.1`, and
writes a lock whose `fixture.test/wrap@1` row has
`tested = ["fixture.test/json@1 sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7"]`. Now `hirsel check app --locked` passes. With the
direct row changed to bind root `sha256:1ad0b7636d8b0df47998006c9c53324001ceba7c99cbdd1d7658f2adb191107b`,
the `C` of `jy`'s files under `version = "1.0.0"`, packed into `vendor/jz.hpkg`, and the
selection removed, both `hirsel check app` and `hirsel update`<!-- doc-ref: planned --> exit 2 with exactly

```text
hirsel: fixture.test/json@1 1.0.0 is two roots:
  local/app@1 -> fixture.test/json@1 = sha256:1ad0b7636d8b0df47998006c9c53324001ceba7c99cbdd1d7658f2adb191107b
  local/app@1 -> fixture.test/wrap@1 -> fixture.test/json@1 = sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7
```

The law catches a build that solves (picks Y on its own), an `update` that picks a root an
edge refuses, a floor read as an exact pin or as no pin at all, a displaced root that is
checked as the effective binding, and a version that two contents can both claim.

**L2. Stale lock.** This is a single-package workspace: `app` binds
`json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "vendor/jx.hpkg" }` and `std = { major = 1 }`.
`hirsel lock app` writes exactly the lock bytes shown in section 8. Rewrite the binding to
`<Y>` with `archive = "vendor/jy.hpkg"`, and then:

- `hirsel check app --locked` exits 2, leaves `hirsel.lock` byte-identical, and writes exactly:

  ```text
  hirsel: hirsel.lock is stale under --locked:
    fixture.test/json@1: version 1.0.0 -> 1.0.1
    fixture.test/json@1: root sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 -> sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433
    fixture.test/json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg
  ```

- `hirsel check app` without the flag passes, writes `hirsel: updated hirsel.lock:` and the
  same three lines, and leaves the section 8 lock with the X version, root and source replaced
  by Y's.
- A stale lock in a transitive position is reported the same way. Workspace members `app ->
  util` (a path binding), with `util` binding the json root: changing `util`'s root and running
  `--locked` from `app` gives the same three lines. This catches a freshness check that compares
  only the named package's own manifest.

**L3. Frozen with missing content.** Take L2's `app` with root X, but give it the source
`url = "https://hirsel.invalid/jx.hpkg"`. Fill store S1 with `hirsel pack jx --output jx.hpkg
--package-store S1`, and write the fresh lock with `hirsel lock app --package-store S1`. Then
`hirsel check app --frozen --package-store S2`, with S2 a new empty directory, exits 2, S2 stays
empty, and its stderr is exactly:

```text
hirsel: fixture.test/json@1 sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 is not in the package store, and --frozen does not fetch (source url:https://hirsel.invalid/jx.hpkg)
```

`.invalid` never resolves, so a command that tried to fetch would report `fetching … failed`
instead. With the store holding the X entry with one byte flipped, the line says `is damaged
in` in place of `is not in`. With the source changed to `archive = "vendor/jx.hpkg"` and the
lock rewritten, `hirsel check app --locked` against a new empty store S3 fills S3 with X and
passes, because `--locked` fetches. Then, with that `archive` source and lock and with S2 empty
again, make `vendor/jx.hpkg` a FIFO and have the harness block opening its write end on
another thread: `hirsel check app --frozen --package-store S2` gives the `is not in` line
with `(source archive:vendor/jx.hpkg)`, and the harness's open has not returned when the
command exits, so the command never opened the archive. (The harness then opens the read end
itself to release its thread.)

**L4. A symlink escaping the package.** Package `p` lists `"a.hirsel" = "a"`, where `a.hirsel`
is a symbolic link to `../outside.hirsel`, a regular file beside `p`, and `p` has no lock.
Run from `p`'s parent, `hirsel check p` and `hirsel pack p --output p.hpkg` both exit 2, create no
`hirsel.lock` and no archive, and write exactly this to stderr. The refusal is in resolution step
2, before the lock is derived:

```text
hirsel: p/hirsel.hcfg: sources: "a.hirsel" resolves outside the package
```

In the control case `a.hirsel` links to `real/a.hirsel` inside `p`: the check passes, and the printed root equals the root of a copy of `p` in which `a.hirsel` is a regular file
holding the same bytes. The law catches a walker that follows links out of the package, and an
encoder that hashes the link target's text instead of the file's bytes.

**L5. A test-only file is excluded from the root.** This is `jx` with
`tests`, `limit = 67108864` and `[tests.sources] "json_laws.hirsel" = "json"`, where
`json_laws.hirsel` holds an `@test` function in module `json`, the library's own module.
`hirsel pack` prints `fixture.test/json@1 1.0.0 <X>`, the root of `jx` without any test. `hirsel test` on it runs
the test, so the test file did join module `json` for its own run. The law catches an
inventory that walks the directory, and one that keys exclusion by module instead of by file.

**L6. A mismatched fetch keeps nothing.** `app` binds `<X>` with `archive = "vendor/jy.hpkg"`.
`hirsel check app` exits 2 with
`hirsel: fixture.test/json@1: archive:vendor/jy.hpkg holds sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433, but the binding pins sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7`,
and the store has no entry named by Y or by X.

A non-leaf bundle is checked edge by edge. Package `fixture.test/wrap@1` is the review fixture of section 1's
vectors: one file `wrap.hirsel`, module `wrap`, holding `pub fn one() -> u64 { 1u64 }` and a
line feed, version 1.0.0, and one binding, `json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }`.
Its `C` is 141 bytes and its root is W = `sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5`. `hirsel pack wrap
--output wrap.hpkg` prints `fixture.test/wrap@1 1.0.0 <W>` and then `fixture.test/json@1 1.0.0 <X>`. `app` binds
`wrap = { package = "fixture.test/wrap", major = 1, root = "<W>", archive = "vendor/wrap.hpkg" }`, and `hirsel check app`
passes with the store holding W and X. Three archives built by hand from the same entries are
refused, each leaving the store as it was:

- head W with the entry Y in place of X: `hirsel: fixture.test/wrap@1: archive:vendor/wrap.hpkg lacks
  sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7, which fixture.test/wrap@1 binds as fixture.test/json@1`;
- head W alone, with the store already holding X: the same line, because the store never
  completes an archive;
- head W with entries X and Y: `hirsel: fixture.test/wrap@1: archive:vendor/wrap.hpkg holds
  sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433, which nothing in it binds`.

**L7. A dependency change reaches each key.** `app`'s test calls `json.version()`, and the lock
binds X.

- After the test passes once, rebinding to Y makes `hirsel test` run the test again (its
  address changed) and `hirsel inspect app --affected-tests fixture.test/json@1` list it.
- Rebinding instead to the root of a `jx` copy whose only difference is a second listed file
  that `app` never reaches makes `hirsel test` report the pass as reused.
- No body record checked under X is reused under Y. The environment holds the root, so the
  check of `app` under Y reuses no record, `fixture.test/json@1`'s included, even though `json.hirsel`'s
  bytes differ in one body only.
- `hirsel build --emit recipe --code-store DIR` of a program over X prints the same R from a store at two different paths,
  and the same R again when `json` is instead a workspace member holding `jx`'s files, bound by
  path, so equal content gives equal names whatever supplies it.

**L8. Foreign metadata reaches every reuse key.** Workspace members `app` and `math`; `app`
binds `math = { package = "fixture.test/math", major = 1, path = "../math" }`, and its entry calls `math.ping`, a wrapper in
module `math` over the foreign module `native` (prefix `hm_`, one x86_64 row whose file is a
real indexed archive built from the FFI law adapter, with its pin). Every step below runs
with one private cache and one code store kept across the steps, and is compared with the
same command run cold, with an empty cache and an empty code store, on the same files.

- `hirsel check app` passes. Delete `[foreign.native]` from `math/hirsel.hcfg` and leave every
  `.hirsel` file unchanged: the warm `hirsel check app` is refused, as the cold one is,
  because `native`'s bodyless headers now stand in an ordinary module.
- Restore the table and change only the prefix to `hn_`: the warm `hirsel build app --emit recipe --code-store DIR` prints
  the cold run's R, which differs from the R printed before the change.
- Restore the prefix and add an `aarch64-unknown-linux-gnu` row: the warm `hirsel build app --emit recipe --code-store DIR`
  for x86_64 prints the cold run's R, which differs from the R before (FFI-9).
- Replace the x86_64 archive's bytes with other bytes and keep the pin: the warm `hirsel build
  app` is refused with exactly the cold run's `FFI-3: archive … does not match its pin: …`
  text, and the linker is never started.

The law catches a whole-request record, image key or definition key that hashes only source
files, and a warm build that links an archive without checking it.


**L9. A moved site ends a cached pass.** Two published pins of `fixture.test/math@1`, one file
`math.hirsel`, module `math`: in A it holds `pub fn quot(x: u64, y: u64, z: u64) -> u64 { x / y / z }`,
and in B the body is `(x / y) / z`. Both lower to the same two divisions with the same slots,
so A and B give `quot` one code address. `app`'s parameterless test calls `quot(8, 0, 1)` and
`quot(8, 1, 0)`, each inside an `isolate`, and returns `Ok(())` exactly when the two failures'
`Cause.site` values are equal. Under A both sites start at `x`, and the test passes. Rebind
`app` to B: the outer division's site now starts at `(`, and `hirsel test app` reports the
test executed and failed, as a cold run under B does, instead of reusing A's pass. The law
catches a pass key that holds the address but not the reached annexes.

**L10. A coalesced member keeps its pin.** Workspace members `app` and `json`, where `json`
is `jx` and `app` binds `json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }`.
The member and the pin coalesce, and `hirsel lock` writes exactly:

```text
# hirsel.lock: written by hirsel; do not edit.
version = 2

[[package]]
id = "fixture.test/json@1"
version = "1.0.0"
root = "sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7"
source = "member:json"
edges = []

[[package]]
id = "local/app@1"
version = "1.0.0"
source = "member:app"
edges = ["fixture.test/json@1"]
```

Change the member to `jy` and the binding to `<Y>` with `jy.hpkg` in one edit:
`hirsel check app --locked` exits 2, leaves the lock byte-identical and writes exactly

```text
hirsel: hirsel.lock is stale under --locked:
  fixture.test/json@1: version 1.0.0 -> 1.0.1
  fixture.test/json@1: root sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 -> sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433
```

Instead replacing the binding by `json = { package = "fixture.test/json", major = 1, path = "../json" }` gives the one change
line `  fixture.test/json@1: root sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 -> none`, the member's version staying 1.0.0. The law catches a lock that drops the
published obligation when a member satisfies it.

**L11. Every edge checks the identity it binds.** Fill the law's store with `jx` by
`hirsel pack jx`. `app` binds `yaml = { package = "fixture.test/yaml", major = 1, root = "<X>" }` and nothing else, so X is
opened from the store and no archive is read. `hirsel lock app` exits 2 with exactly
`hirsel: fixture.test/yaml@1: sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7 is fixture.test/json@1`. With `app` binding
`json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "vendor/jx.hpkg" }` as well, so X is visited first
under the right identity, the refusal is the same line. The law catches a resolver that checks
identity only on a cold archive head, or once per root instead of once per edge.

**L12. Member files are read inside their directories.** Workspace root `W` lists `app` and
`lib`, where `W/lib` is a symbolic link to `../elsewhere/lib`, a valid package outside `W`.
`hirsel lock W` exits 2 with exactly
`hirsel: W/hirsel.hcfg: workspace.members: "lib" resolves outside the workspace`. Separately,
package `p` lists `"a.hirsel" = "a"`, where `p/up` is a link to `..` and `a.hirsel` is a
link to `up/p/real.hirsel`, which names `p/real.hirsel` again: `hirsel check p`, run from
`p`'s parent, exits 2 with `hirsel: p/hirsel.hcfg: sources: "a.hirsel" resolves outside the
package`, because the walk left the package although the final file is inside. The law catches
containment checked on a normalised pathname instead of on the walk actually opened. The
concurrent-replacement case that the capture rule closes has no deterministic CLI oracle, and
no law claims it.

**L13. An image supplies only the closure.** Member `core_only` binds nothing, and its module
`main` holds `pub fn main() -> u64 { std.x }` for any std name `x`. `hirsel check core_only`
is refused by name resolution because `local/core_only@1` does not bind `std`, with the same
diagnostic from a source checkout and from an installed toolchain whose std image is
registered. Given `--image` naming an image of `local/core_only@1` and `std@1`, the command exits 2
with exactly `hirsel: --image <path> does not hold exactly this request's packages`. The law
catches a toolchain image chosen before resolution.

**L14. A rename reaches the image and the names.** Member `fixture.test/json@1` has one file, `old.hirsel`
in module `json`, holding `pub fn half(x: u64) -> u64 { 8u64 / x }` and a line feed, and `app`
binds it by path; `app`'s entry calls `json.half(0)`. `hirsel check app` builds the closure
image. Rename the file to `new.hirsel`, module and bytes unchanged. The failure `hirsel run
app` reports now names its site in `fixture.test/json@1/new.hirsel`, and `hirsel build app --emit recipe --code-store DIR`
prints the R of a cold run with no image, on the warm image path as on the cold one. The law
catches an image keyed without file names and a producer that synthesises names.

**Laws for `hirsel add`.** L15–L22 cover S6 in `s6_add_protocol`, L25–L28 cover S8 in
`s8_add_workspace`, and L29 covers S9 in `s9_add_forms_that_fetch_nothing`. L23 and L24
specify the `github:` form as a git source, and L30 to L37 the git sources of section 7;
their fixtures are under **Git fixtures** after L29. No CLI law claims collision detection:
no public SHA-1 collision is a Git object, because the `blob <size>` header changes the state
the collision blocks need. The check is a unit case beside the hash's own vectors instead: the
one function every object id goes through (GIT-9 step 2) is given the raw bytes of the
published `shattered-1.pdf` (SHA-1 `38762cf7f55934b34d179ae6a4c80cadccbb7f0a`) and must report
a collision.
X and Y are `jx`'s and `jy`'s roots and W is `fixture.test/wrap@1`'s (L6). Each `add` runs in a workspace
whose `hirsel.hcfg` and `hirsel.lock` bytes the law records before it starts.

**L15 (L6.1). A refused add changes no file.** The workspace resolves `fixture.test/json@1` at X through
member `a`. Package `app` runs `hirsel add`<!-- doc-ref: planned --> on an archive whose closure holds `fixture.test/json@1` at Y
(`fixture.test/wrap@1` rebuilt over `jy`). It exits 2 with the diamond refusal, and every manifest and
`hirsel.lock` is byte-identical afterwards. The law catches Cargo's order: writing the manifest
before resolving it.

**L16 (L6.2). `--root`<!-- doc-ref: planned --> stops a substituted archive.** `vendor/jx.hpkg` holds `jy`'s archive;
`hirsel add vendor/jx.hpkg --root <X>`<!-- doc-ref: planned --> exits 2 with
`fetched root differs from --root`<!-- doc-ref: planned --> naming Y and X, writes nothing, and the store holds no entry
named Y.

**L17 (L6.3). The written binding builds offline, warm and cold.** After `hirsel add`<!-- doc-ref: planned --> of
`vendor/jx.hpkg`, `hirsel build --frozen` passes with no network, and passes again over an
empty store after `hirsel fetch` from the written source alone.

**L18 (L6.4). The same source with other bytes is refused.** After adding `vendor/jx.hpkg`,
replace the file with `jy`'s archive: `hirsel add vendor/jx.hpkg`<!-- doc-ref: planned --> exits 2 naming X and Y,
with the last line `to take the fetched root, run: hirsel update json`, and changes nothing.
Then `hirsel update json`<!-- doc-ref: planned --> binds Y, printing the version change
1.0.0 to 1.0.1 and an empty authority diff: `jy` changes one body and reaches no kind.

**L19 (L6.5). Comments and order are kept.** A manifest with comments and a sorted
`dependencies` gains the new key in sorted position, every other byte unchanged; the oracle is
a hand-written expected file.

**L20 (L6.6). One capture.** An HTTPS fixture that serves `jx`'s archive and then `jy`'s for the
same URL gets exactly one request, and the admitted entries are X's. A local archive replaced
after its read is read once. A head whose name changed cannot evade the same-source rule.

**L21 (L6.7). Caps and partial admission.** A streamed response over 512 MiB, a bad final
entry, and a missing or extra closure entry each admit nothing. An I/O failure injected during
admission reports `cache: partial` and changes no manifest. One peak-RSS reading on a
near-limit fixture and on many small entries stays under the 2 GiB cap.

**L22 (L6.8). Crash points.** A crash after the manifest rename and before the lock commit
leaves a lock that `hirsel lock` repairs, and the next `add`, even `unchanged`, repairs it too.
Two concurrent adds of different sources both end committed or one refuses; neither loses the
other's binding. The oracle is the lock `hirsel lock` derives from the committed manifests.

**L23 (L7.1). A `github:` reference is a commit.** The git fixture server stands in for
`github.com` and serves repository r1 (below) at `/o/r`, with `refs/tags/v1` and
`refs/heads/main` at C1, a decoy `refs/tags/v10` at C2, and `HEAD` pointing at
`refs/heads/main`. In a single-package
workspace `app`, `hirsel add github:o/r/wrap@v1` writes exactly

```text
wrap = { package = "fixture.test/wrap", major = 1, root = "sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5", git = "https://github.com/o/r", rev = "daf4441842dbd47464a183be86f1128a41b2bf26", ref = "refs/tags/v1", dir = "wrap" }
```

and `hirsel add github:o/r/wrap` (no `REF`) in a fresh copy writes the same with
`ref = "refs/heads/main"`. `github:o/r/wrap@daf4441842dbd47464a183be86f1128a41b2bf26` writes
it with no `ref`, and the request log shows no `ls-refs`. The expected lines are written by
hand from W (section 6) and C1. The law catches a shorthand that still names a release asset,
a `/DIR` read as a package name, a reference matched by prefix (the decoy `v10` would give
C2 and W2), and a written `ref` that is not the full name.

**L24 (L7.2). Each source text has one meaning.** Each of these exits 2 with the `add, bad git
source` refusal and writes nothing: `github:o/r/../x`, `github:o`, `git+https://alpha.test/r1?x=1`,
`git+https://user@alpha.test/r1`, `git+https://alpha.test/r1#` (empty `REF`), `git+http://alpha.test/r1`,
`git+ssh://alpha.test/r1`. With r1 also holding a branch named `v1`,
`hirsel add git+https://alpha.test/r1?dir=wrap#v1` refuses with `add, ambiguous ref`, and
`#refs/tags/v1` succeeds. `hirsel add https://alpha.test/r1`, where the fixture answers with
an HTML page, fails as an archive (`malformed`) and the request log holds one GET to `/r1`
and nothing under `/r1/info/refs`. The law catches a source that is sniffed and a short ref
that picks one of two meanings.

**L25 (L8.1). Two members share one binding.** Two members adding `vendor/jx.hpkg` with
`--workspace`<!-- doc-ref: planned --> end with one workspace dependency entry and two
`Inherit` bindings.

**L26 (L8.2). Paths are relative to the declaring manifest.** A member at `apps/ui` adding
`vendor/jx.hpkg` writes `../../vendor/jx.hpkg` locally and `vendor/jx.hpkg` in the root, and a
cold fetch from each succeeds.

**L27 (L8.3). A virtual root only declares.** `--workspace`<!-- doc-ref: planned --> at a
virtual root leaves `hirsel.lock` byte-identical.

**L28 (L8.4). Partial completion.** A crash between the root and member operations leaves a
workspace that `hirsel lock --locked` accepts after one derive, and rerunning the same `add`
completes it. Two members with different local pins of `fixture.test/json@1` (X and Y) refuse through the
resolver's diamond rule.

**L29 (S9). Forms that fetch nothing.** `hirsel add std@1` for a toolchain package and, from
a subdirectory of member `apps/ui`, `hirsel add ../../../libs/a` for a listed member each write
a binding (`a = { package = "local/a", major = 1, path = "../../libs/a" }`, spelled from the declaring manifest) that
the next `--frozen` build of an entry using both accepts. `std@2`, an unlisted directory and
`--root` with either form refuse and write nothing.

**Git fixtures.** L23, L24 and L30 to L37 run a local Git server in the package-directory
harness, behind the CONNECT proxy and test CA of the existing HTTPS fixture, as `alpha.test`
(and as `github.com` for L23). No law reaches the internet or runs a host `git`. The server
holds repositories as tables of objects and refs written in the law. It answers
`info/refs` with a protocol version 2 advertisement (`version 2`, `ls-refs`,
`fetch=shallow`, `object-format=sha1`), `ls-refs` from its ref table, and `fetch` with a
version 2 pack of every object reachable from the wanted commit at depth 1, undeltified
unless the law says otherwise, written from Git's pack format documentation
(`gitformat-pack`). It logs every request: its path, its `command`, and its `want` and
`ref-prefix` lines. The commit ids below were computed by Git 2.43.0 over exactly these
bytes, with author and committer `Fixture <fixture@hirsel.invalid>`, so they are independent
of hirsel and of the fixture's own writer; the law fails if the fixture's objects do not
hash to them.

Repository r1 holds five files, each ending in one line feed:

- `hirsel.hcfg` contains `pkg_v1.Workspace(members: ["json", "wrap"])`.
- `json/hirsel.hcfg` contains `pkg_v1.Package(name: "fixture.test/json", version: "1.0.0", charge: ChargeV1, sources: [("json.hirsel", "json")], dependencies: [])`.
- `json/json.hirsel` contains the source of `jx` (section 13, **Known roots**).
- `wrap/hirsel.hcfg` contains the following bytes.

```hcfg
pkg_v1.Package(
    name: "fixture.test/wrap",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("wrap.hirsel", "wrap")],
    dependencies: [
        ("json", Path(package: "fixture.test/json", major: 1, path: "../json", authority: [])),
    ],
)
```

- `wrap/wrap.hirsel`: `pub fn one() -> u64 { 1u64 }`, `fixture.test/wrap@1`'s file of L6.

Its commit C1 = `daf4441842dbd47464a183be86f1128a41b2bf26` (time `1700000000 +0000`, message
`v1`) has tree `9f16615bb5871e0ee3340022cf0dfc99b2404553`. C2 =
`a9bad28634316d4b90052da470a735871d764838` (time `1700000060 +0000`, message `v2`, parent C1)
changes only `wrap/wrap.hirsel` to `pub fn one() -> u64 { 2u64 }`. `wrap` at C1 packs to W of
L6 and binds `fixture.test/json@1` at X; at C2 its `C` is 141 bytes and its root is
W2 = `sha256:f9ce1f7be3bb0142c589e371080224a62b625a483bba4347f9e3df47483146c5`, computed by
a reference encoder written from section 1, which also reproduces X and W. Unless a law says
otherwise, `refs/tags/v1` and `refs/heads/main` point at C1 and `HEAD` at `refs/heads/main`.

Each variant below is r1 at time `1700000000 +0000` with message `v1`, changed as stated,
and is served with its own commit as `refs/tags/v1`, `refs/heads/main` and `HEAD`'s target:

- r2 = `3f0715f5398ee15cddb4a7c97b281ad89e482da7`: `wrap/wrap.hirsel` is a symbolic link
  (mode `120000`) to `../json/json.hirsel`;
- r3 = `b26b8634da6312f3ef26f7894df256307c95ac9f`: `wrap/wrap.hirsel` is a link to
  `real/wrap.hirsel`, and `wrap/real/wrap.hirsel` holds the bytes;
- r4 = `cb5d68e5223fad4ae1e476c6468f88517d3ea356`: adds the three files below and the
  submodule entry `vendor/evil` (mode `160000`, commit C2); every other blob is r1's;
- r5 = `d37fdca1ca37d5ec8bdefe37f477281a51857a8f`: `wrap/hirsel.hcfg` has the line
  `"ext/x.hirsel" = "x"` right after `"wrap.hirsel" = "wrap"`, and `wrap/ext` is a submodule
  entry (commit C2).

r4's `.gitattributes`, `.gitmodules` (its indents are tab characters) and `.lfsconfig` are,
each line ending in a line feed:

```text
* text eol=crlf filter=sentinel ident export-subst
wrap/wrap.hirsel export-ignore
```

```text
[submodule "evil"]
	path = vendor/evil
	url = https://alpha.test/evil
```

```text
[lfs]
	url = https://alpha.test/lfs
```

r6 = `48dc7934b5ac1c3f8b65be96ba996dfe67f922b5` (time `1700000000 +0000`, message `v1`, no
parent; its own commit is `refs/tags/v1`, `refs/heads/main` and `HEAD`'s target) is `fixture.test/demo@1`
of section 13 at the top of the tree. Its `hirsel.hcfg` is:

```hcfg
pkg_v1.Package(
    name: "fixture.test/demo",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("api.hirsel", "api"), ("aa.hirsel", "aa"), ("b.hirsel", "b")],
    dependencies: [],
    foreign: Some([
        ("aa", Native(
            symbols: "ha_",
            archives: [
                ArchiveFile(
                    target: "x86_64-unknown-linux-gnu",
                    file: "native/aa-x86.a",
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                ),
                ArchiveFile(
                    target: "aarch64-unknown-linux-gnu",
                    file: "native/aa-arm.a",
                    sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                ),
            ],
        )),
        ("b", Native(
            symbols: "hb_",
            archives: [
                ArchiveFile(
                    target: "x86_64-unknown-linux-gnu",
                    file: "native/b.a",
                    sha256: "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d",
                ),
            ],
        )),
    ]),
)
```

`api.hirsel` is `pub opaque type A {}` and `pub opaque type B {}`, `aa.hirsel` is
`pub fn ping(f: Foreign[api.A]) -> u64`, `b.hirsel` is `pub fn pong(f: Foreign[api.B]) -> u64`,
`native/aa-x86.a` is `abc` and `native/aa-arm.a` is empty, each source line ending in a line
feed, as in section 13. `native/b.a` is the 126-byte Git LFS pointer below, whose SHA-256 is
`70edffc238bc4d1e48b82fd69143269662f8daf7556f399d76669f3976dad323`:

```text
version https://git-lfs.github.com/spec/v1
oid sha256:3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d
size 1
```

The ids above were produced by a script running Git 2.43.0 over exactly these bytes; a law
needs only the bytes stated here.

**L30. A package in a subdirectory with a sibling `path` dependency.** In a single-package
workspace `app` binding nothing, `hirsel add git+https://alpha.test/r1?dir=wrap#v1` exits 0
and writes L23's line with `git = "https://alpha.test/r1"`. The lock is exactly:

```text
# hirsel.lock: written by hirsel; do not edit.
version = 2

[[package]]
id = "fixture.test/json@1"
version = "1.0.0"
root = "sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7"
source = "via:fixture.test/wrap@1"
edges = []

[[package]]
id = "fixture.test/wrap@1"
version = "1.0.0"
root = "sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5"
source = "git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap"
edges = ["fixture.test/json@1"]

[[package]]
id = "local/app@1"
version = "1.0.0"
source = "member:."
edges = ["fixture.test/wrap@1"]
```

The store holds exactly the entries X and W, and the request log is one `info/refs` GET, one
`ls-refs` with `ref-prefix refs/tags/v1` and `ref-prefix refs/heads/v1`, and one `fetch`
whose only `want` is C1. `hirsel pack wrap --output w.hpkg`, run in a checkout of r1 at C1,
prints `fixture.test/wrap@1 1.0.0 <W>` then `fixture.test/json@1 1.0.0 <X>`: the same roots. Three variants keep W:

- the server sends the same objects as a pack with `OFS_DELTA` and `REF_DELTA` entries;
- r1 with a third member `broken` listed, whose `broken/hirsel.hcfg` is not TOML: `broken`
  is never reached, so it is never read;
- `wrap` binds `json = { package = "fixture.test/json", major = 1, root = "<X>", archive = "../vendor/jx.hpkg" }` and the
  commit holds `vendor/jx.hpkg`, `jx`'s archive: X is read from the snapshot, and the request
  log gains nothing;
- `wrap` binds `json = { package = "fixture.test/json", major = 1, root = "<X>", git = "https://alpha.test/rj", rev = "<J>" }`,
  where `rj` is a server-built commit J holding `jx` at the top of its tree: X is fetched from
  `rj`, the request log gains one `info/refs` GET and one `fetch` whose only `want` is J and
  no `ls-refs`, and `fetched` lists `git:https://alpha.test/rj#<J>`.

The law catches a sibling `path` dependency that is not packed to its own root, a member
binding encoded by its path instead of its root, a `dir` that selects the wrong package, a
root over Git object ids instead of over `C`, and a resolver that reads members the package
never reaches.

**L31. A tampered or substituted commit keeps nothing.** In `app`, written by hand:
`wrap = { package = "fixture.test/wrap", major = 1, root = "<X>", git = "https://alpha.test/r1", rev = "<C1>", dir = "wrap" }`.
`hirsel check app` exits 2 with exactly
`hirsel: fixture.test/wrap@1: git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap holds sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5, but the binding pins sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7`,
and the store stays empty. With the binding's root W restored, two servers that lie are
refused, each leaving the store empty:

- one that sends `wrap/wrap.hirsel` with the bytes of C2 in place of C1's blob
  `b8b63ee2ea62f1ec73509eedeed52c7206deaffe`: `sent a bad pack: object b8b63ee2ea62f1ec73509eedeed52c7206deaffe is not in the pack`,
  because the bytes sent hash to C2's blob id and nothing names that;
- one that answers `want <C1>` with C2's objects: `sent a bad pack: commit daf4441842dbd47464a183be86f1128a41b2bf26 is not in the pack`.

Two packs built by the server with bad deltas are refused the same way: a `REF_DELTA` whose
base is absent, and two `REF_DELTA` entries that name each other, each `sent a bad pack: delta
base <oid> is not in the pack` with the base the first delta names. And with L30's nested
variant (`wrap` binding `json` by `git` from `rj`), the binding's root set to X makes the
outer root differ after the nested source fetched and verified X: the command refuses and the
store stays empty, X included.

The law catches a fetch that trusts the server's object ids or the ref, a missing delta base
treated as empty or followed for ever, and an admission, nested or outer, before the root
check.

**L32. A link that leaves the package.** `hirsel add git+https://alpha.test/r2?dir=wrap#v1`
exits 2, writes nothing and leaves the store empty, with exactly
`hirsel: git:https://alpha.test/r2#3f0715f5398ee15cddb4a7c97b281ad89e482da7/wrap/hirsel.hcfg: sources: "wrap.hirsel" resolves outside the package`,
although the target is inside the commit. In the control r3, the link stays inside `wrap`, and
the add writes root W: the link's bytes are never hashed and the unlisted `real` directory
never enters. A server-built commit in which `wrap/wrap.hirsel` reaches its file through a
chain of 40 links inside `wrap` gives W; with 41 it is refused as `does not resolve to a
regular file`. Three more cases:

- r1 with an extra top-level tree entry named `json/json.hirsel`, holding `jy`'s source (a
  server-built tree; Git never writes a name with `/`): the add still writes W and the lock
  still pins X, because a path is looked up one component at a time;
- `wrap/up` is a link to `..` and `wrap/wrap.hirsel` a link to `up/wrap/real.hirsel`, with
  `wrap/real.hirsel` holding the bytes: refused as `"wrap.hirsel" resolves outside the
  package`, although the final file is inside, as L12 is for a member;
- the workspace at `ws/` lists `lib`, `ws/lib` is a link to `../elsewhere/lib` holding a
  valid package, and `ws/app` binds it by `path`, with `dir = "ws/app"`: refused as
  `hirsel: git:https://alpha.test/<repo>#<rev>/ws/hirsel.hcfg: workspace.members: "lib" resolves outside the workspace`.

The law catches a walk that contains links only to the commit instead of the package or the
workspace, containment decided on a normalised path, a tree flattened into a map of paths, a
link loop decided by the host, and an encoder that hashes link text.

**L33. Nothing in the repository runs or changes bytes.** The law's `PATH` begins with a
directory holding an executable `git` that creates the file `SENTINEL` and exits 1, and
`GIT_CONFIG_GLOBAL` names a file setting `filter.sentinel.smudge`, `filter.sentinel.clean`,
`filter.sentinel.process` and `core.hooksPath` (holding every hook) to programs that each
create `SENTINEL`. `hirsel add git+https://alpha.test/r4?dir=wrap#v1` exits 0 and writes root
W with `rev = "cb5d68e5223fad4ae1e476c6468f88517d3ea356"`; `SENTINEL` does not exist; the
request log holds nothing under `/evil` or `/lfs`. A fetch through the `git` program fails or
creates `SENTINEL`; a checkout that applies `eol=crlf` makes `wrap.hirsel` 30 bytes and
another root; one that applies `export-ignore` loses `wrap.hirsel`; one that follows
`.gitmodules` requests `/evil`. Each is red here.

**L34. Locked and offline builds use the commit, never the ref.** After L30:

1. With the server stopped, `hirsel build app --frozen` passes.
2. With a new empty store S, `hirsel check app --locked --package-store S` passes, S holds
   exactly X and W, and the request log is one `info/refs` GET and one `fetch` whose only
   `want` is C1, with no `ls-refs`.
3. With `refs/tags/v1` moved to C2 on the server, step 2 gives the same result.
4. With a store holding W and not X, `--locked` fetches C1 again through `fixture.test/wrap@1`'s source
   (resolution step 3.3) and passes.
5. With a server that answers `want <C1>` with `ERR upload-pack: not our ref
   daf4441842dbd47464a183be86f1128a41b2bf26`, step 2 exits 2 with exactly
   `hirsel: fixture.test/wrap@1: fetching git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap failed: the server would not send commit daf4441842dbd47464a183be86f1128a41b2bf26`.
6. `hirsel check app --frozen --package-store S` over an empty S exits 2 with exactly
   `hirsel: fixture.test/wrap@1 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5 is not in the package store, and --frozen does not fetch (source git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap)`,
   and the request log is empty.

7. With `info/refs` on `alpha.test` redirecting to the same repository on a second fixture
   host `beta.test`, step 2 passes and the request log shows the `fetch` POST once, on
   `beta.test`. When `beta.test` answers that POST with a redirect, step 2 exits 2 with
   `the server answered 302`, and when the first redirect points at `http://beta.test/…`, it
   exits 2 with `redirected to http://beta.test/r1/info/refs?service=git-upload-pack, which is not https`.

The law catches a build that resolves a ref, a `--frozen` command that touches the network,
a store miss that cannot be refilled from the pinned commit, and a POST that follows a
redirect or a base taken from anywhere but the `info/refs` answer.

**L35. A moved tag is reported and never taken silently.** After L30, move `refs/tags/v1` to
C2 on the server. `hirsel add git+https://alpha.test/r1?dir=wrap#v1` exits 2, leaves every
manifest, the lock and the store as they were (no W2 entry), and writes exactly:

```text
hirsel: https://alpha.test/r1 refs/tags/v1 moved:
  bound: daf4441842dbd47464a183be86f1128a41b2bf26 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5
  fetched: a9bad28634316d4b90052da470a735871d764838 sha256:f9ce1f7be3bb0142c589e371080224a62b625a483bba4347f9e3df47483146c5
to take it, run: hirsel update wrap
```

`hirsel update wrap`<!-- doc-ref: planned --> then writes `rev = "a9bad28634316d4b90052da470a735871d764838"`
and root W2, keeps `ref = "refs/tags/v1"`, and prints
`hirsel: updated wrap = fixture.test/wrap@1 1.0.0 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5 -> 1.0.0 sha256:f9ce1f7be3bb0142c589e371080224a62b625a483bba4347f9e3df47483146c5`. The same
version under two roots on one tag is reported, not refused, because the old root is no longer
in the closure.
Bound instead from `#main` and with `refs/heads/main` moved to C2, the first line reads
`hirsel: https://alpha.test/r1 refs/heads/main now names another commit:`. Bound from `#v1`,
with a tag `v2` at C2 and `v1` unmoved, `hirsel add git+https://alpha.test/r1?dir=wrap#v2`
exits 0, writes `rev` C2, `ref = "refs/tags/v2"` and root W2, and prints the
update line. The law catches an add that follows a moved tag, one that cannot tell a moved tag
from a new source, and one that calls a branch's change progress.

**L36. Submodules and LFS pointers.** `hirsel add git+https://alpha.test/r5?dir=wrap#v1` exits
2 with exactly
`hirsel: git:https://alpha.test/r5#d37fdca1ca37d5ec8bdefe37f477281a51857a8f/wrap/hirsel.hcfg: sources: "ext/x.hirsel" reaches submodule "ext", and hirsel does not fetch submodules`,
and the request log holds no second repository. With r6 tagged `v1`,
`hirsel add git+https://alpha.test/r6#v1` exits 2 with exactly
`hirsel: git:https://alpha.test/r6#48dc7934b5ac1c3f8b65be96ba996dfe67f922b5/hirsel.hcfg: foreign.b: archive "native/b.a" for x86_64-unknown-linux-gnu does not match its pin: expected sha256 3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d, found sha256 70edffc238bc4d1e48b82fd69143269662f8daf7556f399d76669f3976dad323; it is a Git LFS pointer, and hirsel does not fetch LFS content`.
r4's unread submodule entry was ignored in L33. The law catches submodule recursion and a
pointer packed as if it were the archive.

**L37. Every bound holds before memory or the store is spent.** Each of these servers makes
`hirsel check` of L31's binding with root W exit 2 with `sent a bad pack: over the limit of
<limit>` and leaves the store empty: a pack header declaring 4 194 305 objects (`4194304
objects`), refused before any object arrives, which the server's still-open response shows;
objects whose inflated bytes pass 4 GiB together (`4 GiB inflated`); a chain of 4096 deltas
(`4095 delta links`); and a delta bomb, a 16 MiB blob of zeros and one `OFS_DELTA` of 4096
copy instructions of 16 777 215 bytes each (the most one copy instruction names), whose
instructions are 16 KiB and whose result would be nearly 64 GiB (`512 MiB object`), refused
before anything is built for it. A response that streams past 512 MiB is the git protocol row
of section 9 instead: `fetching <source> failed: the response is over the limit of 512 MiB;
hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git
source`. An object whose inflated length is not its header's is refused as `object at
<offset> inflates to <n> bytes, not <m>`. A pack near the bounds, C1's objects with a 500 MiB
blob of zeros and seven deltas that each copy it and insert one byte (500 MiB on the wire,
4 194 304 007 bytes inflated), is checked by a child process whose peak RSS stays under L21's
2 GiB cap. A chain of 257 repositories, each commit's package
binding the next by `git` and `rev`, makes `hirsel add` of the first refuse with
`hirsel: <id>: git:https://alpha.test/n0#<rev> causes more than 256 fetches` and leaves the
store empty. The law catches a client that reads a hostile pack to its end before applying a
bound, a delta whose result is built before its size is checked, and nesting with no bound.

**Laws for Phase 0 identity, selection and authority.** L38 to L42 are the gate laws of the
packages blueprint: L38 is G7, L39 is G8, L40 is G10, L41 is G9 and L42 is G1. Their
fixtures' roots are computed by the reference encoder of section 13's known roots from the
bytes stated, never by the implementation; a law writes `<R name>` for such a root.

**L38. Scoped names and aliases.** Packages `a.test/json` and `b.test/json`, both version
1.0.0, each have one file `json.hirsel` in module `json`, holding
`pub fn version() -> u64 { 1u64 }` and `pub fn version() -> u64 { 2u64 }` respectively.
Package `fixture.test/pt`, version 1.0.0, has `pt.hirsel` in module `pt`:
`pub type T { v: u64 }`, `pub fn make() -> T { T { v: 3u64 } }` and
`pub fn get(t: T) -> u64 { t.v }`, each on its own line. A single-package workspace `app`
binds `json = { package = "a.test/json", … }`, `bjson = { package = "b.test/json", … }`, and
`p1` and `p2`, both to `fixture.test/pt` at one root, each by archive. Its `main` returns
`json.version() * 100 + bjson.version() * 10 + p2.get(p1.make())`, and `hirsel run app`
prints 123. So two packages both named `json` from two hosts build in one closure, and a value
of `p1.T` is a `p2.T`. The lock has one row for each of `a.test/json@1`, `b.test/json@1` and
`fixture.test/pt@1`, and `local/app@1`'s edges are those three and `std@1`. Then:

- `hirsel inspect app --affected-tests a.test/json@1/json/version` lists the test of `app`
  that calls `json.version()`, and not one that calls only `bjson.version()`;
- a `[tests.runs]` key `local/app@1/main/uses_json` and the grant
  `--grant Foreign=fixture.test/demo@1/api/A` (on a workspace binding `fixture.test/demo` at D)
  are read back as the identity, module and item they name, each split at the first `@`;
- a run's failure site in `a.test/json` is named `a.test/json@1/json.hirsel`, and PROG-9's
  path component for the package is `a%2Etest%2Fjson@1`;
- `hirsel add` of `b.test/json`'s archive without `--as`<!-- doc-ref: planned --> in a fresh `app` that already binds
  `json` to `a.test/json` refuses with `add, alias taken`, and with `--as bjson`<!-- doc-ref: planned --> writes
  `bjson = { package = "b.test/json", major = 1, root = "<R b>", archive = "…" }`.

The law catches a name split at the first `/` instead of the first `@`, an identity that drops
the host, an alias that enters a definition's identity, and an escape that maps two names to
one path.

**L39. Selection obeys every floor, and a selection that breaks a reader leaves nothing.**
Packages `fixture.test/num` at 0.3.1, 0.3.4 and 0.4.0 each have `num.hirsel` in module `num`
with `pub fn n() -> u64 { 31u64 }`, `{ 34u64 }` and `{ 40u64 }`. `fixture.test/reader`,
version 1.0.0, has `reader.hirsel` in module `reader`, `pub fn r() -> u64 { num.n() }`, and
binds `num` as a floor on 0.3.1, its tested root `<R 0.3.1>`; its archive is written by the
reference encoder, as L1b's is. A single-package workspace `app` binds `reader` and binds
`num` exactly to `<R 0.3.4>`:

1. `hirsel check app` refuses as a diamond and offers `hirsel update fixture.test/num@0`<!-- doc-ref: planned -->.
2. `hirsel update fixture.test/num@0`<!-- doc-ref: planned --> selects 0.3.4 and writes one
   selection row. `hirsel check app --frozen` then passes and leaves the lock byte-identical.
3. With `app`'s exact row on `<R 0.4.0>` instead, step 1's refusal names both edges and ends
   with `no single root satisfies every edge; the exact edges of local/app@1 must name one root`,
   because 0.4.0 is outside the floor's minor, and `hirsel update`<!-- doc-ref: planned --> refuses with the same block.
4. With `app`'s exact row removed and `hirsel.hcfg` holding the step 2 selection rewritten by
   hand to `<R 0.3.0>`, a 0.3.0 whose `n` is `30u64`, the build refuses as
   `selection below a floor` (`requires 0.3.1 or a later 0.3.x`).
5. `fixture.test/num` 0.3.9, whose `num.hirsel` has no `n`, is bound exactly by `app` in place
   of 0.3.4: `hirsel update`<!-- doc-ref: planned --> selects 0.3.9, its overlay check of `reader` fails on the missing
   `num.n`, it exits 2 with that check diagnostic, and `app/hirsel.hcfg` and `hirsel.lock` are
   byte-identical to their state before it ran.

The law catches a build that solves, a `0.x` selection that crosses a minor, a selection under
a floor, and an `update` that writes before its check.

**L40. `pack` checks before it publishes.** In a workspace with members `lib` and `app`:

- `lib`'s only file holds `pub fn f() -> u64 { true }`. `hirsel pack lib --output l.hpkg` exits
  2 with the check's type diagnostic and `hirsel: pack: lib/hirsel.hcfg does not check; nothing
  was written`, and neither `l.hpkg` nor any store entry exists afterwards.
- `lib` checks but binds L41's `netpkg` with no `authority`: `pack` refuses with
  `authority exceeded`, as `check` does.
- `lib` has `name = "local/lib"`: `pack` refuses with `pack, local` before anything is read but
  the manifest.
- `lib` lists 65 537 source files: `pack` refuses with `over the limit of 65536 files and
  archive rows` before it encodes `C`.

The law catches a `pack` that publishes a package the toolchain would refuse.

**L41. The authority ceiling.** Each fixture below is a published package bound by a single
`app` row with no `authority` unless the case says otherwise, and every refusal is
`authority exceeded` with the kinds and the least witness path the case names.

1. `fixture.test/netpkg` exports `ping(n: Net)`, which connects through `std` on its parameter:
   refused naming `Net`, the witness `ping -> std … -> Connect`. With `authority = ["Net"]` the
   check passes. A member `m` binding it with `["Net"]`, and `app` binding both `m` and `netpkg`
   with `[]`, is refused on `app`'s row: a direct row narrows.
2. `fixture.test/hof` exports `pub fn twice(f: fn() -> u64) -> u64 { f() + f() }`, and `app`
   passes a closure that writes to standard output: passes with no list, because the closure is
   `app`'s. Its card says `conditional`.
3. `fixture.test/rd` exports `read_all(d: Dir) -> Bytes`, which opens a file under `d` through
   `std`: refused naming `Dir`, although `app` supplies the handle.
4. `fixture.test/gen` exports a generic `run[T: Emit](t: T) -> u64` that calls `t.emit()`, and
   `app`'s `Emit` implementation writes to standard output: passes, the implementation being
   `app`'s. A second export `stamp[T](t: T) -> u64` that reads the wall clock is refused naming
   `Clock`, the instantiation being `gen`'s.
5. `fixture.test/fake` exports a constructor of a virtual clock (`Provide`): refused naming
   `Clock`; with `["Clock"]` it passes and its card lists `Clock` as provided.
6. `fixture.test/spawn` spawns a child process: refused naming `Exec`.
7. A package binding `fixture.test/demo` at D and calling `aa.ping`: refused naming `Foreign`.
   The same package over a contained module of FFI-10 passes with no list, and its card notes
   the contained module.
8. Every case's refusal also holds on a warm build from an image and on `hirsel check` and
   `hirsel pack`.
9. After case 1 passes with `["Net"]`, rebinding `netpkg` to a version whose `ping` also reads
   the environment makes `hirsel update`<!-- doc-ref: planned --> print the per-export diff `ping: + Env` and exit 2 with
   `authority exceeded`, leaving the manifest and lock as they were.

The unresolved case has no CLI fixture, because no checked program the front end accepts leaves
a reach unresolved; it is a case in the reachability tests of `hirsel-core-driver`: a program
whose instance calls a runtime function absent from the runtime's table is refused as
`authority unresolved` under a list naming all fifteen kinds.

The law catches a ceiling that charges the wrong package for a closure, an implementation or an
instantiation, a handle passed in treated as no authority, a provider not counted, `Exec` or
`Foreign` missing from the table, a cached build that skips the check, and an unresolved reach
that passes.

**L42. `--frozen` makes no connection.** After L17's `hirsel fetch`, with `[cache.remote]`
configured to `https://cache.invalid`, `hirsel build app --frozen` runs in a new user and
network namespace under a `connect` and DNS counter, once as a one-shot command and once
through a resident `hirsel serve` that has also served an ordinary request. Each makes zero
`connect` calls and zero DNS queries and passes. The same build without `--frozen` makes at
least one `connect` attempt to the cache, so the counter is shown to see one. The law catches a
remote cache, an upload or a resident compiler that ignores the request's transport permission.
