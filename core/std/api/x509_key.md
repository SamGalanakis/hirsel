# X.509 public key extraction

[Standard library](../API.md). [Operations](../x509_key.hirsel).

`std.x509_key.read(der)` reads the `subjectPublicKeyInfo` of one DER certificate.
It returns `P256(point)` or `P384(point)` for `id-ecPublicKey` with the matching
named curve and a 65-byte or 97-byte uncompressed SEC 1 point, respectively.
Both points start with `0x04`. For `id-Ed25519` with absent parameters it
returns `Ed25519(key)`, a raw 32-byte public key.

`std.x509_key.spki_range(der)` returns the half-open byte range of the whole
DER SubjectPublicKeyInfo, including its tag and length, or `None` on malformed
certificate framing. It can locate the field regardless of its key algorithm.

`Invalid` covers malformed framing, including non-minimal lengths and trailing
bytes, and unsupported key encodings. The reader treats other certificate field
contents as opaque. Point coordinates are returned as encoded.
