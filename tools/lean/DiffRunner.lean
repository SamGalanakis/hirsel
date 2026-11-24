/-
  KC4: run the extracted Lean against the reference evaluator's traces.

  `diff/diff_traces.hirsel` drives the unmodified Hirsel definitions on the reference
  evaluator (`hirsel-core-contract-eval`, a fresh memory store per trace). This program
  mirrors that driver: the same LCG, the same operation choice, the same printers, and one
  `Hirsel.run` of the extracted bodies per store transaction. A transaction whose run ends in
  `.ok (.ok _)` commits its data; any other ends with the data it started from (S1).

  The only inputs taken from the reference output are its `env` lines, the call environment
  the store handed each transaction (S3/S5: the store assigns versions and times). Every
  other line is computed here and compared byte for byte, trace by trace.

  usage: diffrunner <reference.txt> [<own-output.txt>]
-/
import Generated.Triggers

open Hirsel
open runner.triggers

namespace Diff

instance : Inhabited Id := ⟨{ store_hi := 0, store_lo := 0, branch := 0, version := 0, index := 0 }⟩

/-- `DiffGen` and `diff_next`: the shared LCG (Knuth MMIX constants). -/
def next (n : UInt64) : StateM UInt64 UInt64 := do
  let s := (← MonadState.get) * 6364136223846793005 + 1442695040888963407
  set s
  return (s >>> 33) % n

inductive Op where
  | write (k arg : UInt64) (when : Due) (twice : Bool)
  | start (id : Id) (revision : Bytes)
  | hold (a : Attempt) (step : UInt64) (expected actual : Bytes)
  | release (id : Id) (revision : Option Bytes)
  | cancel (id : Id)
  | finish (a : Attempt) (t : Terminal)
  | finishCancel (id : Id)
  | finishTransact (id : Id) (t : Terminal)
  | fence (a : Attempt)

inductive Reply where
  | wrote (id : Id) | began (a : Attempt) | held | fenced (b : Bool) | applied

def bytes (n : UInt64) : Bytes := if n != 0 then [n.toUInt8] else []

/-- An opaque argument codec: `arg` bytes are never printed. -/
def codec : Hirsel.std.codec.Codec UInt64 := ⟨fun x => .ok [x.toUInt8]⟩

def entry (k : UInt64) : Entry UInt64 :=
  { ref := { path := if k == 2 then bytes 5 else bytes 1,
             schema := if k == 1 then bytes 4 else bytes 3,
             granted := k != 2 },
    codec := codec }

def pending (id : Id) : Pending :=
  { id := id, row := { path := [], schema := [], arg := [], due := 0, granted := false, attempt := 0,
                       revision := [], state := .Waiting, cancel := false } }

/-- `diff_apply`: one transaction body. -/
def apply (entries : Entries) (target : Entry UInt64) :
    Op → Prog (Hirsel.store.Tx Tables.Data) Error Reply
  | .write _ arg when twice => do
    if twice then
      let _ ← write Tables.handle entries target arg when
    return .wrote (← write Tables.handle entries target arg when)
  | .start id revision => do
    match ← start Tables.handle (pending id) revision with
    | .Body a => return .began a
    | .Held => return .held
  | .hold a step expected actual => do
    hold Tables.handle a step expected actual
    return .applied
  | .release id revision => do
    release Tables.handle id revision
    return .applied
  | .cancel id => do
    request_cancel Tables.handle id
    return .applied
  | .finish a t => do
    finish Tables.handle a t
    return .applied
  | .finishCancel id => do
    finish_cancel Tables.handle (pending id)
    return .applied
  | .finishTransact id t => do
    finish_transact Tables.handle (pending id) t
    return .applied
  | .fence a => do
    return .fenced (← fence Tables.handle a)

def showBytes (b : Bytes) : String := "[" ++ ",".intercalate (b.map (toString ·.toNat)) ++ "]"

def showId (id : Id) : String :=
  s!"{id.store_hi}:{id.store_lo}:{id.branch}:{id.version}:{id.index}"

def showBool (b : Bool) : String := if b then "true" else "false"

def showTerminal : Terminal → String
  | .Returned b => "Returned " ++ showBytes b
  | .Raised k => s!"Raised {k}"
  | .OutOfTime n => s!"OutOfTime {n}"
  | .OutOfBytes a b => s!"OutOfBytes {a} {b}"
  | .Gone => "Gone"
  | .Cancelled => "Cancelled"
  | .Cap => "Cap"

def showError : Error → String
  | .Store _ => "Store"
  | .StaleAttempt => "StaleAttempt"
  | .MissingEntry => "MissingEntry"
  | .AlreadyTriggerable => "AlreadyTriggerable"
  | .AttemptLimit => "AttemptLimit"
  | .IndexLimit => "IndexLimit"
  | .NotHeld => "NotHeld"
  | .CancelRequested => "CancelRequested"
  | .NotDue => "NotDue"

