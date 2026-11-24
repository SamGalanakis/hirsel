# X.509 certificate building

[Standard library](../API.md). [Operations](../x509_build.hirsel).

`std.x509_build.self_signed_p256(key, random, point, serial, common_name,
not_before, not_after)` returns a DER-encoded X.509 v3 certificate. The issuer
and subject use the same common name. The point is the 65-byte uncompressed
public key from `secret.p256_key`. Use a positive serial and validity times
with whole seconds. `Sign` carries a refusal from `secret.sign`. `Invalid`
covers invalid arguments or a signature that fails verification with the
supplied point.

The builder signs the DER `TBSCertificate` with SHA-256 and converts the
fixed-width signature into the two positive DER integers required by
[RFC 3279 §2.2.3](https://www.rfc-editor.org/rfc/rfc3279#section-2.2.3).
For a WebRTC fingerprint, pass the complete returned certificate to
`sha256.hash`. The digest includes the signature.
