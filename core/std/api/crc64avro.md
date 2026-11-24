# CRC-64-AVRO

[API overview](../API.md).

`fingerprint(bytes)` computes the Apache Avro schema fingerprint, with no
allocation. The input bytes use Avro's reflected polynomial
`0xc15d213aa4d7a795`. [Codec schemas](codec.md)
