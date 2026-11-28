# WebSocket protocol fixtures

The fixtures are the `core/programs` workspace member `wire` (`hirsel.hcfg`).
`wire_build.py` builds its entries with `hirsel build core/programs/wire
--libraries core` from the current checkout's core/std sources and the existing
compiler and runtime Buck targets, adding each entry's grants and a 256 MiB
logical root within the 2 GiB process ceiling. `//:core-wire-fixtures` builds
the echo, finish, session and migration fixtures and runs the checks below.
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
script starts and stops only its own direct children. The loopback fixtures
use kernel-assigned ports and report their bound port to the checks. The echo
check runs two copies together and checks which process owns each connection.
Its test-only accept4 interposer reduces
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
That check and the TLS ingress check use private Docker network namespaces.
They use `sudo -n nsenter` to run the local peers in those namespaces.

W3b's dormant runner session has a service fixture in `session.hirsel`. Build it
with the same compiler, runtime and linker, using `wire_build.py --session-only`
and either native backend. It listens on loopback under a 64 MiB
logical root. Run `wire_wsock.py --session <output>/session-<backend>` for
concurrent upgrades, strict Frame decoding, blocked output and bounded Close.
Add --idle-session <output>/session-idle-<backend> to check idle Ping and Close;
--session-codes-only selects just the text and idle Close-code cases.
Its test service returns deterministic Batches and Resets and refuses Hello;
the production service is exercised by the W3c fixture below.


W3c's `admission.hirsel` starts the shared ingress policy and real store head.
Build with `wire_build.py --admission-only` and run `wire_admission.py --server
<output>/admission-<backend>`. It uses a 1.5 GiB logical root
inside the unchanged 2 GiB process ceiling, covering the 32-session admission
law's reservations. The hand-written codec derives identities from PROG-9 and
fingerprints from SEM-10 schema bytes; store versions and reply sizes are fixed
by the spec. `--case` selects individual laws. The shared runner harness's
`--part admission` checks the exact outcome reserve and Reset retention on all
engines. `//:core-runner-r1` runs those state laws.
