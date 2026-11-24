#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR TEST_DATA_DIR [hirsel test options]
# REFERENCE_DIR is the WEBP_REFERENCE_DIR output of build.sh.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
work=$1
reference=$2
data=$3
shift 3
python3 "$codec/vectors/prepare.py" "$work" "$reference" "$data" "$llvm"
"$compiler" test "$work/image_webp" --exact image_webp@1/tests/libwebp_vectors \
    --exact image_webp@1/tests/decode_lossless_rgba --grant Foreign=image_webp@1/webp/Webp "$@"
