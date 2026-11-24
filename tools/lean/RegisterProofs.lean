import Generated.Register

namespace RegisterProofs
open Hirsel store.store

/-- A readback of a different length cannot be identified as the previous head. -/
theorem claim_rejects_different_length (candidate observed : Bytes) (saved : Vector UInt8 48)
    (h : list.len observed ≠ list.len candidate) :
    history_claim_same candidate saved observed = .ok false := by
  simp [history_claim_same, Hirsel.orElse, h, bind, Except.bind, pure, Except.pure]

/-- The first eight bytes come from the prepared candidate. -/
theorem claim_rejects_changed_prefix (candidate observed : Bytes) (saved : Vector UInt8 48)
    (before after : Bytes) (length : list.len observed = list.len candidate)
    (hc : list.slice candidate 0 8 = .ok before)
    (ho : list.slice observed 0 8 = .ok after)
    (changed : after ≠ before) :
    history_claim_same candidate saved observed = .ok false := by
  simp [history_claim_same, Hirsel.orElse, length, ho, hc, changed, bind, Except.bind, pure, Except.pure]

#print axioms claim_rejects_different_length
#print axioms claim_rejects_changed_prefix

end RegisterProofs
