# Stream monitors

`spawn_monitor` watches the read half for disconnect or excess pipelined input.
`spawn_alarm` wakes the stream owner for promotion deadlines and heartbeats.
The owner cancels and joins both tasks when the stream ends.
