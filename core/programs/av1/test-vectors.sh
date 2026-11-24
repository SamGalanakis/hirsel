#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR [hirsel test options]
# REFERENCE_DIR is the AV1_REFERENCE_DIR output of build.sh.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
reference=$2
mkdir -p "$work"
shift 2
cp "$codec"/*.hirsel "$codec"/*.a "$work"/
python3 "$codec/vectors/prepare.py" "$work" "$reference"
"$compiler" test "$work" --exact av1@1/tests/codec_vectors \
    --grant Foreign=av1@1/av1/DecoderCodec --grant Foreign=av1@1/av1/EncoderCodec "$@"
