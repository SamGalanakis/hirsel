/-
  The store interface model (hand-written, trusted; the KC4 differential checks it against the
  real store on the reference evaluator). Only what a transaction body sees: its CallInfo and
  the typed contents of the tables its handle record names.

  Modelling assumptions (REPORT.md S1..S5); every theorem that relies on one says so:
  * S1: an aborted transaction (an `Error` return or a cause) keeps no write.
  * S2: a refused commit keeps no write.
  * S3: each committed transaction runs at a version above every earlier commit.
  * S4: `put`, `fetch` and `delete` are total (a real `Full`/`Limit`/`Io` refusal is
    propagated with `?`, so the transaction aborts and by S1 keeps nothing).
  * S5: the ids an operation names come from committed state, below the running version.
  A typed table returns exactly the value last put under its key (codec round trip).
-/
import Hirsel

namespace Hirsel.store

structure StoreId where
  hi : UInt64
  lo : UInt64
  deriving DecidableEq, Repr, Inhabited

structure CallId where
  store : StoreId
  branch : UInt32
  version : UInt64
  deriving DecidableEq, Repr, Inhabited

structure CallInfo where
  id : CallId
  «at» : UInt64
  seed : UInt64
  deriving DecidableEq, Repr, Inhabited

inductive Limit
  | Key | Value | Transaction | Name | Ids | Manifest | Triggers
  deriving DecidableEq, Repr, Inhabited

/-- The cases of `store.Error` an extracted body names; every other case is `Other`, since a
    body only propagates it. -/
inductive Error
  | Limit (l : Limit)
  | Missing (name : String)
  | Exists (name : String)
  | Other
  deriving DecidableEq, Repr, Inhabited

/-- A transaction: its call info and the contents of the tables its handle record names. -/
structure Tx (D : Type) where
  info : CallInfo
  data : D

/-- A table handle: where the table's contents live in `D`. -/
structure Table (D K V : Type) where
  get : D → Hirsel.Map K V
  set : D → Hirsel.Map K V → D

/-- An index handle. An index is a function of its table, so it holds no state of its own. -/
structure Index (J K V P : Type) : Type where
  mk ::

variable {D E K V : Type}

/-- `store.info(mut tx)`. -/
def info : Hirsel.Prog (Tx D) E CallInfo := do
  return (← Hirsel.get).info

/-- `store.fetch(tx, t, k)`. -/
def fetch [DecidableEq K] (t : Table D K V) (k : K) : Hirsel.Prog (Tx D) E (Except Error (Option V)) := do
  return .ok ((t.get (← Hirsel.get).data).get k)

/-- `store.put(mut tx, t, k, v)`. -/
def put [DecidableEq K] (t : Table D K V) (k : K) (v : V) : Hirsel.Prog (Tx D) E (Except Error Unit) := do
  Hirsel.modify fun tx => { tx with data := t.set tx.data ((t.get tx.data).insert k v) }
  return .ok ()

/-- `store.delete(mut tx, t, k)`: whether `k` was present. -/
def delete [DecidableEq K] (t : Table D K V) (k : K) : Hirsel.Prog (Tx D) E (Except Error Bool) := do
  let tx ← Hirsel.get
  Hirsel.modify fun tx => { tx with data := t.set tx.data ((t.get tx.data).erase k) }
  return .ok ((t.get tx.data).get k).isSome

@[simp] theorem run_info (tx : Tx D) :
    Hirsel.run (info : Hirsel.Prog (Tx D) E CallInfo) tx = (.ok (.ok tx.info), tx) := rfl

@[simp] theorem run_fetch [DecidableEq K] (t : Table D K V) (k : K) (tx : Tx D) :
    Hirsel.run (fetch t k : Hirsel.Prog (Tx D) E _) tx = (.ok (.ok (.ok ((t.get tx.data).get k))), tx) := rfl

@[simp] theorem run_put [DecidableEq K] (t : Table D K V) (k : K) (v : V) (tx : Tx D) :
    Hirsel.run (put t k v : Hirsel.Prog (Tx D) E _) tx =
      (.ok (.ok (.ok ())), { tx with data := t.set tx.data ((t.get tx.data).insert k v) }) := rfl

@[simp] theorem run_delete [DecidableEq K] (t : Table D K V) (k : K) (tx : Tx D) :
    Hirsel.run (delete t k : Hirsel.Prog (Tx D) E _) tx =
      (.ok (.ok (.ok ((t.get tx.data).get k).isSome)),
        { tx with data := t.set tx.data ((t.get tx.data).erase k) }) := rfl

end Hirsel.store

namespace Hirsel.std.codec

inductive EncodeError
  | CountOverflow (n : UInt64)
  deriving DecidableEq, Repr, Inhabited

/-- `std.codec.Codec[A]`: only its encoder is reachable from extracted bodies. -/
structure Codec (A : Type) where
  encode : A → Except EncodeError Hirsel.Bytes

/-- `std.codec.encode(c, x, mut out)`: the `Result` and the new `out`. -/
def encode {A : Type} (c : Codec A) (x : A) (out : Hirsel.Bytes) : Except EncodeError Unit × Hirsel.Bytes :=
  match c.encode x with
  | .ok b => (.ok (), out ++ b)
  | .error e => (.error e, out)

end Hirsel.std.codec
