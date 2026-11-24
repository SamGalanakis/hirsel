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
