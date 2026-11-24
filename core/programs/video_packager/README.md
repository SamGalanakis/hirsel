# Video packager

Pass pre-encoded samples for one track to `packager.write_segment`. Use a
closed GOP starting with a sync sample for video and an uninterrupted run
for audio. Provide sample flags and signed composition offsets in track
ticks. The returned segment records its presentation interval. Write the
framing buffer followed by the exact payload in sample order. The packager
does not read or copy payload bytes.

Place the output of `write_init` at `r<ID>/init.mp4` and each segment at
`r<ID>/<number>.m4s`. Supply the same track metadata and an RFC 6381
codec string in the rendition. Number segments consecutively and start
each representation at the same presentation position in the program.
Video renditions use aligned segment boundaries. Call `hls_master` for
`master.m3u8` and `hls_media` for each `r<ID>/media.m3u8`.

Each video rendition also has `r<ID>/iframe.m3u8`. Write each
`r<ID>/iframe-<number>.m4s` with `write_iframe` and the leading
`iframe_payload_bytes` of the corresponding segment payload.

The segment limits bound payload size and the number of samples inspected.
The caller owns the framing buffer. Manifest limits reserve storage and
bound work. Exhausting storage inside a manifest call raises Hirsel's
`Capacity` failure. The encryption mode reserves the CBCS/CENC entry
point for TLS-8's `PacketCipher`. Both encryption modes refuse until
that implementation is available.

Call `dash_manifest` for `manifest.mpd`. The MPD uses the same init and
media objects as HLS and records each rendition's integer segment
timeline and presentation offset.
