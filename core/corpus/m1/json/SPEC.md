# JSON parse, path query and pretty-print

Read a slash-separated path followed by one JSON document per line. Parse null, booleans, finite JSON numbers, strings with escapes and UTF-16 surrogate pairs, arrays and objects. Object keys retain first insertion position and the last value wins. Query the document and render the entire document with two-space indentation and no final newline. Print each query as JSON, then aggregate pretty-output bytes and byte sum. The print case also emits the whole pretty document. No semantic nesting-depth limit exists.

## Input and size

128 documents, each with 32 object records, nested arrays, strings, booleans and nulls. Idiomatic nodes are an owned recursive enum tree. Matched nodes are an active-tag byte blob with inline scalar text and u32 child offsets. Temporary construction vectors are freed after publication; blob prefixes remain until document close.

The committed normal input is 248,800 bytes. `cases.json` gives every input path and variant; no input is generated inside the measured process.

## Oracles and failure variants

Every JSON input is constructed by Python json.dumps or is an explicitly written small literal. EXPECTED comes from Python json.loads and json.dumps. Numeric fixture spellings are integers and -2.5; exponent formatting is not a cross-language oracle. error is a trailing comma in document 2. missing and wrong-kind test path errors; invalid-number rejects 1e999 rather than accepting infinity. capacity rejects the third value in preorder in document 2 before publishing it. Document 1 output survives; each acquired document region releases once.

For every case C, `inputs/C.txt` comes from the construction just described and `EXPECTED/C.stdout`, `C.stderr` and `C.exit` come from the corresponding independent Python function in `core/tools/reference.py`. Stderr is empty; exit is 0, 2, 3 or 4 under `core/README.md`. The generator never invokes Rust or Hirsel. The values were fixed before Rust output comparison. Resource names in stdout identify lexical owners, rather than allocator blocks. Named admission failures are logical source sites, never native allocator call numbers. Cleanup does not perform another portable admission.

## Hand check

The small object has two items. Its second name decodes to b, newline, lambda. Duplicate x is true, not null. The query is the JSON string "b\nλ". The first-insertion object order is name then x. Two-space rendering has 115 UTF-8 bytes and byte sum 6418; the pretty case exposes every rendered character for review.

## Spec draft correspondence

See `core/SPEC-COMPARISON.md` for this workload's differences from the spec lane's informative example. Runtime expected values come from this contract and the independent references, never from executing that example.
