# C21-F2 disposition — skip as latent API hardening

Worker accurately identifies publicly constructible transport/auth combinations rejected later by the host, plus late ticket parsing. I reopened ClientConfig/validate/transport_target, Client::new, transport auth handling and FFI constructors; searched direct struct literals/auth/ticket field writes. Current shipped FFI constructors create valid websocket-static, iroh-device and iroh-pairing combinations. The only direct struct literal consumer found outside config is test_config in client_flow.rs; no current production mutation creates the invalid pair.

A future tagged validated config could remove invalid cross-products and a parsing step, but the proposed API-wide constructor/privacy/transport rewrite solves misuse by hypothetical direct callers, not a demonstrated shipped failure or substantial present branching burden. Invalid ticket/host input is already reported as connection failure; changing its timing is not itself a representation defect. No config migration/wire change is warranted from current evidence.

Reject the broad recommendation at this audit's materiality bar; preserve evidence for fresh review. C21's accepted result is F02 generic ThreadAction history identity. No tests/live config reads; unchanged3ee/treea4aac830.
