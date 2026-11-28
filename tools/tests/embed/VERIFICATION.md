# Embedding verification

[EMB-1 through EMB-9](../../../docs/spec/embed.md) define the embedding contract.

## Simulator and macOS oracles

The [Mac workflow](../../../.github/workflows/mac-ci.yml) generates a Swift package,
archives, headers and the symbol reference fixture. These are the qualification criteria.

2. **Check the XCFramework against Apple's tool.** Run `xcodebuild -create-xcframework`
   over the same three archives and headers. Compare its `Info.plist` with the driver's,
   field by field: for each library, `LibraryIdentifier`, `LibraryPath`, `HeadersPath`,
   `SupportedArchitectures`, `SupportedPlatform` and `SupportedPlatformVariant`. Apple's
   tool is the independent oracle for the driver's assembly.
3. **Check the symbol lists and two-library Swift link.** Link the EMB-6 symbol reference
   fixture against the iOS SDK for the device and the simulator. Also compile two generated
   Swift packages with equal binding projections in release mode (`-O`) and link both
   against both lazy archives in both link orders (`A, B` and `B, A`), asserting a
   duplicate `hirsel_embed_v1_*` link error in both orders (EMB-1, EMB-5).
4. **Prepare a simulator.** Run `xcrun simctl list runtimes`, pin the simulator runtime it
   found (never a version written blind), and create a disposable simulator. Add the TLS
   test CA to that simulator's trust store (`xcrun simctl keychain <udid> add-root-cert`).
5. **Run the test.** Build a minimal SwiftUI host with an XCTest target that depends on the
   **generated package as SwiftPM resolves it**. Run it with `xcodebuild test` on the
   simulator. The test checks:
   - the start path, including the anchor and the projection fingerprint: the SHA-256 of
     the shipped `projection.json` equals the generated `manifest_fp` and the projection
     the linked library's `hirsel_embed_v1_manifest` gives;
   - a Hirsel call's decoded result, a call of each entry kind, and a subscription with
     two changes;
   - a room with a state, a mixed-Part message-then-leave, a first-`Full` Part, and a
     leave, matching `sdk/client/room.ts` / `the SDK specification`;
   - abandon, Swift cancellation before issue, held at the C registration boundary, after
     issue, early outcome before C returns, and late cancellation handler safety;
   - close from a host thread, from inside a callback, and racing external/deferred close;
   - the EMB-7 TLS rows:
     - **on every executing host (simulator, macOS and signed device):** a `Trust.System`
       handshake to a public HTTPS host succeeds and an unanchored test CA gives
       `UnknownIssuer`; under `Trust.Roots` matching the test CA, the leaf succeeds and a
       wrong name, expired leaf and not-yet-valid leaf give their named variants; a `Roots`
       grant with another digest is `Denied`;
     - **on hosts deliberately provisioned with the private test CA** (the disposable
       simulator above, and macOS only when the CI job explicitly provisions trust for that
       test CA): the test CA's leaf also succeeds under `Trust.System` and gives
       `NameMismatch`, `Expired` and `NotYetValid` against it;
   - a stop and a second start, after which the old session handle returns
     `HIRSEL_STOPPED` and a new call succeeds;
   - the `Welcome.target` each slice reports.

   The same XCTest runs on the macOS slice without a simulator (with the private-CA-under-
   `Trust.System` assertion scoped to whether the macOS host provisioned that CA).
## Signed device qualification criteria

**First law: confinement.** The test runs inside the signed app's own writable container,
so the OS sandbox cannot be what refuses:

1. The test creates `root/` and a sibling `outside/` in the same container, with a file in
   each. It first proves that the host itself can read and write `outside/` directly.
2. It grants `root/` as a `Dir`. Through the grant, a file inside `root/` opens.
3. A relative symlink inside `root/` that points inside `root/` is followed, where the
   operation follows links.
4. Through the grant, three legal relative names (passing Hirsel's public path check) whose
   symlink targets lead outside `root/` are refused with `Denied`, with the confined open
   reaching the kernel (`O_RESOLVE_BENEATH`) each time:
   - a legal relative name whose symlink inside `root/` has a relative `..` target pointing
     to `outside/`'s file;
   - a legal relative name traversing a symlink directory inside `root/` whose relative
     `..` target points to `outside/`;
   - a legal relative name whose symlink inside `root/` has an absolute target pointing to
     `outside/`'s file.

   On the kernel path, `ENOTCAPABLE` maps to `EXDEV` and then to `Denied`
   (`crates/hirsel-core-rt/src/host/darwin/path.rs`).
5. `outside/`'s file is unchanged afterwards.

The three public-boundary `Denied` assertions of the macOS law (a literal `..` parent
component, an absolute path, and a symlink escape) are kept as well; the language boundary
continues to reject literal `..` and absolute paths before any syscall, while the three
symlink cases above prove kernel `O_RESOLVE_BENEATH` enforcement on legal relative names.
A blanket symlink refusal, a sandbox `EACCES` or a lexical refusal does not pass the
kernel law.

A device that behaves otherwise stops the iOS track. The floor's premise (kernel
enforcement of `O_RESOLVE_BENEATH`) is then false, and the confinement design is reopened.
No userspace walk is built.

**Then** the same simulator test runs on device (using the unprovisioned-host TLS suite
above: public HTTPS `Trust.System` success, unanchored test CA `UnknownIssuer`, and
`Trust.Roots` leaf-variant checks, without installing a private test CA on the user's
device), followed by the runtime qualification that the simulator cannot give:

- **Stack depth.** A recursion of a stated depth runs on the A12, and the guard page is
  observed one frame past the bound.
- **Faults.** The host survives a guard fault (EMB-3), and debugger attachment does not
  break stack growth.
- **The floor and interfaces.** The floor's `O_RESOLVE_BENEATH` holds, and NET-18 works
  against the device's interfaces.
- **Clocks.** A deadline spans a suspension: the test records whether the clock counts
  suspended time (continuous) or only uptime.
- **Memory.** The virtual reservation that `n_place` takes is recorded, with the resident
  memory.
- **Recovery.** An app suspends and resumes with a call in flight. A relaunch in the same
  container finds every acknowledged commit.
