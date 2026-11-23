# HTTP service benchmarks

Run from a prepared Hirsel checkout on Linux with perf user-instruction access,
Valgrind, Cargo, Go 1.22 or later, a C compiler, and llvm-symbolizer on PATH.

```sh
python3 bench/http/run.py --output /path/to/http-results
```

The command builds locked release references, profiles B1 and runs the service
sweeps and continuous soaks. Use `--stage b1` for attribution or
`--stage services --check` to probe the service responses. The defaults pin the
server to CPU 0 and the load generator to CPU 1. Choose distinct physical cores
on the same machine with `--server-cpu` and `--client-cpu`. Each server uses
one runtime worker. Each measured invocation runs under the 2 GiB cap.
The six sequential soaks take twelve hours.

B1 reuses the serial keep-alive workload in `bench/micro/http`. Hardware
instruction counts cover the server during the measured requests, with
separate frame-pointer profiles. The analyzer uses startup-subtracted
Callgrind self instructions for phase attribution when kernel IPs dominate
the hardware samples. Hyper and Hirsel send the same B1 reply with a live Date. B1 reports cycles
alongside user instructions. Hirsel also records replay, charges budgets,
validates Host and rejects duplicate Content-Length. Tokio+httparse is the
minimal server floor with a static response and no Date. The service benchmark
runs all three servers with Date.

The service routes are `/plaintext`, `/json` and `/store`. Hirsel registers
them in a std HTTP route table and builds the JSON document in the request's
scratch region. Each server serializes JSON per request. For storage, each
server sends one request to an in-process counter owner over loopback and gets
the updated value back after durability.
Hirsel uses a place-store read transaction and one write transaction per
request, and replies to a group of requests after one `store.durable` covers
its last commit. Hyper+Axum and Go use one SQLite read transaction and one
write transaction with WAL and synchronous=FULL. All trial databases live on
the same filesystem. The store route includes a short-lived local connection
in every implementation.

The client schedules arrivals at fixed intervals across 64 HTTP/1.1
connections and starts each timer at the scheduled send. A full queue drops
the arrival and disqualifies the point. Each sweep doubles the offered rate
and then narrows the first failing bracket to within 10%. The knee is the
highest tested rate in the bracket with every response valid, at least 99%
completed before the arrival window ended, and p99 at most three times the
low-rate p99. Reported latency points each use 50% or 90% of that server's
own knee. An invalid point is censored.

The plaintext and store soaks run at 70% of each server's knee. The minute
samples track Linux process peak and current RSS. Allocation rates count
successful libc allocation calls and add Go heap allocations for Go. The
counter changes the soak's allocation path. The latency sweep uses the
ordinary allocator. The report includes per-minute ratios and flags when
median RSS in the last third exceeds the first third by more than 5%.
