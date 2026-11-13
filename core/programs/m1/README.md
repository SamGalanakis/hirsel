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

The runner supplies every source in the entry package and its package dependency
closure. The driver builds the entry
once per engine, then runs every applicable case from its workload's
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

The native assembly target `//:core-driver-native` is separate from M1.
It builds real source Result/Exit entries on both engines and tests success,
failure and cache behavior. The source Exit-7 smoke is acceptance for that
program; the M1 program rows and OS grant integration remain separate.

Build/run acceptance belongs to the driver and covers the entry's reachable
instances and the declarations they name. Unreached library gaps do not block a
program row. Package `check` and image production still check all inputs.
Each engine also has a standing `core + std fully checked; canonical image
accepted` row. That row stays red until the canonical image can be produced.

The report includes a census for each stopped program row. Each reached body or
declaration contributes its first stop. Completed bodies follow emitted calls and
function references; incomplete bodies follow their resolved source names, so a
gap at main does not conceal its callees. Counts rank current stops and do not
estimate later work. JSON includes the first-stop definition, rule and byte span.
