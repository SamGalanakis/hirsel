# Examples

Seven complete programs, the seed of the conformance corpus. Each runs with a root limit
of 1,048,576 bytes. **Output** is the exact text the program writes to standard output,
and **outcome** is how the run ends ([PROG-3](programs.md#prog-3-entry)). Each refusal is an
edit of the program; the site is `line:column` in the edited program, at the start of the
token the rule names ([PROG-7](diagnostics.md#prog-7-one-rule-one-site)). Examples are
informative; the rules they cite are normative.

## 1. Bounded scratch in a region

A word is parsed into a region inside a bound scope. The first call fits; the second
exceeds its 8-byte bound when the `Word` record (16 for `Text` plus 8) is placed, and the
scope returns the `Capacity` value.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)
stable type Word { text: Text, len: u64 }

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] {
  let mut line = string.new()
  string.push(mut line, label)?
  string.push_u64(mut line, n)?
  string.push(mut line, "\n")?
  stdout.write(out, line)?
  Ok(())
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

pub fn main(out: Stdout) -> Result[(), AppError] {
  say(out, "fits, length ", measure("hello", 64)?)?
  match measure("hello", 8) {
    Ok(n) => say(out, "fits, length ", n),
    Error(c) => say(out, "refused, requested ", c.requested),
  }
}
```

Output:

```text
fits, length 5
refused, requested 24
```

Outcome: success.

Refused edits:

- Make the region block's value `w` instead of `w.len`: refused, MEM-7 at 22:7, a value
  whose type mentions `r` leaves its block.
- Drop the placement `in r` from the literal in `word`: refused, MEM-2 at 15:3, a stable
  literal needs a region.

## 2. Expression tree in one compact blob

`1 + (2 + 3)` is written into a sized destination, children before parents, and evaluated
by following links. Each node is 9 bytes (a `u8` tag and 8 payload bytes), so the tree
needs 45 bytes: 64 fits and 32 overflows at the fourth node.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)
@compact(u32) stable type Expr = Num(u64) | Add(Link[Expr], Link[Expr])

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] {
  let mut line = string.new()
  string.push(mut line, label)?
  string.push_u64(mut line, n)?
  string.push(mut line, "\n")?
  stdout.write(out, line)?
  Ok(())
}

fn eval(e: Expr) -> u64 {
  match e { Num(n) => n, Add(a, b) => eval(a) + eval(b) }
}

fn write(mut d: Dest[Expr]) -> Result[Child[Expr], BuildError] {
  let two = dest.put(mut d, Num(2))?
  let three = dest.put(mut d, Num(3))?
  let sum = dest.put(mut d, Add(two, three))?
  let one = dest.put(mut d, Num(1))?
  dest.put(mut d, Add(one, sum))
}

fn run(n: u64) -> Result[u64, BuildError] {
  region r {
    let b = build_in(r, bytes n, fn(mut d: Dest[Expr]) -> Result[Child[Expr], BuildError] { write(mut d) })?
    Ok(eval(finish(take b)))
  }
}

pub fn main(out: Stdout) -> Result[(), AppError] {
  let v = match run(64) { Ok(v) => v, Error(_) => 0 }
  say(out, "value ", v)?
  match run(32) {
    Ok(v) => say(out, "value ", v),
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
  refused, MEM-10 at 14:61, a link is never a value.
- Add a variant `Str(String)` to `Expr`: refused, MEM-9 at 2:79, a compact type has no own
  field.

## 3. Tallying words, and a push that returns its value

Map insertion returns its arguments with the `Capacity` error. In a 64-byte bound a list of
`u64` takes 32 bytes for its first four elements; growing to eight requests 64 more while
32 remain, so the fifth push hands its value back.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] {
  let mut line = string.new()
  string.push(mut line, label)?
  string.push_u64(mut line, n)?
  string.push(mut line, "\n")?
  stdout.write(out, line)?
  Ok(())
}

fn add(mut ws: List[String], take w: String) -> Result[(), Capacity] {
  match list.push(mut ws, take w) { Ok(()) => Ok(()), Error((c, _)) => Error(c) }
}

fn tally(ws: List[String]) -> Result[Map[String, u64], Capacity] {
  let mut m: Map[String, u64] = map.new()
  for w in ws {
    let n: u64 = match map.get(m, w) { Some(v) => v, None => 0 }
    let k = string.copy(w)?
    match map.insert(mut m, take k, n + 1) { Ok(()) => {}, Error((c, _, _)) => return Error(c) }
  }
  Ok(m)
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

pub fn main(out: Stdout) -> Result[(), AppError] {
  let mut ws: List[String] = list.new()
  add(mut ws, "a")?
  add(mut ws, "b")?
  add(mut ws, "a")?
  add(mut ws, "c")?
  add(mut ws, "a")?
  let m = tally(ws)?
  say(out, "a ", match map.get(m, "a") { Some(v) => v, None => 0 })?
  say(out, "distinct ", map.len(m))?
  say(out, "first refused push ", first_refused(64)?)
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

- Drop the `?` after `add(mut ws, "c")`: refused, LANG-18 at 43:3, an expression statement
  whose value is a `Result`.
- Grow `ws` while iterating it: insert `for w in ws { add(mut ws, "d")? }` before `let m`:
  refused, OWN-10 at 45:21, `ws` is frozen by the loop.

## 4. Pieces shared between a document and its undo list

Pieces are counted. The undo list shares the first piece, so `try_unwrap` cannot take it
and returns the handle.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)
type Piece: shareable { text: String }

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] {
  let mut line = string.new()
  string.push(mut line, label)?
  string.push_u64(mut line, n)?
  string.push(mut line, "\n")?
  stdout.write(out, line)?
  Ok(())
}

fn piece(s: String) -> Result[Rc[Piece], Capacity] {
  let t = string.from(s)?
  match rc.new(Piece(text: take t)) { Ok(take h) => Ok(take h), Error((c, _)) => Error(c) }
}

fn keep(mut xs: List[Rc[Piece]], take h: Rc[Piece]) -> Result[(), Capacity] {
  match list.push(mut xs, take h) { Ok(()) => Ok(()), Error((c, _)) => Error(c) }
}

fn bytes_of(ps: List[Rc[Piece]]) -> u64 {
  let mut n: u64 = 0
  for p in ps { n += string.byte_len(p.text) }
  n
}

pub fn main(out: Stdout) -> Result[(), AppError] {
  let mut doc: List[Rc[Piece]] = list.new()
  keep(mut doc, piece("hello")?)?
  let mut undo: List[Rc[Piece]] = list.new()
  for p in doc { keep(mut undo, rc.share(p))? }
  keep(mut doc, piece(", world")?)?
  say(out, "doc ", bytes_of(doc))?
  say(out, "undo ", bytes_of(undo))?
  let first = match list.pop(mut undo) { Some(take h) => h, None => return Ok(()) }
  match rc.try_unwrap(take first) {
    Ok(_) => stdout.write(out, "unique\n")?,
    Error(_) => stdout.write(out, "shared\n")?,
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

- Declare `type Doc: send { pieces: List[Rc[Piece]] }`: refused, CONC-2 at 3:31, `Rc` is
  not `send`.
- Use `first` after `try_unwrap`: insert `let again = rc.share(first)` before the last
  `Ok(())`: refused, OWN-5 at 41:24, `first` was moved.

## 5. Cleanup order and fallible close

`Step` is a resource whose finalizer reports its name. `close` ends a step without its
finalizer. Values are cleaned in reverse order on every exit edge; the second call fails
after two steps exist, so they are dropped before the failure reaches the run.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)

resource type Step { name: String, out: Stdout }
  drop(s) { let _ = stdout.write(s.out, "drop "); let _ = stdout.write(s.out, s.name) }

fn step(out: Stdout, take name: String) -> Result[Step, Capacity] {
  Step(name: take name, out: out)
}

fn close(take s: Step) -> Result[(), IoError] {
  let Step(name: n, out: o) = take s
  stdout.write(o, "close ")?
  stdout.write(o, n)
}

fn steps(out: Stdout, fail_at: u64) -> Result[(), AppError] {
  let a = step(out, "a\n")?
  let b = step(out, "b\n")?
  if fail_at == 2 { fail("stop") }
  let c = step(out, "c\n")?
  close(take c)?
  Ok(())
}

pub fn main(out: Stdout) -> Result[(), AppError] {
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

Outcome: failure, primary cause `Fail("stop")` at 19:6; the first call printed its three
lines, the second dropped `b` then `a` before the failure ended the run.

Refused edits:

- Close `c` twice: repeat the line `close(take c)?`: refused, OWN-5 at 22:14, `c` was
  moved.
- Use `?` in the finalizer: `drop(s) { stdout.write(s.out, s.name)? }`: refused, FAIL-11 at
  4:40, no `?` in a finalizer.

## 6. Three tasks with bounded channels

A source sends 1 to 5 through a channel of two slots, a second task squares them into
another, and `main` sums what arrives. Each task's last sender closes its channel when the
task is cleaned, which ends the next loop. The output does not depend on the schedule.

```hirsel
type AppError = Io(IoError) | Mem(Capacity) | Join(JoinError)
type Ends: send { input: Receiver[u64], output: Sender[u64] }

fn say(out: Stdout, label: String, n: u64) -> Result[(), AppError] {
  let mut line = string.new()
  string.push(mut line, label)?
  string.push_u64(mut line, n)?
  string.push(mut line, "\n")?
  stdout.write(out, line)?
  Ok(())
}

fn source(take tx: Sender[u64]) -> u64 {
  let mut i: u64 = 1
  while i <= 5 {
    match channel.send(tx, i) { Ok(()) => { i += 1 }, Error(_) => break }
  }
  i - 1
}

fn square(take e: Ends) -> u64 {
  let mut n: u64 = 0
  while let Ok(x) = channel.receive(e.input) {
    match channel.send(e.output, x * x) { Ok(()) => { n += 1 }, Error(_) => break }
  }
  n
}

pub fn main(out: Stdout, tasks: Tasks) -> Result[(), AppError] {
  let (tx1, rx1): (Sender[u64], Receiver[u64]) = channel.new(2)?
  let (tx2, rx2): (Sender[u64], Receiver[u64]) = channel.new(2)?
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
  say(out, "sent ", task.join(take src)?)?
  say(out, "squared ", task.join(take sq)?)?
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
  before `let mut total`: refused, OWN-5 at 41:27, `rx1` was moved.
- Give `Ends` a field `seen: Rc[u64]`: refused, CONC-2 at 2:68, `Rc` is not `send`.

## 7. A child's failure is a value at join

The child adds two `u8` values of 200 and fails with `Overflow`. Its cleanup runs in the
child; the joiner receives the cause chain as an error value and carries on.

```hirsel
type AppError = Io(IoError) | Mem(Capacity)

fn sum(take xs: List[u8]) -> u8 {
  let mut t: u8 = 0
  for x in xs { t += x }
  t
}

pub fn main(out: Stdout, tasks: Tasks) -> Result[(), AppError] {
  let mut xs: List[u8] = list.new()
  list.fill(mut xs, 2, 200)?
  let h = match task.spawn(tasks, bytes 4096, take xs, sum) {
    Ok(take h) => h,
    Error((c, _)) => return Error(Mem(c)),
  }
  match task.join(take h) {
    Ok(_) => stdout.write(out, "sum fits\n")?,
    Error(Failed(causes)) => {
      for c in causes {
        match c.kind { Overflow => stdout.write(out, "child failed: Overflow\n")?, _ => stdout.write(out, "child failed\n")? }
      }
    }
    Error(Cancelled) => stdout.write(out, "child cancelled\n")?,
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
  refused, OWN-5 at 16:20, `xs` was moved.
- Pass `xs` to the spawn without `take`: refused, OWN-7 at 12:47, a `take` parameter needs
  `take xs`.
