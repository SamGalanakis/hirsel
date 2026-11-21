# Tape format

The format-4 header, block framing, checksums and frame codec for a recording
file (tape), shared by the recording runtime and `hirsel tape show`
(`docs/recording.md#format`). This crate owns the portable provider operation
codes it encodes. It has no execution or host authority. The runtime tape
harness exercises byte vectors, corruption, truncation and schedule laws.

Format 4 records socket payloads and the original Net rules for local narrowing.
REPEAT stores periodic CHOOSE frames and replay expands them to their original positions.
