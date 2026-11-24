# Video thumbnails

`thumb.index` indexes an immutable MP4 or WebM object through an exact range
reader. Use `media_container.probe.pread` for an open file. The same range
reader supplies the selected packets to `thumb.poster` and `thumb.sprite`.
Both select the latest indexed keyframe at or before the requested time.
Timestamps are milliseconds on the presentation timeline.

The decoder accepts progressive 8-bit 4:2:0 AVC and 8-bit 4:2:0 AV1.
`thumb.poster` returns RGBA8 and a ThumbHash. `thumb.sprite` returns a WebP
or AVIF sheet encoded by Image Pipeline, plus a WebVTT index. Supply the sheet URL in the layout.

Generate the ffmpeg fixtures and run the conformance law with an installed
toolchain. The two MP4 files contain about 1 GB of media data each.
`ffmpeg-commands.txt` in the output directory records the reference commands.

```sh
core/programs/video_thumb/test-vectors.sh /path/to/vectors
```
