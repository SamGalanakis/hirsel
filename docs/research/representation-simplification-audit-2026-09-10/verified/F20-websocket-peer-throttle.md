# F20 — WSS authentication throttle lacks stable peer identity

Recommend; high confidence, high priority. Owner C25, worker C25-01 ../workers/C25-HOST-OPS.md plus coordinator correction. Independently reopened main serve construction, ws_handler extractor/conversion, Peer/run_protocol error branch and AuthThrottle.

Production serves Router directly, so optional ConnectInfo is absent. Peer::WebSocket.addr=None causes failed authentication to skip throttle entirely. Merely adding the connect-info maker is insufficient: current ws_handler serializes full SocketAddr including the ephemeral source port, so each reconnect would still use a fresh key. Stable remote IP identity must own repeated-attempt accounting.

Target wire ConnectInfo<SocketAddr> through the production make-service and matching WSS test servers, require peer extraction, and key WebSocket throttle by actual peer IP (not port); preserve distinct Iroh node identity. Avoid an optional no-peer bypass in the production contract. Do not trust arbitrary forwarded headers or introduce proxy inference; actual connection peer is the source unless an explicit trusted-proxy contract already exists. Root should document shared-proxy address behavior as needed.

Scope main/ws/Peer construction and focused real TCP/WSS repeated-bad-hello tests using separate source ports plus a valid login. Existing direct AuthThrottle tests do not prove transport wiring. Worker also proposed generic bounded peer-cache eviction; that is not necessary to fix the verified missing/stable-key bug and is deferred unless root establishes a concrete memory-risk requirement. No auth policy/provider/credential changes or live endpoint tests. Audit ran no tests.

Independent materiality qualification: clients behind the same NAT/proxy share the actual-peer IP throttle. Document this intentional transport tradeoff; do not silently trust forwarded headers to avoid it.
