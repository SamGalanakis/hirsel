#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR [hirsel test options]
# REFERENCE_DIR is the OPUS_REFERENCE_DIR output of build.sh.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
reference=$2
shift 2
python3 "$codec/vectors/prepare.py" "$work" "$reference"
"$compiler" test "$work" --exact opus@1/tests/rfc_vectors \
    --exact opus@1/tests/decode_stereo_frame --grant Foreign=opus@1/opus/Opus "$@"
