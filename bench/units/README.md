# Fair Hirsel and Tokio benchmark

Run from a clean checkout on the benchmark host.

```sh
taskset -c 6-7 bench/units/fair.py --cells a1,regimes --quick --out "$results/quick-1"
taskset -c 6-7 bench/units/fair.py --cells a1,regimes --quick --out "$results/quick-2" --reverse
taskset -c 6-7 bench/units/fair.py --cells a1,regimes --full --out "$results/full"
```

The runner builds the current commit with Buck2 and the capped benchmark
recipes on CPUs 4–5. It verifies cached executable hashes before use.
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
- The controller takes the regime lock before the board lock. It waits
  outside the locks while host load exceeds 8 or a hold would overlap
  02:45–04:00 local time. Both boundaries check for `cc1plus`. A hold that
  crosses the load limit or sees a compiler at either boundary is discarded.
  The runner alternates arm order and waits at least 15 seconds between
  holds. It pins the server, load generator and helper to their assigned CPU
  sets. The receipt records the observed host checks for every attempted hold.

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
| `version`, `commit`, `config` | Schema version, source commit and selected workloads, CPU masks and repetition count |
| `host`, `started`, `updated`, `host_load_start`, `host_load_last` | Host name, local timestamps with offset, and load averages |
| `binary_sha256`, `toolchain` | SHA-256 by binary path relative to the commit's cache, plus compiler and runtime provenance |
| `expected_rows`, `rows`, `failures`, `complete` | Expected and observed goal-board rows, recorded failures and completeness |
| `retention_8192_over_256` | Per-filesystem high/low ratio for each port, with the metric named in each entry |

Rows use `goal`, `row`, `status`, `commit`, `metrics`, `samples` and
`ratios`. Each `samples` map is keyed by `hirsel` and `tokio`.
`ratios.tokio[metric]` divides the Hirsel metric by Tokio's. Ceiling rows use
passing offered turns/s. Matched A1 rows use server CPU µs per turn, where a
smaller ratio means less Hirsel CPU. Regime rows use work/s and speedup over
that port's width-1 work/s. A1 samples also include the actual offer,
intended-arrival p99 and server-added p99 in nanoseconds, goodput, sync
groups per turn, SHA backend and raw trial identifier. A missing result or
unsupported metric is JSON null or absent and cannot qualify its row.

Full retention compares passing ceilings and is the result to compare with
the 0.90 goal. Quick retention compares measured goodput at the same fixed
offer. Raw A1 JSONL and regime `pair.json` files preserve individual
measurements and host guard records. Full also retains perf data for the CPU
profiles.
