# Examples

Seven complete programs, the seed of the conformance corpus. Each runs with a root limit
of 1,048,576 bytes. **Output** is the exact text the program writes to standard output,
and **outcome** is how the run ends ([PROG-3](programs.md#prog-3-entry)). Each refusal is an
edit of the program; the site is `line:column` in the edited program, at the start of the
token the rule names ([PROG-7](diagnostics.md#prog-7-one-rule-one-site)). Examples are
informative; the rules they cite are normative. `?` never converts an error
([LANG-18](language.md#lang-18-errors)), so each program converts with `mem` and `put`.

## 1. Bounded scratch in a region

A word is copied into a region inside a bound scope. With 64 bytes it fits: the `Text`
copy of `"hello"` charges 16 (8 count + 5 bytes, rounded up to 8) and the `Word` record 24
(16 for the `Text` reference + 8). With 8 bytes the `Text` copy is refused, and the scope
returns that `Capacity` value.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)
stable type Word { text: Text, len: u64 }

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] waits {
  let mut buf = string.new()
  mem(string.push(mut buf, label))?
  mem(string.push_u64(mut buf, n))?
  mem(string.push(mut buf, "\n"))?
  put(out, buf)
}

fn word(r: Region, s: String) -> Result[Word in r, Capacity] {
  let t = text.copy_in(r, s)?
  Word(text: t, len: string.byte_len(s)) in r
}

fn measure(s: String, limit: u64) -> Result[u64, Capacity] {
  bound bytes limit {
    region r {
      let w = word(r, s)?
      w.len
    }
  }
}

pub fn main(out: Stdout) -> Result[(), AppError] waits {
  say(out, "fits, length ", mem(measure("hello", 64))?)?
  match measure("hello", 8) {
    Ok(n) => say(out, "fits, length ", n),
    Error(c) => say(out, "refused, requested ", c.requested),
  }
}
```

Output:

```text
fits, length 5
refused, requested 16
```

Outcome: success.

Refused edits:

- Make the region block's value `w` instead of `w.len`: refused, MEM-7 at 29:7, a value
  whose type mentions `r` leaves its block.
- Drop the placement `in r` from the literal in `word`: refused, MEM-2 at 22:3, a stable
  literal needs a region.

## 2. Expression tree in one compact blob

`1 + (2 + 3)` is written into a sized destination, children before parents, and evaluated
by following links. Each node is 9 bytes: a `u8` tag, then a `u64` or two `u32` links. The
puts land at offsets 0, 9, 18, 27 and 36, and the root is the last, at 36, so the tree
needs 45 bytes. In 32 bytes the fourth put (27 + 9 > 32) writes nothing and returns
`Overflow`. Each `Child` carries the destination's identity, and every put checks it.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)
@compact(u32) stable type Expr = Num(u64) | Add(Link[Expr], Link[Expr])

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] waits {
  let mut buf = string.new()
  mem(string.push(mut buf, label))?
  mem(string.push_u64(mut buf, n))?
  mem(string.push(mut buf, "\n"))?
  put(out, buf)
}

fn eval(e: Expr) -> u64 {
  match e { Num(n) => n, Add(a, b) => eval(a) + eval(b) }
}

fn write(mut d: Dest[Expr]) -> Result[Child[Expr], BuildError] {
  let two: Child[Expr] = dest.put(mut d, Num(2))?
  let three: Child[Expr] = dest.put(mut d, Num(3))?
  let sum: Child[Expr] = dest.put(mut d, Add(two, three))?
  let one: Child[Expr] = dest.put(mut d, Num(1))?
  dest.put(mut d, Add(one, sum))
}

fn run(n: u64) -> Result[u64, BuildError] {
  region r {
    let b = build_in(r, bytes n, fn(mut d: Dest[Expr]) -> Result[Child[Expr], BuildError] { write(mut d) })?
    Ok(eval(finish(take b)))
  }
}

pub fn main(out: Stdout) -> Result[(), AppError] waits {
  let v = match run(64) { Ok(w) => w, Error(_) => 0 }
  say(out, "value ", v)?
  match run(32) {
    Ok(w) => say(out, "value ", w),
    Error(Overflow) => say(out, "overflow at ", 32),
    Error(Capacity(c)) => say(out, "capacity ", c.requested),
  }
}
```

Output:

```text
value 6
overflow at 32
```

Outcome: success.

Refused edits:

- Bind a link as a value: replace `eval(a)` with `{ let k: Link[Expr] = a; eval(k) }`:
  refused, MEM-10 at 21:48, a link is never a value.
- Add a variant `Str(String)` to `Expr`: refused, MEM-9 at 2:79, a compact type has no own
  field.

## 3. Tallying words, and a push that returns its value

Map insertion returns its arguments with the `Capacity` error. In a 64-byte bound the
first push grows an empty `List[u64]` to 4 elements (32 bytes). The fifth push grows it to
8: the new 64 bytes are admitted before the old 32 are refunded, 32 + 64 > 64, so that
push hands its value, 4, back.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] waits {
  let mut buf = string.new()
  mem(string.push(mut buf, label))?
  mem(string.push_u64(mut buf, n))?
  mem(string.push(mut buf, "\n"))?
  put(out, buf)
}

fn add(mut ws: List[String], take w: String) -> Result[(), AppError] {
  match list.push(mut ws, take w) { Ok(()) => Ok(()), Error((c, _)) => Error(Mem(c)) }
}

fn tally(ws: List[String]) -> Result[Map[String, u64], Capacity] {
  let mut m: Map[String, u64] = map.new()
  for w in ws {
    let n: u64 = match map.get(m, w) { Some(v) => v, None => 0 }
    let k = string.from(w)?
    match map.insert(mut m, take k, n + 1) { Ok(()) => {}, Error((c, _, _)) => return Error(c) }
  }
  Ok(take m)
}

fn first_refused(limit: u64) -> Result[u64, Capacity] {
  bound bytes limit {
    let mut xs: List[u64] = list.new()
    let mut i: u64 = 0
    let mut back: u64 = 0
    loop {
      match list.push(mut xs, i) { Ok(()) => { i += 1 }, Error((_, b)) => { back = b; break } }
    }
    back
  }
}

pub fn main(out: Stdout) -> Result[(), AppError] waits {
  let mut ws: List[String] = list.new()
  add(mut ws, "a")?
  add(mut ws, "b")?
  add(mut ws, "a")?
  add(mut ws, "c")?
  add(mut ws, "a")?
  let m = mem(tally(ws))?
  say(out, "a ", match map.get(m, "a") { Some(v) => v, None => 0 })?
  say(out, "distinct ", map.len(m))?
  say(out, "first refused push ", mem(first_refused(64))?)
}
```

Output:

```text
a 3
distinct 3
first refused push 4
```

Outcome: success.

Refused edits:

- Drop the `?` after `add(mut ws, "c")`: refused, LANG-18 at 50:3, an expression statement
  whose value is a `Result`.
- Grow `ws` while iterating it: insert `for w in ws { add(mut ws, "d")? }` before `let m`:
  refused, OWN-10 at 52:21, `ws` is frozen by the loop.

## 4. Pieces shared between a document and its undo list

Pieces are counted. The undo list shares the first piece, so `try_unwrap` cannot take it
and returns the handle.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)
type Piece: shareable { text: String }

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] waits {
  let mut buf = string.new()
  mem(string.push(mut buf, label))?
  mem(string.push_u64(mut buf, n))?
  mem(string.push(mut buf, "\n"))?
  put(out, buf)
}

fn piece(s: String) -> Result[Rc[Piece], AppError] {
  let t = mem(string.from(s))?
  match rc.new(Piece(text: take t)) { Ok(take h) => Ok(take h), Error((c, _)) => Error(Mem(c)) }
}

fn keep(mut xs: List[Rc[Piece]], take h: Rc[Piece]) -> Result[(), AppError] {
  match list.push(mut xs, take h) { Ok(()) => Ok(()), Error((c, _)) => Error(Mem(c)) }
}

fn bytes_of(ps: List[Rc[Piece]]) -> u64 {
  let mut n: u64 = 0
  for p in ps { n += string.byte_len(p.text) }
  n
}

pub fn main(out: Stdout) -> Result[(), AppError] waits {
  let mut doc: List[Rc[Piece]] = list.new()
  keep(mut doc, piece("hello")?)?
  let mut undo: List[Rc[Piece]] = list.new()
  for p in doc { keep(mut undo, rc.share(p))? }
  keep(mut doc, piece(", world")?)?
  say(out, "doc ", bytes_of(doc))?
  say(out, "undo ", bytes_of(undo))?
  let first = match list.pop(mut undo) { Some(take h) => h, None => return Ok(()) }
  match rc.try_unwrap(take first) {
    Ok(_) => put(out, "unique\n")?,
    Error(_) => put(out, "shared\n")?,
  }
  Ok(())
}
```

Output:

```text
doc 12
undo 5
shared
```

Outcome: success.

Refused edits:

- Insert `type Doc: send { pieces: List[Rc[Piece]] }` as line 3: refused, CONC-2 at 3:31,
  `Rc` is not `send`.
- Use `first` after `try_unwrap`: insert `let again = rc.share(first)` before the last
  `Ok(())`: refused, OWN-5 at 48:24, `first` was moved.

## 5. Cleanup order and fallible close

`Step` is a resource that holds its `Stdout` as a field; its finalizer reports its name and
may call waiting operations. `close` ends a step without its finalizer. Values are cleaned
in reverse order on every exit edge; the second call fails after two steps exist, so they
are dropped before the failure reaches the run.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)

resource type Step { name: String, out: Stdout }
  drop(s) { let _ = stdout.write(s.out, "drop "); let _ = stdout.write(s.out, s.name) }

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn step(out: Stdout, take name: String) -> Result[Step, AppError] {
  mem(Step(name: take name, out: out))
}

fn close(take s: Step) -> Result[(), AppError] waits {
  let Step(name: n, out: o) = take s
  put(o, "close ")?
  put(o, n)
}

fn steps(out: Stdout, fail_at: u64) -> Result[(), AppError] waits {
  let a = step(out, "a\n")?
  let b = step(out, "b\n")?
  if fail_at == 2 { fail("stop") }
  let c = step(out, "c\n")?
  close(take c)?
  Ok(())
}

pub fn main(out: Stdout) -> Result[(), AppError] waits {
  steps(out, 0)?
  steps(out, 2)
}
```

Output:

```text
close c
drop b
drop a
drop b
drop a
```

Outcome: failure, primary cause `Fail("stop")` at 27:21; the first call printed its three
lines, the second dropped `b` then `a` before the failure ended the run.

Refused edits:

- Close `c` twice: repeat the line `close(take c)?`: refused, OWN-5 at 30:14, `c` was
  moved.
- Use `?` in the finalizer: `drop(s) { stdout.write(s.out, s.name)? }`: refused, FAIL-11 at
  4:40, no `?` in a finalizer.

## 6. Three tasks with bounded channels

A source sends 1 to 5 through a channel of two slots, a second task squares them into
another, and `main` sums what arrives. The endpoints travel as spawn payloads. Each task's
last sender closes its channel when the task is cleaned, which ends the next loop. The
output does not depend on the schedule.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity) | Join(JoinError)
type Ends: send { input: Receiver[u64], output: Sender[u64] }

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] waits {
  let mut buf = string.new()
  mem(string.push(mut buf, label))?
  mem(string.push_u64(mut buf, n))?
  mem(string.push(mut buf, "\n"))?
  put(out, buf)
}

fn joined(take h: Task[u64]) -> Result[u64, AppError] waits {
  match task.join(take h) { Ok(r) => Ok(r), Error(take e) => Error(Join(take e)) }
}

fn source(take tx: Sender[u64]) -> u64 waits {
  let mut i: u64 = 1
  while i <= 5 {
    match channel.send(tx, i) { Ok(()) => { i += 1 }, Error(_) => break }
  }
  i - 1
}

fn square(take e: Ends) -> u64 waits {
  let mut n: u64 = 0
  while let Ok(x) = channel.receive(e.input) {
    match channel.send(e.output, x * x) { Ok(()) => { n += 1 }, Error(_) => break }
  }
  n
}

pub fn main(out: Stdout, tasks: Tasks) -> Result[(), AppError] waits {
  let (tx1, rx1): (Sender[u64], Receiver[u64]) = mem(channel.new(2))?
  let (tx2, rx2): (Sender[u64], Receiver[u64]) = mem(channel.new(2))?
  let src = match task.spawn(tasks, bytes 4096, take tx1, source) {
    Ok(take h) => h,
    Error((c, _)) => return Error(Mem(c)),
  }
  let ends = Ends(input: take rx1, output: take tx2)
  let sq = match task.spawn(tasks, bytes 4096, take ends, square) {
    Ok(take h) => h,
    Error((c, _)) => return Error(Mem(c)),
  }
  let mut total: u64 = 0
  while let Ok(y) = channel.receive(rx2) { total += y }
  say(out, "sent ", joined(take src)?)?
  say(out, "squared ", joined(take sq)?)?
  say(out, "total ", total)
}
```

Output:

```text
sent 5
squared 5
total 55
```

Outcome: success.

Refused edits:

- Receive from `rx1` after it moved into `ends`: insert `let _ = channel.receive(rx1)`
  before `let mut total`: refused, OWN-5 at 52:27, `rx1` was moved.
- Give `Ends` a field `seen: Rc[u64]`: refused, CONC-2 at 2:68, `Rc` is not `send`.

## 7. A child's failure is a value at join

The child adds two `u8` values of 200 and fails with `Overflow`. Its cleanup runs in the
child; the joiner receives `Failed` with the cause chain as an error value and carries on.

```hirsel
type AppError = Io(WriteError) | Mem(Capacity)

fn sum(take xs: List[u8]) -> u8 {
  let mut t: u8 = 0
  for x in xs { t += x }
  t
}

fn mem[T](take r: Result[T, Capacity]) -> Result[T, AppError] {
  match take r { Ok(take x) => Ok(take x), Error(c) => Error(Mem(c)) }
}

fn put(out: Stdout, s: String) -> Result[(), AppError] waits {
  match stdout.write(out, s) { Ok(()) => Ok(()), Error(e) => Error(Io(e)) }
}

pub fn main(out: Stdout, tasks: Tasks) -> Result[(), AppError] waits {
  let mut xs: List[u8] = list.new()
  mem(list.fill(mut xs, 2, 200))?
  let h = match task.spawn(tasks, bytes 4096, take xs, sum) {
    Ok(take k) => k,
    Error((c, _)) => return Error(Mem(c)),
  }
  match task.join(take h) {
    Ok(_) => put(out, "sum fits\n")?,
    Error(Failed(take causes)) => {
      for c in causes {
        match c.kind { Overflow => put(out, "child failed: Overflow\n")?, _ => put(out, "child failed\n")? }
      }
    }
    Error(Cancelled(_)) => put(out, "child cancelled\n")?,
  }
  Ok(())
}
```

Output:

```text
child failed: Overflow
```

Outcome: success. The child's cause is `Overflow` (SEM-1) at the `+=` in `t += x`, 5:19.

Refused edits:

- Use `xs` after the spawn: insert `let n = list.len(xs)` before `match task.join`:
  refused, OWN-5 at 24:20, `xs` was moved.
- Pass `xs` to the spawn without `take`: refused, OWN-7 at 20:47, a `take` parameter needs
  `take xs` for a place.
