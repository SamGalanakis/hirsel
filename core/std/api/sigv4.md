# SigV4

`std.sigv4` signs AWS Signature Version 4 Authorization headers. Open one
sendable `Signer` per sealed access key, region and service. The root Secret
requires the `kdf` use. The key chain derives a `mac`-only signing key for
the UTC day. Opening during the first 15 minutes of a day also prepares the
previous day's key.

The cache owner loops over `wait_for_change`. That function waits on the
wall clock until midnight to derive the new key, or until 00:15 to discard
the previous key. After each change, send each lane a `share_keys(signer)` snapshot
and drop the replaced snapshot. Each snapshot adds a holder using
`secret.share` and has no derivation authority.

Use `write_date` to format the timestamp for `x-amz-date`. Call
`authorize` with the same timestamp and the unadjusted wall clock reading.
Pass the AWS-encoded URI and sorted encoded query. Supply the headers
to sign, sorted by lowercase name, with repeated fields combined. Include
`host` and `x-amz-date`. A temporary credential's session token is public
and must be included as `x-amz-security-token`.

The `Tls` and `Plaintext` payload modes use `UNSIGNED-PAYLOAD`. For S3,
send that value in `x-amz-content-sha256`. `Plaintext` also requires a
signed CRC32C or CRC64NVME checksum header. The `Sha256` mode signs a
supplied SHA-256 payload digest.

`authorize` writes the header value to a caller-provided buffer and uses
caller-provided scratch space for canonicalization. It returns `NoSpace`
if either buffer has insufficient capacity. `sign` accepts an already
prepared string to sign. Both operations allocate no storage and reject a
timestamp with no eligible cached key.

`share_signer` gives another lane a signer with the same root, scope and
keys. `refresh` updates the cache for the current wall time without waiting.
