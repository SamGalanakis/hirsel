# Hirsel cookbook

Seven complete, checked recipes bridging the [Primer](PRIMER.md) and the
[Specification](spec/index.md). Every Hirsel snippet below is checked against the
compiler. Always run `hirsel primer` and `hirsel inspect --std --interface` from your
installed binary as the exact authority for your installed version.

## 1. CLI configuration with `Args` and `Env`

`std.config` reads flags from `Args` and environment variables from an optional `Env`
grant in a single pass. Every key (`APP_PORT`) maps automatically to one CLI flag
(`--app-port` <!-- doc-ref: external -->), and `std.config.finish` reports all errors at once without leaking secret
values.

```hirsel
type Settings {
  host: String,
  port: u16,
  verbose: bool,
  inputs: List[String]
}

fn load_settings(env: Env, argv: Args) -> Result[Settings, String] {
  let mut r = std.config.new(Some(env), argv, 4)
  let host = std.config.string_or(mut r, "APP_HOST", "127.0.0.1")
  let port = std.config.port_or(mut r, "APP_PORT", 8080)
  let verbose = std.config.bool_or(mut r, "APP_VERBOSE", false)
  let inputs = std.config.positionals(mut r, 0, 8)
  match std.config.finish(take r) {
    Ok(()) => Ok(Settings(host: take host, port: port, verbose: verbose, inputs: take inputs)),
    Error(errors) => Error(std.config.format_errors(errors)),
  }
}

pub fn main(env: Env, argv: Args, out: Stdout) -> Result[(), String] waits {
  let cfg = load_settings(env, argv)?
  let mut line = string.new()
  string.push(mut line, cfg.host)
  string.push(mut line, ":")
  string.push_u64(mut line, int.widen(cfg.port))
  string.push(mut line, "\n")
  let _ = stdout.write(out, line)
  Ok(())
}
```

Run with explicit grants for `Env` and `Stdout`:

```sh
hirsel run --grant 'Env=APP_HOST,APP_PORT,APP_VERBOSE' --grant Stdout -- --app-port 9000 --app-verbose
```

## 2. Files, confined directories, and bounded reads

Every filesystem operation goes through a `Dir` capability granted with `--dir`. Use
`std.dir.narrow` to attenuate authority to a subdirectory before handing it to helper
code, and wrap untrusted reads in `bound bytes` + `isolate` so an oversized file raises a
catchable `Capacity` failure instead of exhausting process memory.

```hirsel
fn write_and_read(root: Dir) -> Result[String, std.file.Error] waits {
  let _ = std.dir.create_dir(root, "data")
  let sub = match std.dir.narrow(root, "data") {
    Ok(d) => d,
    Error(e) => return Error(Io(e)),
  }
  match std.file.write(sub, "greeting.txt", b"hello from hirsel\n") {
    Ok(()) => {},
    Error(e) => return Error(Io(e)),
  }
  std.file.read_string(sub, "greeting.txt")
}

fn bounded_copy(input: String, limit: u64) -> bool {
  match isolate(fn() -> Result[String, Capacity] {
    bound bytes limit { string.from(input) }
  }) {
    Ok(Ok(Ok(_))) => true,
    _ => false,
  }
}

@test
fn directory_round_trip(files: Dir) -> Result[(), ()] waits {
  let text = result.unwrap(write_and_read(files))
  if text != "hello from hirsel\n" {
    fail("unexpected file contents")
  }
  if !bounded_copy(text, 256) || bounded_copy(text, 8) {
    fail("unexpected byte bound verdict")
  }
  Ok(())
}
```

Add `dirs = ["."]` in `[tests]` in `hirsel.toml`, then run `hirsel test`.

## 3. HTTP server, parameterized router, and JSON responses

`std.router` matches literal paths and `{name}` captures without overlapping ambiguity.
Inside a handler, `std.http.capture(req, "id")` returns a scoped view tied to `req`, and
`std.json.write` renders JSON directly into an owned `String` buffer.

