# C26-BUILD-TOOLING audit

## Verdict

Two material findings survive the supplied exclusions:

| ID | Priority | Recommendation | State | Confidence |
| --- | --- | --- | --- | --- |
| C26-01 | High | Make the committed `Cargo.lock` a required input to every workspace dependency-resolving build, check, metadata, and run path. | Latent but reachable whenever a manifest changes without a synchronized lock; the current snapshot itself is structurally consistent. | High |
| C26-02 | Medium | Make `android/build-native.sh` the sole native-library and UniFFI-binding producer used by both CI and release, and include `release.yml` in the Android CI path filter. | The release path currently has a second, already-divergent recipe; stale generated output is latent in a fresh checkout and reachable in a reused/generated tree. | High |

No source file was edited. No tests, builds, installs, migrations, application
execution, live-data/config reads, provider calls, process actions, commits,
or pushes were performed.

## Snapshot, constraints, and integrity

The requested fixed snapshot was verified before inspection:

```text
HEAD:   3ee0621a603659ab0168f565b99012b642415419
tree:   a4aac830c45398a66591f2c44b707aaf3cef281b
status: empty (`git status --porcelain`)
```

The complete C26 specification, exclusions, worker prelude, `CONTRIBUTING.md`,
and `CLAUDE.md` were read first. This pass used both requested lenses: the
schemasmash lens for invalid representations, duplicate truth, and conversion
loss; and the audit-your-codebase lens for ownership, control flow, and
simplification. The exact source boundary below was inspected without taking
ownership of consumer files. There are no tests or fixtures in the exact owned
files; adjacent consumers were inspected only to establish the build and
generated-output paths. No validation command was executed.

## Coverage contract

The following is the complete 31-file C26 ownership boundary; every listed file
was inspected in full or, for the two lockfiles and the binary wrapper, parsed
structurally. No shared exact definitions were assigned to C26.

```text
.github/workflows/ci.yml
.github/workflows/release.yml
.pre-commit-config.yaml
Cargo.lock
Cargo.toml
android/build-native.sh
android/build.gradle.kts
android/gradle.properties
android/gradle/libs.versions.toml
android/gradle/wrapper/gradle-wrapper.jar
android/gradle/wrapper/gradle-wrapper.properties
android/gradlew
android/gradlew.bat
android/settings.gradle.kts
app/.gitignore
app/.oxlintrc.json
app/package-lock.json
app/package.json
app/src/vite-env.d.ts
app/tsconfig.json
app/vite.config.ts
crates/hirsel-client-ffi/Cargo.toml
crates/hirsel-drivers/Cargo.toml
crates/hirsel-host/Cargo.toml
crates/hirsel-host/build.rs
crates/hirsel-plugin-api/Cargo.toml
crates/hirsel-plugins/Cargo.toml
crates/hirsel-proto/Cargo.toml
justfile
scripts/check-production-file-size.sh
scripts/check-static.sh
```

Read-only consumer context covered `app/artifact-preview.config.ts`,
`crates/hirsel-client-core/Cargo.toml`,
`crates/hirsel-client-ffi/src/bin/uniffi-bindgen.rs`,
`crates/hirsel-client-ffi/uniffi.toml`, and the plugin sync scripts. C22 remains
the owner of Android app/build files, generated Kotlin, and FFI source/bindgen
definitions; C23 remains the owner of plugin synchronization. Those consumers
are not re-reported here.

No-finding areas and explicit skips:

- The Rust manifests and current lock snapshot have matching workspace package
  entries and pinned Lash revisions. C26-01 is the missing enforcement of that
  relationship, not a claim that this snapshot already has a mismatch.
- The Gradle wrapper is a checked-in generated wrapper with Gradle 9.4.1 and a
  SHA-256 distribution checksum (`android/gradle/wrapper/gradle-wrapper.properties:1-8`);
  `libs.versions.toml`, `settings.gradle.kts`, and the small root Gradle files
  have no independent duplicate version/state problem.
