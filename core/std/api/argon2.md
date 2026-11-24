# Argon2id password hashing

`argon2.hash(password, salt, cost)` returns a PHC string with a 32-byte tag.
Supply a fresh random salt, typically 16 bytes from `host.random`. Use
`argon2.recommended()` for the RFC 9106 profile of 64 MiB, three passes and
four lanes.

`argon2.verify(password, phc)` returns `Ok(true)` for a matching password and
`Ok(false)` for a different tag. It requires canonical Argon2id version 19 PHC
with ordered decimal costs and unpadded standard base64. Other encodings return
`Malformed`. Kernel refusals return `Argon2(error)`.

The stored costs determine verification work. Bound steps and bytes when
verifying untrusted PHC strings. The kernel takes one step per rounded working
block per pass, plus the call step. It refunds its working memory before
returning. See the [Argon2id specification](../../../docs/spec/core.md#argon2id).