def showRow (r : SubRow) : String :=
  let state := match r.state with
    | .Waiting => "Waiting"
    | .Running => "Running"
    | .Parked (.Request a st x y) => s!"Request {a} {st} {showBytes x} {showBytes y}"
    | .Parked (.Revision x y) => s!"Revision {showBytes x} {showBytes y}"
  s!"path={showBytes r.path} schema={showBytes r.schema} due={r.due} granted={showBool r.granted} " ++
    s!"attempt={r.attempt} revision={showBytes r.revision} state={state} cancel={showBool r.cancel}"

/-- The table's scan order: the `put_id` key, big-endian fields in declaration order. -/
def idLe (a b : Id) : Bool :=
  let ka := [a.store_hi.toNat, a.store_lo.toNat, a.branch.toNat, a.version.toNat, a.index.toNat]
  let kb := [b.store_hi.toNat, b.store_lo.toNat, b.branch.toNat, b.version.toNat, b.index.toNat]
  decide (ka ≤ kb)

def sorted {V : Type} (m : Map Id V) : List (Id × V) := m.entries.mergeSort (fun a b => idLe a.1 b.1)

def attempt (ids : Array Id) (numbers : Array UInt64) (i : Nat) : StateM UInt64 Attempt := do
  let n := numbers[i]!
  let pick ← next 8
  let num := if pick == 0 then n - 1 else if pick == 1 then n + 1 else if pick == 2 then 0 else n
  return { id := ids[i]!, number := num }

def terminal : StateM UInt64 Terminal := do
  let pick ← next 7
  return match pick with
    | 0 => .Returned (bytes 4) | 1 => .Raised 3 | 2 => .OutOfTime 9 | 3 => .OutOfBytes 1 2
    | 4 => .Gone | 5 => .Cancelled | _ => .Cap

def maybeBytes (base : UInt64) : StateM UInt64 Bytes := do
  let pick ← next 3
  return if pick == 0 then [] else bytes (base + pick)

def chooseOp (kind : UInt64) (i : Nat) (ids : Array Id) (numbers : Array UInt64) : StateM UInt64 Op := do
  let id := ids[i]!
  match kind with
  | 0 =>
    let k ← next 3
    let w ← next 3
    let when := if w == 0 then Due.AfterCommit else Due.At (w - 1)
    let arg ← next 1000
    return .write k arg when ((← next 4) == 0)
  | 1 => return .start id (← maybeBytes 5)
  | 2 =>
    let a ← attempt ids numbers i
    let step ← next 4
    let expected ← maybeBytes 10
    return .hold a step expected (← maybeBytes 20)
  | 3 =>
    if (← next 2) == 0 then return .release id none
    return .release id (some (← maybeBytes 5))
  | 4 => return .cancel id
  | 5 =>
    let a ← attempt ids numbers i
    return .finish a (← terminal)
  | 6 => return .finishCancel id
  | 7 => return .finishTransact id (← terminal)
  | _ => return .fence (← attempt ids numbers i)

def kindOf (r : UInt64) : UInt64 :=
  if r < 4 then 0 else if r < 6 then 1 else if r < 9 then r - 4 else if r < 11 then 5
  else if r < 15 then r - 5 else 10

/-- The call environment of one transaction, from an `env hi lo branch version at` line. -/
structure Env where
  hi : UInt64
  lo : UInt64
  branch : UInt32
  version : UInt64
  «at» : UInt64

def parseEnv (line : String) : Option Env :=
  match line.splitOn " " with
  | ["env", hi, lo, br, v, at_] => do
    return { hi := (← hi.toNat?).toUInt64, lo := (← lo.toNat?).toUInt64, branch := (← br.toNat?).toUInt32,
             version := (← v.toNat?).toUInt64, «at» := (← at_.toNat?).toUInt64 }
  | _ => none