- `app/package.json` and the root entry in `app/package-lock.json` match; the
  lockfile is version 3 and its declared dependency keys were structurally
  present. The Vite, TypeScript, Oxlint, and ignore-file configuration had no
  material invalid representation.
- `.pre-commit-config.yaml`, `scripts/check-static.sh`, and
  `scripts/check-production-file-size.sh` were inspected as gates. The current
  production roots are explicitly `crates` and `app/src`, and no current source
  file exceeds the configured production limit. The local-only security hooks
  and the plugin sync invocation were not promoted: the former is a policy-gate
  question rather than a C26 representation defect, and C23 owns the latter.
- `crates/hirsel-host/build.rs` was inspected. Its `HIRSEL_GIT_SHA` invalidation
  can be stale when a branch ref advances without changing `.git/HEAD`, but it
  is diagnostic-only, no stale value is demonstrated by this snapshot, and it
  was not promoted over the two material findings above.
- The release input's raw concurrency key versus its later optional-`v`
  normalization was also considered. It is a reachable manual-dispatch
  serialization edge case, but it is lower-frequency and orthogonal to the two
  selected fixes; it is not counted as a recommendation here.
- `plugins/` contains only `.gitkeep`, so no installed-plugin size or generated
  manifest omission is a current C26 finding. The empty workspace glob remains
  deliberate setup for C23's plugin surface.

## Finding C26-01 — the committed Cargo lockfile is not an enforced build input

### Verdict and concrete state

**Recommend; high confidence and high materiality.** The repository stores a
generated `Cargo.lock`, but every inspected workspace build/check/metadata/run
path invokes Cargo in its default resolving mode. If a manifest constraint or
git revision changes without regenerating the lock, the checkout represents two
different dependency states: the reviewed `Cargo.lock` and the constraints in
the manifests. Unlocked Cargo can rewrite the lock in the runner and continue,
so the CI or release artifact can be built from a graph that was not the
reviewed lockfile.

The current snapshot is not being accused of containing that mismatch. Its
lockfile is generated and includes the expected workspace packages:

```text
Cargo.lock:1-3
  # This file is automatically @generated by Cargo.
  # It is not intended for manual editing.
  version = 4

Cargo.lock:1644-1666
  [[package]]
  name = "hirsel-client-core"
  ...
  [[package]]
  name = "hirsel-client-ffi"
  version = "0.1.0"

Cargo.toml:1-14
  [workspace]
  members = [...]
  resolver = "3"

Cargo.toml:21-38
  [workspace.dependencies]
  ...
  lash-core = { package = "lash-internal-core", git = "https://github.com/Ascending-AI/lash", rev = "10af7f410ee54a6b7f2d9d8bffbbed8ceafd04ec" }
  lash = { package = "lash-runtime", git = "https://github.com/Ascending-AI/lash", rev = "10af7f410ee54a6b7f2d9d8bffbbed8ceafd04ec", features = ["rlm"] }
```

For example, changing the manifest's Lash `rev` while retaining the old
`Cargo.lock` source is a valid Git state. The lock currently records the
resolved revision at `Cargo.lock:2461-2464` and `Cargo.lock:2772-2775`; there is
no gate requiring a manifest edit to update those records before a build is
accepted.

### Evidence at every affected execution layer

The CI Rust job runs unlocked workspace build and test commands:

```text
.github/workflows/ci.yml:41-53
  rust:
    ...
    - run: cargo build --workspace
    - run: cargo test --workspace
```

The static gate runs unlocked Clippy:

```text
scripts/check-static.sh:7-10
  bash scripts/check-production-file-size.sh
  bash scripts/check-plugins-synced.sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
```

The developer build/test/run recipes are also unlocked:

```text
justfile:31
  ... entr -rn cargo run -p hirsel-host ) &
justfile:52
  cargo run --release -p hirsel-host
justfile:61
  cargo run --quiet -p hirsel-host --bin hirsel-pair -- "$1"
justfile:64
  cargo build --release --workspace
justfile:68
  cargo test --workspace
```

The Android native/bindgen path has the same workspace-resolution behavior:

