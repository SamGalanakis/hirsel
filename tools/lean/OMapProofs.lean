import Generated.OMap

namespace OMapProofs
open std.omap

theorem new_empty : len (new : OMap K V) = 0 := rfl

theorem root_round_trip (m : OMap K V) : from_root (root m) = m := by
  cases m with
  | mk head => cases head <;> rfl

theorem node_count_shape (n : Node K V) :
    node_count n = if node_level n = 0 then Hirsel.list.len n.rows else Hirsel.list.len n.kids := by
  unfold node_count node_level
  simp

theorem snapshot_preserves_root (m : OMap K V) : snapshot m = m := by
  cases m with
  | mk head => cases head <;> rfl

/-- Any number of successive snapshots preserves the original bindings and root metadata. -/
theorem snapshot_history (m : OMap K V) (history : List Unit) :
    history.foldl (fun version _ => snapshot version) m = m := by
  induction history generalizing m with
  | nil => rfl
  | cons _ rest ih => rw [List.foldl_cons, ih, snapshot_preserves_root]

#print axioms new_empty
#print axioms root_round_trip
#print axioms node_count_shape
#print axioms snapshot_history

end OMapProofs