```hirsel
fn health(
  req: std.http.Request,
  scratch: Region,
  mut out: std.http.Responder,
  state: ()
) -> Result[std.http.Outcome, std.http.HandlerError] {
  Ok(Reply(std.http.response(200, b"{\"ok\":true}", true)?))
}

fn user_by_id(
  req: std.http.Request,
  scratch: Region,
  mut out: std.http.Responder,
  state: ()
) -> Result[std.http.Outcome, std.http.HandlerError] {
  let Some(id) = std.http.capture(req, "id") else {
    return Ok(Reply(std.http.response(400, b"missing id", true)?))
  }
  let mut json_text = string.new()
  std.json.write(mut json_text, false, fn(w: std.json.Writer) {
    std.json.object(w, 0, fn(w2: std.json.Writer, mut items: std.json.Items) {
      std.json.write_member(w2, mut items, "id")
      std.json.write_string_value(w2, id)
    })
  })
  let mut body: Bytes = list.new()
  list.append(mut body, string.as_bytes(json_text))
  Ok(Reply(std.http.response(200, take body, true)?))
}

type ServerJob: send {
  listener: Listener,
  clock: Clock,
  tasks: Tasks
}

fn serve_api(take job: ServerJob) waits {
  let ServerJob(listener: take listener, clock: c, tasks: tasks) = take job
  let mut listeners: List[Listener] = list.new()
  list.push(mut listeners, take listener)
  let mut builder: std.router.Builder[()] = std.router.builder()
  result.unwrap(std.router.add(mut builder, "GET", "/health", std.router.raw(health)))
  result.unwrap(std.router.add(mut builder, "GET", "/users/{id}", std.router.raw(user_by_id)))
  let routes = result.unwrap(std.router.build(take builder))
  let cfg = std.http.defaults()
  result.unwrap(std.http.serve(take listeners, c, tasks, take cfg, take routes, ()))
}

@test
fn routes_user_json(n: Net, c: Clock, tasks: Tasks) -> Result[(), ()] waits {
  let (take listener, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 8))
  let server = task.spawn(tasks, bytes 16777216, ServerJob(listener: take listener, clock: c, tasks: tasks), serve_api)
  let deadline = clock.deadline_after_ms(host.mono_ns(c), 5000)
  let req = std.http_client.Request(method: "GET", target: "/users/ada", headers: list.new(), body: Fixed(0))
  let mut client = result.unwrap(std.http_client.dial(n, addr, "127.0.0.1", c, req, deadline))
  result.unwrap(std.http_client.finish_request(mut client, 1024))
  if std.http_client.status(std.http_client.head(client)) != 200 {
    fail("expected 200")
  }
  let mut body: Bytes = list.with_capacity(32)
  while result.unwrap(std.http_client.read(mut client, mut body)) != 0 {}
  if body != b"{\"id\":\"ada\"}" {
    fail("unexpected JSON body")
  }
  std.http_client.close(take client)
  let _ = task.cancel_join(take server)
  Ok(())
}
```

Grant loopback listen/connect in `[tests]` in `hirsel.toml`:

```text
[tests]
limit = 67108864
grants = ["Clock", "Tasks", "Net=Listen 127.0.0.1/32 0-65535;Connect 127.0.0.1/32 1-65535"]
```

## 4. Store: typed table, secondary index, and transactions

Bind `store` in `hirsel.toml` with `store = { major = 1 }` (toolchain package). Declare
tables and secondary indexes inside a `store.declare` block, seal the schema, write rows
atomically inside `store.run`, and query immutable snapshots with `store.get` and
`store.find`.

