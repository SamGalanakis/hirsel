#!/bin/sh
set -eu
llvm=$(realpath "$1")
kit=$(realpath "$2")
archives=$(realpath "$3")
native=$(realpath "$4")
queries=$(realpath "$5")
export LD_LIBRARY_PATH="$llvm/lib" LLVM_SYS_231_PREFIX="$llvm" HIRSEL_JOBS=1
export HIRSEL_LINK_KIT="$kit" HIRSEL_RUNTIME_ARCHIVES="$archives"
"$native" --exact object_queries::tests::stale_blob_schema_one_is_refused_with_streamed_schema_two --nocapture --test-threads=1
"$queries" --exact object_query_tests::actual_ready_fixture_emits_links_restores_and_exits_on_both_backends --nocapture --test-threads=1
