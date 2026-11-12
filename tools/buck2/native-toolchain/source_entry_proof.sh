#!/bin/sh
# Only the actual source-entry refusal body is needed.
set -eu
llvm=$(realpath "$1")
native=$(realpath "$2")
export LD_LIBRARY_PATH="$llvm/lib" HIRSEL_JOBS=1 RUST_TEST_THREADS=1
"$native" --exact root_source::tests::immutable_constructor_refuses_a_verified_nonentry_body_within_bounds --nocapture --test-threads=1
