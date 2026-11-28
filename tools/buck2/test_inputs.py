"""Repository inputs read by Rust test binaries at runtime."""

REVIEWED_HTTP1 = {"HIRSEL_REVIEWED_HTTP1": "//tools/decoder-fuzz:http1"}

TEST_FILES_ENV = {
    "hirsel-compiler-unittest": {
        "HIRSEL_COMPILER_WORKER": "//:hirsel-core-driver-bin-hirsel",
    },
    "hirsel-core-driver-test-sdk_manifest": {
        "HIRSEL_SWIFT_LINK_KIT": "//tools/buck2/native-toolchain:kit",
        "HIRSEL_SWIFT_MAC_RUNTIME": "//:hirsel-core-rt-archives[aarch64-apple-darwin]",
        "HIRSEL_SWIFT_MAC_EMBED": "//:hirsel-core-rt-embed-archives[aarch64-apple-darwin]",
        "HIRSEL_SWIFT_IOS_EMBED": "//:hirsel-core-rt-ios-embed-archives",
    },
    "hirsel-core-program-tests-test-cranelift_source_aarch64": {
        "HIRSEL_CONTAINED_DRIVER_TESTS": "//:hirsel-core-driver-unittest-harness",
        "HIRSEL_CONTAINED_RUNTIME": "//:hirsel-core-rt-archives[aarch64]",
        "HIRSEL_CONTAINED_RECORD_RUNTIME": "//:hirsel-core-rt-record-archives[aarch64]",
    },
}

# What each crate's tests read at run time outside their crate: repository
# paths they reach from CARGO_MANIFEST_DIR. A test runs from the project root
# and sees only its declared inputs on the pool. Those inputs are part of its
# cached verdict's key.
RUNTIME_DATA = {
    # tools/native-toolchain/check reads Cargo.lock's llvm-sys pin.
    "hirsel-llvm": ["core/core/**", "crates/hirsel-core-ir/samples/**", "crates/hirsel-core-rt/include/**", "core/front/**", "Cargo.lock", "tools/native-toolchain/llvm-23.lock", "tools/native-toolchain/check", "tools/native-toolchain/fixtures/**"],
}

# What a test binary reads at run time from its own crate, beyond the non-Rust
# files in its own directory (`src/` or `tests/<name>/`: snapshots, ratchets
# and goldens), which it always carries. No binary carries its crate's other
# files: editing one binary's tests, or a crate's library source, must not
# invalidate another binary's passing verdicts. A source change still reruns
# every test whose harness it changes.
TEST_DATA = {
    "hirsel-core-syntax-unittest": [
        "core/core/**", "core/std/**", "core/front/cases/**", "core/front/fixtures/**",
        "core/programs/m1/**", "core/programs/m2b/**", "core/corpus/m2b/**",
        "core/langarena/**/*.hirsel", "core/twins/**/*.hirsel",
    ],
    "hirsel-core-diag-unittest": ["docs/spec/**", "crates/hirsel-core-syntax/src/**", "crates/hirsel-core-resolve/src/**", "crates/hirsel-core-front/src/**"],
    "hirsel-core-resolve-unittest": ["core/core/**", "core/std/**"],
    "hirsel-core-front-unittest": ["core/front/**", "core/core/**", "core/std/**", "core/programs/m1/**"],
    "hirsel-core-program-tests-unittest": [
        "core/front/**", "core/core/**", "core/std/**", "core/programs/m1/**",
        "crates/hirsel-core-front/src/testdata/**", "crates/hirsel-core-rt/fixtures/order/step-bounds.hirsel",
        "README.md", "docs/COOKBOOK.md", "docs/PRIMER.md", "docs/spec/**",
        *[f"core/programs/{package}/{pattern}"
          for package in ("dtls", "media_bits", "moq", "quic", "rtp", "rtp_jitter",
                          "runner", "sframe", "srtp", "store", "stun_ice", "turn",
                          "webrtc_sfu", "webtransport")
          for pattern in ("*.hirsel", "hirsel.hcfg")],
        "core/programs/webrtc_sfu/cascade/*.hirsel",
    ],
    "hirsel-core-program-tests-test-cranelift_source": ["core/front/**", "core/core/**", "core/std/**", "crates/hirsel-core-rt/include/**"],
    "hirsel-core-program-tests-test-cranelift_source_aarch64": [
        "core/front/**", "core/core/**", "core/std/**", "core/programs/wasm_laws/**",
        "crates/hirsel-core-driver/Cargo.toml", "crates/hirsel-core-rt/include/**",
    ],
    "hirsel-core-program-tests-test-llvm_source": ["core/front/**", "core/core/**", "core/std/**", "crates/hirsel-core-rt/include/**"],
    "hirsel-core-driver-unittest": ["core/tests/decoder-corpus/archive.json", "core/programs/platform/*.hirsel", "core/programs/platform/hirsel.hcfg",
        "core/tools/store_tests.py", "core/tools/case_progress.py", "core/tools/std_native.py",
        "core/tools/contract_exec.py", "core/tools/std_contracts.py", "tools/loopback_peer.py", "core/core/**", "core/programs/ffi_laws/**", "core/programs/wasm_laws/**", "core/programs/sqlite/**", "core/front/**", "core/std/**", "core/programs/m1/**", "core/programs/m2b/**", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel", "core/programs/runner/tests/*.hirsel", "core/programs/store/hirsel.hcfg", "core/programs/runner/hirsel.hcfg", "core/langarena/**/*.hirsel", "core/HOOK-ALLOWLIST", "crates/hirsel-core-rt/include/**"],
    "hirsel-core-driver-test-sdk_manifest": ["core/core/**", "core/std/**", "core/programs/store/*.hirsel", "core/programs/store/hirsel.hcfg", "core/programs/runner/*.hirsel", "core/programs/runner/observe/*.hirsel", "core/programs/runner/hirsel.hcfg", "core/programs/runner/tests/*.hirsel", "sdk/client/tests/packed_consumer.hirsel"],
    "hirsel-cranelift-test-core_native": ["core/front/**", "core/core/**"],
    "hirsel-cranelift-test-core_aarch64": ["core/front/**", "core/core/**"],
}
