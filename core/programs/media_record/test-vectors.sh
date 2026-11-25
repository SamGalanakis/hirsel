#!/bin/sh
# Usage: test-vectors.sh WORK_DIR [hirsel test options]
# Records each video codec with Opus through the recorder into WORK_DIR/out,
# then checks the files with ffprobe and ffmpeg (vectors/validate.py).
set -eu
package=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
shift
python3 -I "$package/vectors/prepare.py" "$work"
(cd "$work" && "$compiler" test media_record --exact media_record@1/vectors/dump "$@")
python3 -I "$package/vectors/validate.py" "$work"
