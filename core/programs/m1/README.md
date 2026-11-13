# M1 native milestone

Run every manifest entry and its corpus cases on both native engines:

```sh
python3 core/tools/m1.py --compiler /path/to/hirsel-core --runtime /path/to/libhirsel_core_rt.a --report /tmp/m1.json
```

`--backend cranelift|llvm|both` selects engines; LLVM uses release optimization.
`--only bfs json` selects workloads for iteration. `--store` retains checked,
object and executable products between invocations. Without it the runner owns
and removes a private store. Buck targets `//:core-m1-cranelift` and
`//:core-m1-llvm` each cover the complete manifest for one engine. These are
explicit integration gates and fail until every row passes.

The runner selects one entry module and its package dependency closure, builds
it once per engine, then runs every applicable case from its workload's
`core/corpus/m1/<workload>/cases.json`. Each case gets a fresh working directory
with a copy of its input, the relative input path and variant as arguments, and
any `env` entries over the host environment. Optional `inputs` entries copy
additional named fixtures into that directory. Corpus `layouts` restrict a
case to its named source variants.

The runner compares stdout and stderr as bytes and the exit status as a number.
`EXPECTED/<case>.files/`, when present, lists exact changed output paths and
bytes. Without that directory, no files may change. Input deletion or mutation
also fails. Cleanup-event order is part of the exact stdout oracle. Every
applicable case runs even after a case fails; the table retains the first failure.
A checker refusal/gap, unsupported emission, link error or missing native product
produces a failing row. Empty entry/case selections fail. JSON reports retain
all checker diagnostics and the table is written beside them as Markdown.

The native assembly fixture target `//:core-driver-native` is separate from M1.
It exercises scalar checked bodies and a test-only C Result carrier. It does
not establish source-language entry, capability or corpus acceptance.
