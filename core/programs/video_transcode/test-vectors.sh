#!/bin/sh
# Usage: test-vectors.sh WORK_DIR [law | bench WORKERS] [hirsel test options]
# Prepares fixtures with ffmpeg, runs the ladder law through Hirsel into a
# simulated S3 bucket, then checks the stored objects with ffprobe/ffmpeg.
set -eu
package=$(cd "$(dirname "$0")" && pwd)
compiler=${HIRSEL:-hirsel}
work=$1
mode=${2:-law}
shift
[ $# -gt 0 ] && shift
if [ "$mode" = bench ]; then
    workers=$1
    shift
    python3 -I "$package/vectors/prepare.py" "$work" bench "$workers"
else
    python3 -I "$package/vectors/prepare.py" "$work" law
fi
(cd "$work" && "$compiler" test video_transcode --exact hirsel.ai/video_transcode@1/tests/ladder \
    --grant Foreign=hirsel.ai/av1@1/av1/DecoderCodec --grant Foreign=hirsel.ai/av1@1/av1/EncoderCodec \
    --grant Foreign=hirsel.ai/h264@1/h264/H264 --grant Foreign=hirsel.ai/video_yuv@1/video_yuv/Yuv "$@")
python3 -I "$package/vectors/validate.py" "$work"
