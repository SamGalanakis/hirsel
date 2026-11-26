// Quint specification of Hirsel Fleet per-Place move authorization (docs/spec/fleet.md FLT-2, FLT-8).
// Supports randomized invariant checking (`quint run`) and model-based testing trace generation
// (`quint run --mbt --out-itf=...`).

module fleet_move {
  type ClaimClass = str

  type Head = {
    epoch: int,
    holder: int,
    origin_epoch: int,
    origin_attempt: int,
    origin_class: ClaimClass,
    permit_epoch: int,
    permit_attempt: int,
  }

  type PermitRow = {
    attempt: int,
    expect_epoch: int,
    expect_holder: int,
    target: int,
  }

  type InFlightOp = {
    attempt: int,
    expect_epoch: int,
    expect_holder: int,
    class: ClaimClass,
    holder: int,
    target: int,
  }

  pure val NODES: Set[int] = Set(1, 2, 3)
  pure val MAX_EPOCH: int = 5
  pure val MAX_INFLIGHT: int = 3

  var reg_head: Head
  var incumbent: int
  var next_holder: int
  var next_permit_attempt: int
  var next_continue_attempt: int
  var open_permits: Set[PermitRow]
  var inflight: Set[InFlightOp]
  var epoch_winners: int -> Set[int]
  var authorized: Set[PermitRow]
  var consumed_predecessors: int -> Set[int]
  var mbt_action: str

