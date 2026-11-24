#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR [hirsel test options]
# REFERENCE_DIR is the JPEG_REFERENCE_DIR output of build.sh.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
reference=$2
shift 2
python3 "$codec/vectors/prepare.py" "$work" "$reference"
"$compiler" test "$work/codec" --exact image_jpeg@1/tests/libjpeg_vectors \
    --grant Foreign=image_jpeg@1/image_jpeg/Jpeg "$@"
