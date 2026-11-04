#!/bin/sh
set -eu
llvm=$(realpath "$1")
kit=$(realpath "$2")
archives=$(realpath "$3")
engine=$(realpath "$4")
native=$(realpath "$5")
batch=$(realpath "$6")
export HIRSEL_LINK_KIT="$kit" HIRSEL_RUNTIME_ARCHIVES="$archives"
export LD_LIBRARY_PATH="$llvm/lib" LLVM_SYS_231_PREFIX="$llvm" HIRSEL_JOBS=1
"$engine" store::request:: --nocapture --test-threads=1
"$native" object_queries::tests:: --nocapture --test-threads=1
"$batch" --exact object_query_tests::actual_ready_fixture_emits_links_restores_and_exits_on_both_backends --nocapture --test-threads=1
