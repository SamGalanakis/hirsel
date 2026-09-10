# F23 — CI and release have separate native/binding producers

Recommend, medium priority, high confidence. C26-02. Coordinator reopened entire android/build-native.sh, release native generation block, CI invocation/path filter and release setup ordering. Re-ran exact recipe query: 8 matching lines.

Both CI and release currently produce the same Android native libraries and Kotlin bindings through separate recipes. The script owns whole-package generated cleanup while release deletes one filename; edits to the script affect CI but never release. No stale extra Kotlin output exists in the audited snapshot, so do not claim a currently contaminated APK. The material simplification is deletion of an active second build producer with already different cleanup semantics, not a hypothetical new caching/type layer.

Target release invokes existing android/build-native.sh after its current SDK/Rust/cargo-ndk setup, preserving signing/release assembly. CI Android path filter also includes release.yml so invocation changes trigger the existing platform gate. No generated source/FFI/schema edits or new build abstraction. Recheck script environment sourcing and working-directory assumptions under the release environment; run existing native/bindgen/debug/release qualification as root authorizes, with no paid/live operation from the audit. No tests/builds performed here.
