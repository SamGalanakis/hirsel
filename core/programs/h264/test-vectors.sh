#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR OPENH264_TARBALL [hirsel test options]
# REFERENCE_DIR is the OPENH264_REFERENCE_DIR output of build.sh.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
reference=$2
tarball=$3
shift 3
python3 -I "$codec/vectors/prepare.py" "$work" "$reference" "$tarball"
"$compiler" test "$work/h264" --exact h264@1/tests/conformance --grant Foreign=h264@1/h264/H264 "$@"
