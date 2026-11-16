# Socket ownership and Net rules

Shared pure rule intersection and run-owned socket records for the evaluator and
native runtime. Language handles are eight-byte keys. Splitting changes the
record's directional owner bits and creates no record or allocation. The final
owner closes its OwnedFd; a write-half release first performs SHUT_WR.

Provider attempts remain refused with Other(38) until W1b. Test harnesses can
supply owned descriptors directly; no program or CLI operation can do so.
