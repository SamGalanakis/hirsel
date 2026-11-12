# Rust baseline

Measured on 2026-11-12 from base `f889fb65`, with the final corpus sources committed beside this table. Rust 1.99.0, x86_64-unknown-linux-gnu, x86-64 CPU target, CPU 16, one sample per configuration. All 216 configurations completed and all measured run outputs matched EXPECTED. No Hirsel program was run.

Each cell contains **idiomatic / matched** values. Compile costs are thousands of user instructions per nonblank/noncomment source line. Run times are whole-process milliseconds. RSS is the largest process high-water mark, in MiB. These are single samples on a shared host; a small difference is not a speed claim.

| Workload | Input bytes | Source lines | Dev cold check K/line | Dev cold build K/line | Release cold build K/line | Dev warm run ms | Release warm run ms | Release warm run RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| request-scratch | 334,676 | 147 / 155 | 604.38 / 600.84 | 3211.19 / 3121.77 | 11420.09 / 10858.18 | 28.689 / 20.817 | 18.152 / 17.215 | 2.75 / 2.75 |
| json | 248,800 | 436 / 513 | 373.07 / 339.58 | 1408.68 / 1260.52 | 4408.06 / 3993.04 | 37.051 / 58.121 | 13.043 / 12.276 | 2.25 / 2.50 |
| lru | 405,196 | 223 / 250 | 375.03 / 350.88 | 2213.75 / 1987.67 | 7676.70 / 6808.02 | 31.870 / 32.704 | 5.165 / 5.158 | 2.25 / 2.25 |
| bfs | 697,566 | 164 / 181 | 504.48 / 487.07 | 2583.11 / 2398.38 | 7639.61 / 7102.30 | 26.062 / 26.993 | 11.042 / 7.675 | 4.75 / 4.00 |
| pipeline | 614,403 | 299 / 307 | 350.28 / 341.26 | 3931.30 / 3833.79 | 19910.23 / 19427.34 | 19.014 / 19.229 | 11.560 / 8.304 | 3.75 / 3.75 |
| versions | 2,539 | 238 / 260 | 374.65 / 351.93 | 1505.38 / 1350.57 | 3902.94 / 3731.08 | 2.117 / 2.490 | 1.260 / 1.418 | 1.75 / 1.75 |
| rope | 69,851 | 219 / 239 | 424.51 / 419.49 | 1751.81 / 1705.06 | 5355.45 / 5201.55 | 9.612 / 10.968 | 4.527 / 2.999 | 2.75 / 2.50 |
| files | 288,000 | 250 / 250 | 378.95 / 378.95 | 1528.49 / 1528.42 | 3601.38 / 3604.74 | 4.397 / 4.241 | 3.268 / 2.692 | 2.25 / 2.25 |
| generics | 83,968 | 147 / 184 | 572.33 / 522.01 | 3251.23 / 2270.86 | 10650.08 / 6847.68 | 13.154 / 11.776 | 5.754 / 9.382 | 3.50 / 3.50 |

The full [TSV](BASELINE.tsv) includes cold/warm check, build and run in both profiles, raw instructions, exact line counts, nanosecond-resolution elapsed seconds, maximum process RSS, rounded cgroup memory peak and wrapper/admission time. [Environment](BASELINE-ENVIRONMENT.json) records rustc identity, host and flags. [Protocol](MEASURE.md) defines equivalent comparisons and missing/censored rows. Raw sample logs stay outside the repository in the lane evidence directory.

Compilation directly invokes rustc and includes all linker/worker children plus the GNU time child. It excludes Cargo orchestration and uses prebuilt std. Cold does not mean the shared host OS page cache was flushed. Warm compilation is a fresh recompilation, not a no-op. File-lock twins intentionally have identical source; their timing differences show sample noise. Source lines include the common CLI and trace module. The check startup floor is therefore charged to a small denominator; do not compare these K/line numbers with a large-project Rust check reference as if they were the same experiment.
