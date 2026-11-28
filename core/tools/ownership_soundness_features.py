#!/usr/bin/env python3
"""Six-feature expansion for the Hirsel ownership soundness generator.

Statement builders for `Code[F]` handles, `Held` custody, `std.data.decode`, `bound steps` /
`bound bytes`, `isolate` and checked arithmetic and indexing. Each statement carries the value
the language rules require (SEM-1, MEM-25, MEM-30, FAIL-8, FAIL-16, PACK-14, PACK-16), computed
here from the generated inputs, and reports a disagreement on its own output line. The
three-engine comparison therefore also has an oracle none of the engines produced.
"""
from string import Template

FEATURE_SURFACES = ('code', 'held', 'data_decode', 'bound', 'isolate', 'arith')
MASK = (1 << 64) - 1
PRIME = 4294967291
FAIL_BASE = 9000000000
ENTRY_REFUSED = 9500000000
NONE_VALUE = 77777
CAUSE = {'Overflow': 1, 'Bounds': 2, 'Division': 4, 'Unwrap': 8, 'Fail': 16, 'Capacity': 64}
HUGE_BYTES = 1 << 40

FEATURE_PRELUDE = '''
resource type Logged { log: Sender[u64], tag: u64 } drop(t) { let _ = channel.try_send(t.log, t.tag) }
type CodeBox { f: Code[fn(u64) -> u64], bias: u64 }
type Rec: key { a: u64, b: u32, c: u8 }
type Deep: key { name: String, xs: List[u64], r: Rec }

fn expect(out: Stdout, tag: u64, got: u64, want: u64) -> u64 waits {
  if got != want {
    let mut line = string.new()
    string.push(mut line, "oracle ")
    string.push_u64(mut line, tag)
    string.push(mut line, " ")
    string.push_u64(mut line, got)
    string.push(mut line, " ")
    string.push_u64(mut line, want)
    string.push(mut line, "\\n")
    let _ = stdout.write(out, line)
  }
  got
}

fn drain_log(rx: Receiver[u64]) -> u64 {
  let mut order: u64 = 0u64
  while let Ok(v) = channel.try_receive(rx) { order = order * 10u64 + v }
  order
}

fn fold_iso(r: Result[Result[u64, Causes], Capacity]) -> u64 {
  match r { Ok(Ok(v)) => v % 4294967291u64, Ok(Error(cs)) => 9000000000u64 + cause_mask(cs), Error(_) => 9500000000u64 }
}

fn spin(n: u64) -> u64 {
  let mut i: u64 = 0u64
  let mut s: u64 = 0u64
  while i < n { s = s + i; i = i + 1u64 }
  s
}

fn mk_deep(n: u64) -> Deep { Deep(name: string.from("deep"), xs: mk_list(n, n), r: Rec(a: n, b: 7u32, c: 1u8)) }

fn code_add(x: u64) -> u64 { x + 5u64 }
fn code_scale(x: u64) -> u64 { x * 3u64 + 1u64 }
fn code_boom(x: u64) -> u64 { if x % 2u64 == 0u64 { fail("code_boom") }; x + 9u64 }
fn code_take_log(take l: Logged, x: u64) -> u64 { l.tag * 100u64 + x }
fn code_push(mut xs: List[u64], x: u64) -> u64 { list.push(mut xs, x); list.len(xs) }
fn code_id[T](take x: T) -> T { x }
fn code_apply(c: Code[fn(u64) -> u64], x: u64) -> u64 { c(c(x)) }

fn code_slot_probe(lo: Option[Loader], c: Code[fn(u64) -> u64], x: u64) -> u64 {
  match lo {
    Some(l) => {
      let mut s: CodeSlot[fn(u64) -> u64] = code.slot(l)
      let d0 = match code.dispatch(s, x) { Ok(v) => v, Error(Empty()) => 1000u64 }
      let a = match code.activate(mut s, 0u64, code.share(c)) {
        Ok(v) => v, Error(WrongPool) => 2000u64, Error(Stale(n)) => 3000u64 + n, Error(Capacity) => 4000u64,
      }
      let d1 = match code.dispatch(s, x) { Ok(v) => v, Error(Empty()) => 1000u64 }
      d0 + a + d1
    },
    None => c(x) + 7u64,
  }
}

fn code_worker(take w: (Sender[u64], Code[fn(u64) -> u64], u64)) -> u64 waits {
  let (tx, f, x) = take w
  let _ = channel.send(tx, f(x))
  f(x + 1u64)
}

fn held_code(take h: Held) -> u64 {
  match (code.take_as(take h) : Result[u64, (code.TakeError, Held)]) {
    Ok(v) => v,
    Error((_, take h1)) => match (code.take_as(take h1) : Result[String, (code.TakeError, Held)]) {
      Ok(take s) => 1000u64 + string.byte_len(s),
      Error((_, take h2)) => match (code.take_as(take h2) : Result[Logged, (code.TakeError, Held)]) {
        Ok(take l) => 2000u64 + l.tag,
        Error((_, take h3)) => match (code.take_as(take h3) : Result[Held, (code.TakeError, Held)]) {
          Ok(take inner) => 3000u64 + held_code(take inner),
          Error((_, take _d)) => 9000u64,
        },
      },
    },
  }
}

fn held_send_worker(take w: (Sender[Held[send]], Held[send])) -> u64 waits {
  let (tx, h) = take w
  match (code.take_as(take h) : Result[String, (code.TakeError, Held[send])]) {
    Ok(take s) => {
      let mut grown = take s
      string.push(mut grown, "!")
      let back: Held[send] = code.hold(take grown)
      let _ = channel.send(tx, take back)
      1u64
    },
    Error((_, take other)) => { let _ = channel.send(tx, take other); 2u64 },
  }
}

fn iso_worker(take w: (Sender[u64], u64, u64)) -> u64 waits {
  let (tx, kind, seed) = take w
  let xs = mk_list(seed, 3u64)
  let r = isolate(fn() -> u64 {
    let tr = result.unwrap(Track(id: seed, note: string.from("iso")))
    trigger_failure(kind, xs) + tr.id
  })
  let _ = channel.send(tx, list.sum(xs))
  fold_iso(r)
}

fn sum_worker(take w: (u64, u64)) -> u64 { list.sum(mk_list(w.0, w.1)) }

fn bits_u8(v: u8) -> u64 { int.widen(v) }
fn bits_u16(v: u16) -> u64 { int.widen(v) }
fn bits_u32(v: u32) -> u64 { int.widen(v) }
fn bits_u64(v: u64) -> u64 { v }
fn bits_i8(v: i8) -> u64 { let w: i64 = int.widen(v); int.truncate(w) }
fn bits_i16(v: i16) -> u64 { let w: i64 = int.widen(v); int.truncate(w) }
fn bits_i32(v: i32) -> u64 { let w: i64 = int.widen(v); int.truncate(w) }
fn bits_i64(v: i64) -> u64 { int.truncate(v) }
'''

