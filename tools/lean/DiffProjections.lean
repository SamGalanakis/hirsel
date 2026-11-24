import Generated.OMap
import Generated.Register

open std.omap

private def leaf (stamp : UInt64) (count firstKey : Nat) : Node UInt64 UInt64 :=
  .mk stamp 0 (UInt64.ofNat count) ⟨0, 0⟩
    ((List.range' firstKey count).map fun k => ⟨UInt64.ofNat k, UInt64.ofNat k⟩) [] none

private def row (version : OMap UInt64 UInt64) : String :=
  let count := toString (len version)
  match root version with
  | none => count
  | some n => s!"{count} {node_id n} {node_level n} {node_count n} {node_len n}"

private def omapRows : List String :=
  row (snapshot new) :: (List.range 32).flatMap fun i =>
    let left := leaf 10 (i + 1) 0
    let right := leaf 20 (32 - i) 32
    let branch := Node.mk 30 1 33 ⟨0, 0⟩ [] [left, right] (some [.Decoded 32])
    [row (snapshot (from_root (some branch))), row (snapshot (from_root (some left)))]

private def registerRows : IO (List String) := (List.range 122).mapM fun edit => do
  let candidate : Hirsel.Bytes := (List.range 120).map UInt8.ofNat
  let keep : Nat → Bool := fun i =>
    (8 ≤ i && i < 16) || (48 ≤ i && i < 56) || (80 ≤ i && i < 112)
  let saved := Vector.ofFn (n := 48) fun i =>
    UInt8.ofNat (i.val + if i.val < 8 then 9 else if i.val < 16 then 41 else 65)
  let observed : Hirsel.Bytes := (List.range (if edit == 121 then 119 else 120)).map fun i =>
    UInt8.ofNat (i + (if keep i then 1 else 0) + if edit == i then 2 else 0)
  match store.store.history_claim_same candidate saved observed with
  | .ok reply => return toString reply
  | .error e => throw <| IO.userError s!"register observation {edit} raised {repr e}"

def main (args : List String) : IO Unit := do
  match args with
  | ["omap"] => for line in omapRows do IO.println line
  | ["register"] => for line in ← registerRows do IO.println line
  | _ => throw <| IO.userError "expected omap or register"
