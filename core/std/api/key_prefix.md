# Ordered key prefixes

`matches(full, leading)` reports whether `leading` encodes complete leading
components of `full`. Tuples and generated record keys concatenate their
components, so their grouping does not change the prefix. Strings and bytes
include their terminators, and a `Desc` component matches as a whole. The unit
codec supplies an empty prefix.

The function reads the two [key schemas](codec.md) and propagates a describer's
`EncodeError`. Hand-written codecs must describe the same ordered format that
they encode.
