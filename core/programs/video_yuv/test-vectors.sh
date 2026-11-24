#!/bin/sh
# Usage: test-vectors.sh WORK_DIR REFERENCE_DIR [hirsel test options]
# REFERENCE_DIR is the YUV_REFERENCE_DIR output of build.sh for this host.
set -eu
codec=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
llvm=${LLVM_SYS_231_PREFIX:?set LLVM_SYS_231_PREFIX to the pinned LLVM tree}
mkdir -p "$1/fixtures"
work=$(cd "$1" && pwd)
reference=$(cd "$2" && pwd)
shift 2
cp "$codec"/*.hirsel "$codec"/*.a "$codec/vectors/tests.hirsel" "$work"
python3 - "$codec" "$work" <<'PYTHON'
import json
from pathlib import Path
import sys

codec, work = map(Path, sys.argv[1:])
manifest = (codec / "hirsel.toml").read_text()
tests = (codec / "vectors/test.toml").read_text()
(work / "hirsel.toml").write_text(
    manifest + tests.replace('"fixtures"', json.dumps(str(work / "fixtures"), ensure_ascii=False))
)
PYTHON
"$llvm/bin/clang" -O2 -std=c11 -Wall -Wextra -Werror -I "$reference/include" \
    "$codec/vectors/reference.c" "$reference/libyuv.a" -o "$work/reference"
(cd "$work/fixtures" && ../reference)
"$compiler" test "$work" --exact video_yuv@1/tests/direct_libyuv_vectors \
    --grant Foreign=video_yuv@1/video_yuv/Yuv "$@"
