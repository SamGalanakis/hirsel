# X.509 public key extraction

[Standard library](../API.md). [Operations](../x509_key.hirsel).

`std.x509_key.read(der)` reads the `subjectPublicKeyInfo` of one DER certificate.
It returns `P256(point)` or `P384(point)` for `id-ecPublicKey` with the matching
named curve and a 65-byte or 97-byte uncompressed SEC 1 point, respectively.
The returned bytes start with `0x04`.

`Invalid` covers malformed framing, including non-minimal lengths and trailing
bytes, and unsupported key encodings. The reader treats other certificate field
contents as opaque. Point coordinates are returned as encoded.