DATA_PRELUDE = '''
type CfgMode: codec = Fast | Slow(u64) | Tuned(level: u64, bias: u8)
type CfgDoc: codec {
  name: String,
  count: u64,
  small: u8,
  delta: i32,
  on: bool,
  flags: List[u64],
  labels: Map[String, u8],
  mode: CfgMode,
  pair: (u64, u8),
  opt: Option[u64],
  retries: u64 = 3,
}

fn cfg_limits(knob: u64, value: u64) -> std.data.Limits {
  let mut l = std.data.Limits(input_bytes: 1048576, depth: 64, nodes: 131072, collection: 65536,
    decoded_bytes: 4194304, comparison_bytes: 16777216, diagnostics: 1)
  if knob == 1u64 { l.input_bytes = value }
  if knob == 2u64 { l.depth = value }
  if knob == 3u64 { l.nodes = value }
  if knob == 4u64 { l.collection = value }
  if knob == 5u64 { l.decoded_bytes = value }
  if knob == 6u64 { l.comparison_bytes = value }
  if knob == 7u64 { l.diagnostics = value }
  l
}

fn cfg_reason(r: std.data.Reason) -> u64 {
  match r {
    Invalid(why) => {
      if why == "unknown field" { 101u64 } else if why == "duplicate field" { 102u64 }
      else if why == "missing required field" { 103u64 } else if why == "wrong nominal root" { 104u64 }
      else if why == "data syntax" { 105u64 } else if why == "expected scalar literal" { 106u64 }
      else if why == "duplicate map key" { 107u64 } else if why == "unexpected data case" { 108u64 }
      else if why == "wrong positional arity" { 109u64 } else if why == "expected bare case" { 110u64 }
      else if why == "expected an option constructor" { 111u64 } else if why == "expected a labelled field" { 112u64 }
      else { 199u64 }
    },
    Limit(label) => {
      if label == "input bytes" { 201u64 } else if label == "depth" { 202u64 } else if label == "nodes" { 203u64 }
      else if label == "collection length" { 204u64 } else if label == "decoded bytes" { 205u64 }
      else if label == "key comparisons" { 206u64 } else if label == "diagnostics" { 207u64 }
      else { 299u64 }
    },
  }
}

fn cfg_digest(d: CfgDoc) -> u64 {
  let mut h: u64 = string.byte_len(d.name) + d.count + int.widen(d.small) + d.retries
  if d.on { h = h + 1000u64 }
  if d.delta < 0i32 { h = h + 500u64 }
  for f in d.flags { h = h + f }
  for (key, value) in d.labels { h = h + string.byte_len(key) * 10u64 + int.widen(value) }
  h = h + match d.mode { Fast => 1u64, Slow(n) => 2u64 + n, Tuned(level: l, bias: b) => 3u64 + l + int.widen(b) }
  h = h + d.pair.0 + int.widen(d.pair.1)
  h + match d.opt { Some(n) => n, None => 7u64 }
}

fn cfg_decode(input: Bytes, l: std.data.Limits) -> (u64, u64) {
  let w: std.data.Witness[CfgDoc] = std.data.witness("cfg")
  match std.data.decode(w, input, l) {
    Ok(d) => (cfg_digest(d) + 1000000u64, 0u64),
    Error(e) => (cfg_reason(e.reason), list.len(e.path) * 1000000u64 + e.primary.start * 1000u64 + e.primary.end),
  }
}

fn cfg_held(input: Bytes) -> u64 {
  let w: std.data.Witness[CfgDoc] = std.data.witness("cfg")
  match std.data.decode(w, input, cfg_limits(0u64, 0u64)) {
    Ok(take d) => {
      let h: Held = code.hold(take d)
      match (code.take_as(take h) : Result[u64, (code.TakeError, Held)]) {
        Ok(v) => v,
        Error((_, take back)) => match (code.take_as(take back) : Result[CfgDoc, (code.TakeError, Held)]) {
          Ok(take doc) => cfg_digest(doc) + 1000000u64,
          Error((_, take _d)) => 0u64,
        },
      }
    },
    Error(e) => cfg_reason(e.reason),
  }
}
'''


def sub(template: str, /, **values) -> str:
    return Template(template).substitute({key: str(value) for key, value in values.items()})


def mk_list(seed: int, n: int) -> list[int]:
    return [(seed + i * 3) % 97 for i in range(n)]


def spin(n: int) -> int:
    return n * (n - 1) // 2


def failure_value(kind: int, xs_len: int) -> tuple[bool, int]:
    """Model of the prelude's `trigger_failure`: (failed, cause mask or value)."""
    masks = {1: CAUSE['Overflow'], 2: CAUSE['Bounds'], 3: CAUSE['Division'], 4: CAUSE['Unwrap'], 5: CAUSE['Fail']}
    return (True, masks[kind]) if kind in masks else (False, xs_len + 3)


def log_order(tags) -> int:
    order = 0
    for tag in tags:
        order = order * 10 + tag
    return order


CHECK = 'acc = mix(acc, expect(out, ${tag}u64, ${got}, ${want}u64))'