```hirsel
type Account: send {
  id: u64,
  owner: String,
  tier: u64
}

fn account_put(a: Account, mut b: Bytes) -> Result[(), std.codec.EncodeError] {
  std.codec.put_u64(a.id, mut b)
  std.codec.put_u64(a.tier, mut b)
  std.codec.put_string(a.owner, mut b)
}

fn account_take(b: Bytes, mut at: u64) -> Result[Account, std.codec.DecodeError] {
  let id = std.codec.take_u64(b, mut at)?
  let tier = std.codec.take_u64(b, mut at)?
  let owner = std.codec.take_string(b, mut at)?
  Ok(Account(id: id, owner: take owner, tier: tier))
}

fn account_schema(mut s: std.codec.Schema, dict: std.codec.Codec[Account]) -> Result[(), std.codec.EncodeError] {
  std.codec.define(mut s, "cookbook.Account", 0)?
  if std.codec.record(mut s, 3)? {
    return Ok(())
  }
  std.codec.field(mut s, "id")?
  std.codec.schema_u64(mut s)
  std.codec.field(mut s, "tier")?
  std.codec.schema_u64(mut s)
  std.codec.field(mut s, "owner")?
  std.codec.schema_string(mut s)
  std.codec.close(mut s)
  Ok(())
}

fn account_codec() -> std.codec.Codec[Account] {
  std.codec.make(account_put, account_take, account_schema, 16)
}

fn account_tiers(key: u64, a: Account) -> Result[List[u64], Capacity] {
  let mut keys: List[u64] = list.new()
  list.push(mut keys, a.tier)
  Ok(take keys)
}

@test
fn store_table_and_index(clock: Clock, random: Random) -> Result[(), store.Error] waits {
  let opts = result.unwrap(store.sized(Memory, 67108864u64))
  let mut db = store.memory(clock, random, opts)

  let mut d = store.declare(db, Migrate)?
  let mut td = store.declare_table(mut d, "accounts", std.codec.key_u64(), account_codec())?
  let by_tier = store.index(mut d, mut td, "by_tier", Many, code.describe(account_tiers), std.codec.key_u64())?
  let accounts = store.finish(mut d, take td)?
  let _ = store.seal(mut db, take d)?

  let saved = store.run(mut db, None, store.copy_env(accounts), fn(mut tx, tbl) -> Result[(), store.Error] {
    store.put(mut tx, tbl, 1, Account(id: 1, owner: string.from("ada"), tier: 2))?
    store.put(mut tx, tbl, 2, Account(id: 2, owner: string.from("grace"), tier: 1))
  })?
  let Committed(_) = take saved else {
    fail("transaction did not commit")
  }

  let snap = store.snapshot(db)
  let r = store.reader(db, 0u64)?
  let Some(ada) = store.get(r, snap, accounts, 1)? else {
    fail("missing account 1")
  }
  if ada.owner != "ada" {
    fail("wrong owner")
  }
  let mut rows = store.find(r, snap, by_tier, Included(2), Included(2))?
  let Some((found_id, _)) = store.next(r, mut rows)? else {
    fail("missing indexed row for tier 2")
  }
  if found_id != 1 {
    fail("wrong indexed account")
  }
  store.close(take db)
}
```

Add `store = { major = 1 }` under `[dependencies]` and `grants = ["Clock", "Random"]`
under `[tests]` in `hirsel.toml`.

## 5. Task fan-out over channels and isolated units

Tasks within a unit share nothing except values moved through `task.spawn`, channels, and
`Arc`. For strict memory-budget isolation across units, use `task.spawn_unit` with `cross`
payloads and `Shared[T]` for immutable cross-unit sharing.

```hirsel
type Work: send {
  input: Receiver[u64],
  output: Sender[u64]
}

fn worker(take w: Work) -> u64 waits {
  let mut done: u64 = 0
  while let Ok(x) = channel.receive(w.input) {
    match channel.send(w.output, x * x) {
      Ok(()) => { done += 1 },
      Error(_) => break,
    }
  }
  done
}

fn sum_unit(take xs: List[u64]) -> u64 {
  let mut total: u64 = 0
  for x in xs { total += x }
  total
}

@test
fn fan_out_and_unit(tasks: Tasks) -> Result[(), ()] waits {
  let (tx_in, rx_in): (Sender[u64], Receiver[u64]) = channel.new(4)
  let (tx_out, rx_out): (Sender[u64], Receiver[u64]) = channel.new(4)
  let h = task.spawn(tasks, bytes 8192, Work(input: take rx_in, output: take tx_out), worker)

  for n in 1u64 .. 5 {
    result.unwrap(channel.send(tx_in, n))
  }
  drop(take tx_in)

  let mut squares: List[u64] = list.new()
  while let Ok(v) = channel.receive(rx_out) {
    list.push(mut squares, v)
  }
  if result.unwrap(task.join(take h)) != 4 {
    fail("expected 4 items")
  }

  let helper = result.unwrap(task.spawn_unit(tasks, bytes 8192, take squares, sum_unit))
  if result.unwrap(task.join(take helper)) != 30 {
    fail("expected 1 + 4 + 9 + 16 = 30")
  }
  Ok(())
}
```

