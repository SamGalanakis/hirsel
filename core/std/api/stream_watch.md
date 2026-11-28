# Stream monitors

`spawn_monitor` watches the read half for disconnect or excess pipelined input.
`spawn_alarm` wakes the stream owner for promotion deadlines and heartbeats.
The owner cancels and joins both tasks when the stream ends.

A release guard sends its opaque key to a Place-owned reader. A full or closed
queue, a denial, or a reply timeout of 100 ms denies the write. Guarded streams
check before releasing data and poll during idle periods. Unguarded expiry
uses a single wall-clock alarm. A blocked guarded write checks again when its
100 ms wait ends or the socket accepts a partial write.
