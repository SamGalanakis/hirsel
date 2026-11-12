# Request server with request-local scratch

Consume a finite scripted request plan. Each blank-line-separated request starts with GET and a route, followed by key/value headers. Parse headers, look up one immutable Arc-held configuration, render banner:id:content into request-local output, and deliver it through the scripted writer. The service driver reports the first request failure. Each request has its own socket and region lifetime; it never stores parsed fields in server configuration. This fixture measures the whole scripted request lifecycle, without a network throughput claim.

## Input and size

2,048 requests, alternating /alpha and /beta, with decimal ids and 128-byte content prefixes. Each request fits the 64 KiB plan. Idiomatic scratch owns strings; matched scratch stores UTF-8 fields in one byte arena with u32 ranges.

The committed normal input is 334,676 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

normal and small come from the request-plan formula in reference.py. error fails request 2 write after reply preparation; malformed has a header without a colon. capacity fails request 2 header 2; capacity-reply fails request 2 reply storage. Failed request 2 never delivers a partial reply. Request 1 remains delivered. The failing request releases scratch before socket, after request 1 has already released its pair.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

The two bodies are welcome:1:abc and ready:2:xy. Their lengths are 13 and 10. Their byte sums are 1207 and 940, giving 23 bytes and 2147. A parsed header is a lexical scratch view; it cannot survive the scratch close. The first request completes before the second acquires scratch.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