## 6. Deterministic `sim` clock/network testing and property laws

Hirsel's virtual providers (`provide.net`, `std.sim.world`, `std.sim.net`) let tests
simulate fragmented packets, latency, and virtual time with zero host sockets and full
seed determinism. Pair them with `std.prop` laws to fuzz and shrink counterexamples.

```hirsel
type SimLink: send {
  port: Served[NetOp],
  clock: Clock,
  plan: std.sim.world.Plan
}

fn drive_clock(take w: std.sim.world.World) waits {
  let mut world = take w
  std.sim.world.run(mut world)
}

fn serve_net(take link: SimLink) waits {
  let SimLink(port: take port, clock: c, plan: take initial) = take link
  let mut plan = take initial
  let cfg = std.sim.net.Config(queue_bytes: 64, fragment_bytes: 4, latency_ns: 1_000_000)
  let net_sim = std.sim.net.new(cfg)
  let _ = std.sim.net.serve(net_sim, mut plan, c, take port)
}

fn send_all(mut wr: WriteHalf, data: Bytes) waits {
  let end = list.len(data)
  let mut at: u64 = 0
  while at < end {
    let n = result.unwrap(host.send_pair(mut wr, list.slice(data, at, end), b"", Never))
    if n == 0 {
      fail("peer closed during send")
    }
    at += n
  }
}

fn recv_exact(mut rd: ReadHalf, count: u64) -> Bytes waits {
  let mut buf: Bytes = list.with_capacity(count)
  while list.len(buf) < count {
    let need = count - list.len(buf)
    if result.unwrap(host.recv_bounded(mut rd, mut buf, need, Never)) == 0 {
      fail("unexpected EOF")
    }
  }
  take buf
}

fn echo_once(take l_in: Listener) waits {
  let mut listener = take l_in
  let (stream, _) = result.unwrap(host.accept(mut listener, Never))
  let (rd_in, wr_in) = net.split(take stream)
  let mut rd = take rd_in
  let mut wr = take wr_in
  let msg = recv_exact(mut rd, 5)
  send_all(mut wr, msg)
}

@test
fn simulated_network_round_trip(tasks: Tasks) -> Result[(), ()] waits {
  let (w_in, c) = std.sim.world.new(42, 0, 0)
  let mut world = take w_in
  let plan = std.sim.world.detach(mut world)
  let (n, port) = provide.net(bytes 16384)

  let clock_task = task.spawn(tasks, bytes 65536, take world, drive_clock)
  let net_task = task.spawn(tasks, bytes 262144, SimLink(port: take port, clock: c, plan: take plan), serve_net)

  let (listener, addr) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433), port: 0), 4))
  let peer = task.spawn(tasks, bytes 65536, take listener, echo_once)

  let client = result.unwrap(host.connect(n, addr, Never))
  let (rd_in, wr_in) = net.split(take client)
  let mut rd = take rd_in
  let mut wr = take wr_in
  send_all(mut wr, b"hello")

  let reply = recv_exact(mut rd, 5)
  if reply != b"hello" {
    fail("unexpected echo reply")
  }
  let _ = task.join(take peer)
  let _ = task.cancel_join(take net_task)
  let _ = task.cancel_join(take clock_task)
  Ok(())
}

fn show_pair(p: (u64, u64), mut out: String) {
  string.push_u64(mut out, p.0)
  string.push(mut out, ",")
  string.push_u64(mut out, p.1)
}

@test
fn law_addition_commutes() -> Result[(), ()] {
  let gen = std.prop.make(fn(mut s: std.prop.Source) -> (u64, u64) {
    (std.prop.below(mut s, 1000), std.prop.below(mut s, 1000))
  }, show_pair)
  let outcome = std.prop.search(std.prop.config(), gen, fn(p: (u64, u64)) -> Result[(), String] {
    if p.0 + p.1 == p.1 + p.0 {
      Ok(())
    } else {
      Error(string.from("addition did not commute"))
    }
  })
  match outcome {
    Passed(_) => Ok(()),
    _ => Error(()),
  }
}
```

