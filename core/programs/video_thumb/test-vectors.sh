#!/bin/sh
# Usage: test-vectors.sh WORK_DIR [hirsel test options]
set -eu
package=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
work=$1
shift
compiler=${HIRSEL:-hirsel}
python3 "$package/vectors/prepare.py" "$work"
"$compiler" test "$work/video_thumb" --release \
    --exact hirsel.ai/video_thumb@1/tests/sparse_posters_and_sprite_cues \
    --grant Foreign=hirsel.ai/h264@1/h264/H264 \
    --grant Foreign=hirsel.ai/av1@1/av1/DecoderCodec \
    --grant Foreign=hirsel.ai/av1@1/av1/EncoderCodec \
    --grant Foreign=hirsel.ai/video_yuv@1/video_yuv/Yuv \
    --grant Foreign=hirsel.ai/image_webp@1/webp/Webp "$@"
