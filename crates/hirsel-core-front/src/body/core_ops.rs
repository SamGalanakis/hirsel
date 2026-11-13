//! Binding of canonical core declarations to their call-site kernels.
//! Identity is resolved once when the program's declarations are published.
use super::*;
use hirsel_core_ir::{Counted, RtOp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kernel {
    NewOwner(Option<Counted>),
    Share(Counted),
    TryUnwrap(Counted),
    Unbox,
    Spawn,
    CheckCancel,
    Cancel,
    Join,
    ChannelNew,
    ChannelSender,
    Send,
    TrySend,
    Receive,
    Blocked(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    Numeric,
    Value,
    Language,
    PendingCode,
    Runtime(Kernel),
}
struct Entry {
    module: &'static str,
    name: &'static str,
    lane: Lane,
}
macro_rules! operations {
    ($(($module:literal, $name:literal, $lane:expr)),* $(,)?) => {
        const OPERATIONS: &[Entry] = &[$(Entry { module: $module, name: $name, lane: $lane }),*];
    };
}

operations! {
    ("arc", "new", Lane::Runtime(Kernel::NewOwner(Some(Counted::Arc)))),
    ("arc", "share", Lane::Runtime(Kernel::Share(Counted::Arc))),
    ("arc", "try_unwrap", Lane::Runtime(Kernel::TryUnwrap(Counted::Arc))),
    ("args", "len", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("args", "get", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("box", "new", Lane::Runtime(Kernel::NewOwner(None))),
    ("box", "unbox", Lane::Runtime(Kernel::Unbox)),
    ("channel", "new", Lane::Runtime(Kernel::ChannelNew)),
    ("channel", "sender", Lane::Runtime(Kernel::ChannelSender)),
    ("channel", "send", Lane::Runtime(Kernel::Send)),
    ("channel", "try_send", Lane::Runtime(Kernel::TrySend)),
    ("channel", "receive", Lane::Runtime(Kernel::Receive)),
    ("char", "from_u32", Lane::Numeric),
    ("char", "to_u32", Lane::Numeric),
    ("core", "fail", Lane::Language),
    ("core", "replace", Lane::Value),
    ("core", "clone", Lane::Language),
    ("core", "clone_in", Lane::Language),
    ("core", "build_in", Lane::Language),
    ("core", "finish", Lane::Language),
    ("core", "finish_trim", Lane::Language),
    ("core", "build_stream_in", Lane::Language),
    ("core", "emit", Lane::Language),
    ("dest", "put", Lane::Language),
    ("float", "mul_add", Lane::Numeric),
    ("float", "total_order", Lane::Numeric),
    ("float", "to_int", Lane::Numeric),
    ("float", "from_int", Lane::Numeric),
    ("float", "widen", Lane::Numeric),
    ("float", "narrow", Lane::Numeric),
    ("float", "to_bits32", Lane::Numeric),
    ("float", "to_bits64", Lane::Numeric),
    ("float", "from_bits32", Lane::Numeric),
    ("float", "from_bits64", Lane::Numeric),
    ("host", "open", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "read", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "write", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "close", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "try_lock", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "unlock", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "mkdir", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "mkdtemp", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "remove", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "remove_tree", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "wall_ms", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("host", "mono_ns", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("indexed", "get", Lane::Language),
    ("int", "wrapping_add", Lane::Numeric),
    ("int", "wrapping_sub", Lane::Numeric),
    ("int", "wrapping_mul", Lane::Numeric),
    ("int", "wrapping_neg", Lane::Numeric),
    ("int", "wrapping_shl", Lane::Numeric),
    ("int", "checked_add", Lane::Numeric),
    ("int", "checked_sub", Lane::Numeric),
    ("int", "checked_mul", Lane::Numeric),
    ("int", "checked_div", Lane::Numeric),
    ("int", "saturating_add", Lane::Numeric),
    ("int", "saturating_sub", Lane::Numeric),
    ("int", "saturating_mul", Lane::Numeric),
    ("int", "mul_wide", Lane::Numeric),
    ("int", "leading_zeros", Lane::Numeric),
    ("int", "widen", Lane::Numeric),
    ("int", "narrow", Lane::Numeric),
    ("int", "try_narrow", Lane::Numeric),
    ("int", "truncate", Lane::Numeric),
    ("int", "parse", Lane::Numeric),
    ("list", "new", Lane::Value),
    ("list", "with_capacity", Lane::Value),
    ("list", "len", Lane::Value),
    ("list", "capacity", Lane::Value),
    ("list", "reserve", Lane::Value),
    ("list", "get", Lane::Value),
    ("list", "slice", Lane::Value),
    ("list", "push", Lane::Value),
    ("list", "pop", Lane::Value),
    ("list", "set", Lane::Value),
    ("list", "clear", Lane::Value),
    ("list", "fill", Lane::PendingCode),
    ("list", "append", Lane::PendingCode),
    ("list", "sum", Lane::PendingCode),
    ("map", "new", Lane::Value),
    ("map", "len", Lane::Value),
    ("map", "get", Lane::Value),
    ("map", "insert", Lane::Value),
    ("map", "set", Lane::Value),
    ("map", "remove", Lane::Value),
    ("option", "unwrap", Lane::Value),
    ("rc", "new", Lane::Runtime(Kernel::NewOwner(Some(Counted::Rc)))),
    ("rc", "share", Lane::Runtime(Kernel::Share(Counted::Rc))),
    ("rc", "try_unwrap", Lane::Runtime(Kernel::TryUnwrap(Counted::Rc))),
    ("result", "unwrap", Lane::Value),
    ("seq", "len", Lane::Language),
    ("seq", "nth", Lane::Language),
    ("seq", "index_in", Lane::Language),
    ("seq", "get", Lane::Language),
    ("span", "builder", Lane::Language),
    ("span", "with_capacity", Lane::Language),
    ("span", "push", Lane::Language),
    ("span", "finish", Lane::Language),
    ("span", "len", Lane::Language),
    ("span", "get", Lane::Language),
    ("stderr", "write", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("stdout", "write", Lane::Runtime(Kernel::Blocked("provider extern rows are not built (FOR FORM)"))),
    ("string", "new", Lane::PendingCode),
    ("string", "from", Lane::PendingCode),
    ("string", "from_utf8", Lane::PendingCode),
    ("string", "into_bytes", Lane::PendingCode),
    ("string", "as_bytes", Lane::PendingCode),
    ("string", "slice", Lane::PendingCode),
    ("string", "next_scalar", Lane::PendingCode),
    ("string", "byte_len", Lane::PendingCode),
    ("string", "push", Lane::PendingCode),
    ("string", "push_char", Lane::PendingCode),
    ("string", "push_u64", Lane::PendingCode),
    ("string", "reserve", Lane::PendingCode),
    ("string", "truncate", Lane::PendingCode),
    ("task", "spawn", Lane::Runtime(Kernel::Spawn)),
    ("task", "join", Lane::Runtime(Kernel::Join)),
    ("task", "cancel", Lane::Runtime(Kernel::Cancel)),
    ("task", "check_cancel", Lane::Runtime(Kernel::CheckCancel)),
    ("text", "len", Lane::Language),
    ("text", "as_string", Lane::Language),
    ("text", "copy_in", Lane::Language),
    ("unique", "new", Lane::Runtime(Kernel::Blocked("Unique runtime identity binding is not specified (FOR FORM)"))),
    ("writer", "append", Lane::Language),
    ("writer", "scalar", Lane::Language),
    ("writer", "push", Lane::Language),
    ("writer", "close", Lane::Language),
    ("writer", "completed", Lane::Language),
    ("writer", "last_completed", Lane::Language),
}

pub(crate) fn bind(fns: &crate::Rows<FnDecl>) -> Vec<Option<Kernel>> {
    fns.iter()
        .map(|f| {
            if !f.path.package_is("core", "1") || f.body.is_some() {
                return None;
            }
            OPERATIONS.iter().find_map(|entry| {
                (f.path.0.modules == [entry.module] && f.path.0.name == entry.name)
                    .then_some(entry.lane)
                    .and_then(|lane| match lane {
                        Lane::Runtime(kernel) => Some(kernel),
                        _ => None,
                    })
            })
        })
        .collect()
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn core_operation(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let kernel = self.p.core_operations[f]?;
        Some(self.runtime_kernel(kernel, types, vals, result, n))
    }

    fn runtime_kernel(
        &mut self,
        kernel: Kernel,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        match kernel {
            Kernel::NewOwner(kind) => self.runtime_new_owner(kind, types[0], vals[0], result, n),
            Kernel::Share(kind) => self.runtime_share(kind, vals[0]?, result, n),
            Kernel::TryUnwrap(kind) => self.runtime_try_unwrap(kind, types[0], vals[0]?, result, n),
            Kernel::Unbox => self.runtime_unbox(types[0], vals[0]?, n),
            Kernel::Blocked(what) => {
                self.gap(n, what);
                None
            }
            Kernel::Spawn => {
                self.runtime_spawn(types[0], types[1], vals[1]?, vals[2], vals[3]?, result, n)
            }
            Kernel::CheckCancel => {
                self.runtime_row(RtOp::CheckCancel, &[], n)?;
                None
            }
            Kernel::Cancel => {
                let handle = self.runtime_handle(vals[0]?, n);
                self.runtime_row(RtOp::Cancel, &[Arg::Value(handle)], n)?;
                None
            }
            Kernel::ChannelSender => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [handle, ..] =
                    self.runtime_row(RtOp::ChannelSender, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, handle, n)
            }
            Kernel::ChannelNew => self.runtime_channel(types[0], vals[0]?, result, n),
            Kernel::Send | Kernel::TrySend => {
                self.runtime_send(kernel, types[0], vals[0]?, vals[1], result, n)
            }
            Kernel::Receive => self.runtime_receive(types[0], vals[0]?, result, n),
            Kernel::Join => self.runtime_join(types[0], vals[0]?, result, n),
        }
    }

    /// Runtime rows use their own fixed ABI, with the context supplied by the engine.
    /// A call's failure edge is captured before any normal-result custody changes.
    fn runtime_row(&mut self, op: RtOp, args: &[Arg], n: NodeId) -> Option<[ValueId; 3]> {
        if !self.live() {
            return None;
        }
        let signature = self.e.sig(op.signature());
        let mut output = [ValueId(0); 3];
        let count = self.e.module.signatures[signature.index()].results.len();
        for (i, value) in output.iter_mut().enumerate().take(count) {
            *value = self.value(self.e.module.signatures[signature.index()].results[i]);
        }
        let args = PoolRange::push(&mut self.b.pools.args, args.iter().copied());
        let results = PoolRange::push(&mut self.b.pools.values, output[..count].iter().copied());
        let fail = if op.fails() { self.fail_to(n) } else { None };
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Runtime(op),
            signature,
            args,
            results,
            fail,
        });
        self.inst(Inst::Call { call }, n);
        Some(output)
    }

    fn runtime_handle(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let field = self.address(AddressBase::Value(value), &[], PlaceTy::Value(Ty::Ptr), n);
        self.read_bits(field, Ty::Ptr, n)
    }

    fn runtime_type(&mut self, ty: TyId, n: NodeId) -> Option<TypeId> {
        let id = self.e.type_row(self.p, &mut self.t, ty);
        if id.is_none() {
            self.gap(n, "runtime operand layout is not built");
        }
        id
    }

    fn runtime_place(
        &mut self,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<(TypeId, AddressId)> {
        let id = self.runtime_type(ty, n)?;
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let place = match rep {
            Rep::Place(_) => {
                self.address(AddressBase::Value(value?), &[], PlaceTy::Declared(id), n)
            }
            _ => {
                let slot = self.new_slot(PlaceTy::Declared(id), n);
                if let Some(stored) = rep.stored() {
                    let field =
                        self.address(AddressBase::Address(slot), &[Step::Field(0)], stored, n);
                    self.install(field, rep, value, n);
                }
                slot
            }
        };
        Some((id, place))
    }

    fn runtime_field(
        &mut self,
        to: AddressId,
        steps: &[Step],
        ty: TyId,
        n: NodeId,
    ) -> Option<AddressId> {
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let place = rep.stored()?;
        let at = self.address(AddressBase::Address(to), steps, place, n);
        // A builtin handle field is stored directly in its parent's recipe.
        // Rebase its carrier before addressing fields of that carrier.
        if let PlaceTy::Declared(_) = place {
            let pointer = self.pointer(at, n);
            Some(self.address(AddressBase::Value(pointer), &[], place, n))
        } else {
            Some(at)
        }
    }

    fn runtime_store(&mut self, to: AddressId, steps: &[Step], ty: Ty, value: ValueId, n: NodeId) {
        let address = self.address(AddressBase::Address(to), steps, PlaceTy::Value(ty), n);
        self.inst(Inst::Store { address, value }, n);
    }

    fn runtime_carrier(&mut self, ty: TyId, handle: ValueId, n: NodeId) -> Option<ValueId> {
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.runtime_store(to, &[Step::Field(0)], Ty::Ptr, handle, n);
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    fn runtime_compare(&mut self, ty: Ty, value: ValueId, bits: u64, n: NodeId) -> Option<ValueId> {
        let right = self.konst(ty, bits, n)?;
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result,
                op: CompareOp::Eq,
                left: value,
                right,
            },
            n,
        );
        Some(result)
    }

    fn runtime_nonzero(&mut self, ty: Ty, value: ValueId, n: NodeId) -> Option<ValueId> {
        let right = self.konst(ty, 0, n)?;
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result,
                op: CompareOp::Ne,
                left: value,
                right,
            },
            n,
        );
        Some(result)
    }

    fn runtime_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }

    /// Both arms initialize an ordinary Result's tag and active payload. Its owner
    /// starts only after the join, so failure before the call commits cleans inputs.
    fn runtime_result(
        &mut self,
        ty: TyId,
        ok: ValueId,
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, TyId),
        error: impl FnOnce(&mut Self, AddressId, TyId),
    ) -> Option<ValueId> {
        let TyKind::Adt(_, arguments) = self.t.kind(ty).clone() else {
            return None;
        };
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(ok, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        success(self, to, arguments[0]);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        error(self, to, arguments[1]);
        self.jump(join, &[], n);
        self.start(join);
        if !self.live() {
            return None;
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    fn runtime_channel(
        &mut self,
        item: TyId,
        cap: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        // Charge-v1 uses logical size, never the carrier's native stride.
        if has_param(&self.t, item) {
            self.gap(
                n,
                "generic runtime logical-charge substitution is not built",
            );
            return None;
        }
        let logical = self.runtime_logical(item, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let size = self.konst(u, logical, n)?;
        let base = self.konst(u, hirsel_core_layout::charge::CHANNEL_BASE, n)?;
        let requested = self.runtime_charge(cap, size, base, n)?;
        let [sender, receiver, available] =
            self.runtime_row(RtOp::ChannelNew { item: id }, &[Arg::Value(cap)], n)?;
        let ok = self.runtime_nonzero(Ty::Ptr, sender, n)?;
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
                    for (i, (ty, handle)) in types.into_iter().zip([sender, receiver]).enumerate() {
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

    /// Saturated exact charge. Overflow is a recoverable refusal by the runtime;
    /// it is not a source Overflow failure while building the Capacity record.
    fn runtime_charge(
        &mut self,
        count: ValueId,
        size: ValueId,
        base: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let product = self.value(u);
        let mul_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: product,
                flag: Some(mul_overflow),
                op: ArithOp::Mul,
                mode: ArithMode::Checked,
                left: count,
                right: size,
            },
            n,
        );
        let total = self.value(u);
        let add_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: total,
                flag: Some(add_overflow),
                op: ArithOp::Add,
                mode: ArithMode::Checked,
                left: product,
                right: base,
            },
            n,
        );
        let overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Binary {
                result: overflow,
                op: BinaryOp::BitOr,
                left: mul_overflow,
                right: add_overflow,
                fail: None,
            },
            n,
        );
        let max = self.konst(u, u64::MAX, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[u]);
        self.runtime_branch(overflow, yes, no, n);
        self.start(yes);
        self.jump(join, &[max], n);
        self.start(no);
        self.jump(join, &[total], n);
        self.start(join);
        Some(
            self.b.blocks[join.index()]
                .params
                .get(&self.b.pools.values)?[0],
        )
    }

    fn runtime_logical(&mut self, ty: TyId, n: NodeId) -> Option<u64> {
        if has_param(&self.t, ty) {
            self.gap(
                n,
                "generic runtime logical-charge substitution is not built",
            );
            return None;
        }
        let mark = self.e.module.types.len();
        let id = self.runtime_type(ty, n)?;
        let logical = self
            .e
            .recipe(id, mark)
            .and_then(|recipe| recipe.logical_size);
        let logical = if self.e.is_resource(id) {
            Some(8)
        } else {
            logical
        };
        if logical.is_none() {
            self.gap(n, "runtime logical size is not built");
        }
        logical
    }

    fn runtime_capacity(
        &mut self,
        to: AddressId,
        requested: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        for (field, value) in [requested, available].into_iter().enumerate() {
            self.runtime_store(
                to,
                &[Step::Field(field as u32)],
                Ty::Scalar(Scalar::U64),
                value,
                n,
            );
        }
    }

    fn runtime_send(
        &mut self,
        kernel: Kernel,
        item: TyId,
        sender: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let sender = self.runtime_handle(sender, n);
        let (id, input) = self.runtime_place(item, value, n)?;
        let op = if kernel == Kernel::Send {
            RtOp::Send { item: id }
        } else {
            RtOp::TrySend { item: id }
        };
        let [status, ..] = self.runtime_row(op, &[Arg::Value(sender), Arg::Address(input)], n)?;
        let ok = if kernel == Kernel::Send {
            status
        } else {
            self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?
        };
        // Both a published item and a returned error now own the taken value.
        self.consume(value, n);
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, error| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    if kernel == Kernel::Send {
                        cx.runtime_closed(to, item, value, n);
                    } else {
                        let full = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n);
                        if let Some(full) = full {
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
                        }
                    }
                }
            },
        )
    }

    fn runtime_case_payload(
        &mut self,
        to: AddressId,
        case: u32,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) {
        self.inst(Inst::SetCase { address: to, case }, n);
        let rep = match self.rep(ty) {
            Ok(r) => r,
            Err(what) => {
                self.gap(n, what);
                return;
            }
        };
        if let Some(at) = self.runtime_field(to, &[Step::CaseField { case, field: 0 }], ty, n) {
            self.install(at, rep, value, n);
        }
    }

    fn runtime_closed(&mut self, to: AddressId, ty: TyId, value: Option<ValueId>, n: NodeId) {
        self.runtime_case_payload(to, 0, ty, value, n);
    }

    fn runtime_receive(
        &mut self,
        item: TyId,
        receiver: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let receiver = self.runtime_handle(receiver, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [ok, ..] = self.runtime_row(
            RtOp::Receive { item: id },
            &[Arg::Value(receiver), Arg::Address(output)],
            n,
        )?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    cx.runtime_closed(to, UNIT, None, n);
                }
            },
        )
    }

    fn runtime_read(&mut self, from: AddressId, ty: TyId, n: NodeId) -> Option<ValueId> {
        match self.rep(ty) {
            Ok(Rep::Place(_)) => Some(self.pointer(from, n)),
            Ok(Rep::Scalar(t)) => {
                let field = self.address(
                    AddressBase::Address(from),
                    &[Step::Field(0)],
                    PlaceTy::Value(t),
                    n,
                );
                Some(self.read_bits(field, t, n))
            }
            Ok(Rep::None) => None,
            Ok(Rep::View(_)) => {
                let field = self.address(
                    AddressBase::Address(from),
                    &[Step::Field(0)],
                    PlaceTy::Value(Ty::Ptr),
                    n,
                );
                Some(self.read_bits(field, Ty::Ptr, n))
            }
            Err(what) => {
                self.gap(n, what);
                None
            }
        }
    }

    fn runtime_join(
        &mut self,
        item: TyId,
        task: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let handle = self.runtime_handle(task, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [status, causes, ..] = self.runtime_row(
            RtOp::Join { r: id },
            &[Arg::Value(handle), Arg::Address(output)],
            n,
        )?;
        self.consume(Some(task), n);
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), status, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let value = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, value, n);
            },
            |cx, to, error| {
                let TyKind::Adt(decl, _) = cx.t.kind(error).clone() else {
                    return;
                };
                let DeclBody::Variant(cases) = &cx.p.decls[decl.index()].body else {
                    return;
                };
                let cause_ty = cases[0].fields[0].ty;
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                {
                    if let Some(failed) = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n) {
                        let yes = cx.new_block(&[]);
                        let no = cx.new_block(&[]);
                        let join = cx.new_block(&[]);
                        cx.runtime_branch(failed, yes, no, n);
                        for (block, case) in [(yes, 0), (no, 1)] {
                            cx.start(block);
                            cx.inst(Inst::SetCase { address: to, case }, n);
                            if let Some(field) = cx.runtime_field(
                                to,
                                &[Step::CaseField { case, field: 0 }],
                                cause_ty,
                                n,
                            ) {
                                cx.runtime_store(field, &[Step::Field(0)], Ty::Ptr, causes, n);
                            }
                            cx.jump(join, &[], n);
                        }
                        cx.start(join);
                    }
                }
            },
        )
    }
}

