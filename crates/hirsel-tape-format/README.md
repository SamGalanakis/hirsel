# Tape format

The format-3 header, block framing, checksums and frame codec shared by the
runtime and `hirsel tape show`. This crate owns the portable provider operation
codes it encodes. It has no execution or host authority. The runtime tape
harness exercises byte vectors, corruption, truncation and schedule laws.

Format 3 records socket payloads and the original Net rules for local narrowing.
The reader refuses format 2 at block zero.
