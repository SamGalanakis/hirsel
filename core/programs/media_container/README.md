# Media containers

The package handles media framing without I/O or codec execution. Supply complete
container units in a byte window and retain unread bytes when a parser returns
`Truncated`. Demux callbacks borrow packet data from that window. Mux functions
write framing into reusable caller-owned buffers. Send the framing and borrowed
payload in order.

## Ogg and Opus

`ogg.demux` accepts one page at a time and follows one logical stream. The
callback receives a packet fragment and flags marking the beginning and end of
the packet. Reuse the same stream state for later pages, including pages in later
input windows. Use one stream state per serial number for interleaved streams.
`ogg.read` reads one page without stream state.

`ogg.write_page` takes a page header, a lacing table and the matching contiguous
body. It writes up to 282 header bytes, including the checksum over the header
and body. For Ogg Opus, use `opus.write_head` and `opus.write_tags` to write the
initial mono or stereo identification packet and vendor-only comment packet.
Pass encoded audio directly as the bodies of later pages. The caller supplies
granule positions in 48 kHz samples, including pre-skip and any final trim.

## Fragmented MP4

`mp4.read_init` reads an initialization segment and loans each audio or video
track's sample-entry box with its `trex` defaults. `mp4.write_init` writes a
single-track initialization segment from that metadata. The caller supplies the
codec configuration as a sample-entry box with data reference index 1.

`mp4.write_fragment` writes a `moof` and `mdat` header. Send the encoded
samples after that header in the same order as their duration, size, flags and
composition offsets. `mp4.read_fragment` accepts one `moof` with a single
`traf`, followed by its `mdat`, and loans each sample with its timing. It
supports moof-relative offsets and uses the supplied track defaults if a run
omits a field. `mp4.read_box` can scan other top-level boxes.

## WebM and Matroska

`webm.read_init` loans codec identifiers and private data for tracks in a
WebM or Matroska Segment. It reports the Segment's timestamp scale and the
offset of the next child element. `webm.read_element` scans those children.
`webm.read_cluster` loans frames from SimpleBlock and BlockGroup elements.
It accepts no lacing, Xiph lacing, fixed-size lacing and EBML lacing. A laced
frame carries the block's timestamp and its index in the lace. The caller
can apply track defaults or codec timing to derive individual timestamps.
BlockGroup duration is in timestamp ticks and discard padding is in nanoseconds.

`webm.write_init` writes a WebM header with one TrackEntry per `webm.Track`
(up to 64, numbered distinctly) for Opus, VP8, VP9 or AV1.
`webm.write_frame` writes the Cluster and SimpleBlock prefix for one frame.
To put audio and video in one file, queue each track's frames in its own
timestamp order in a `webm.Interleave` and write frames in the order
`webm.next` releases them. It releases the earliest queued frame once every
open track has a frame queued, or once more than its limit wait; `webm.close`
stops a finished track from holding the others back. Released timestamps never
decrease.
For a keyframe with discard padding, send the prefix, frame and suffix from
`webm.write_padded_frame` in that order. The caller keeps all frame payloads.