def check(k: int, j: int, got: str, want: int) -> str:
    return sub(CHECK, tag=k * 10 + j, got=got, want=want)


# --- Code[F] / code.bind / code.dispatch ---

CODE_FNS = {'code_add': lambda x: x + 5, 'code_scale': lambda x: x * 3 + 1}


def code_stmt(stream, k: int) -> str:
    choice = stream.below(7)
    name = stream.pick(sorted(CODE_FNS))
    f = CODE_FNS[name]
    a = 1 + stream.below(50)
    b = 1 + stream.below(50)
    if choice == 0:
        return sub(
            'let cc${k}: Code[fn(u64) -> u64] = code.bind(${name})\n'
            'let cd${k} = code.share(cc${k})\n'
            'acc = mix(acc, list.len(code.identity(cc${k})) + match code.revision(cd${k}) { Some(r) => list.len(r), None => 0u64 })\n',
            k=k, name=name) + check(k, 0, f'cc{k}({a}u64) + cd{k}({b}u64)', f(a) + f(b))
    if choice == 1:
        return sub('let cb${k} = CodeBox(f: code.bind(${name}), bias: ${a}u64)\n', k=k, name=name, a=a) + check(
            k, 0, f'code_apply(cb{k}.f, cb{k}.bias)', f(f(a)))
    if choice == 2:
        want = FAIL_BASE + CAUSE['Fail'] if b % 2 == 0 else 100 + a + b + 9 + 2
        return sub(
            'let (ctx${k}, crx${k}): (Sender[u64], Receiver[u64]) = channel.new(8u64)\n'
            'let cbm${k}: Code[fn(u64) -> u64] = code.bind(code_boom)\n'
            'let ctl${k}: Code[fn(take Logged, u64) -> u64] = code.bind(code_take_log)\n'
            'let ci${k} = isolate(fn() -> u64 {\n'
            '  let la${k} = result.unwrap(Logged(log: channel.sender(ctx${k}), tag: 1u64))\n'
            '  let lb${k} = result.unwrap(Logged(log: channel.sender(ctx${k}), tag: 2u64))\n'
            '  let first${k} = ctl${k}(take la${k}, ${a}u64)\n'
            '  first${k} + cbm${k}(${b}u64) + lb${k}.tag\n})\n',
            k=k, a=a, b=b) + check(k, 0, f'fold_iso(ci{k})', want) + '\n' + check(k, 1, f'drain_log(crx{k})', 12)
    if choice == 3:
        return sub(
            'let ch${k}: Held = code.hold(CodeBox(f: code.bind(${name}), bias: ${a}u64))\n'
            'let cv${k} = match (code.take_as(take ch${k}) : Result[u64, (code.TakeError, Held)]) {\n'
            '  Ok(v) => v + 100000u64,\n'
            '  Error((_, take back${k})) => match (code.take_as(take back${k}) : Result[CodeBox, (code.TakeError, Held)]) {\n'
            '    Ok(take box${k}) => box${k}.f(box${k}.bias) + ${b}u64,\n'
            '    Error((_, take _d${k})) => 200000u64,\n  },\n}\n',
            k=k, name=name, a=a, b=b) + check(k, 0, f'cv{k}', f(a) + b)
    if choice == 4:
        return sub(
            'let (cwt${k}, cwr${k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
            'let cwc${k}: Code[fn(u64) -> u64] = code.bind(${name})\n'
            'let cwh${k} = task.spawn(tasks, bytes 65536, (take cwt${k}, code.share(cwc${k}), ${a}u64), code_worker)\n'
            'let cwm${k} = match channel.receive(cwr${k}) { Ok(x) => x, Error(_) => 0u64 }\n'
            'let cwj${k}: u64 = match task.join(take cwh${k}) { Ok(x) => x, Error(_) => 0u64 }\n',
            k=k, name=name, a=a) + check(k, 0, f'cwm{k} + cwj{k} + cwc{k}({b}u64)', f(a) + f(a + 1) + f(b))
    if choice == 5:
        return sub(
            'let cs${k}: Code[fn(u64) -> u64] = code.bind(${name})\n'
            'let cn${k}: Option[Loader] = None\n', k=k, name=name) + check(
                k, 0, f'code_slot_probe(cn{k}, cs{k}, {a}u64)', f(a) + 7)
    n = 1 + stream.below(4)
    xs = mk_list(a, n) + [b, b + 1]
    return sub(
        'let cp${k}: Code[fn(mut List[u64], u64) -> u64] = code.bind(code_push)\n'
        'let cg${k}: Code[fn(take u64) -> u64] = code.bind(code_id)\n'
        'let mut cx${k} = mk_list(${a}u64, ${n}u64)\n'
        'let cl${k} = cp${k}(mut cx${k}, ${b}u64) + cp${k}(mut cx${k}, cg${k}(${b1}u64))\n',
        k=k, a=a, b=b, b1=b + 1, n=n) + check(k, 0, f'cl{k} + list.sum(cx{k})', (n + 1) + (n + 2) + sum(xs))


# --- Held: hold / take_as / cleanup ordering ---

def held_payload(stream, tag: int, sender: str) -> tuple[str, int, list[int]]:
    """A payload expression for `code.hold`, its `held_code` value and the tags its drop logs."""
    kind = stream.below(5)
    v = stream.below(900)
    if kind == 0:
        return f'{v}u64', v, []
    if kind == 1:
        text = 'abcdefgh'[:1 + stream.below(8)]
        return f'string.from("{text}")', 1000 + len(text), []
    if kind == 2:
        return f'result.unwrap(Logged(log: channel.sender({sender}), tag: {tag}u64))', 2000 + tag, [tag]
    if kind == 3:
        return f'{{ let inner: Held = code.hold({v}u64); inner }}', 3000 + v, []
    return f'{v % 200}u8', 9000, []


