# Fair Hirsel and Tokio benchmark

Run from a clean checkout on the benchmark host.

```sh
taskset -c 26-27 bench/units/fair.py --cells a1,regimes --quick --out "$results/quick-1"
taskset -c 26-27 bench/units/fair.py --cells a1,regimes --quick --out "$results/quick-2" --reverse
taskset -c 26-27 bench/units/fair.py --cells a1,regimes --full --out "$results/full"
```

The runner builds the current commit with Buck2 and the capped benchmark
recipes on CPUs 8–11 at four jobs. Builds take neither measurement lock.
The runner verifies cached executable hashes before use.
`--cache` defaults to `fair-bin-cache` beside the output directory. Use
the same parent directory for both quick runs to share the cache. Each
output directory must be new and outside the checkout.

- Quick runs hardware SHA-256 A1 at 3,000 offered turns/s on ext4 with 256
  and 8,192 connections. It also runs fan-in and the pipeline at 100 µs
  and widths 1 and 8. The ratio uses the median of three paired samples.
- Full searches A1 ceilings with 256, 2,048 and 8,192 connections on ext4
  and tmpfs, then runs the software SHA-256 control at 8,192 on ext4. It
  profiles a matched ext4 offer on both ports. Full runs all 17 executable
  regime cases at widths 1, 4 and 8 and all five grains. Checkbox has one
  native grain. Each regime also has a width-8 CPU profile at 10 µs or
  native grain. The tmpfs control requires noninteractive mount and unmount
  access for the private journal directory.
- Use `--cells a1` or `--cells regimes` to run one family. Use
  `--regimes pairs,sockets64` to narrow the regime family. Increase the number
  of paired samples with `--repeats`. Full defaults to one pair per regime
  point. The measurement wait defaults to 30 minutes for quick and four
  hours for full. `--deadline-seconds` overrides that bound after builds.
- Before taking the locks, the runner samples `/proc/stat` for one second
  and requires every server CPU to be under 5% busy. The controller takes
  the regime lock before the board lock and releases both after the timed
  pair. Holds avoid 02:45–04:00 local time and are separated by at least
  15 seconds. The runner records host load and `pgrep -x cc1plus` at both
  boundaries. Neither reading is an admission or discard rule.
- Each arm spans at least one second. For short regimes the runner repeats
  the same checksum-checked invocation within the arm and reports the rate
  from the mean invocation time. Raw receipts retain every invocation.
- A scheduler trace charges on-CPU intervals to each arm's measured window.
  For each CPU set, foreign share is on-CPU time outside the arm's process
  tree divided by the arm's wall time and the number of CPUs. An arm needs
  a complete trace and foreign share below 1.5% on both the server and load
  sets. The runner retains the timings and trace regardless of qualification.
  Raw `arm_cpu` rows include the window, root PIDs, foreign nanoseconds per
  CPU and the shares by role. No task names are excluded.
- Use `--server-cpus`, `--load-cpus`, `--helper-cpus` and
  `--controller-cpus` to select masks. The defaults are server 0–3 and
  16–19 on CCD0, load generator 24–25, and helper and controller 26–27.
  Builds use only 8–11. CPUs 4–7, 12–15, 20–23 and 28–31 are excluded.

A1 clients schedule requests independently of replies. An A1 pass requires
intended-arrival p99 at most 5 ms, an independently correct reply for every
issued turn, zero timeouts and clean exits. The clock includes the prescribed
1 ms tool holds and any queueing on a connection. Server-added latency is
reported separately. Both servers run one owner per connection. The eight
committers each wait 400 µs after their first record, drain up to 4,096
records and call `fdatasync` once before acknowledging the batch. At an
identical offered rate the observed groups per turn must agree within 5%.

## Receipt

`fair.json` is published with the goal board's atomic receipt writer after
each completed cell. `SUMMARY.md` renders the same result for readers.
The receipt has these top-level fields.

| Field | Meaning |
|---|---|
| `version`, `commit`, `config` | Schema version, source commit, workload selection, CPU masks, build budget, sync policy, foreign-share limit and minimum arm length |
| `host`, `started`, `updated`, `host_load_start`, `host_load_last` | Host name, local timestamps with offset, and load averages |
| `binary_sha256`, `toolchain` | SHA-256 by binary path relative to the commit's cache, plus compiler and runtime provenance |
| `expected_rows`, `rows`, `failures`, `complete` | Expected and observed goal-board rows, recorded failures and completeness |
| `retention_8192_over_256` | Per-filesystem high/low ratio for each port, with the metric named in each entry |

Rows use `goal`, `row`, `status`, `commit`, `metrics`, `samples` and
`ratios`. Each `samples` map is keyed by `hirsel` and `tokio`.
`ratios.tokio[metric]` divides the Hirsel metric by Tokio's. Ceiling rows use
passing offered turns/s, goodput and intended-arrival p99. Matched A1 rows
use server CPU µs per turn, where a smaller ratio means less Hirsel CPU.
Full also publishes the intended-arrival p99 ratio for its matched rows. Regime rows use work/s and speedup over
that port's width-1 work/s. A1 samples also include the actual offer,
intended-arrival p99 and server-added p99 in nanoseconds, goodput, sync
groups per turn, SHA backend, foreign share by CPU role and raw trial identifier. A missing result or
unsupported metric is JSON null or absent and cannot qualify its row.

Full retention compares passing ceilings and is the result to compare with
the 0.90 goal. Quick retention compares measured goodput at the same fixed
offer. Raw A1 JSONL and regime `pair.json` files preserve individual
measurements and host guard records. Full also retains perf data for the CPU
profiles.