```text
android/build-native.sh:15-24
  cargo ndk \
      ... \
      build --release -p hirsel-client-ffi
  cargo build --release -p hirsel-client-ffi
  target_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)"
  cargo run --release -p hirsel-client-ffi --features bindgen-cli --bin uniffi-bindgen -- \
      generate ...
```

The release workflow repeats those unlocked operations:

```text
.github/workflows/release.yml:137-151
  cargo ndk \
    ... \
    build --release -p hirsel-client-ffi
  cargo build --release -p hirsel-client-ffi
  target_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)"
  cargo run --release -p hirsel-client-ffi --features bindgen-cli --bin uniffi-bindgen -- \
    generate ...
```

The reproducible consumer query is:

```sh
rg -n 'cargo (ndk|build|test|check|clippy|run|metadata)' \
  .github/workflows/ci.yml .github/workflows/release.yml \
  android/build-native.sh scripts/check-static.sh justfile
```

It returns **16** matches. Every graph-affecting match lacks `--locked`:

```text
justfile:31,52,61,64,68
.github/workflows/release.yml:137,143,144,145
scripts/check-static.sh:10
.github/workflows/ci.yml:52,53
android/build-native.sh:15,22,23,24
```

The existing `--locked` at `.github/workflows/ci.yml:125` and
`.github/workflows/release.yml:128` belongs to `cargo install cargo-ndk
--version 4.1.2 --locked`; it locks the installer package, not the Hirsel
workspace graph.

### Reachability, duplicate truth, and target state

The mismatch is **latent in the current tree but reachable through an ordinary
manifest-only edit**. A developer or merge can update a dependency constraint,
workspace member, feature, or Lash revision in `Cargo.toml` or one of the six
owned crate manifests while leaving the generated lockfile unchanged. No
application-level second writer was found. The concrete one-copy-without-the-
other write path is the normal Cargo invocation itself: when unlocked Cargo
finds the committed lock incompatible, it is allowed to resolve and rewrite
`Cargo.lock` in the ephemeral checkout. That makes the runner an implicit second
authority, while the reviewed lock remains unchanged in Git.

The exact target representation is:

```text
Manifest layer:
  Cargo.toml and each crate Cargo.toml declare the only allowed constraints,
  features, workspace membership, and git revisions.

Resolved layer:
  the committed Cargo.lock is the exact package/source/checksum graph for that
  manifest snapshot.

Execution layer:
  every Cargo graph operation uses locked mode and fails if the two layers do
  not match; only an intentional dependency update regenerates and commits the
  lockfile.

Artifact layer:
  CI, Android native/bindgen generation, release, and the checked-in developer
  recipes all consume the same lockfile graph.
```

The smallest credible affected scope is the command sites in
`.github/workflows/ci.yml`, `.github/workflows/release.yml`,
`android/build-native.sh`, `scripts/check-static.sh`, and the Cargo build/test/
run recipes in `justfile`. The manifest and lockfile formats do not need to
change. Add `--locked` to each supported Cargo subcommand, including the
`cargo-ndk` pass-through form, and document lock regeneration as part of
intentional dependency updates. This removes the runner's implicit lock writer
and makes a stale lock an immediate, reviewable failure.

### Risk and validation

Cutover risk is low but deliberately fail-closed: a pre-existing stale lock in
an unreleased branch or tag will fail rather than silently select a new graph.
The current snapshot's matching workspace entries reduce that risk for this
head. The `cargo-ndk` option placement must be checked against the installed
wrapper syntax when implemented; no such command was run in this audit.

Existing evidence is insufficient: no exact owned file is a test or fixture,
and no test asserts that an incompatible manifest/lock pair is rejected. No
tests or builds were run here. Required post-cutover validation, not performed
in this pass, is:

- inspect each command with `rg` to establish zero unlocked workspace Cargo
  invocations (apart from formatting and the separately locked installer);
- run the CI-equivalent locked workspace build, test, and Clippy commands;
- run locked metadata and bindgen/native commands through both the shared script
  and release-equivalent environment; and
- create a temporary manifest/lock mismatch in an isolated verification copy
  and confirm locked mode fails without rewriting the lock.