def held_stmt(stream, k: int) -> str:
    choice = stream.below(6)
    v = 1 + stream.below(40)
    open_log = f'let (htx{k}, hrx{k}): (Sender[u64], Receiver[u64]) = channel.new(16u64)\n'
    if choice == 0:
        # An untaken Held is one owner among the locals: cleanup runs in reverse declaration order.
        slot = stream.below(3)
        lines = []
        for i in range(3):
            logged = f'result.unwrap(Logged(log: channel.sender(htx{k}), tag: {i + 1}u64))'
            lines.append(f'  let hl{i}_{k}: Held = code.hold({logged})' if i == slot else f'  let hl{i}_{k} = {logged}')
        return open_log + '{\n' + '\n'.join(lines) + '\n}\n' + check(k, 0, f'drain_log(hrx{k})', 321)
    if choice == 1:
        payload, value, tags = held_payload(stream, 4, f'htx{k}')
        return (open_log + f'let hw{k}: Held = code.hold({payload})\n' + check(k, 0, f'held_code(take hw{k})', value)
                + '\n' + check(k, 1, f'drain_log(hrx{k})', log_order(tags)))
    if choice == 2:
        count = 2 + stream.below(4)
        pushes, values, tags = [], [], []
        for i in range(count):
            payload, value, logged = held_payload(stream, i + 1, f'htx{k}')
            pushes.append(f'list.push(mut hs{k}, code.hold({payload}))')
            values.append(value)
            tags.append(logged)
        total = 0
        order = []
        for value, logged in zip(reversed(values), reversed(tags)):
            total = total * 31 + value
            order += logged
        return (open_log + f'let mut hs{k}: List[Held] = list.new()\n' + '\n'.join(pushes) + '\n'
                + f'let mut ht{k}: u64 = 0u64\n'
                + f'while let Some(take hh{k}) = list.pop(mut hs{k}) {{ ht{k} = ht{k} * 31u64 + held_code(take hh{k}) }}\n'
                + check(k, 0, f'ht{k}', total) + '\n' + check(k, 1, f'drain_log(hrx{k})', log_order(order)))
    if choice == 3:
        text = 'abcdefghij'[:1 + stream.below(10)]
        return sub(
            'let (hst${k}, hsr${k}): (Sender[Held[send]], Receiver[Held[send]]) = channel.new(2u64)\n'
            'let hsv${k}: Held[send] = code.hold(string.from("${text}"))\n'
            'let hsh${k} = task.spawn(tasks, bytes 65536, (take hst${k}, take hsv${k}), held_send_worker)\n'
            'let hsg${k} = match channel.receive(hsr${k}) {\n'
            '  Ok(take got${k}) => match (code.take_as(take got${k}) : Result[String, (code.TakeError, Held[send])]) {\n'
            '    Ok(take s${k}) => string.byte_len(s${k}),\n'
            '    Error((_, take _d${k})) => 900u64,\n  },\n'
            '  Error(_) => 800u64,\n}\n'
            'let hsj${k}: u64 = match task.join(take hsh${k}) { Ok(x) => x, Error(_) => 700u64 }\n',
            k=k, text=text) + check(k, 0, f'hsg{k} * 10u64 + hsj{k}', (len(text) + 1) * 10 + 1)
    if choice == 4:
        kind = stream.below(6)
        failed, value = failure_value(kind, 3)
        want = FAIL_BASE + value if failed else value + 1 + 3
        return (open_log + sub(
            'let hu${k} = isolate(fn() -> u64 {\n'
            '  let ha${k} = result.unwrap(Logged(log: channel.sender(htx${k}), tag: 1u64))\n'
            '  let hb${k}: Held = code.hold(result.unwrap(Logged(log: channel.sender(htx${k}), tag: 2u64)))\n'
            '  let hp${k}: Held = code.hold(mk_pair(${v}u64))\n'
            '  let hc${k} = result.unwrap(Logged(log: channel.sender(htx${k}), tag: 3u64))\n'
            '  trigger_failure(${kind}u64, mk_list(${v}u64, 3u64)) + ha${k}.tag + hc${k}.tag\n})\n',
            k=k, v=v, kind=kind) + check(k, 0, f'fold_iso(hu{k})', want) + '\n' + check(k, 1, f'drain_log(hrx{k})', 321))
    misses = 1 + stream.below(3)
    n = 1 + stream.below(5)
    return sub(
        'let mut hm${k}: Held = code.hold(mk_list(${v}u64, ${n}u64))\n'
        'let mut hi${k}: u64 = 0u64\n'
        'while hi${k} < ${misses}u64 {\n'
        '  let hn${k}: Held = match (code.take_as(take hm${k}) : Result[u64, (code.TakeError, Held)]) {\n'
        '    Ok(_) => code.hold(0u64),\n'
        '    Error((_, take back${k})) => back${k},\n  }\n'
        '  hm${k} = take hn${k}\n'
        '  hi${k} = hi${k} + 1u64\n}\n'
        'let hv${k} = match (code.take_as(take hm${k}) : Result[List[u64], (code.TakeError, Held)]) {\n'
        '  Ok(take xs${k}) => list.sum(xs${k}) + list.len(xs${k}),\n'
        '  Error((_, take _d${k})) => 900000u64,\n}\n',
        k=k, v=v, n=n, misses=misses) + check(k, 0, f'hv{k}', sum(mk_list(v, n)) + n)


# --- bound steps / bound bytes ---

STEP_OUT = 'match ${value} { Ok(v) => v, Error(t) => 7000u64 + t.steps }'
BYTE_OUT = 'match ${value} { Ok(v) => v, Error(_) => 8000u64 }'