Run across seeds or check law strength:

```sh
hirsel test --sim --seeds 10
hirsel test --laws
```

## 7. High-throughput loops: `Inline[T, N]`, `Simd[T, N]`, `bytes[T]`, and `bound steps`

Use `Inline[T, N]` for zero-allocation fixed tables, `Simd[u8, 16]` for portable 16-byte
vector scans (`list.lanes`, `simd.eq`, `simd.bitmask`), `bytes[T]` + `charge.seq` to derive
exact logical byte budgets from types rather than magic numbers, and `bound steps` to cap
worst-case CPU work on untrusted inputs.

```hirsel
const DELIMS: Inline[u8, 16] = [
  b',', b',', b',', b',', b',', b',', b',', b',',
  b',', b',', b',', b',', b',', b',', b',', b','
]

fn count_commas(buf: Bytes) -> u64 {
  let target: Simd[u8, 16] = simd.from(DELIMS)
  let n = list.len(buf)
  let mut i: u64 = 0
  let mut hits: u64 = 0
  while i + 16 <= n {
    let chunk: Simd[u8, 16] = list.lanes(buf, i)
    let mut mask = simd.bitmask(simd.eq(chunk, target))
    while mask != 0 {
      hits += 1
      mask = mask & (mask - 1)
    }
    i += 16
  }
  while i < n {
    if buf[i] == b',' {
      hits += 1
    }
    i += 1
  }
  hits
}

fn buffer_budget(capacity: u64) -> Option[u64] {
  charge.seq(bytes[u8], capacity)
}

fn bounded_count(buf: Bytes, max_steps: u64) -> Result[u64, OutOfTime] {
  bound steps max_steps { count_commas(buf) }
}

@test
fn simd_and_step_budget() -> Result[(), ()] {
  let sample = b"a,b,c,d,e,f,g,h,i,j,k,l,m,n,o,p,q,r"
  if count_commas(sample) != 17 {
    fail("wrong comma count")
  }
  if buffer_budget(64) != Some(64) {
    fail("unexpected seq charge")
  }
  match bounded_count(sample, 1000) {
    Ok(17) => {},
    _ => fail("expected bounded count to finish"),
  }
  match bounded_count(sample, 2) {
    Error(OutOfTime(steps: _)) => Ok(()),
    _ => fail("expected OutOfTime under 2 steps"),
  }
}
```

### Find the hot spot with `hirsel prof`

Profile before tuning, and never tune from `objdump` output. `prof run` builds the package,
runs it with the same grants, flags and `--` arguments that `hirsel run` takes, and samples
it into a `.hprof` file:

```sh
hirsel prof run . --release --grant Stdout --pmu cycles,branch-misses --out run1.hprof -- ARGS
hirsel prof top run1.hprof --limit 10            # hottest functions, per event
hirsel prof annotate run1.hprof main.hirsel:42   # periods and compiler remarks for one line
hirsel prof callers run1.hprof app@1/main/work   # who calls one definition (package@major/module/fn)
hirsel prof diff run1.hprof run2.hprof           # what a change moved
```

`top` names each row by its definition path, or by `file:line` for inlined code, and ranks by
`self_total`, the sampled event periods. Read `verdict` and `warnings` first: `degraded` means
samples were lost or stacks failed to unwind, so trust the ranking but not small differences.
`--out` refuses an existing file, so give each run a new name.
