# Single-core object transfer benchmark

The client reuses one HTTP/1.1 connection for 8 MiB PUTs or GETs.
The loopback server drains PUTs before replying and sends a prefilled
object for GETs. There is no disk I/O. The client and server run on
separate physical cores. All TLS clients use the same TLS 1.3
AES-128-GCM server.

On x86-64 Linux, prepare Buck2 as usual, then build the standalone
Rust binary and the Hirsel client. Use a new output directory for each run.

```sh
out=$(mktemp -d)
bin=$(taskset -c 8-11 cargo build --manifest-path bench/objstore/Cargo.toml \
    --locked --release --message-format=json |
    jq -r 'select(.reason == "compiler-artifact" and .executable != null) | .executable')
"$bin" cert "$out/cert"
taskset -c 8-11 python3 bench/objstore/build_client.py "$out/cert" "$out/client"
taskset -c 8-11 cc -O2 -shared -fPIC bench/http/allocation_count.c -o "$out/alloc.so"

systemd-run --user --scope --quiet -p MemoryMax=2G -p MemorySwapMax=0 \
    taskset -c 8-11 python3 bench/objstore/run.py smoke \
    --rust "$bin" --cert "$out/cert" --hirsel "$out/client/hirsel" --output "$out/smoke.jsonl"
systemd-run --user --scope --quiet -p MemoryMax=2G -p MemorySwapMax=0 \
    taskset -c 8-11 python3 bench/objstore/run.py time \
    --rust "$bin" --cert "$out/cert" --hirsel "$out/client/hirsel" --output "$out/time.jsonl"
```

The smoke run checks every byte in both directions. The timing run reports 60
interleaved samples of eight requests per cell after one warmup request per
connection. It gates client cycles with perf and locks the local timing cores.
Divide bytes by nanoseconds for GB/s and cycles by bytes for cycles/byte.

Use `instructions` with the shipping client for user instructions. Use
`allocations --preload "$out/alloc.so"` with `--hirsel "$out/client/hirsel-allocations"`
for successful allocation calls during the measurement phase. Rust counts
glibc allocation calls and Hirsel counts allocations through the instrumented
runtime. The Hirsel DONE marker adds a small fixed cost per batch.

The default matrix includes `ktls-send` (kernel TLS using ordinary
`IORING_OP_SEND` and fixed-buffer receives). Select `--mode ktls` to
require `IORING_OP_SEND_ZC` on the kTLS socket. Linux software kTLS may
refuse it with `EOPNOTSUPP`. SEND_ZC usage counts include the warmup
and exclude the TLS handshake. The io_uring client waits for each send's
notification before reusing transmit storage. Buffered rustls uses
registered staging buffers for its ciphertext.