def bound_stmt(stream, k: int) -> str:
    choice = stream.below(8)
    v = 1 + stream.below(40)
    if choice == 0:
        # Inner exhaustion, outer continuation.
        inner = 5 + stream.below(36)
        tail = 2 + stream.below(18)
        return sub(
            'let bo${k} = bound steps 100000 {\n'
            '  let bi${k} = bound steps ${inner} { spin(${long}u64) }\n'
            '  let bt${k} = spin(${tail}u64)\n'
            '  match bi${k} { Ok(v) => v, Error(t) => 7000u64 + t.steps } + bt${k}\n}\n',
            k=k, inner=inner, long=1000 + v, tail=tail) + check(
                k, 0, sub(STEP_OUT, value=f'bo{k}'), 7000 + inner + spin(tail))
    if choice == 1:
        # The outer bound has too little left to reserve the inner one: the inner body never runs.
        outer = 40 + stream.below(40)
        return sub(
            'let bo${k} = bound steps ${outer} {\n'
            '  let bi${k} = bound steps 50000 { spin(3u64) }\n'
            '  match bi${k} { Ok(v) => v, Error(t) => 7000u64 + t.steps } + spin(4u64)\n}\n',
            k=k, outer=outer) + check(k, 0, sub(STEP_OUT, value=f'bo{k}'), 7000 + 50000 + spin(4))
    if choice == 2:
        # Repeated array initialization pays the MEM-30 copy row before its first store.
        if stream.below(4) == 0:
            return sub('let br${k} = bound steps 0 { let ba${k} = [(); ${n}]; ba${k}[${last}]; ${v}u64 }\n',
                       k=k, n=4096 * v, last=4096 * v - 1, v=v) + check(k, 0, sub(STEP_OUT, value=f'br{k}'), v)
        pair = stream.below(2) == 1
        size = 9 if pair else 8
        count = stream.pick((1, 455, 456, 511, 512, 513, 1024, 1025, 4096, 8191))
        need = -(-count * size // 4096)
        budget = need + stream.pick((-1, 0, 3))
        element = f'({v}u64, 7u8)' if pair else f'{v}u64'
        read = f'ba{k}[0].0 + ba{k}[{count - 1}].0' if pair else f'ba{k}[0] + ba{k}[{count - 1}]'
        return sub('let br${k} = bound steps ${budget} { let ba${k} = [${element}; ${count}]; ${read} }\n',
                   k=k, budget=budget, element=element, count=count, read=read) + check(
                       k, 0, sub(STEP_OUT, value=f'br{k}'), 2 * v if budget >= need else 7000 + budget)
    if choice == 3:
        # Structural equality and clone take work steps: a zero bound stops the clone.
        if stream.below(3) == 0:
            return sub(
                'let bd${k} = mk_deep(${v}u64)\n'
                'let bc${k} = bound steps 0 { let be${k}: Deep = clone(bd${k}); list.len(be${k}.xs) }\n', k=k, v=v) + check(
                    k, 0, sub(STEP_OUT, value=f'bc{k}'), 7000)
        return sub(
            'let bd${k} = mk_deep(${v}u64)\n'
            'let bc${k} = bound steps 20000 {\n'
            '  let be${k}: Deep = clone(bd${k})\n'
            '  let same${k} = if bd${k} == be${k} { 1u64 } else { 0u64 }\n'
            '  let mut bm${k} = be${k}\n'
            '  list.push(mut bm${k}.xs, 1u64)\n'
            '  same${k} * 100u64 + if bd${k} == bm${k} { 10u64 } else { 0u64 } + list.len(bm${k}.xs) - list.len(bd${k}.xs)\n}\n',
            k=k, v=v) + check(k, 0, sub(STEP_OUT, value=f'bc{k}'), 101)
    if choice == 4:
        # bound bytes: a refused entry is an Error, a body that runs out is the failure Capacity.
        mode = stream.below(4)
        n = 2 + stream.below(12)
        if mode == 0:
            return sub('let bb${k} = bound bytes ${huge} { ${v}u64 }\n', k=k, huge=HUGE_BYTES, v=v) + check(
                k, 0, sub(BYTE_OUT, value=f'bb{k}'), 8000)
        if mode == 1:
            return sub(
                'let bb${k} = bound bytes 65536 {\n'
                '  let bn${k} = bound bytes ${huge} { 1u64 }\n'
                '  let bx${k} = mk_list(${v}u64, ${n}u64)\n'
                '  match bn${k} { Ok(v) => v, Error(_) => 8000u64 } + list.sum(bx${k})\n}\n',
                k=k, huge=HUGE_BYTES, v=v, n=n) + check(k, 0, sub(BYTE_OUT, value=f'bb{k}'), 8000 + sum(mk_list(v, n)))
        if mode == 2:
            return sub(
                'let bb${k} = bound bytes 262144 {\n'
                '  let bi${k} = isolate(fn() -> u64 {\n'
                '    let bn${k} = bound bytes 128 { let big${k} = mk_list(${v}u64, 400u64); list.sum(big${k}) }\n'
                '    match bn${k} { Ok(v) => v, Error(_) => 8000u64 }\n  })\n'
                '  let bx${k} = mk_list(${v}u64, ${n}u64)\n'
                '  fold_iso(bi${k}) + list.sum(bx${k})\n}\n',
                k=k, v=v, n=n) + check(
                    k, 0, sub(BYTE_OUT, value=f'bb{k}'), FAIL_BASE + CAUSE['Capacity'] + sum(mk_list(v, n)))
        return sub('let bb${k} = bound bytes 65536 { let bx${k} = mk_list(${v}u64, ${n}u64); list.sum(bx${k}) }\n',
                   k=k, v=v, n=n) + check(k, 0, sub(BYTE_OUT, value=f'bb{k}'), sum(mk_list(v, n)))
    if choice == 5:
        # Step bounds and byte limits are independent and nest in either order.
        n = 2 + stream.below(12)
        if stream.below(2) == 0:
            return sub(
                'let bs${k} = bound steps 100000 { bound bytes 65536 { let bx${k} = mk_list(${v}u64, ${n}u64); list.sum(bx${k}) } }\n'
                'let bq${k} = match bs${k} { Ok(Ok(v)) => v, Ok(Error(_)) => 8000u64, Error(t) => 7000u64 + t.steps }\n',
                k=k, v=v, n=n) + check(k, 0, f'bq{k}', sum(mk_list(v, n)))
        inner = 5 + stream.below(30)
        return sub(
            'let bs${k} = bound bytes 65536 { let bx${k} = mk_list(${v}u64, ${n}u64); bound steps ${inner} { spin(${long}u64 + list.len(bx${k})) } }\n'
            'let bq${k} = match bs${k} { Ok(Ok(v)) => v, Ok(Error(t)) => 7000u64 + t.steps, Error(_) => 8000u64 }\n',
            k=k, v=v, n=n, inner=inner, long=1000 + v) + check(k, 0, f'bq{k}', 7000 + inner)
    if choice == 6:
        # A finalizer that would run under its exhausted bound is abandoned (MEM-30, Exhaustion).
        exhausted = stream.below(2) == 0
        budget = 10 + stream.below(30) if exhausted else 100000
        work = 1000 + v if exhausted else 2 + stream.below(20)
        want = 7000 + budget if exhausted else spin(work) + 6
        return sub(
            'let (bft${k}, bfr${k}): (Sender[u64], Receiver[u64]) = channel.new(4u64)\n'
            'let bf${k} = bound steps ${budget} {\n'
            '  let bl${k} = result.unwrap(Logged(log: channel.sender(bft${k}), tag: 6u64))\n'
            '  spin(${work}u64) + bl${k}.tag\n}\n',
            k=k, budget=budget, work=work) + check(k, 0, sub(STEP_OUT, value=f'bf{k}'), want) + '\n' + check(
                k, 1, f'drain_log(bfr{k})', 0 if exhausted else 6)
    # A working value moved into the scope returns with the result or is dropped by the stop.
    n = 1 + stream.below(5)
    pushes = 2 + stream.below(6)
    exhausted = stream.below(2) == 0
    budget = 1 if exhausted else 100000
    want = 7000 + budget if exhausted else sum(mk_list(v, n)) + spin(pushes) + n + pushes
    return sub(
        'let bw${k} = mk_list(${v}u64, ${n}u64)\n'
        'let bv${k} = bound steps ${budget} {\n'
        '  let mut bu${k} = bw${k}\n'
        '  for bq${k} in 0u64 .. ${pushes}u64 { list.push(mut bu${k}, bq${k}) }\n'
        '  bu${k}\n}\n'
        'let bz${k} = match bv${k} { Ok(take done${k}) => list.sum(done${k}) + list.len(done${k}), Error(t) => 7000u64 + t.steps }\n',
        k=k, v=v, n=n, pushes=pushes, budget=budget) + check(k, 0, f'bz{k}', want)


# --- isolate ---

def isolate_stmt(stream, k: int) -> str:
    choice = stream.below(7)
    v = 1 + stream.below(40)
    kind = stream.below(6)
    failed, value = failure_value(kind, 3)
    open_log = f'let (itx{k}, irx{k}): (Sender[u64], Receiver[u64]) = channel.new(16u64)\n'
    if choice == 0:
        # Every checked runtime error is caught as a value with its cause.
        if stream.below(4) == 0:
            return sub(
                'let ic${k} = isolate(fn() -> u64 {\n'
                '  let ib${k} = bound bytes 64 { let big${k} = mk_list(${v}u64, 300u64); list.sum(big${k}) }\n'
                '  match ib${k} { Ok(v) => v, Error(_) => 8000u64 }\n})\n', k=k, v=v) + check(
                    k, 0, f'fold_iso(ic{k})', FAIL_BASE + CAUSE['Capacity'])
        return sub('let ic${k} = isolate(fn() -> u64 { trigger_failure(${kind}u64, mk_list(${v}u64, 3u64)) + ${v}u64 })\n',
                   k=k, kind=kind, v=v) + check(k, 0, f'fold_iso(ic{k})', FAIL_BASE + value if failed else value + v)
    if choice == 1:
        # Entry-capacity refusal: no room for the cause cell, so the body never runs.
        refused = stream.below(2) == 0
        return (open_log + sub(
            'let ie${k} = bound bytes ${room} {\n'
            '  let ir${k} = isolate(fn() -> u64 { let _ = channel.try_send(itx${k}, 7u64); ${v}u64 })\n'
            '  fold_iso(ir${k})\n}\n', k=k, room=0 if refused else 4096, v=v)
            + check(k, 0, sub(BYTE_OUT, value=f'ie{k}'), ENTRY_REFUSED if refused else v) + '\n'
            + check(k, 1, f'drain_log(irx{k})', 0 if refused else 7))
    if choice == 2:
        # Cleanup preservation: every owner made before the failure is cleaned once, newest first.
        count = 2 + stream.below(4)
        at = stream.below(count + 1)
        lines = []
        for i in range(count):
            if i == at:
                lines.append(f'  let _ = trigger_failure({kind}u64, mk_list({v}u64, 3u64))')
            lines.append(f'  let il{i}_{k} = result.unwrap(Logged(log: channel.sender(itx{k}), tag: {i + 1}u64))')
            lines.append(f'  let is{i}_{k} = string.from("owned{i}")')
        if at == count:
            lines.append(f'  let _ = trigger_failure({kind}u64, mk_list({v}u64, 3u64))')
        tail = ' + '.join(f'il{i}_{k}.tag + string.byte_len(is{i}_{k})' for i in range(count))
        made = at if failed else count
        want = FAIL_BASE + value if failed else sum(i + 1 + 6 for i in range(count))
        return (open_log + f'let ip{k} = isolate(fn() -> u64 {{\n' + '\n'.join(lines) + f'\n  {tail}\n}})\n'
                + check(k, 0, f'fold_iso(ip{k})', want) + '\n'
                + check(k, 1, f'drain_log(irx{k})', log_order(range(made, 0, -1))))
    if choice == 3:
        # Peer state invariance: viewed places keep their entry value, issued sends persist.
        n = 2 + stream.below(5)
        xs = mk_list(v, n)
        failed, value = failure_value(kind, n)
        return (open_log + sub(
            'let iv${k} = mk_list(${v}u64, ${n}u64)\n'
            'let ib${k} = list.sum(iv${k})\n'
            'let ii${k} = isolate(fn() -> u64 {\n'
            '  let mut local${k}: List[u64] = clone(iv${k})\n'
            '  list.push(mut local${k}, 1u64)\n'
            '  let _ = channel.try_send(itx${k}, 3u64)\n'
            '  let t${k} = trigger_failure(${kind}u64, iv${k})\n'
            '  let _ = channel.try_send(itx${k}, 4u64)\n'
            '  list.sum(local${k}) + t${k}\n})\n', k=k, v=v, n=n, kind=kind)
            + check(k, 0, f'fold_iso(ii{k})', FAIL_BASE + value if failed else sum(xs) + 1 + value) + '\n'
            + check(k, 1, f'list.sum(iv{k}) * 100u64 + list.len(iv{k}) - ib{k} * 100u64', n) + '\n'
            + check(k, 2, f'drain_log(irx{k})', 3 if failed else 34))
    if choice == 4:
        # Nested isolates: the inner boundary catches, the outer body goes on.
        outer_kind = stream.below(6)
        outer_failed, outer_value = failure_value(outer_kind, 3)
        inner = FAIL_BASE + value if failed else value
        want = FAIL_BASE + outer_value if outer_failed else inner % 1000 + outer_value
        return sub(
            'let io${k} = isolate(fn() -> u64 {\n'
            '  let tr${k} = result.unwrap(Track(id: ${v}u64, note: string.from("outer")))\n'
            '  let in${k} = isolate(fn() -> u64 { trigger_failure(${kind}u64, mk_list(tr${k}.id, 3u64)) })\n'
            '  fold_iso(in${k}) % 1000u64 + trigger_failure(${outer}u64, mk_list(${v}u64, 3u64))\n})\n',
            k=k, v=v, kind=kind, outer=outer_kind) + check(k, 0, f'fold_iso(io{k})', want)
    if choice == 5:
        # A caught failure in one task leaves its peer task's result alone.
        n = 1 + stream.below(6)
        caught = FAIL_BASE + value if failed else value + v
        return sub(
            'let (iwt${k}, iwr${k}): (Sender[u64], Receiver[u64]) = channel.new(2u64)\n'
            'let iwa${k} = task.spawn(tasks, bytes 65536, (take iwt${k}, ${kind}u64, ${v}u64), iso_worker)\n'
            'let iwb${k} = task.spawn(tasks, bytes 65536, (${v}u64, ${n}u64), sum_worker)\n'
            'let iwm${k} = match channel.receive(iwr${k}) { Ok(x) => x, Error(_) => 0u64 }\n'
            'let iwx${k}: u64 = match task.join(take iwa${k}) { Ok(x) => x, Error(_) => 1u64 }\n'
            'let iwy${k}: u64 = match task.join(take iwb${k}) { Ok(x) => x, Error(_) => 2u64 }\n',
            k=k, kind=kind, v=v, n=n) + check(k, 0, f'iwx{k}', caught) + '\n' + check(
                k, 1, f'iwm{k} * 1000u64 + iwy{k}', sum(mk_list(v, 3)) * 1000 + sum(mk_list(v, n)))
    # OutOfTime passes an isolate while its bound is current and is caught once the bound has ended.
    budget = 10 + stream.below(40)
    if stream.below(2) == 0:
        return sub('let it${k} = bound steps ${budget} { isolate(fn() -> u64 { spin(${long}u64) }) }\n'
                   'let iq${k} = match it${k} { Ok(r) => fold_iso(r), Error(t) => 7000u64 + t.steps }\n',
                   k=k, budget=budget, long=1000 + v) + check(k, 0, f'iq{k}', 7000 + budget)
    return sub(
        'let it${k} = isolate(fn() -> u64 { match (bound steps ${budget} { spin(${long}u64) }) { Ok(x) => x, Error(t) => 7000u64 + t.steps } })\n',
        k=k, budget=budget, long=1000 + v) + check(k, 0, f'fold_iso(it{k})', 7000 + budget)


FEATURE_BUILDERS = {'code': code_stmt, 'held': held_stmt, 'bound': bound_stmt, 'isolate': isolate_stmt}

# (name, surface, expected rules, source)
FEATURE_CONTROLS = (
    ('code_bind_closure', 'code', ('PACK-14',),
     'fn bad() -> Code[fn() -> u64] { let f = fn() -> u64 { 1u64 }; code.bind(f) }\n'),
    ('code_call_waits_entry', 'code', ('PACK-14',),
     'fn entry(rx: Receiver[u64]) -> u64 waits { result.unwrap(channel.receive(rx)) }\n'
     'fn bad(rx: Receiver[u64]) -> u64 waits { let c: Code[fn(Receiver[u64]) -> u64 waits] = code.bind(entry); c(rx) }\n'),
    ('code_dispatch_waits_in_sync', 'code', ('LANG-8',),
     'fn bad(s: CodeSlot[fn() waits]) -> Result[(), code.DispatchError] { code.dispatch(s) }\n'),
    ('code_use_after_hold', 'code', ('OWN-5',),
     'fn entry(x: u64) -> u64 { x }\n'
     'fn bad() -> u64 { let c: Code[fn(u64) -> u64] = code.bind(entry); let h: Held = code.hold(take c); let _ = take h; c(1u64) }\n'),
    ('held_view_payload', 'held', ('PACK-16',), 'fn bad(s: String) -> Held { let v = view s; code.hold(v) }\n'),
    ('held_scoped_payload', 'held', ('PACK-16',),
     'scoped type Cur { input: View[String] }\nfn bad(s: String) -> Held { code.hold(Cur(input: view s)) }\n'),
    ('held_region_payload', 'held', ('PACK-16',),
     'fn bad() -> u64 { region r { let t = text.copy_in(r, "hi"); let h: Held = code.hold(t); 1u64 } }\n'),
    ('held_duplicate', 'held', ('OWN-5',), 'fn bad(take h: Held) -> (Held, Held) { (h, h) }\n'),
    ('held_take_as_view', 'held', ('OWN-7',),
     'fn bad(h: Held) -> u64 { match (code.take_as(h) : Result[u64, (code.TakeError, Held)]) { Ok(v) => v, Error(_) => 0u64 } }\n'),
    ('bound_steps_outer_write', 'bound', ('MEM-30',),
     'fn bad() -> u64 { let mut n: u64 = 0u64; let _ = bound steps 10 { n = n + 1u64; n }; n }\n'),
    ('bound_steps_outer_lend', 'bound', ('MEM-30',),
     'fn bad(mut xs: List[u64]) -> u64 { let _ = bound steps 10 { list.push(mut xs, 1u64); 0u64 }; list.len(xs) }\n'),
    ('bound_steps_unknown_callback', 'bound', ('MEM-30',),
     'fn bad(f: scoped fn() -> u64) -> u64 { match (bound steps 10 { f() }) { Ok(v) => v, Error(_) => 0u64 } }\n'),
    ('bound_steps_move_then_use', 'bound', ('OWN-16',),
     'fn bad(take s: String) -> u64 { let _ = bound steps 10 { let b = string.into_bytes(take s); list.len(b) }; string.byte_len(s) }\n'),
    ('isolate_unknown_callback', 'isolate', ('FAIL-16',),
     'fn bad(f: scoped fn() -> u64) -> u64 { match isolate(fn() -> u64 { f() }) { Ok(Ok(v)) => v, _ => 0u64 } }\n'),
    ('isolate_lend_mut', 'isolate', ('FAIL-16',),
     'fn bad(mut xs: List[u64]) -> u64 { let _ = isolate(fn() -> u64 { list.push(mut xs, 1u64); 0u64 }); list.len(xs) }\n'),
    ('arith_const_overflow', 'arith', ('LANG-20',), 'const BIG: u8 = 200 + 100\n'),
    ('arith_const_division', 'arith', ('LANG-20',), 'const BAD: u64 = 1 / 0\n'),
    ('arith_widen_narrowing', 'arith', ('SEM-1',), 'fn bad(x: u64) -> u8 { int.widen(x) }\n'),
    ('arith_truncate_widening', 'arith', ('SEM-1',), 'fn bad(x: u8) -> u32 { int.truncate(x) }\n'),
    ('arith_mixed_width', 'arith', ('LANG-21',), 'fn bad(a: u8, b: u64) -> u64 { a + b }\n'),
    ('arith_signed_index', 'arith', ('LANG-11',), 'fn bad(xs: List[u64], i: i64) -> u64 { xs[i] }\n'),
)

# Unsound statements injected into generated bodies, by surface: (name, expected rules, statement).
FEATURE_MUTATIONS = {
    'code': (
        ('mut_code_bind_closure', ('PACK-14',),
         'let cf_m = fn(x: u64) -> u64 { x }\nlet cc_m: Code[fn(u64) -> u64] = code.bind(cf_m)\nacc = mix(acc, cc_m(1u64))'),
        ('mut_code_use_after_hold', ('OWN-5',),
         'let cc_m: Code[fn(u64) -> u64] = code.bind(code_add)\nlet ch_m: Held = code.hold(take cc_m)\n'
         'acc = mix(acc, cc_m(1u64) + held_code(take ch_m))'),
    ),
    'held': (
        ('mut_held_view_payload', ('PACK-16',), 'let hv_m: Held = code.hold(view outer.inner.label)\nacc = mix(acc, held_code(take hv_m))'),
        ('mut_held_double_take', ('OWN-5',), 'let hd_m: Held = code.hold(1u64)\nacc = mix(acc, held_code(take hd_m) + held_code(take hd_m))'),
        ('mut_held_region_payload', ('PACK-16',),
         'let hr_m = region rr_m { let ht_m = text.copy_in(rr_m, outer.inner.label); let hh_m: Held = code.hold(ht_m); 1u64 }\nacc = mix(acc, hr_m)'),
    ),
    'bound': (
        ('mut_bound_steps_outer_write', ('MEM-30',), 'let _ = bound steps 10 { acc = acc + 1u64; acc }'),
        ('mut_bound_steps_outer_lend', ('MEM-30',), 'let _ = bound steps 10 { list.push(mut outer.extra, 1u64); 0u64 }'),
        ('mut_bound_steps_move_then_use', ('OWN-16',),
         'let _ = bound steps 10 { let bb_m = string.into_bytes(take local_s); list.len(bb_m) }\nacc = mix(acc, string.byte_len(local_s))'),
    ),
    'isolate': (
        ('mut_isolate_lend_mut', ('FAIL-16',), 'let _ = isolate(fn() -> u64 { list.push(mut outer.extra, 1u64); 0u64 })'),
        ('mut_isolate_assign_outer', ('FAIL-16',), 'let _ = isolate(fn() -> u64 { acc = acc + 1u64; acc })'),
    ),
    'arith': (
        ('mut_arith_widen_narrowing', ('SEM-1',), 'let aw_m: u8 = int.widen(acc)\nacc = mix(acc, int.widen(aw_m))'),
        ('mut_arith_mixed_width', ('LANG-21',), 'let am_m: u8 = 3u8\nacc = mix(acc, acc + am_m)'),
    ),
    'data_decode': (
        ('mut_data_witness_alias', ('LANG-12',),
         'let da_m = string.from("cfg")\nlet dw_m: std.data.Witness[CfgDoc] = std.data.witness(da_m)\n'
         'acc = mix(acc, match std.data.decode(dw_m, b"x", cfg_limits(0u64, 0u64)) { Ok(_) => 1u64, Error(_) => 2u64 })'),
        ('mut_data_decoded_double_move', ('OWN-5',),
         'let dw_m: std.data.Witness[CfgDoc] = std.data.witness("cfg")\n'
         'match std.data.decode(dw_m, b"x", cfg_limits(0u64, 0u64)) {\n'
         '  Ok(take dd_m) => { let dh_m: Held = code.hold(take dd_m); acc = mix(acc, cfg_digest(dd_m) + held_code(take dh_m)) },\n'
         '  Error(_) => {},\n}'),
    ),
}
