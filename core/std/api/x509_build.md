# X.509 certificate building

[Standard library](../API.md). [Operations](../x509_build.hirsel).

`std.x509_build.self_signed_p256(key, random, point, serial, common_name,
not_before, not_after)` returns a DER-encoded X.509 v3 certificate. The issuer
and subject use the same common name. The point is the 65-byte uncompressed
public key from `secret.p256_key`. Use a positive serial and validity times
with whole seconds. `Sign` carries a refusal from `secret.sign`. `Invalid`
covers invalid arguments or a signature that fails verification with the
supplied point.

An `Issuer` holds a signing key, its 65-byte point and its common name.
`ca_p256(issuer, random, serial, not_before, not_after)` returns the issuer's
self-signed CA certificate: basicConstraints with cA set and keyUsage
keyCertSign and cRLSign, both critical, and a subjectKeyIdentifier.
`leaf_p256(issuer, random, point, serial, common_name, names, not_before,
not_after)` returns a server certificate for `point` signed by the issuer:
basicConstraints without cA and keyUsage digitalSignature, both critical,
extKeyUsage serverAuth, a subjectAltName holding `names`, and the subject and
authority key identifiers. A `San` is `Dns(name)`, a DNS name of 1 to 253
bytes whose labels are 1 to 63 letters, digits and hyphens with an optional
leading `*.`, or `Address(ip)`, encoded as 4 or 16 octets ([RFC 5280
§4.2.1.6](https://www.rfc-editor.org/rfc/rfc5280#section-4.2.1.6)). An empty
`names` is `Invalid`: verifiers check the SAN only, not the common name
([RFC 6125 §6.4.4](https://www.rfc-editor.org/rfc/rfc6125#section-6.4.4)).
Key identifiers are the SHA-1 of the point (RFC 5280 §4.2.1.2, method 1).

The builder signs the DER `TBSCertificate` with SHA-256 and converts the
fixed-width signature into the two positive DER integers required by
[RFC 3279 §2.2.3](https://www.rfc-editor.org/rfc/rfc3279#section-2.2.3).
For a WebRTC fingerprint, pass the complete returned certificate to
`sha256.hash`. The digest includes the signature.