Confidence: **high**.

## Finding C26-02 — release Android generation duplicates and diverges from the canonical recipe

### Verdict and concrete state

**Recommend; high confidence and medium-to-high materiality.** CI already calls
`android/build-native.sh`, but the release workflow copies the entire native
library and Kotlin binding recipe into its YAML. The copies are not equivalent:
the script removes the complete generated package directory, while release
removes only one generated Kotlin file. They are two active producers of the
same Android outputs, with no test or shared assertion that they remain equal.

The canonical script owns the output roots and broad cleanup:

```text
android/build-native.sh:11-13
  output="$repo_root/android/app/src/main/jniLibs"
  bindings="$repo_root/android/app/src/main/kotlin"
  rm -rf "$output/arm64-v8a" "$output/x86_64" "$bindings/dev/hirsel/core"
```

It then owns the complete native build, target discovery, and binding
generation:

```text
android/build-native.sh:15-30
  cargo ndk \
      --target arm64-v8a \
      --target x86_64 \
      --platform 26 \
      --output-dir "$output" \
      build --release -p hirsel-client-ffi
  cargo build --release -p hirsel-client-ffi
  target_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)"
  cargo run --release -p hirsel-client-ffi --features bindgen-cli --bin uniffi-bindgen -- \
      generate \
      --library "$target_dir/release/libhirsel_client_ffi.so" \
      --language kotlin \
      --config "$repo_root/crates/hirsel-client-ffi/uniffi.toml" \
      --out-dir "$bindings" \
      --no-format
```

CI consumes that producer directly:

```text
.github/workflows/ci.yml:153-160
  - name: Build Android native libraries and Kotlin bindings from source
    if: steps.changes.outputs.android == 'true'
    run: bash android/build-native.sh
  - name: Build debug APK
    ...
    run: ./gradlew assembleDebug
```

Release instead has a second recipe with narrower cleanup and hard-coded paths:

```text
.github/workflows/release.yml:133-151
  - name: Build Android native libraries and Kotlin bindings from source
    run: |
      rm -rf android/app/src/main/jniLibs/arm64-v8a android/app/src/main/jniLibs/x86_64
      rm -f android/app/src/main/kotlin/dev/hirsel/core/hirsel_client_ffi.kt
      cargo ndk \
        --target arm64-v8a \
        --target x86_64 \
        --platform 26 \
        --output-dir android/app/src/main/jniLibs \
        build --release -p hirsel-client-ffi
      cargo build --release -p hirsel-client-ffi
      target_dir="$(cargo metadata --format-version=1 --no-deps | jq -r .target_directory)"
      cargo run --release -p hirsel-client-ffi --features bindgen-cli --bin uniffi-bindgen -- \
        generate \
        --library "$target_dir/release/libhirsel_client_ffi.so" \
        --language kotlin \
        --config crates/hirsel-client-ffi/uniffi.toml \
        --out-dir android/app/src/main/kotlin \
        --no-format
```

The source-level binding contract and Android packaging consumer are separate
and remain outside C26 ownership:

```text
crates/hirsel-client-ffi/Cargo.toml:8-24
  [lib]
  crate-type = ["cdylib", "lib"]
  ...
  [[bin]]
  name = "uniffi-bindgen"
  path = "src/bin/uniffi-bindgen.rs"
  required-features = ["bindgen-cli"]

crates/hirsel-client-ffi/uniffi.toml:1-3
  [crates.hirsel_client_ffi.bindings.kotlin]
  package_name = "dev.hirsel.core"
  cdylib_name = "hirsel_client_ffi"

android/app/build.gradle.kts:66-72
  packaging {
      jniLibs {
          useLegacyPackaging = true
      }
  }
```

The reproducible recipe query is:

```sh
rg -n 'android/build-native\.sh|cargo ndk|cargo build --release -p hirsel-client-ffi|cargo run --release -p hirsel-client-ffi|hirsel_client_ffi\.kt' \
  .github/workflows/ci.yml .github/workflows/release.yml android/build-native.sh
```