/-- `diff_trace`: one trace's text, given its call environments in order. -/
def trace (seed : UInt64) (steps : Nat) (envs : List Env) : Except String String := Id.run do
  let mut out := s!"trace {seed}\n"
  let mut g : UInt64 := seed
  let mut entries : Entries := { listed := [] }
  let mut ids : Array Id := #[0, 1, 2].map fun j => { store_hi := 0, store_lo := 0, branch := 0, version := 0, index := j }
  let mut numbers : Array UInt64 := #[0, 0, 0]
  let mut data : Tables.Data := { rows := Map.empty, outcomes := Map.empty }
  let mut envs := envs
  let opening := seed % 3
  out := out ++ s!"allow {opening}"
  match run (allow (entry opening)) entries with
  | (.ok (.ok ()), e) => entries := e; out := out ++ " ok\n"
  | _ => return .error "the opening allow is refused"
  for _ in [0:steps] do
    let (r, g1) := (next 16).run g
    let (i, g2) := (next 3).run g1
    g := g2
    let kind := kindOf r
    let i := i.toNat
    if kind == 9 then
      let (k, g3) := (next 3).run g
      g := g3
      out := out ++ s!"allow {k}"
      match run (allow (entry k)) entries with
      | (.ok (.ok ()), e) => entries := e; out := out ++ " ok\n"
      | (.ok (.error err), e) => entries := e; out := out ++ s!" err {showError err}\n"
      | (.error c, _) => return .error s!"allow raised {repr c}"
    else if kind == 10 then
      let (k, g3) := (next 3).run g
      let (f, g4) := (next 2).run g3
      g := g4
      let flip := f == 0
      let e := entry k
      out := out ++ s!"listed {k} {showBool flip} {showBool (listed entries e.ref.path e.ref.schema (e.ref.granted != flip))}\n"
    else
      let (op, g3) := (chooseOp kind i ids numbers).run g
      g := g3
      let target := match op with
        | .write k .. => entry k
        | _ => entry 0
      let some env := envs.head? | return .error "the reference has fewer env lines"
      envs := envs.tail
      out := out ++ s!"env {env.hi} {env.lo} {env.branch} {env.version} {env.at}\n"
      out := out ++ s!"op {kind} slot {i}"
      let tx : Hirsel.store.Tx Tables.Data :=
        { info := { id := { store := { hi := env.hi, lo := env.lo }, branch := env.branch, version := env.version },
                    «at» := env.at, seed := 0 },
          data := data }
      match run (apply entries target op) tx with
      | (.ok (.ok reply), tx') =>
        data := tx'.data
        out := out ++ " commit "
        match reply with
        | .wrote new =>
          out := out ++ "wrote " ++ showId new
          ids := ids.set! i new
          numbers := numbers.set! i 0
        | .began a =>
          out := out ++ s!"began {showId a.id} {a.number}"
          numbers := numbers.set! i a.number
        | .held => out := out ++ "held"
        | .fenced b => out := out ++ "fenced " ++ showBool b
        | .applied => out := out ++ "applied"
      | (.ok (.error err), _) => out := out ++ " abort " ++ showError err
      | (.error c, _) => out := out ++ s!" raised {repr c}"
      out := out ++ "\n"
      for (id, row) in sorted data.rows do
        out := out ++ s!"  row {showId id} {showRow row}\n"
      for (id, t) in sorted data.outcomes do
        out := out ++ s!"  outcome {showId id} {showTerminal t}\n"
  return .ok out

/-- The reference output, split into traces: seed, text, env lines. -/
def traces (lines : Array String) : Array (UInt64 × String × List Env) := Id.run do
  let mut out := #[]
  let mut cur : Option (UInt64 × String × Array Env) := none
  for line in lines do
    if line.startsWith "trace " then
      if let some (s, t, e) := cur then out := out.push (s, t, e.toList)
      cur := some ((line.drop 6).toNat!.toUInt64, line ++ "\n", #[])
    else if let some (s, t, e) := cur then
      let e := match parseEnv line with | some v => e.push v | none => e
      cur := some (s, t ++ line ++ "\n", e)
  if let some (s, t, e) := cur then out := out.push (s, t, e.toList)
  return out

end Diff

def main (args : List String) : IO UInt32 := do
  let some ref := args.head? | IO.eprintln "usage: diffrunner <reference.txt> [<own-output.txt>]"; return 2
  let lines ← IO.FS.lines ref
  let ts := Diff.traces lines
  let mut agree := 0
  let mut own := ""
  let mut first : Option String := none
  for (seed, text, envs) in ts do
    let steps := 60
    match Diff.trace seed steps envs with
    | .error e =>
      own := own ++ s!"trace {seed}\nERROR {e}\n"
      if first.isNone then first := some s!"trace {seed}: {e}"
    | .ok mine =>
      own := own ++ mine
      if mine == text then
        agree := agree + 1
      else if first.isNone then
        let a := (text.splitOn "\n").toArray
        let b := (mine.splitOn "\n").toArray
        let mut k := 0
        while k < a.size && k < b.size && a[k]! == b[k]! do k := k + 1
        first := some s!"trace {seed} line {k + 1}:\n  reference: {a.getD k "<end>"}\n  lean:      {b.getD k "<end>"}"
  if let some path := args.drop 1 |>.head? then IO.FS.writeFile path own
  IO.println s!"traces {ts.size} agree {agree} disagree {ts.size - agree}"
  if let some f := first then IO.println s!"first disagreement: {f}"
  return if agree == ts.size && ts.size > 0 then 0 else 1
