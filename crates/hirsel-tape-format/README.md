# Tape format

The format-2 header, block framing, checksums and frame codec shared by the
runtime and `hirsel tape show`. This crate owns the portable provider operation
codes it encodes. It has no execution or host authority. The runtime tape
harness exercises byte vectors, corruption, truncation and schedule laws.
