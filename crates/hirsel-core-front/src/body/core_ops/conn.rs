//! Units and connections (CONC-15, CONC-17): `task.spawn_unit` and the `conn`
//! module, lowered onto their runtime rows.
use super::*;

const WORD: Ty = Ty::Scalar(Scalar::U64);
const BYTE: Ty = Ty::Scalar(Scalar::U8);

impl Cx<'_, '_, '_> {
    /// `conn.new[T](cap, bytes b)`: charge `64 + cap × size(T) + b`.
    pub(super) fn runtime_conn_new(
        &mut self,
        item: TyId,
        cap: ValueId,
        b: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        let size = self.layout_size(item, true, n)?;
        let base = self.konst(WORD, hirsel_core_layout::charge::CONNECTION_BASE, n)?;
        let header = self.runtime_charge(cap, size, base, n)?;
        let one = self.konst(WORD, 1, n)?;
        let requested = self.runtime_charge(b, one, header, n)?;
        let [client, inbox, available, ..] = self.runtime_row(
            RtOp::ConnNew { item: id },
            &[Arg::Value(cap), Arg::Value(b)],
            n,
        )?;
        let ok = self.runtime_nonzero(Ty::Ptr, client, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, pair| {
                let TyKind::Tuple(types) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], pair, n)
                {
                    for (i, (ty, handle)) in types.into_iter().zip([client, inbox]).enumerate() {
                        if let Some(field) = cx.runtime_field(to, &[Step::Field(i as u32)], ty, n) {
                            cx.runtime_store(field, &[Step::Field(0)], Ty::Ptr, handle, n);
                        }
                    }
                }
            },
            |cx, to, capacity| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], capacity, n)
                {
                    cx.runtime_capacity(to, requested, available, n);
                }
            },
        )
    }

    /// `conn.send` and `conn.try_send`. The runtime leaves a closed or full item
    /// in its place, and the error then owns it, as a channel send's does.
    pub(super) fn runtime_conn_send(
        &mut self,
        wait: bool,
        item: TyId,
        client: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let client = self.runtime_handle(client, n);
        let (id, input) = self.runtime_place(item, value, n)?;
        let op = if wait {
            RtOp::ConnSend { item: id }
        } else {
            RtOp::ConnTrySend { item: id }
        };
        let [outcome, ..] = self.runtime_row(op, &[Arg::Value(client), Arg::Address(input)], n)?;
        let ok = self.runtime_compare(BYTE, outcome, 0, n)?;
        self.consume(value, n);
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, error| {
                let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                else {
                    return;
                };
                if wait {
                    // Closed[T] = Closed(T)
                    cx.runtime_case_payload(to, 0, item, value, n);
                    return;
                }
                // SendError[T] = Full(T) | Closed(T); the row answers 2 for full.
                let Some(full) = cx.runtime_compare(BYTE, outcome, 2, n) else {
                    return;
                };
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(full, yes, no, n);
                cx.start(yes);
                cx.runtime_case_payload(to, 0, item, value, n);
                cx.jump(join, &[], n);
                cx.start(no);
                cx.runtime_case_payload(to, 1, item, value, n);
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }

    /// `conn.receive` (`InboxError = Closed | Capacity(Capacity)`) and
    /// `conn.try_receive` (`InboxPoll = Empty | Closed | Capacity(Capacity)`).
    pub(super) fn runtime_conn_receive(
        &mut self,
        wait: bool,
        item: TyId,
        inbox: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let inbox = self.runtime_handle(inbox, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let op = if wait {
            RtOp::ConnReceive { item: id }
        } else {
            RtOp::ConnTryReceive { item: id }
        };
        let [outcome, requested, available, ..] =
            self.runtime_row(op, &[Arg::Value(inbox), Arg::Address(output)], n)?;
        let ok = self.runtime_compare(BYTE, outcome, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                else {
                    return;
                };
                // The error's cases in order, and the row's outcome for each.
                let cases: &[u64] = if wait { &[1, 2] } else { &[3, 1, 2] };
                let capacity_case = cases.len() as u32 - 1;
                let join = cx.new_block(&[]);
                for (case, &code) in cases.iter().enumerate() {
                    let case = case as u32;
                    let last = case == capacity_case;
                    let next = if last {
                        None
                    } else {
                        let Some(hit) = cx.runtime_compare(BYTE, outcome, code, n) else {
                            return;
                        };
                        let yes = cx.new_block(&[]);
                        let no = cx.new_block(&[]);
                        cx.runtime_branch(hit, yes, no, n);
                        cx.start(yes);
                        Some(no)
                    };
                    if last {
                        cx.inst(Inst::SetCase { address: to, case }, n);
                        let capacity = cx.capacity_ty(n);
                        if let Some(at) =
                            cx.runtime_field(to, &[Step::CaseField { case, field: 0 }], capacity, n)
                        {
                            cx.runtime_capacity(at, requested, available, n);
                        }
                    } else {
                        cx.runtime_case_payload(to, case, UNIT, None, n);
                    }
                    cx.jump(join, &[], n);
                    if let Some(no) = next {
                        cx.start(no);
                    }
                }
                cx.start(join);
            },
        )
    }

    /// `task.spawn_unit`: `task.spawn`'s operands and entry glue, with the
    /// runtime answering which admission refused (CONC-15).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn runtime_spawn_unit(
        &mut self,
        x: TyId,
        r: TyId,
        tasks: ValueId,
        bytes: ValueId,
        value: Option<ValueId>,
        function: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let xrep = self.rep_at(x, n)?;
        let rid = self.runtime_type(r, n)?;
        let (xid, input, function) = self.task_entry_adapter(x, r, function, value, n)?;
        let [task, requested, available, ..] = self.runtime_row(
            RtOp::SpawnUnit { x: xid, r: rid },
            &[
                Arg::Value(bytes),
                Arg::Address(input),
                Arg::Value(function),
                Arg::Value(tasks),
            ],
            n,
        )?;
        self.consume(value, n);
        let ok = self.runtime_nonzero(Ty::Ptr, task, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, task, n);
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(types) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                {
                    if let Some(capacity) = cx.runtime_field(to, &[Step::Field(0)], types[0], n) {
                        cx.runtime_capacity(capacity, requested, available, n);
                    }
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(1)], x, n) {
                        cx.install(at, xrep, value, n);
                    }
                }
            },
        )
    }
}