It returns **8** matches: three commands in `android/build-native.sh`, the CI
script call, and four release references (the cleanup line plus the three
commands). The duplicated command sequence is therefore directly enumerable,
not inferred from naming.

### Reachability, duplicate truth, and target state

The duplicate producer is **reachable on every Android release run** and on
every Android CI run. The current checkout contains only the tracked generated
core binding, so no current stale extra file was claimed. The latent invalid
state is a generated Kotlin package containing an output other than the one
hard-coded release filename: the shared script deletes all of
`android/app/src/main/kotlin/dev/hirsel/core`, whereas release leaves any such
file in place and then regenerates into the parent directory. A stale generated
source can consequently be compiled into the release APK while the debug CI
path starts from the stronger cleanup state.

There is no separate durable schema or application writer here. The duplicate
truth is the two shell/YAML recipes. A change to
`android/build-native.sh` updates the CI producer but not the release producer;
a change to the release block updates the release producer but not CI. The
already different `rm -rf .../core` versus `rm -f .../hirsel_client_ffi.kt` is a
current write-path divergence, even though the present one-file output does not
yet expose an extra stale file.

The exact target representation is:

```text
Producer:
  android/build-native.sh is the sole owner of native cleanup, cargo-ndk
  targets/output, release FFI build, target-directory lookup, and UniFFI Kotlin
  generation.

CI consumer:
  ci.yml invokes bash android/build-native.sh, then assembles the debug APK.

Release consumer:
  release.yml installs cargo-ndk and invokes the same script, then assembles
  the signed release APK. It contains no copied generation commands or cleanup.

Generated outputs:
  jniLibs/arm64-v8a and jniLibs/x86_64 plus the Kotlin output under the package
  declared by uniffi.toml; the generated Kotlin source remains C22-owned.

Gate:
  ci.yml's Android path filter includes both the canonical script's inputs and
  release.yml, so changes to either the producer or release invocation receive
  the Android debug gate.
```

The smallest credible affected scope is `android/build-native.sh`,
`.github/workflows/release.yml`, and the Android filter in
`.github/workflows/ci.yml`. Replace the release block with
`run: bash android/build-native.sh`; retain release-only tool installation and
APK signing. Add `.github/workflows/release.yml` to the existing Android filter
at `.github/workflows/ci.yml:87-93`. Do not modify generated Kotlin, FFI
interfaces, or the C22-owned Android app build contract.

This target removes a real second producer and makes cleanup semantics
identical. It also ensures that an invocation-level release change is visible to
the Android CI gate. It does not add an abstraction or a wrapper layer: the
existing script becomes the one producer already used by CI.

### Risk and validation

Cutover risk is low, but release environment assumptions must be checked: the
release job must have `cargo-ndk` installed before the script call, must invoke
the script from the repository root, and must preserve the existing signing
environment. The current workflow satisfies the first two by installing
`cargo-ndk` at `.github/workflows/release.yml:127-128` and checking out the tag
before the build. The script's optional `HIRSEL_ANDROID_ENV` sourcing is
preserved. No Android build was run here.

Existing evidence is insufficient: no exact owned file is a test or fixture,
and no fixture compares debug and release generated outputs. No APK was built
and no live provider/config value was read; the checked-in generated Kotlin
directory was inspected only to establish its current single-file state.
Required post-cutover validation, not performed in this pass, is:

- run `bash android/build-native.sh` from a clean checkout and inspect both ABI
  directories and the generated Kotlin package;
- assemble the debug APK through the CI-equivalent path;
- run the release-equivalent script followed by `./android/gradlew assembleRelease`
  with signing secrets supplied through the existing protected environment;
- verify that release and debug generation have identical output paths and no
  stale generated Kotlin files; and
- change the release workflow or script in an isolated verification branch and
  confirm the Android path filter selects the Android job.

Confidence: **high**.

## Final integrity check

After writing this report, the source checkout remained unchanged:

```text
HEAD:   3ee0621a603659ab0168f565b99012b642415419
tree:   a4aac830c45398a66591f2c44b707aaf3cef281b
status: empty (`git status --porcelain`)
```
