# WebSocket protocol fixtures

Build the current checkout's core/std sources with the existing compiler and
runtime Buck targets. The fixture builder creates the explicit compiler inputs
and grants, with a 256 MiB logical root within the 2 GiB process ceiling.
Replace the three tool paths below with the outputs of the corresponding Buck
targets and pinned linker. Use a fresh output directory for another compiler.

```sh
python3 tools/tests/wire_build.py \
  --compiler <hirsel-core-driver-bin-hirsel> \
  --runtime <core-driver-runtime-pic> --cc <hirsel-clang> \
  --backend llvm --output /tmp/wire-fixtures
python3 tools/tests/wire_finish.py --probe /tmp/wire-fixtures/finish-llvm
python3 tools/tests/wire_wsock.py \
  --echo /tmp/wire-fixtures/main-llvm \
  --client /tmp/wire-fixtures/client_probe-llvm \
  --idle-echo /tmp/wire-fixtures/idle_probe-llvm
python3 tools/tests/wire_autobahn.py \
  --echo /tmp/wire-fixtures/main-llvm --reports /tmp/wire-autobahn
```

`--backend cranelift` builds the other native engine's fixtures. Each test
script starts and stops only its own direct children. The loopback harness
requires ports 18089-18091 to be free. Its test-only accept4 interposer reduces
the accepted socket's send buffer to force real prefix sends and blocked output.
The protocol endpoint and normal provider retain their normal socket options.

The echo endpoint accepts text and binary at `/hirsel`. Its protocol-suite
configuration sets max_frame to 16 MiB and the reader reservation to 96 MiB.
The runner's wsock.defaults remains 256 KiB. The endpoint has a one-item notice
channel and joins its reader and close timer before accepting the next session.
It releases pending admission at protocol upgrade because it has no runner Hello.

Autobahn runs the pinned independent suite in Docker, sections 1-7 and 9, with
its own expected verdicts. It records both behavior and close verdicts and
splits the UTF-8 section into bounded invocations. `--resume` retains completed
sections when report cleanup or the runner is interrupted. Use a new report
directory after changing protocol behavior. Compression sections are excluded.
The proxy examples have their own real-container check in tools/tests/wire_proxy.py.