  action init = all {
    reg_head' = {
      epoch: 1,
      holder: 1,
      origin_epoch: 1,
      origin_attempt: 0,
      origin_class: "Adopt",
      permit_epoch: 0,
      permit_attempt: 0,
    },
    incumbent' = 1,
    next_holder' = 2,
    next_permit_attempt' = 1,
    next_continue_attempt' = 1001,
    open_permits' = Set(),
    inflight' = Set(),
    epoch_winners' = 1.to(MAX_EPOCH).mapBy(e => if (e == 1) Set(1) else Set()),
    authorized' = Set(),
    consumed_predecessors' = 1.to(MAX_EPOCH).mapBy(_ => Set()),
    mbt_action' = "init",
  }

  action auth_primary(target: int): bool = all {
    reg_head.epoch < MAX_EPOCH,
    open_permits.size() == 0,
    inflight.size() < MAX_INFLIGHT,
    NODES.contains(target),
    target != incumbent,
    val att = next_permit_attempt
    val h = next_holder
    val p = { attempt: att, expect_epoch: reg_head.epoch, expect_holder: reg_head.holder, target: target }
    val op = { attempt: att, expect_epoch: reg_head.epoch, expect_holder: reg_head.holder, class: "Permit", holder: h, target: target }
    all {
      open_permits' = Set(p),
      authorized' = authorized.union(Set(p)),
      inflight' = inflight.union(Set(op)),
      next_permit_attempt' = next_permit_attempt + 1,
      next_holder' = next_holder + 1,
      mbt_action' = "auth_primary",
      reg_head' = reg_head,
      incumbent' = incumbent,
      next_continue_attempt' = next_continue_attempt,
      epoch_winners' = epoch_winners,
      consumed_predecessors' = consumed_predecessors,
    }
  }

  action auth_recovery(target: int): bool = all {
    reg_head.epoch < MAX_EPOCH,
    open_permits.size() == 1,
    inflight.size() < MAX_INFLIGHT,
    NODES.contains(target),
    target != incumbent,
    nondet primary = open_permits.oneOf()
    all {
      target != primary.target,
      val att = next_permit_attempt
      val h = next_holder
      val p = { attempt: att, expect_epoch: primary.expect_epoch, expect_holder: primary.expect_holder, target: target }
      val op = { attempt: att, expect_epoch: primary.expect_epoch, expect_holder: primary.expect_holder, class: "Permit", holder: h, target: target }
      all {
        open_permits' = open_permits.union(Set(p)),
        authorized' = authorized.union(Set(p)),
        inflight' = inflight.union(Set(op)),
        next_permit_attempt' = next_permit_attempt + 1,
        next_holder' = next_holder + 1,
        mbt_action' = "auth_recovery",
        reg_head' = reg_head,
        incumbent' = incumbent,
        next_continue_attempt' = next_continue_attempt,
        epoch_winners' = epoch_winners,
        consumed_predecessors' = consumed_predecessors,
      }
    }
  }

  action spawn_continue: bool = all {
    reg_head.epoch < MAX_EPOCH,
    inflight.size() < MAX_INFLIGHT,
    not(inflight.exists(op => op.class == "Continue" and op.expect_epoch == reg_head.epoch)),
    val att = next_continue_attempt
    val h = next_holder
    val op = { attempt: att, expect_epoch: reg_head.epoch, expect_holder: reg_head.holder, class: "Continue", holder: h, target: incumbent }
    all {
      inflight' = inflight.union(Set(op)),
      next_continue_attempt' = next_continue_attempt + 1,
      next_holder' = next_holder + 1,
      mbt_action' = "spawn_continue",
      reg_head' = reg_head,
      incumbent' = incumbent,
      next_permit_attempt' = next_permit_attempt,
      open_permits' = open_permits,
      epoch_winners' = epoch_winners,
      authorized' = authorized,
      consumed_predecessors' = consumed_predecessors,
    }
  }

  action exec_cas(op: InFlightOp): bool = all {
    inflight.contains(op),
    inflight' = inflight.exclude(Set(op)),
    if (op.expect_epoch == reg_head.epoch and op.expect_holder == reg_head.holder and reg_head.epoch < MAX_EPOCH) {
      val new_ep = reg_head.epoch + 1
      val p_ep = if (op.class == "Permit") new_ep else reg_head.permit_epoch
      val p_att = if (op.class == "Permit") op.attempt else reg_head.permit_attempt
      all {
        reg_head' = {
          epoch: new_ep,
          holder: op.holder,
          origin_epoch: new_ep,
          origin_attempt: op.attempt,
          origin_class: op.class,
          permit_epoch: p_ep,
          permit_attempt: p_att,
        },
        epoch_winners' = epoch_winners.set(new_ep, epoch_winners.get(new_ep).union(Set(op.holder))),
        consumed_predecessors' = consumed_predecessors.set(op.expect_epoch, consumed_predecessors.get(op.expect_epoch).union(Set(new_ep))),
        mbt_action' = "exec_cas_won",
        incumbent' = incumbent,
        next_holder' = next_holder,
        next_permit_attempt' = next_permit_attempt,
        next_continue_attempt' = next_continue_attempt,
        open_permits' = open_permits,
        authorized' = authorized,
      }
    } else {
      all {
        mbt_action' = "exec_cas_superseded",
        reg_head' = reg_head,
        incumbent' = incumbent,
        next_holder' = next_holder,
        next_permit_attempt' = next_permit_attempt,
        next_continue_attempt' = next_continue_attempt,
        open_permits' = open_permits,
        epoch_winners' = epoch_winners,
        authorized' = authorized,
        consumed_predecessors' = consumed_predecessors,
      }
    }
  }

  action read_settle: bool = all {
    open_permits.size() > 0,
    open_permits.forall(p => reg_head.epoch > p.expect_epoch),
    val winners = open_permits.filter(p => reg_head.permit_attempt == p.attempt and reg_head.permit_epoch >= p.expect_epoch + 1)
    all {
      if (winners.size() > 0) {
        nondet w = winners.oneOf()
        incumbent' = w.target
      } else {
        incumbent' = incumbent
      },
      open_permits' = Set(),
      mbt_action' = "read_settle",
      reg_head' = reg_head,
      next_holder' = next_holder,
      next_permit_attempt' = next_permit_attempt,
      next_continue_attempt' = next_continue_attempt,
      inflight' = inflight,
      epoch_winners' = epoch_winners,
      authorized' = authorized,
      consumed_predecessors' = consumed_predecessors,
    }
  }

  action step = any {
    nondet t = NODES.oneOf()
    auth_primary(t),
    nondet t = NODES.oneOf()
    auth_recovery(t),
    spawn_continue,
    all {
      inflight.size() > 0,
      nondet op = inflight.oneOf()
      exec_cas(op),
    },
    read_settle,
  }

  // ---- Safety invariants ----

  val inv_origin_epoch_matches_head: bool =
    reg_head.origin_epoch == reg_head.epoch

  val inv_unique_holder_per_epoch: bool =
    1.to(MAX_EPOCH).forall(e => epoch_winners.get(e).size() <= 1) and
    1.to(MAX_EPOCH).forall(e1 =>
      1.to(MAX_EPOCH).forall(e2 =>
        e1 == e2 or epoch_winners.get(e1).intersect(epoch_winners.get(e2)).size() == 0
      )
    )

  val inv_rule_f1_shared_expect: bool =
    open_permits.forall(p1 =>
      open_permits.forall(p2 =>
        p1.expect_epoch == p2.expect_epoch and p1.expect_holder == p2.expect_holder
      )
    )

  val inv_permit_winner_authorized: bool =
    reg_head.origin_class != "Permit" or
    authorized.exists(p => p.attempt == reg_head.origin_attempt and p.expect_epoch + 1 == reg_head.epoch)

  val inv_origin_lineage_preserved: bool =
    reg_head.permit_attempt == 0 or (
      reg_head.permit_epoch >= 2 and
      reg_head.permit_epoch <= reg_head.epoch and
      authorized.exists(p => p.attempt == reg_head.permit_attempt and p.expect_epoch + 1 == reg_head.permit_epoch)
    )

  val inv_predecessor_consumed_once: bool =
    1.to(MAX_EPOCH).forall(e => consumed_predecessors.get(e).size() <= 1)

  val all_invariants: bool = all {
    inv_origin_epoch_matches_head,
    inv_unique_holder_per_epoch,
    inv_rule_f1_shared_expect,
    inv_permit_winner_authorized,
    inv_origin_lineage_preserved,
    inv_predecessor_consumed_once,
  }
}