#[cfg(test)]
pub(crate) fn assert_declarations(p: &Program<'_>) {
    use std::collections::BTreeSet;
    let actual: BTreeSet<_> = p
        .fns
        .iter()
        .filter(|f| f.path.package_is("core", "1") && f.body.is_none())
        .map(|f| (f.path.0.modules[0].as_str(), f.path.0.name.as_str()))
        .collect();
    let recorded: BTreeSet<_> = OPERATIONS.iter().map(|e| (e.module, e.name)).collect();
    assert_eq!(
        recorded.len(),
        OPERATIONS.len(),
        "duplicate core operation binding"
    );
    assert_eq!(
        actual, recorded,
        "assign each declaration to its lowering lane or pending ordinary code"
    );
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    #[allow(clippy::too_many_arguments)]
    fn runtime_spawn(
        &mut self,
        x: TyId,
        r: TyId,
        bytes: ValueId,
        value: Option<ValueId>,
        function: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // Plain callable values already use typed address inputs and a destination
        // result (callable_abi), exactly the runtime's child-entry convention.
        let logical = self.runtime_logical(r, n)?;
        let xrep = self.rep(x).ok()?;
        let rid = self.runtime_type(r, n)?;
        let (xid, input) = self.runtime_place(x, value, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        let base = self.konst(
            u,
            hirsel_core_layout::charge::task(logical, 0).unwrap_or(u64::MAX),
            n,
        )?;
        let requested = self.runtime_charge(bytes, one, base, n)?;
        let [task, available, ..] = self.runtime_row(
            RtOp::Spawn { x: xid, r: rid },
            &[Arg::Value(bytes), Arg::Address(input), Arg::Value(function)],
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

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn owner_charge(&mut self, ty: TyId, kind: Option<Counted>, n: NodeId) -> Option<u64> {
        let logical = self.runtime_logical(ty, n)?;
        let charge = if kind.is_some() {
            hirsel_core_layout::charge::counted(logical)
        } else {
            hirsel_core_layout::charge::boxed(logical)
        };
        if charge.is_none() {
            self.gap(n, "allocation charge overflow flag is not built (FOR FORM)");
        }
        charge
    }

    fn runtime_new_owner(
        &mut self,
        kind: Option<Counted>,
        item: TyId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let logical_charge = self.owner_charge(item, kind, n)?;
        let rep = self.rep(item).ok()?;
        let id = self.runtime_type(item, n)?;
        let payload = self.e.size(id);
        let align = if self.e.is_resource(id) {
            hirsel_core_layout::POINTER
        } else {
            self.e.block(id).1
        };
        let header = if kind.is_some() { 8 } else { 0 };
        let Some(size) = payload.checked_add(header) else {
            self.gap(n, "owner native storage size exceeds the engine's range");
            return None;
        };
        let u = Ty::Scalar(Scalar::U64);
        let charge = self.konst(u, logical_charge, n)?;
        let size = self.konst(u, size, n)?;
        let node = self.value(Ty::Ptr);
        let available = self.value(u);
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: node,
                available,
                region_entry: None,
                kind: kind.map_or(AllocKind::Own, AllocKind::Counted),
                place: AllocPlace::Current,
                charge,
                overflow: None,
                size,
                align: align.max(if kind.is_some() { 8 } else { 1 }),
                fail,
            },
            n,
        );
        // Allocation commits normally to either the new owner or the handback
        // error. A grant failure enters cleanup before this consumption.
        self.consume(value, n);
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, owner| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], owner, n)
                {
                    cx.runtime_store(at, &[Step::Field(0)], Ty::Ptr, node, n);
                    let at = cx.owner_payload(node, id, kind.is_some(), n);
                    if let Some(at) = at {
                        let at = match rep {
                            Rep::Scalar(_) | Rep::View(_) => cx.address(
                                AddressBase::Address(at),
                                &[Step::Field(0)],
                                rep.stored().unwrap(),
                                n,
                            ),
                            _ => at,
                        };
                        cx.install(at, rep, value, n);
                    }
                }
            },
            |cx, to, pair| {
                let TyKind::Tuple(parts) = cx.t.kind(pair).clone() else {
                    return;
                };
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], pair, n)
                {
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(0)], parts[0], n) {
                        cx.runtime_capacity(at, charge, available, n);
                    }
                    if let Some(at) = cx.runtime_field(to, &[Step::Field(1)], item, n) {
                        cx.install(at, rep, value, n);
                    }
                }
            },
        )
    }

    fn owner_payload(
        &mut self,
        node: ValueId,
        id: TypeId,
        counted: bool,
        n: NodeId,
    ) -> Option<AddressId> {
        let place = PlaceTy::Declared(id);
        if counted {
            let offset = self.konst(Ty::Scalar(Scalar::U64), 8, n)?;
            let at = self.address(AddressBase::Value(node), &[Step::Byte(offset)], place, n);
            let pointer = self.pointer(at, n);
            Some(self.address(AddressBase::Value(pointer), &[], place, n))
        } else {
            Some(self.address(AddressBase::Value(node), &[], place, n))
        }
    }

    fn runtime_share(
        &mut self,
        kind: Counted,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let node = self.runtime_handle(value, n);
        self.inst(Inst::Share { node, kind }, n);
        self.runtime_carrier(result, node, n)
    }

    fn runtime_try_unwrap(
        &mut self,
        kind: Counted,
        item: TyId,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // A failed unwrap returns this holder unchanged, without sharing it.
        let charge = self.owner_charge(item, Some(kind), n)?;
        let id = self.runtime_type(item, n)?;
        let node = self.runtime_handle(value, n);
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::TryUnwrap {
                result: ok,
                node,
                kind,
            },
            n,
        );
        self.consume(Some(value), n);
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(payload) = cx.owner_payload(node, id, true, n) {
                    let value = cx.runtime_read(payload, ty, n);
                    cx.runtime_case_payload(to, 0, ty, value, n);
                    if let Some(charge) = cx.konst(Ty::Scalar(Scalar::U64), charge, n) {
                        cx.inst(
                            Inst::Free {
                                block: node,
                                charge,
                            },
                            n,
                        );
                    }
                }
            },
            |cx, to, ty| {
                cx.runtime_case_payload(to, 1, ty, Some(value), n);
            },
        )
    }

    fn runtime_unbox(&mut self, item: TyId, value: ValueId, n: NodeId) -> Option<ValueId> {
        let charge = self.owner_charge(item, None, n)?;
        let rep = self.rep(item).ok()?;
        let id = self.runtime_type(item, n)?;
        let node = self.runtime_handle(value, n);
        let payload = self.owner_payload(node, id, false, n)?;
        self.consume(Some(value), n);
        let value = self.runtime_read(payload, item, n);
        let output = match rep {
            Rep::Place(_) => {
                let to = self.new_slot(PlaceTy::Declared(id), n);
                self.install(to, rep, value, n);
                let value = self.pointer(to, n);
                self.temp(item, Some(value));
                Some(value)
            }
            _ => value,
        };
        let charge = self.konst(Ty::Scalar(Scalar::U64), charge, n)?;
        self.inst(
            Inst::Free {
                block: node,
                charge,
            },
            n,
        );
        output
    }
}
