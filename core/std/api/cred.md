# Cred

`std.cred` gives one credentials record whatever the key's source.
`credentials` fetches a fresh set from a
[`Cred` grant](../../../docs/spec/tls.md#tls-7-credential-grants). The secret
access key arrives sealed by the host, and the key id and session token are
public Strings. It reserves 128 bytes for the key id and 4096 for the token,
and if either value is longer it fetches once more with the reported lengths.
`expires_ns` is the expiry in nanoseconds since the Unix epoch.

`from_secret` builds the record of a static key from a `Secret` grant, with
`expires_ns` equal to `u64` max.

Each call fetches anew and nothing is cached: when to refresh is the caller's
choice. A key that opens a `sigv4.Signer` needs the `kdf` use in its grant.
