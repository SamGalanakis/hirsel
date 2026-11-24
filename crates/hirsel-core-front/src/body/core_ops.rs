//! Binding of canonical core declarations to their call-site kernels.
//! Identity is resolved once when the program's declarations are published.
//! The operation table lists core functions declared without a body.
use super::*;
use hirsel_core_ir::{Counted, HostOp, RtOp};
use hirsel_core_layout::flow::Op as FlowOp;
mod argon2;
mod binding;
mod cell;
mod charge;
mod conn;
#[cfg(test)]
mod declarations;
mod flow;
mod media;
mod net_error;
mod provide;
mod secret_kdf;
mod secret_key;
mod udp_runtime;
use binding::OPERATIONS;
pub(crate) use binding::binding;
pub(super) use binding::simd_call;
#[cfg(test)]
pub(crate) use declarations::{assert_declarations, assert_no_ambient_input};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kernel {
    Stream(HostOp),
    StreamTerminal(u8),
    CarrierMove,
    CarrierView,
    StringShorten,
    StringList(&'static str),
    Unique,
    Host(HostOp),
    ReadOnlyDir,
    Net(HostOp),
    Media(HostOp),
    SecretSeal,
    SecretRender,
    RootLimit,
    ArgsLen,
    ArgsGet,
    Env(HostOp),
    /// A virtual provider's construction or its hand's operation (core.md, Provide).
    Provide(HostOp),
    NewOwner(Option<Counted>),
    Share(Counted),
    TryUnwrap(Counted),
    WithUnique(Counted),
    Unbox,
    Spawn,
    TrySpawn,
    CheckCancel,
    Cancel,
    Join,
    ChannelNew,
    SpawnUnit,
    ConnNew,
    ConnClient,
    SecretShare,
    ConnSend,
    ConnTrySend,
    ConnReceive,
    ConnTryReceive,
    LatestNew,
    LatestHandle,
    LatestPublish,
    LatestRead,
    SharedNew,
    SharedShare,
    ChannelSender,
    Send,
    TrySend,
    Receive,
    TryReceive,
    CodeIdentity,
    CodeRevision,
    CodeCallable,
    CodeDescribe,
    Flow(FlowOp),
    Cell(cell::CellOp),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    Charge(charge::Price),
    Numeric,
    Value,
    Language,
    Simd(SimdCall),
    PendingCode,
    Declaration,
    Runtime(Kernel),
}
struct Entry {
    module: &'static str,
    name: &'static str,
    lane: Lane,
    #[cfg(test)]
    class: &'static str,
    #[cfg(test)]
    private: bool,
}
impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn builtin_use(&mut self, f: usize, n: NodeId) {
        let function = &self.p.fns[f];
        let definition = &function.path;
        if self.live()
            && function.body.is_none()
            && hirsel_core_ir::is_versioned_builtin(definition)
        {
            self.inst(
                Inst::BuiltinUse {
                    definition: Box::new(definition.clone()),
                },
                n,
            );
        }
    }

    pub(super) fn core_operation(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let binding = self.p.core_operations[f]?;
        if matches!(binding, i if i == u16::MAX - 3 || i == u16::MAX - 4) {
            let u = Ty::Scalar(Scalar::U64);
            let field = self.konst(u, u64::from(binding == u16::MAX - 4), n)?;
            let [handle, ..] = self.runtime_row(RtOp::LaunchHandle, &[Arg::Value(field)], n)?;
            let has_handle = self.runtime_nonzero(u, handle, n)?;
            return Some(self.runtime_result(
                result,
                has_handle,
                n,
                |cx, to, ty| {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                    {
                        cx.runtime_store(at, &[], u, handle, n);
                    }
                },
                |_, _, _| {},
            ));
        }
        if binding == u16::MAX - 2 {
            return Some(
                self.runtime_row(RtOp::DescribeEmit, &[Arg::Value(vals[0]?)], n)
                    .map(|v| v[0]),
            );
        }
        if binding == u16::MAX - 1 {
            let view = self.runtime_type(result, n)?;
            let output = self.new_slot(PlaceTy::Declared(view), n);
            self.runtime_row(RtOp::LaunchTarget { view }, &[Arg::Address(output)], n)?;
            return Some(Some(self.pointer(output, n)));
        }
        if binding == u16::MAX {
            return Some(
                self.runtime_row(
                    RtOp::LaunchWord,
                    &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
                    n,
                )
                .map(|v| v[0]),
            );
        }
        let entry = &OPERATIONS[binding as usize];
        match entry.lane {
            Lane::Charge(price) => Some(self.charge_value(price, vals, result, n)),
            Lane::Runtime(kernel) => Some(self.runtime_kernel(kernel, f, types, vals, result, n)),
            Lane::PendingCode => self.pending_code(entry, f, types, vals, result, n),
            _ => None,
        }
    }

    fn pending_code(
        &mut self,
        entry: &Entry,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let args = self.kids(n);
        match (entry.module, entry.name) {
            ("core", "isolate") => {
                let argument = args.get(1).copied().unwrap_or(n);
                let forbidden = vals
                    .first()
                    .copied()
                    .flatten()
                    .is_some_and(|value| self.isolate_descriptor_forbidden(Some(value)))
                    || args.get(1).is_some_and(|arg| {
                        self.p
                            .target(self.at, *arg)
                            .is_some_and(|target| match target {
                                Target::Binder(slot) => {
                                    let var = self.var(*slot);
                                    matches!(var.origin, Origin::ViewParam | Origin::MutParam)
                                        && self.contains_callback_kind::<true>(var.ty)
                                }
                                _ => false,
                            })
                    });
                if forbidden {
                    self.refuse(
                        "FAIL-16",
                        argument,
                        "isolate's body may not capture a mutable place",
                    );
                } else {
                    let body_ty = self.p.fns[f]
                        .params
                        .first()
                        .map(|p| subst(&mut self.t, p.ty, types))
                        .unwrap_or(ERROR);
                    if let Some(body) = vals.first().copied().flatten() {
                        return Some(self.lower_isolate(body_ty, body, result, n));
                    }
                }
                Some(None)
            }
            _ => None,
        }
    }

    /// Lower `core.isolate`'s value boundary (FORM #101).  The callback's failure
    /// successor enters `handler`, whose first instruction is the catch row; the
    /// ordinary result slot is initialized independently on each of the three exits.
    fn lower_isolate(
        &mut self,
        body_ty: TyId,
        body: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let Some((_, inner, Some(capacity))) = self.result_parts(result) else {
            self.gap(n, "isolate result shape is not built");
            return None;
        };
        let Some((_, payload, Some(causes))) = self.result_parts(inner) else {
            self.gap(n, "isolate result shape is not built");
            return None;
        };
        let (Ok(Rep::Place(result_id)), Ok(Rep::Place(inner_id)), Ok(Rep::Place(capacity_id))) =
            (self.rep(result), self.rep(inner), self.rep(capacity))
        else {
            self.gap(n, "isolate result representation is not built");
            return None;
        };
        if !self.live() {
            return None;
        }

        let to = self.new_slot(PlaceTy::Declared(result_id), n);
        let [entered, room, ..] = match self.runtime_row(RtOp::IsolateEnter, &[], n) {
            Some(values) => values,
            None => return None,
        };
        let body_block = self.new_block(&[]);
        let refused_block = self.new_block(&[]);
        let handler = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(entered, body_block, refused_block, n);

        self.start(body_block);
        let returned = self.call_lowered_fail(body_ty, body, n, handler);
        self.runtime_row(RtOp::IsolateLeave, &[], n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let inside = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(inner_id),
            n,
        );
        self.inst(
            Inst::SetCase {
                address: inside,
                case: 0,
            },
            n,
        );
        if let Some(rep) = self.rep(payload).ok()
            && let Some(value) = returned
            && let Some(place) = rep.stored()
        {
            let at = self.address(
                AddressBase::Address(inside),
                &[Step::CaseField { case: 0, field: 0 }],
                place,
                n,
            );
            self.install(at, rep, Some(value), n);
        }
        self.jump(join, &[], n);

        self.start(handler);
        let [caught, ..] = match self.runtime_row(RtOp::IsolateCatch, &[], n) {
            Some(values) => values,
            None => return None,
        };
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let inside = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(inner_id),
            n,
        );
        self.inst(
            Inst::SetCase {
                address: inside,
                case: 1,
            },
            n,
        );
        if let Some(at) =
            self.runtime_field(inside, &[Step::CaseField { case: 1, field: 0 }], causes, n)
        {
            self.runtime_store(at, &[Step::Field(0)], Ty::Ptr, caught, n);
        }
        self.jump(join, &[], n);

        self.start(refused_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let capacity_at = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(capacity_id),
            n,
        );
        let requested = match self.konst(
            Ty::Scalar(Scalar::U64),
            hirsel_core_layout::charge::CAUSE_CELL,
            n,
        ) {
            Some(value) => value,
            None => return None,
        };
        self.runtime_capacity(capacity_at, requested, room, n);
        self.jump(join, &[], n);

        self.start(join);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        // The operation itself synthesizes a value; its caller owns the result slot.
        Some(value)
    }

    fn runtime_kernel(
        &mut self,
        kernel: Kernel,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        match kernel {
            Kernel::Unique => self
                .runtime_row(RtOp::DestId, &[], n)
                .map(|values| values[0]),
            Kernel::StringShorten => {
                // A String's carrier is the List[u8] row's (stride 1, no bound).
                let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
                self.set_carrier_word(vals[0]?, carrier, SeqWord::Len, vals[1]?, n);
                None
            }
            Kernel::CarrierView => self.runtime_carrier_view(vals[0]?, result, n),
            Kernel::CarrierMove => {
                self.consume(vals[0], n);
                self.temp(result, vals[0]);
                vals[0]
            }
            Kernel::StringList(name) => {
                if !self.live() {
                    return None;
                }
                match self.list_value(name, scalar(Scalar::U8), vals, result, n) {
                    Some(value) => value,
                    None => {
                        self.gap(
                            n,
                            match name {
                                "reserve" => "shared List reserve kernel is not built",
                                "append" => "shared List append kernel is not built",
                                _ => "shared List carrier kernel is not built",
                            },
                        );
                        None
                    }
                }
            }

            Kernel::NewOwner(kind) => self.runtime_new_owner(kind, types[0], vals[0], result, n),
            Kernel::Share(kind) => self.runtime_share(kind, vals[0]?, result, n),
            Kernel::TryUnwrap(kind) => self.runtime_try_unwrap(kind, types[0], vals[0]?, result, n),
            Kernel::WithUnique(kind) => self.runtime_with_unique(kind, types[0], vals, result, n),
            Kernel::Unbox => self.runtime_unbox(types[0], vals[0]?, n),
            Kernel::ArgsLen => self
                .runtime_row(RtOp::ArgsLen, &[Arg::Value(vals[0]?)], n)
                .map(|values| values[0]),
            Kernel::ArgsGet => self.runtime_args_get(vals, result, n),
            Kernel::Env(op) => self.runtime_env(op, vals, result, n),
            Kernel::Provide(op) => self.runtime_provide(f, op, vals, result, n),
            Kernel::Stream(op) => self.runtime_stream(op, vals, result, n),
            Kernel::StreamTerminal(stream) => {
                let kind = self.konst(Ty::Scalar(Scalar::U8), stream.into(), n)?;
                let [value, ..] = self.runtime_row(
                    RtOp::Host(HostOp::StreamTerminal),
                    &[Arg::Value(vals[0]?), Arg::Value(kind)],
                    n,
                )?;
                self.runtime_nonzero(Ty::Scalar(Scalar::U64), value, n)
            }
            Kernel::Host(op) => self.runtime_host(op, f, vals, result, n),
            Kernel::ReadOnlyDir => {
                let ty = Ty::Scalar(Scalar::U64);
                let rights = self.konst(ty, hirsel_core_ids::host_op::DIR_READ_ONLY, n)?;
                let output = self.value(ty);
                self.inst(
                    Inst::Binary {
                        result: output,
                        op: BinaryOp::BitOr,
                        left: vals[0]?,
                        right: rights,
                        fail: None,
                    },
                    n,
                );
                Some(output)
            }
            Kernel::Net(op) => self.runtime_net(op, f, vals, result, n),
            Kernel::Media(op) => self.runtime_media(op, f, vals, result, n),
            Kernel::SecretSeal => self.runtime_secret_seal(f, vals, result, n),
            Kernel::SecretRender => self.static_string(b"[secret]", self.bare(result), n),
            Kernel::Spawn | Kernel::TrySpawn => self.runtime_spawn(
                types[0],
                types[1],
                vals[0]?,
                vals[1]?,
                vals[2],
                vals[3]?,
                result,
                kernel == Kernel::TrySpawn,
                n,
            ),
            Kernel::SpawnUnit => self.runtime_spawn_unit(
                types[0], types[1], vals[0]?, vals[1]?, vals[2], vals[3]?, result, n,
            ),
            Kernel::ConnNew => self.runtime_conn_new(types[0], vals[0]?, vals[1]?, result, n),
            Kernel::ConnClient => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [handle, ..] = self.runtime_row(RtOp::ConnClient, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, handle, n)
            }
            Kernel::SecretShare => {
                let key = self.runtime_key(vals[0]?, n);
                let [key, ..] = self.runtime_row(RtOp::SecretShare, &[Arg::Value(key)], n)?;
                self.secret_carrier(result, key, n)
            }
            Kernel::ConnSend | Kernel::ConnTrySend => self.runtime_conn_send(
                kernel == Kernel::ConnSend,
                types[0],
                vals[0]?,
                vals[1],
                result,
                n,
            ),
            Kernel::ConnReceive | Kernel::ConnTryReceive => self.runtime_conn_receive(
                kernel == Kernel::ConnReceive,
                types[0],
                vals[0]?,
                result,
                n,
            ),
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
            Kernel::LatestNew => self.runtime_latest_new(types[0], vals[0], result, n),
            Kernel::SharedNew => self.runtime_shared_new(types[0], vals[0], result, n),
            Kernel::SharedShare => {
                let node = self.runtime_handle(vals[0]?, n);
                let [node, ..] = self.runtime_row(RtOp::SharedShare, &[Arg::Value(node)], n)?;
                self.runtime_carrier(result, node, n)
            }
            Kernel::LatestHandle => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [handle, ..] =
                    self.runtime_row(RtOp::LatestHandle, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, handle, n)
            }
            Kernel::LatestPublish => {
                let handle = self.runtime_handle(vals[0]?, n);
                let node = self.runtime_handle(vals[1]?, n);
                self.runtime_row(
                    RtOp::LatestPublish,
                    &[Arg::Value(handle), Arg::Value(node)],
                    n,
                )?;
                self.consume(vals[1], n);
                None
            }
            Kernel::LatestRead => {
                let handle = self.runtime_handle(vals[0]?, n);
                let [node, ..] = self.runtime_row(RtOp::LatestRead, &[Arg::Value(handle)], n)?;
                self.runtime_carrier(result, node, n)
            }
            Kernel::Send | Kernel::TrySend => {
                self.runtime_send(kernel, types[0], vals[0]?, vals[1], result, n)
            }
            Kernel::Receive => self.runtime_receive(types[0], vals[0]?, result, n),
            Kernel::TryReceive => self.runtime_try_receive(types[0], vals[0]?, result, n),
            Kernel::Join => self.runtime_join(types[0], vals[0]?, result, n),
            Kernel::RootLimit => self.runtime_row(RtOp::RootLimit, &[], n).map(|v| v[0]),
            Kernel::CodeIdentity => self.runtime_code_identity(vals[0]?, result, n),
            Kernel::CodeRevision => self.runtime_code_revision(vals[0]?, result, n),
            Kernel::CodeCallable => self.runtime_code_callable(vals[0]?, result, n),
            Kernel::CodeDescribe => {
                self.refuse(
                    "PROG-9",
                    n,
                    "code.describe requires the name of a top-level function",
                );
                None
            }
            Kernel::Flow(op) => self.runtime_flow(op, types, vals, result, n),
            Kernel::Cell(op) => self.runtime_cell(op, types, vals, result, n),
        }
    }

    /// Runtime rows use their own fixed ABI, with the context supplied by the engine.
    /// A call's failure edge is captured before any normal-result custody changes.
    pub(super) fn runtime_row(
        &mut self,
        op: RtOp,
        args: &[Arg],
        n: NodeId,
    ) -> Option<[ValueId; 4]> {
        self.runtime_outputs(op, args, n)
    }

    fn runtime_outputs<const N: usize>(
        &mut self,
        op: RtOp,
        args: &[Arg],
        n: NodeId,
    ) -> Option<[ValueId; N]> {
        if !self.live() {
            return None;
        }
        let signature = self.e.sig(op.signature());
        let mut output = [ValueId(0); N];
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
            cold: false,
        });
        self.inst(Inst::Call { call }, n);
        Some(output)
    }

    fn runtime_handle(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let field = self.address(AddressBase::Value(value), &[], PlaceTy::Value(Ty::Ptr), n);
        self.read_bits(field, Ty::Ptr, n)
    }

    pub(super) fn runtime_type(&mut self, ty: TyId, n: NodeId) -> Option<TypeId> {
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
        // A zero-sized field has no address: its callers store nothing (FORM #120).
        let Some(place) = rep.stored() else {
            return None;
        };
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

    /// A `Secret` result: a carrier holding the designation's key (TLS-5).
    fn secret_carrier(&mut self, ty: TyId, key: ValueId, n: NodeId) -> Option<ValueId> {
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.runtime_store(to, &[Step::Field(0)], Ty::Scalar(Scalar::U64), key, n);
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
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

    pub(super) fn runtime_nonzero(&mut self, ty: Ty, value: ValueId, n: NodeId) -> Option<ValueId> {
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

    pub(super) fn runtime_branch(
        &mut self,
        condition: ValueId,
        yes: BlockId,
        no: BlockId,
        n: NodeId,
    ) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }

    /// Both arms initialize a Result or Option's tag and active payload. Its owner
    /// starts only after the join, so failure before the call commits cleans inputs.
    pub(super) fn runtime_result(
        &mut self,
        ty: TyId,
        ok: ValueId,
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, TyId),
        error: impl FnOnce(&mut Self, AddressId, TyId),
    ) -> Option<ValueId> {
        // A zero-sized result (`sleep_until`'s unit) has nothing to build: the call
        // already ran.
        if self.rep_at(ty, n)? == Rep::None {
            return None;
        }
        let TyKind::Adt(_, arguments) = self.t.kind(ty).clone() else {
            self.gap(
                n,
                "a runtime result that is not a declared type is not built",
            );
            return None;
        };
        let (yes_case, no_case) = if arguments.len() == 1 { (1, 0) } else { (0, 1) };
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
                case: yes_case,
            },
            n,
        );
        success(self, to, arguments[0]);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: no_case,
            },
            n,
        );
        error(self, to, arguments.get(1).copied().unwrap_or(UNIT));
        self.jump(join, &[], n);
        self.start(join);
        if !self.live() {
            return None;
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    /// The `ty` an admitting runtime operation builds into a fresh slot once
    /// `ok`; a refusal raises `Capacity { requested, available }` first (MEM-21).
    pub(super) fn runtime_admitted(
        &mut self,
        ty: TyId,
        ok: ValueId,
        [requested, available]: [ValueId; 2],
        n: NodeId,
        build: impl FnOnce(&mut Self, AddressId),
    ) -> Option<ValueId> {
        self.raise_unless(ok, requested, available, n);
        if self.rep_at(ty, n)? == Rep::None {
            return None;
        }
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        build(self, to);
        if !self.live() {
            return None;
        }
        let value = self.pointer(to, n);
        self.temp(ty, Some(value));
        Some(value)
    }

    /// `latest.new`: the root admits `LATEST_ROOT` from the current limit and owns
    /// the taken `Arc[T]`; a refusal raises with the Arc still the caller's (CONC-13).
    fn runtime_latest_new(
        &mut self,
        item: TyId,
        arc: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // The root keeps the drop of its holder, `Arc[T]`.
        let Some(arc_decl) = self.p.core_decl("Arc") else {
            self.gap(n, "latest.new without the prelude's Arc is not built");
            return None;
        };
        let holder = self.t.mk(TyKind::Adt(arc_decl, vec![item]));
        let id = self.runtime_type(holder, n)?;
        let node = self.runtime_handle(arc?, n);
        let requested = self.konst(
            Ty::Scalar(Scalar::U64),
            hirsel_core_layout::charge::LATEST_ROOT,
            n,
        )?;
        let [latest, available, ..] =
            self.runtime_row(RtOp::LatestNew { item: id }, &[Arg::Value(node)], n)?;
        let ok = self.runtime_nonzero(Ty::Ptr, latest, n)?;
        self.runtime_admitted(result, ok, [requested, available], n, |cx, to| {
            cx.consume(arc, n);
            cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, latest, n);
        })
    }

    /// `shared.new`: the runtime admits the node, `charge::shared(size(T))`,
    /// from the current limit and moves the taken value into it; a refusal
    /// raises with the value still the caller's (CONC-15).
    fn runtime_shared_new(
        &mut self,
        item: TyId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let size = self.layout_size(item, true, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        let base = self.konst(u, hirsel_core_layout::charge::SHARED_NODE + 7, n)?;
        let (requested, _) = self.runtime_charge_parts(one, size, base, true, n)?;
        let (id, input) = self.runtime_place(item, value, n)?;
        let [node, available, ..] = self.runtime_row(
            RtOp::SharedNew { item: id },
            &[Arg::Address(input), Arg::Value(requested)],
            n,
        )?;
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        self.runtime_admitted(result, ok, [requested, available], n, |cx, to| {
            cx.consume(value, n);
            cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, node, n);
        })
    }

    fn runtime_channel(
        &mut self,
        item: TyId,
        cap: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        // Charge-v1 uses logical size, relocated with the generic instance.
        let size = self.layout_size(item, true, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let base = self.konst(u, hirsel_core_layout::charge::CHANNEL_BASE, n)?;
        let requested = self.runtime_charge(cap, size, base, n)?;
        let [sender, receiver, available, ..] =
            self.runtime_row(RtOp::ChannelNew { item: id }, &[Arg::Value(cap)], n)?;
        let ok = self.runtime_nonzero(Ty::Ptr, sender, n)?;
        self.runtime_admitted(result, ok, [requested, available], n, |cx, to| {
            let TyKind::Tuple(types) = cx.t.kind(result).clone() else {
                return;
            };
            for (i, (ty, handle)) in types.into_iter().zip([sender, receiver]).enumerate() {
                if let Some(field) = cx.runtime_field(to, &[Step::Field(i as u32)], ty, n) {
                    cx.runtime_store(field, &[Step::Field(0)], Ty::Ptr, handle, n);
                }
            }
        })
    }

    pub(super) fn runtime_capacity(
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

    fn runtime_try_receive(
        &mut self,
        item: TyId,
        receiver: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let receiver = self.runtime_handle(receiver, n);
        let id = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(id), n);
        let [status, ..] = self.runtime_row(
            RtOp::TryReceive { item: id },
            &[Arg::Value(receiver), Arg::Address(output)],
            n,
        )?;
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
                let Some(error_to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                else {
                    return;
                };
                let empty = cx.runtime_compare(Ty::Scalar(Scalar::U8), status, 1, n);
                let Some(empty) = empty else { return };
                let yes = cx.new_block(&[]);
                let no = cx.new_block(&[]);
                let join = cx.new_block(&[]);
                cx.runtime_branch(empty, yes, no, n);
                cx.start(yes);
                cx.runtime_case_payload(error_to, 0, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(no);
                cx.runtime_case_payload(error_to, 1, UNIT, None, n);
                cx.jump(join, &[], n);
                cx.start(join);
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

impl<'c, 'a> Cx<'c, 'a, '_> {
    #[allow(clippy::too_many_arguments)]
    fn runtime_spawn(
        &mut self,
        x: TyId,
        r: TyId,
        tasks: ValueId,
        bytes: ValueId,
        value: Option<ValueId>,
        function: ValueId,
        result: TyId,
        try_spawn: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        // Native task-entry glue follows x/r.carried. The fixed runtime C entry
        // still owns input and result buffers by address.
        let logical = self.layout_size(r, true, n)?;
        let xrep = self.rep_at(x, n)?;
        let rid = self.runtime_type(r, n)?;
        let (xid, input, function) = self.task_entry_adapter(x, r, function, value, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        let overhead = self.konst(
            u,
            hirsel_core_layout::charge::TASK_RECORD + hirsel_core_layout::charge::CAUSE_CELL,
            n,
        )?;
        let base = self.runtime_charge(logical, one, overhead, n)?;
        let requested = self.runtime_charge(bytes, one, base, n)?;
        let [task, available, ..] = self.runtime_row(
            RtOp::Spawn { x: xid, r: rid },
            &[
                Arg::Value(bytes),
                Arg::Address(input),
                Arg::Value(function),
                Arg::Value(tasks),
            ],
            n,
        )?;
        let ok = self.runtime_nonzero(Ty::Ptr, task, n)?;
        // `task.spawn` raises with `x` still the caller's; `task.try_spawn` hands
        // it back in `Error((c, x))` (CONC-3).
        if !try_spawn {
            return self.runtime_admitted(result, ok, [requested, available], n, |cx, to| {
                cx.consume(value, n);
                cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, task, n);
            });
        }
        self.consume(value, n);
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
    pub(super) fn owner_charge(
        &mut self,
        ty: TyId,
        kind: Option<Counted>,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let logical = self.layout_size(ty, true, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        // round8(payload + counted header); overflow remains Capacity, not Overflow.
        let extra = self.konst(u, if kind.is_some() { 15 } else { 7 }, n)?;
        self.runtime_charge_parts(logical, one, extra, true, n)
    }

    fn runtime_new_owner(
        &mut self,
        kind: Option<Counted>,
        item: TyId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (charge, overflow) = self.owner_charge(item, kind, n)?;
        let rep = self.rep_at(item, n)?;
        let id = self.runtime_type(item, n)?;
        let payload = self.layout_size(item, false, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let size = if kind.is_some() {
            let one = self.konst(u, 1, n)?;
            let header = self.konst(u, 8, n)?;
            // Physical exhaustion is a host stop after logical admission.
            self.runtime_charge(payload, one, header, n)?
        } else {
            payload
        };
        let node = self.value(Ty::Ptr);
        let available = self.value(u);
        let edge = match kind {
            None => FieldTy::Box(TypeRef(id.0)),
            Some(Counted::Rc) => FieldTy::Rc(TypeRef(id.0)),
            Some(Counted::Arc) => FieldTy::Arc(TypeRef(id.0)),
        };
        self.inst(
            Inst::Alloc {
                result: node,
                available,
                region_entry: None,
                kind: AllocKind::Own { edge },
                place: AllocPlace::Current,
                charge,
                overflow: Some(overflow),
                size,
                align: 8,
            },
            n,
        );
        // A refusal raises with the value still the caller's, so cleanup drops it.
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        self.runtime_admitted(result, ok, [charge, available], n, |cx, to| {
            cx.consume(value, n);
            cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, node, n);
            if let Some(at) = cx.owner_payload(node, id, kind.is_some(), n) {
                let at = match rep {
                    Rep::Scalar(_) => cx.address(
                        AddressBase::Address(at),
                        &[Step::Field(0)],
                        rep.stored().unwrap(),
                        n,
                    ),
                    _ => at,
                };
                cx.install(at, rep, value, n);
            }
        })
    }

    pub(super) fn list_ty(&mut self, item: TyId, n: NodeId) -> Option<TyId> {
        let list = self.prelude("List", n)?;
        Some(self.t.mk(TyKind::Adt(list, vec![item])))
    }

    /// The word at `offset` in `descriptor`.
    pub(super) fn descriptor_word(
        &mut self,
        descriptor: ValueId,
        offset: u64,
        ty: Ty,
        n: NodeId,
    ) -> Option<ValueId> {
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
        let at = self.address(
            AddressBase::Value(descriptor),
            &[Step::Byte(offset)],
            PlaceTy::Value(ty),
            n,
        );
        Some(self.read_bits(at, ty, n))
    }

    /// `code.identity(s)`: the bytes after `s`'s descriptor's three words, as
    /// a view.
    fn runtime_code_identity(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (descriptor, length) = self.code_descriptor(value, n)?;
        let offset = self.konst(Ty::Scalar(Scalar::U64), 24, n)?;
        let at = self.address(
            AddressBase::Value(descriptor),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let bytes = self.pointer(at, n);
        let Some(value) = self.identity_carrier(bytes, length, result, n) else {
            self.gap(n, "StaticFn identity carrier is not built");
            return None;
        };
        self.temp(result, Some(value));
        Some(value)
    }

    /// `code.revision(s)`: the descriptor's third word addresses the
    /// function's revision carrier `{bytes, 32, 0}`, which the link defines
    /// from the program's reach; a unit linked without it keeps its own
    /// `{0, 0, 0}`, and the result is None.
    fn runtime_code_revision(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (descriptor, _) = self.code_descriptor(value, n)?;
        let carrier = self.descriptor_word(descriptor, 16, Ty::Ptr, n)?;
        let bytes = self.descriptor_word(carrier, 0, Ty::Ptr, n)?;
        let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
            self.gap(
                n,
                "a code revision that is not a declared type is not built",
            );
            return None;
        };
        let payload = arguments[0];
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let none = self.runtime_compare(Ty::Ptr, bytes, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, no, yes, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let at = self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], payload, n)?;
        // The carrier is static and has capacity 0: the payload is its 24-byte
        // non-owning value.
        self.install_view(at, self.bare(payload), carrier, n)?;
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }

    /// A `Bytes` view `{bytes, length, 0}` of static identity bytes.
    fn identity_carrier(
        &mut self,
        bytes: ValueId,
        length: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        // A generic `identity[F]` result keeps its anchor on the StaticFn
        // parameter.  The carrier layout is still the ordinary Bytes row, so
        // erase only that semantic anchor when asking the emitter for storage.
        let bare = self.bare(result);
        let id = self
            .e
            .type_row(self.p, &mut self.t, result)
            .or_else(|| self.e.type_row(self.p, &mut self.t, bare));
        let Some(id) = id else {
            self.gap(n, "runtime operand layout is not built");
            return None;
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let carrier = self.pointer(to, n);
        self.sequence_word(carrier, 0, Ty::Ptr, bytes, n);
        self.sequence_word(carrier, 8, Ty::Scalar(Scalar::U64), length, n);
        self.sequence_word(carrier, 16, Ty::Scalar(Scalar::U64), zero, n);
        Some(self.pointer(to, n))
    }

    /// `code.callable(s)`: the code word of `s`'s descriptor, or the function
    /// itself when this body described `s`.
    fn runtime_code_callable(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let code = if let Some(&(function, _, _)) = self.static_fns.get(&value) {
            let code = self.value(Ty::Ptr);
            self.inst(
                Inst::FunctionRef {
                    result: code,
                    function,
                },
                n,
            );
            code
        } else {
            let Some(descriptor) = self.static_fn_pointer(value, n) else {
                self.gap(n, "StaticFn callable carriage is not built");
                return None;
            };
            let at = self.address(
                AddressBase::Value(descriptor),
                &[],
                PlaceTy::Value(Ty::Ptr),
                n,
            );
            self.read_bits(at, Ty::Ptr, n)
        };
        self.temp(result, Some(code));
        Some(code)
    }

    /// A StaticFn's descriptor address: the value itself when it is carried as a
    /// pointer, else its word reinterpreted.
    pub(super) fn static_fn_pointer(&mut self, value: ValueId, n: NodeId) -> Option<ValueId> {
        (self.b.values.get(value.index()) == Some(&Ty::Ptr))
            .then_some(value)
            .or_else(|| self.reinterpret(value, Ty::Scalar(Scalar::U64), Ty::Ptr, n))
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

    fn runtime_with_unique(
        &mut self,
        kind: Counted,
        item: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(item, n)?;
        // A mut holder argument points at its carrier, whose first word is the node.
        let node = self.runtime_handle(vals[0]?, n);
        let callback = vals[1]?;
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::TryUnwrap {
                result: ok,
                node,
                kind,
            },
            n,
        );
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(payload) = cx.owner_payload(node, id, true, n) {
                    let pointer = cx.pointer(payload, n);
                    let callable = cx.t.mk(TyKind::Fn(
                        vec![(Mode::Mut, item)],
                        ty,
                        false,
                        Callable::Scoped,
                    ));
                    let value = cx.call_lowered(callable, callback, &[Some(pointer)], n);
                    cx.runtime_case_payload(to, 1, ty, value, n);
                }
            },
            |_, _, _| {},
        )
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
        let (charge, _) = self.owner_charge(item, Some(kind), n)?;
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
                    cx.inst(
                        Inst::Free {
                            block: node,
                            charge,
                        },
                        n,
                    );
                }
            },
            |cx, to, ty| {
                cx.runtime_case_payload(to, 1, ty, Some(value), n);
            },
        )
    }

    fn runtime_unbox(&mut self, item: TyId, value: ValueId, n: NodeId) -> Option<ValueId> {
        let (charge, _) = self.owner_charge(item, None, n)?;
        let rep = self.rep_at(item, n)?;
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

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_carrier_view(&mut self, value: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => {
                self.gap(n, "a carrier view needs its value place");
                return None;
            }
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        // FORM #56: copy data, length and the owner's capacity, without an owner.
        self.install_view(to, self.bare(result), value, n)?;
        Some(self.pointer(to, n))
    }

    fn runtime_args_get(
        &mut self,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [value, ..] = self.runtime_row(
            RtOp::ArgsGet,
            &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
            n,
        )?;
        self.runtime_optional_view(value, result, n)
    }

    fn runtime_env(
        &mut self,
        op: HostOp,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let args: Vec<_> = vals
            .iter()
            .copied()
            .map(|v| v.map(Arg::Value))
            .collect::<Option<_>>()?;
        let [value, error, ..] = self.runtime_row(RtOp::Host(op), &args, n)?;
        match op {
            HostOp::EnvLen => Some(value),
            HostOp::EnvName => self.runtime_optional_view(value, result, n),
            HostOp::EnvGet => {
                let tag = Ty::Scalar(Scalar::U8);
                let ok = self.runtime_compare(tag, error, 0, n)?;
                self.runtime_result(
                    result,
                    ok,
                    n,
                    |cx, to, ty| {
                        let view = cx.runtime_optional_view(value, ty, n);
                        cx.runtime_case_payload(to, 0, ty, view, n);
                    },
                    |cx, to, ty| {
                        let Some(at) =
                            cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                        else {
                            return;
                        };
                        let denied = cx.runtime_compare(tag, error, 1, n).unwrap();
                        let no_name = cx.new_block(&[]);
                        let invalid_value = cx.new_block(&[]);
                        let done = cx.new_block(&[]);
                        cx.runtime_branch(denied, no_name, invalid_value, n);
                        cx.start(no_name);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                        cx.jump(done, &[], n);
                        cx.start(invalid_value);
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 1,
                            },
                            n,
                        );
                        cx.jump(done, &[], n);
                        cx.start(done);
                    },
                )
            }
            _ => unreachable!(),
        }
    }

    fn runtime_optional_view(
        &mut self,
        value: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
            self.gap(n, "a provider lookup requires an Option of a view");

            return None;
        };
        let payload = arguments[0];
        let id = self.runtime_type(result, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let none = self.runtime_compare(Ty::Ptr, value, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, no, yes, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let at = self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], payload, n)?;
        // The provider pointer pins a capacity-0 carrier. The Some payload is its
        // 24-byte non-owning value, not the address of that run-owned carrier.
        self.install_view(at, self.bare(payload), value, n)?;
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_stream(
        &mut self,
        op: HostOp,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let [_, error, code, written] = self.runtime_row(
            RtOp::Host(op),
            &[Arg::Value(vals[0]?), Arg::Value(vals[1]?)],
            n,
        )?;
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |_, _, _| {},
            |cx, to, ty| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_write_error(to, ty, written, error, code, n);
                }
            },
        )
    }

    fn runtime_write_error(
        &mut self,
        to: AddressId,
        ty: TyId,
        written: ValueId,
        error: ValueId,
        code: ValueId,
        n: NodeId,
    ) {
        self.runtime_store(to, &[Step::Field(0)], Ty::Scalar(Scalar::U64), written, n);
        let TyKind::Adt(decl, _) = self.t.kind(ty) else {
            return;
        };
        let DeclBody::Record(fields) = &self.p.decls[decl.index()].body else {
            return;
        };
        let io = fields[1].ty;
        if let Some(to) = self.runtime_field(to, &[Step::Field(1)], io, n) {
            self.runtime_io_error(to, error, code, n);
        }
    }

    fn runtime_io_error(&mut self, to: AddressId, error: ValueId, code: ValueId, n: NodeId) {
        let join = self.new_block(&[]);
        // Provider tags are 1-based, source variant indices 0-based. The fifth
        // variant alone has a payload. The runtime has already classified errno.
        for case in 0..4 {
            let Some(on) =
                self.runtime_compare(Ty::Scalar(Scalar::U8), error, u64::from(case + 1), n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 4,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 4, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn runtime_host(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let handle = if matches!(
            op,
            HostOp::Read
                | HostOp::Write
                | HostOp::ReadAt
                | HostOp::WriteAt
                | HostOp::NextEntries
                | HostOp::StatFile
                | HostOp::Close
                | HostOp::TryLock
                | HostOp::Unlock
                | HostOp::Sync
        ) {
            self.runtime_key(vals[0]?, n)
        } else {
            vals[0]?
        };
        let mut args = [Arg::Value(handle); 3];
        let len = match op {
            HostOp::Rename | HostOp::ReadAt | HostOp::WriteAt => {
                args[1] = Arg::Value(vals[1]?);
                args[2] = Arg::Value(vals[2]?);
                3
            }
            HostOp::SleepUntil | HostOp::SleepUntilWall => {
                args[1] = Arg::Value(vals[1]?);
                2
            }
            HostOp::Open => {
                args[1] = Arg::Value(vals[1]?);
                args[2] = Arg::Value(self.runtime_mode(f, 2, vals[2]?, n)?);
                3
            }
            HostOp::Entries => {
                let option = self.p.fns[f].params[1].ty;
                args[1] = Arg::Value(self.runtime_optional_carrier(option, vals[1], n)?);
                2
            }
            HostOp::Read
            | HostOp::Write
            | HostOp::OpenDir
            | HostOp::NextEntries
            | HostOp::Stat
            | HostOp::Mkdir
            | HostOp::Mkdtemp
            | HostOp::Mkstemp
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::Stdout
            | HostOp::Stderr
            | HostOp::Random => {
                args[1] = Arg::Value(vals[1]?);
                2
            }
            _ => 1,
        };
        if matches!(op, HostOp::Random | HostOp::Stopped) {
            return self.runtime_host_void(op, &args[..len], n);
        }
        if op == HostOp::Close {
            // Explicit CLOSE consumes on error and pre-issue cancellation too.
            // Its failure cleanup must not close the same key a second time.
            self.consume(vals[0], n);
        }
        let stat = matches!(op, HostOp::Stat | HostOp::StatFile);
        let (value, error, code, written, kind) = if stat {
            let [value, error, code, time, kind] =
                self.runtime_outputs(RtOp::Host(op), &args[..len], n)?;
            (value, error, code, time, Some(kind))
        } else {
            let [value, error, code, written] =
                self.runtime_row(RtOp::Host(op), &args[..len], n)?;
            (value, error, code, written, None)
        };
        if matches!(op, HostOp::WallMs | HostOp::MonoNs) {
            return Some(value);
        }
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| match op {
                HostOp::Open | HostOp::Entries | HostOp::Mkstemp => {
                    if let Some(to) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(to, &[], u64t, value, n);
                    }
                }
                HostOp::Read
                | HostOp::ReadAt
                | HostOp::Mkdtemp
                | HostOp::Random
                | HostOp::OpenDir
                | HostOp::NextEntries => cx.runtime_case_payload(to, 0, ty, Some(value), n),
                HostOp::Stat | HostOp::StatFile => {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_stat(ty, at, kind.unwrap(), value, written, n);
                    }
                }
                HostOp::TryLock | HostOp::ZeroExtension => {
                    if let Some(boolean) = cx.runtime_nonzero(u64t, value, n) {
                        cx.runtime_case_payload(to, 0, ty, Some(boolean), n);
                    }
                }
                _ => {}
            },
            |cx, to, ty| {
                if let Some(to) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    if matches!(
                        op,
                        HostOp::Write | HostOp::WriteAt | HostOp::Stdout | HostOp::Stderr
                    ) {
                        cx.runtime_write_error(to, ty, written, error, code, n);
                    } else {
                        cx.runtime_io_error(to, error, code, n);
                    }
                }
            },
        )
    }

    fn runtime_stat(
        &mut self,
        ty: TyId,
        at: AddressId,
        kind: ValueId,
        size: ValueId,
        modified_ns: ValueId,
        n: NodeId,
    ) {
        let Some((_, tag)) = self.net_field(ty, at, 0, n) else {
            return;
        };
        self.runtime_store(at, &[Step::Field(1)], Ty::Scalar(Scalar::U64), size, n);
        self.runtime_store(
            at,
            &[Step::Field(2)],
            Ty::Scalar(Scalar::I64),
            modified_ns,
            n,
        );
        let join = self.new_block(&[]);
        for case in 0..3 {
            let Some(matches) = self.runtime_compare(Ty::Scalar(Scalar::U8), kind, case as u64, n)
            else {
                return;
            };
            let yes = self.new_block(&[]);
            let next = self.new_block(&[]);
            self.runtime_branch(matches, yes, next, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: tag, case }, n);
            self.jump(join, &[], n);
            self.start(next);
        }
        self.inst(
            Inst::SetCase {
                address: tag,
                case: 3,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }

    /// A HostFile's entire carrier is the opaque u64 key into the run's
    /// registry, not provider metadata.
    fn runtime_key(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let u64t = Ty::Scalar(Scalar::U64);
        let at = self.address(AddressBase::Value(value), &[], PlaceTy::Value(u64t), n);
        self.read_bits(at, u64t, n)
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn net_field(
        &mut self,
        ty: TyId,
        at: AddressId,
        i: usize,
        n: NodeId,
    ) -> Option<(TyId, AddressId)> {
        let child = match self.t.kind(ty) {
            TyKind::Tuple(xs) => xs[i],
            TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                DeclBody::Record(fs) => fs[i].ty,
                _ => return None,
            },
            _ => return None,
        };
        let field = self.runtime_field(at, &[Step::Field(i as u32)], child, n)?;
        Some((child, field))
    }
    fn net_case_field(
        &mut self,
        ty: TyId,
        at: AddressId,
        case: u32,
        field: u32,
        n: NodeId,
    ) -> Option<(TyId, AddressId)> {
        let TyKind::Adt(d, _) = self.t.kind(ty) else {
            return None;
        };
        let DeclBody::Variant(cases) = &self.p.decls[d.index()].body else {
            return None;
        };
        let child = cases[case as usize].fields[field as usize].ty;
        let address = self.runtime_field(at, &[Step::CaseField { case, field }], child, n)?;
        Some((child, address))
    }

    fn net_read_case(
        &mut self,
        ty: TyId,
        at: AddressId,
        case: u32,
        field: u32,
        n: NodeId,
    ) -> Option<ValueId> {
        let (child, addr) = self.net_case_field(ty, at, case, field, n)?;
        match self.rep(child).ok()? {
            Rep::Scalar(t) => Some(self.read_bits(addr, t, n)),
            Rep::Place(_) => Some(self.pointer(addr, n)),
            Rep::None => None,
        }
    }

    fn tls_trust(&mut self, ty: TyId, at: AddressId, n: NodeId) -> Option<[ValueId; 2]> {
        let k = Ty::Scalar(Scalar::U8);
        let tag = self.net_case(at, n);
        let system = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[k, Ty::Ptr]);
        self.runtime_branch(system, yes, no, n);
        self.start(yes);
        let zero = self.konst(k, 0, n)?;
        // The adapter ignores the roots carrier for System.
        let unused = self.pointer(at, n);
        self.jump(join, &[zero, unused], n);
        self.start(no);
        let one = self.konst(k, 1, n)?;
        let roots = self.net_read_case(ty, at, 1, 0, n)?;
        self.jump(join, &[one, roots], n);
        self.start(join);
        let values = self.block_params(join);
        Some([values[0], values[1]])
    }

    fn runtime_narrow(
        &mut self,
        net: ValueId,
        ty: TyId,
        rule: ValueId,
        n: NodeId,
    ) -> Option<[ValueId; 8]> {
        let (_, at) = self.runtime_place(ty, Some(rule), n)?;
        let tag = self.net_case(at, n);
        let address = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let signature = RtOp::Host(HostOp::NetNarrow).signature();
        let join = self.new_block(&signature.results);
        self.runtime_branch(address, yes, no, n);
        self.start(yes);
        let (_, dir) = self.net_case_field(ty, at, 0, 0, n)?;
        let dir = self.net_case(dir, n);
        let d8 = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result: d8,
                flag: None,
                operand: dir,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let (_, ip) = self.net_case_field(ty, at, 0, 1, n)?;
        let [family, hi, lo] = self.net_ip(ip, n)?;
        let bits = self.net_read_case(ty, at, 0, 2, n)?;
        let (pair, ports) = self.net_case_field(ty, at, 0, 3, n)?;
        let first = self.net_read_field(pair, ports, 0, n)?;
        let last = self.net_read_field(pair, ports, 1, n)?;
        let [value, error, code, written] = self.runtime_row(
            RtOp::Host(HostOp::NetNarrow),
            &[net, d8, family, hi, lo, bits, first, last].map(Arg::Value),
            n,
        )?;
        self.jump(join, &[value, error, code, written], n);
        self.start(no);
        let tls = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 1, n)?;
        let tls_rule = self.new_block(&[]);
        let named_or_unix = self.new_block(&[]);
        self.runtime_branch(tls, tls_rule, named_or_unix, n);
        self.start(named_or_unix);
        let unix = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 3, n)?;
        let unix_rule = self.new_block(&[]);
        let resolve_rule = self.new_block(&[]);
        self.runtime_branch(unix, unix_rule, resolve_rule, n);
        self.start(unix_rule);
        let (_, dir) = self.net_case_field(ty, at, 3, 0, n)?;
        let dir = self.net_case(dir, n);
        let dir8 = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result: dir8,
                flag: None,
                operand: dir,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let root = self.net_read_case(ty, at, 3, 1, n)?;
        let path = self.net_read_case(ty, at, 3, 2, n)?;
        let outputs = self.runtime_row(
            RtOp::Host(HostOp::NetNarrowUnix),
            &[net, dir8, root, path].map(Arg::Value),
            n,
        )?;
        self.jump(join, &outputs, n);
        self.start(resolve_rule);
        let names = self.net_read_case(ty, at, 2, 0, n)?;
        let outputs = self.runtime_row(
            RtOp::Host(HostOp::NetNarrowResolve),
            &[Arg::Value(net), Arg::Value(names)],
            n,
        )?;
        self.jump(join, &outputs, n);
        self.start(tls_rule);
        let names = self.net_read_case(ty, at, 1, 0, n)?;
        let (pair, ports) = self.net_case_field(ty, at, 1, 1, n)?;
        let first = self.net_read_field(pair, ports, 0, n)?;
        let last = self.net_read_field(pair, ports, 1, n)?;
        let (pin, trust) = self.net_case_field(ty, at, 1, 2, n)?;
        let [trust, digest] = self.tls_trust(pin, trust, n)?;
        let [value, error, code, written] = self.runtime_row(
            RtOp::Host(HostOp::NetNarrowTls),
            &[net, names, first, last, trust, digest].map(Arg::Value),
            n,
        )?;
        self.jump(join, &[value, error, code, written], n);
        self.start(join);
        let v = self.block_params(join);
        Some([
            v[0],
            v[1],
            v[2],
            v[3],
            ValueId(0),
            ValueId(0),
            ValueId(0),
            ValueId(0),
        ])
    }

    fn runtime_exec_narrow(
        &mut self,
        exec: ValueId,
        ty: TyId,
        rule: ValueId,
        n: NodeId,
    ) -> Option<[ValueId; 8]> {
        let (_, at) = self.runtime_place(ty, Some(rule), n)?;
        let tag = self.net_case(at, n);
        let op = RtOp::Host(HostOp::ExecNarrow);
        let join = self.new_block(&op.signature().results);
        for case in 0..4 {
            let next = if case != 3 {
                let matches = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, case, n)?;
                let yes = self.new_block(&[]);
                let no = self.new_block(&[]);
                self.runtime_branch(matches, yes, no, n);
                self.start(yes);
                Some(no)
            } else {
                None
            };
            let name = self.net_read_case(ty, at, case as u32, 0, n)?;
            let arg = if case == 0 {
                name
            } else {
                self.net_read_case(ty, at, case as u32, 1, n)?
            };
            let values = self.runtime_row(op, &[exec, tag, name, arg].map(Arg::Value), n)?;
            self.jump(join, &values, n);
            if let Some(next) = next {
                self.start(next);
            }
        }
        self.start(join);
        let v = self.block_params(join);
        Some([
            v[0],
            v[1],
            v[2],
            v[3],
            ValueId(0),
            ValueId(0),
            ValueId(0),
            ValueId(0),
        ])
    }

    fn net_read_field(&mut self, ty: TyId, at: AddressId, i: usize, n: NodeId) -> Option<ValueId> {
        let (child, field) = self.net_field(ty, at, i, n)?;
        match self.rep(child).ok()? {
            Rep::Scalar(t) => Some(self.read_bits(field, t, n)),
            Rep::Place(_) => Some(self.pointer(field, n)),
            Rep::None => None,
        }
    }
    fn net_case(&mut self, at: AddressId, n: NodeId) -> ValueId {
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        tag
    }
    fn net_ip(&mut self, at: AddressId, n: NodeId) -> Option<[ValueId; 3]> {
        let u = Ty::Scalar(Scalar::U64);
        let tag = self.net_case(at, n);
        let family = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result: family,
                flag: None,
                operand: tag,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        let v4 = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[u, u]);
        self.runtime_branch(v4, yes, no, n);
        self.start(yes);
        let at4 = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Value(Ty::Scalar(Scalar::U32)),
            n,
        );
        let lo4 = self.read_bits(at4, Ty::Scalar(Scalar::U32), n);
        let lo = self.value(u);
        self.inst(
            Inst::Convert {
                result: lo,
                flag: None,
                operand: lo4,
                mode: ConvertMode::Widen,
            },
            n,
        );
        let zero = self.konst(u, 0, n)?;
        self.jump(join, &[zero, lo], n);
        self.start(no);
        let hi_at = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Value(u),
            n,
        );
        let lo_at = self.address(
            AddressBase::Address(at),
            &[Step::CaseField { case: 1, field: 1 }],
            PlaceTy::Value(u),
            n,
        );
        let hi = self.read_bits(hi_at, u, n);
        let lo = self.read_bits(lo_at, u, n);
        self.jump(join, &[hi, lo], n);
        self.start(join);
        let values = self.block_params(join);
        Some([family, values[0], values[1]])
    }
    fn net_until(&mut self, ty: TyId, value: ValueId, n: NodeId) -> Option<[ValueId; 3]> {
        let (_, at) = self.runtime_place(ty, Some(value), n)?;
        let tag = self.net_case(at, n);
        let never = self.runtime_compare(Ty::Scalar(Scalar::U32), tag, 0, n)?;
        let u = Ty::Scalar(Scalar::U64);
        let k = Ty::Scalar(Scalar::U8);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[k, u, u]);
        self.runtime_branch(never, yes, no, n);
        self.start(yes);
        let zero = self.konst(u, 0, n)?;
        let off = self.konst(k, 0, n)?;
        self.jump(join, &[off, zero, zero], n);
        self.start(no);
        let mut fields = Vec::new();
        for field in 0..2 {
            let field = self.address(
                AddressBase::Address(at),
                &[Step::CaseField { case: 1, field }],
                PlaceTy::Value(u),
                n,
            );
            fields.push(self.read_bits(field, u, n));
        }
        let on = self.konst(k, 1, n)?;
        self.jump(join, &[on, fields[0], fields[1]], n);
        self.start(join);
        let values = self.block_params(join);
        Some([values[0], values[1], values[2]])
    }
    fn net_store_address(&mut self, to: AddressId, ty: TyId, values: &[ValueId], n: NodeId) {
        let Some((_, ip)) = self.net_field(ty, to, 0, n) else {
            return;
        };
        let v4 = self
            .runtime_compare(Ty::Scalar(Scalar::U8), values[0], 0, n)
            .unwrap();
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(v4, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: ip,
                case: 0,
            },
            n,
        );
        let lo = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Convert {
                result: lo,
                flag: None,
                operand: values[2],
                mode: ConvertMode::Truncate,
            },
            n,
        );
        self.runtime_store(
            ip,
            &[Step::CaseField { case: 0, field: 0 }],
            Ty::Scalar(Scalar::U32),
            lo,
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: ip,
                case: 1,
            },
            n,
        );
        for field in 0..2 {
            self.runtime_store(
                ip,
                &[Step::CaseField { case: 1, field }],
                Ty::Scalar(Scalar::U64),
                values[field as usize + 1],
                n,
            );
        }
        self.jump(join, &[], n);
        self.start(join);
        self.runtime_store(to, &[Step::Field(1)], Ty::Scalar(Scalar::U16), values[3], n);
    }
    fn exec_tag(&mut self, at: AddressId, n: NodeId) -> ValueId {
        let tag = self.net_case(at, n);
        self.exec_byte(tag, n)
    }

    fn exec_byte(&mut self, value: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand: value,
                mode: ConvertMode::Truncate,
            },
            n,
        );
        result
    }

    fn runtime_exec(
        &mut self,
        op: HostOp,
        function: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let operand_ty = self.p.fns[function].params[1].ty;
        let mut args = vec![if op == HostOp::ExecSpawn {
            vals[0]?
        } else {
            self.runtime_key(vals[0]?, n)
        }];
        match op {
            HostOp::ExecSpawn => {
                let (_, launch) = self.runtime_place(operand_ty, vals[1], n)?;
                for index in 0..5 {
                    args.push(self.net_read_field(operand_ty, launch, index, n)?);
                }
                for index in 5..8 {
                    let (_, stream) = self.net_field(operand_ty, launch, index, n)?;
                    args.push(self.exec_tag(stream, n));
                }
            }
            HostOp::ExecWait => args.extend(self.net_until(operand_ty, vals[1]?, n)?),
            HostOp::ExecSignal => {
                let (_, signal) = self.runtime_place(operand_ty, vals[1], n)?;
                args.push(self.exec_tag(signal, n));
            }
            _ => unreachable!(),
        }
        let args: Vec<_> = args.into_iter().map(Arg::Value).collect();
        let [value, tag, code, written, stdin, stdout, stderr] =
            self.runtime_outputs::<7>(RtOp::Host(op), &args, n)?;
        if op == HostOp::ExecSpawn {
            let max_error = self.konst(Ty::Scalar(Scalar::U8), 4, n)?;
            let decided = self.test(CompareOp::Le, tag, max_error, n);
            self.raise_unless(decided, value, written, n);
        }
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), tag, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if op != HostOp::ExecSignal
                    && let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    if op == HostOp::ExecSpawn {
                        cx.runtime_spawned(at, ty, [value, stdin, stdout, stderr], n);
                    } else {
                        cx.runtime_termination(at, value, n);
                    }
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_exec_error(at, tag, code, n);
                }
            },
        )
    }

    fn runtime_spawned(
        &mut self,
        to: AddressId,
        ty: TyId,
        [proc, stdin, stdout, stderr]: [ValueId; 4],
        n: NodeId,
    ) {
        let u = Ty::Scalar(Scalar::U64);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.runtime_store(to, &[Step::CaseField { case: 0, field: 0 }], u, proc, n);
        for (i, pipe) in [stdin, stdout, stderr].into_iter().enumerate() {
            let Some((_, option)) = self.net_case_field(ty, to, 0, i as u32 + 1, n) else {
                return;
            };
            let absent = self.runtime_compare(u, pipe, 0, n).unwrap();
            let no = self.new_block(&[]);
            let yes = self.new_block(&[]);
            let join = self.new_block(&[]);
            self.runtime_branch(absent, no, yes, n);
            self.start(no);
            self.inst(
                Inst::SetCase {
                    address: option,
                    case: 0,
                },
                n,
            );
            self.jump(join, &[], n);
            self.start(yes);
            self.inst(
                Inst::SetCase {
                    address: option,
                    case: 1,
                },
                n,
            );
            self.runtime_store(option, &[Step::CaseField { case: 1, field: 0 }], u, pipe, n);
            self.jump(join, &[], n);
            self.start(join);
        }
    }

    fn runtime_termination(&mut self, to: AddressId, value: ValueId, n: NodeId) {
        let max = self.konst(Ty::Scalar(Scalar::U64), 255, n).unwrap();
        let exited = self.test(CompareOp::Le, value, max, n);
        let code = self.exec_byte(value, n);
        let exit = self.new_block(&[]);
        let signal = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(exited, exit, signal, n);
        for (block, case) in [(exit, 0), (signal, 1)] {
            self.start(block);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.runtime_store(
                to,
                &[Step::CaseField { case, field: 0 }],
                Ty::Scalar(Scalar::U8),
                code,
                n,
            );
            self.jump(join, &[], n);
        }
        self.start(join);
    }

    fn runtime_exec_error(&mut self, to: AddressId, tag: ValueId, code: ValueId, n: NodeId) {
        let join = self.new_block(&[]);
        for case in 0..3 {
            let is = self
                .runtime_compare(Ty::Scalar(Scalar::U8), tag, u64::from(case + 1), n)
                .unwrap();
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(is, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 3,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 3, field: 0 }],
            Ty::Scalar(Scalar::U32),
            code,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }

    /// `cred.fetch` (TLS-7). The outcome's error is 0 for the pair (`value` the
    /// Secret's designation, `written` the expiry), else the `CredError` case
    /// plus one: `Room(value, written)`, `Net` and `Token` with the inner tag in
    /// `written` and its code in `code`, `Status(value)` and `Malformed`. Error 6
    /// is the designation's refused admission, which raises (MEM-21).
    fn runtime_cred_fetch(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let byte = Ty::Scalar(Scalar::U8);
        let mut operands = vec![vals[0]?, vals[1]?, vals[2]?];
        operands.extend(self.net_until(self.p.fns[f].params[3].ty, vals[3]?, n)?);
        let args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
        let [value, error, code, written] =
            self.runtime_outputs::<4>(RtOp::Host(HostOp::CredFetch), &args, n)?;
        let malformed = self.konst(byte, 5, n)?;
        let decided = self.test(CompareOp::Le, error, malformed, n);
        self.raise_unless(decided, value, written, n);
        let ok = self.runtime_compare(byte, error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(pair) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    if let Some((_, secret)) = cx.net_field(ty, pair, 0, n) {
                        cx.runtime_store(secret, &[], u, value, n);
                    }
                    cx.runtime_store(pair, &[Step::Field(1)], u, written, n);
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    cx.runtime_cred_error(at, ty, [error, code, value, written], n);
                }
            },
        )
    }

    fn runtime_cred_error(
        &mut self,
        to: AddressId,
        ty: TyId,
        [error, code, value, written]: [ValueId; 4],
        n: NodeId,
    ) {
        let u = Ty::Scalar(Scalar::U64);
        let byte = Ty::Scalar(Scalar::U8);
        let join = self.new_block(&[]);
        for case in 0..4 {
            let Some(on) = self.runtime_compare(byte, error, u64::from(case + 1), n) else {
                return;
            };
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            self.runtime_branch(on, yes, no, n);
            self.start(yes);
            self.inst(Inst::SetCase { address: to, case }, n);
            match case {
                0 => {
                    let room = |field| Step::CaseField { case: 0, field };
                    self.runtime_store(to, &[room(0)], u, value, n);
                    self.runtime_store(to, &[room(1)], u, written, n);
                }
                1 | 2 => {
                    let tag = self.value(byte);
                    self.inst(
                        Inst::Convert {
                            result: tag,
                            flag: None,
                            operand: written,
                            mode: ConvertMode::Truncate,
                        },
                        n,
                    );
                    if let Some((inner, at)) = self.net_case_field(ty, to, case, 0, n) {
                        if case == 1 {
                            self.runtime_net_error(at, inner, tag, code, n);
                        } else {
                            self.runtime_io_error(at, tag, code, n);
                        }
                    }
                }
                _ => {
                    let status = self.value(Ty::Scalar(Scalar::U16));
                    self.inst(
                        Inst::Convert {
                            result: status,
                            flag: None,
                            operand: value,
                            mode: ConvertMode::Truncate,
                        },
                        n,
                    );
                    self.runtime_store(
                        to,
                        &[Step::CaseField { case: 3, field: 0 }],
                        Ty::Scalar(Scalar::U16),
                        status,
                        n,
                    );
                }
            }
            self.jump(join, &[], n);
            self.start(no);
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 4,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }

    fn runtime_net(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if hirsel_core_ids::host_op::udp_operation(op.code()) {
            return self.runtime_udp(op, f, vals, result, n);
        }
        if matches!(
            op,
            HostOp::ExecSpawn | HostOp::ExecWait | HostOp::ExecSignal
        ) {
            return self.runtime_exec(op, f, vals, result, n);
        }
        if op.is_secret_derivation() {
            return self.runtime_secret_derive(op, f, vals, result, n);
        }
        if op.is_secret_mac() {
            return self.runtime_secret_mac(op, f, vals, result, n);
        }
        if op.is_secret_key() {
            return self.runtime_secret_key(op, f, vals, result, n);
        }
        if matches!(
            op,
            HostOp::ScramSha256 | HostOp::ScramFinalLen | HostOp::ScramVerify
        ) {
            return self.runtime_scram(op, f, vals, result, n);
        }
        if op == HostOp::CredFetch {
            return self.runtime_cred_fetch(f, vals, result, n);
        }
        let u = Ty::Scalar(Scalar::U64);
        let key = if matches!(
            op,
            HostOp::Accept
                | HostOp::LocalAddr
                | HostOp::PeerAddr
                | HostOp::UnixAccept
                | HostOp::DatagramConnect
                | HostOp::DatagramSend
                | HostOp::DatagramSendTo
                | HostOp::DatagramRecv
                | HostOp::RecvBounded
                | HostOp::RecvReady
                | HostOp::SendPair
                | HostOp::SendSecret
                | HostOp::SecretNarrow
                | HostOp::NetSplit
                | HostOp::NetJoin
        ) {
            self.runtime_key(vals[0]?, n)
        } else {
            vals[0]?
        };
        let mut operands = vec![key];
        if !matches!(op, HostOp::NetNarrow | HostOp::ExecNarrow) {
            match op {
                HostOp::Connect | HostOp::Listen => {
                    let ty = self.p.fns[f].params[1].ty;
                    let (_, at) = self.runtime_place(ty, vals[1], n)?;
                    let (_, ip) = self.net_field(ty, at, 0, n)?;
                    operands.extend(self.net_ip(ip, n)?);
                    operands.push(self.net_read_field(ty, at, 1, n)?);
                    if op == HostOp::Connect {
                        operands.extend(self.net_until(self.p.fns[f].params[2].ty, vals[2]?, n)?);
                    } else {
                        operands.push(vals[2]?);
                    }
                }
                HostOp::TlsConnect => {
                    operands.extend([self.runtime_key(vals[1]?, n), vals[2]?]);
                    let ty = self.p.fns[f].params[3].ty;
                    let (_, at) = self.runtime_place(ty, vals[3], n)?;
                    operands.extend(self.tls_trust(ty, at, n)?);
                    operands.push(vals[4]?);
                    operands.extend(self.net_until(self.p.fns[f].params[5].ty, vals[5]?, n)?);
                    // The adapter consumes the socket before any outcome, even on
                    // the operation's failure edge.
                    self.consume(vals[1], n);
                }
                HostOp::Accept | HostOp::UnixAccept | HostOp::RecvReady => {
                    operands.extend(self.net_until(self.p.fns[f].params[1].ty, vals[1]?, n)?)
                }
                HostOp::UnixConnect
                | HostOp::UnixListen
                | HostOp::DatagramBind
                | HostOp::DatagramConnect
                | HostOp::DatagramSendTo => {
                    operands.extend([vals[1]?, vals[2]?]);
                    match op {
                        HostOp::UnixConnect => operands.extend(self.net_until(
                            self.p.fns[f].params[3].ty,
                            vals[3]?,
                            n,
                        )?),
                        HostOp::UnixListen => operands.push(vals[3]?),
                        HostOp::DatagramSendTo => {
                            operands.push(vals[3]?);
                            operands.extend(self.net_until(
                                self.p.fns[f].params[4].ty,
                                vals[4]?,
                                n,
                            )?);
                        }
                        _ => {}
                    }
                }
                HostOp::DatagramOpen => {}
                HostOp::DatagramSend | HostOp::DatagramRecv => {
                    operands.push(vals[1]?);
                    operands.extend(self.net_until(self.p.fns[f].params[2].ty, vals[2]?, n)?);
                }
                HostOp::RecvBounded | HostOp::SendPair | HostOp::Resolve => {
                    operands.extend([vals[1]?, vals[2]?]);
                    operands.extend(self.net_until(self.p.fns[f].params[3].ty, vals[3]?, n)?);
                }
                HostOp::StdinRead => {
                    operands.push(vals[1]?);
                    operands.extend(self.net_until(self.p.fns[f].params[2].ty, vals[2]?, n)?);
                }
                HostOp::SendSecret => {
                    // The wire, the secrets' keys and the marks, as carriers.
                    operands.extend([vals[1]?, vals[2]?, vals[3]?]);
                    operands.extend(self.net_until(self.p.fns[f].params[4].ty, vals[4]?, n)?);
                }
                HostOp::SecretNarrow | HostOp::EnvNarrow => operands.push(vals[1]?),
                HostOp::NetSplit | HostOp::LocalAddr | HostOp::PeerAddr => {}
                HostOp::NetJoin => operands.push(self.runtime_key(vals[1]?, n)),
                _ => unreachable!(),
            }
        }
        let values = if op == HostOp::NetNarrow {
            self.runtime_narrow(key, self.p.fns[f].params[1].ty, vals[1]?, n)?
        } else if op == HostOp::ExecNarrow {
            self.runtime_exec_narrow(key, self.p.fns[f].params[1].ty, vals[1]?, n)?
        } else {
            let args: Vec<_> = operands.into_iter().map(Arg::Value).collect();
            self.runtime_outputs::<8>(RtOp::Host(op), &args, n)?
        };
        let (value, error, code, written) = (values[0], values[1], values[2], values[3]);
        if op == HostOp::NetSplit {
            self.consume(vals[0], n);
            let id = self.runtime_type(result, n)?;
            let to = self.new_slot(PlaceTy::Declared(id), n);
            self.runtime_store(to, &[Step::Field(0)], u, value, n);
            self.runtime_store(to, &[Step::Field(1)], u, written, n);
            return Some(self.pointer(to, n));
        }
        if op == HostOp::NetJoin {
            self.consume(vals[0], n);
            self.consume(vals[1], n);
            let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
            return self.runtime_result(
                result,
                ok,
                n,
                |cx, to, ty| {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(at, &[], u, value, n);
                    }
                },
                |cx, to, ty| {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                    {
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                        cx.runtime_store(at, &[Step::CaseField { case: 0, field: 0 }], u, value, n);
                        cx.runtime_store(
                            at,
                            &[Step::CaseField { case: 0, field: 1 }],
                            u,
                            written,
                            n,
                        );
                    }
                },
            );
        }
        let policy_result = matches!(
            op,
            HostOp::NetNarrow | HostOp::ExecNarrow | HostOp::SecretNarrow | HostOp::EnvNarrow
        );
        if policy_result {
            // Error 1 is `Invalid`; a larger one is a refusal, raised (MEM-21).
            let one = self.konst(Ty::Scalar(Scalar::U8), 1, n)?;
            let decided = self.test(CompareOp::Le, error, one, n);
            self.raise_unless(decided, value, written, n);
        }
        let ok = self.runtime_compare(Ty::Scalar(Scalar::U8), error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if op == HostOp::SecretNarrow {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(at, &[], u, value, n);
                    }
                } else if policy_result
                    || matches!(
                        op,
                        HostOp::RecvBounded
                            | HostOp::SendPair
                            | HostOp::Resolve
                            | HostOp::StdinRead
                    )
                {
                    cx.runtime_case_payload(to, 0, ty, Some(value), n);
                } else if op == HostOp::DatagramRecv {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        let flag = cx.runtime_nonzero(u, written, n).unwrap();
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                        cx.runtime_store(at, &[Step::CaseField { case: 0, field: 0 }], u, value, n);
                        cx.runtime_store(
                            at,
                            &[Step::CaseField { case: 0, field: 1 }],
                            Ty::Scalar(Scalar::Bool),
                            flag,
                            n,
                        );
                    }
                } else if matches!(
                    op,
                    HostOp::Connect
                        | HostOp::UnixConnect
                        | HostOp::UnixListen
                        | HostOp::UnixAccept
                        | HostOp::DatagramOpen
                        | HostOp::DatagramBind
                ) {
                    if let Some(at) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(at, &[], u, value, n);
                    }
                } else if op == HostOp::TlsConnect {
                    cx.runtime_tls_result(to, ty, value, written, &values[4..], n);
                } else if matches!(op, HostOp::LocalAddr | HostOp::PeerAddr) {
                    if let Some(addr) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.net_store_address(addr, ty, &values[4..], n);
                    }
                } else if matches!(op, HostOp::Listen | HostOp::Accept) {
                    if let Some(pair) =
                        cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                    {
                        cx.runtime_store(pair, &[Step::Field(0)], u, value, n);
                        if let Some((addr_ty, addr)) = cx.net_field(ty, pair, 1, n) {
                            cx.net_store_address(addr, addr_ty, &values[4..], n);
                        }
                    }
                }
            },
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                {
                    if policy_result {
                        cx.inst(
                            Inst::SetCase {
                                address: at,
                                case: 0,
                            },
                            n,
                        );
                    } else if op == HostOp::StdinRead {
                        cx.runtime_stdin_error(at, error, code, n);
                    } else {
                        if op == HostOp::UnixListen {
                            let bound = cx
                                .runtime_compare(Ty::Scalar(Scalar::U8), error, 10, n)
                                .unwrap();
                            let yes = cx.new_block(&[]);
                            let no = cx.new_block(&[]);
                            let join = cx.new_block(&[]);
                            cx.runtime_branch(bound, yes, no, n);
                            cx.start(yes);
                            cx.inst(
                                Inst::SetCase {
                                    address: at,
                                    case: 9,
                                },
                                n,
                            );
                            cx.runtime_store(
                                at,
                                &[Step::CaseField { case: 9, field: 0 }],
                                Ty::Scalar(Scalar::U32),
                                code,
                                n,
                            );
                            cx.jump(join, &[], n);
                            cx.start(no);
                            cx.runtime_net_error(at, ty, error, code, n);
                            cx.jump(join, &[], n);
                            cx.start(join);
                        } else {
                            cx.runtime_net_error(at, ty, error, code, n);
                        }
                    }
                }
            },
        )
    }

    fn runtime_scram(
        &mut self,
        op: HostOp,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let k = Ty::Scalar(Scalar::U8);
        let mut args = vec![Arg::Value(self.runtime_key(vals[0]?, n))];
        let send = op == HostOp::ScramSha256;
        let before_until = if send { 6 } else { vals.len() };
        for (i, &value) in vals.iter().enumerate().take(before_until).skip(1) {
            // The SCRAM exchange's `s` is a `Secret` carrier (TLS-5).
            args.push(Arg::Value(if send && i == 1 {
                self.runtime_key(value?, n)
            } else {
                value?
            }));
        }
        if send {
            let until = self.net_until(self.p.fns[f].params[6].ty, vals[6]?, n)?;
            args.extend(until.map(Arg::Value));
        }
        if op == HostOp::ScramVerify {
            self.consume(vals[0], n);
        }
        let [value, error, code, written] = self.runtime_row(RtOp::Host(op), &args, n)?;
        if op == HostOp::ScramVerify {
            return self.runtime_nonzero(u, value, n);
        }
        if send {
            let maximum = self.konst(k, 5, n)?;
            let valid = self.test(CompareOp::Le, error, maximum, n);
            self.raise_unless(valid, value, written, n);
        }
        let ok = self.runtime_compare(k, error, 0, n)?;
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                if let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
                {
                    cx.runtime_store(at, &[], u, value, n);
                }
            },
            |cx, to, ty| {
                let Some(at) =
                    cx.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], ty, n)
                else {
                    return;
                };
                let join = cx.new_block(&[]);
                let last = if send { 4 } else { 3 };
                for case in 0..last {
                    let on = cx
                        .runtime_compare(k, error, u64::from(case + 1), n)
                        .unwrap();
                    let yes = cx.new_block(&[]);
                    let no = cx.new_block(&[]);
                    cx.runtime_branch(on, yes, no, n);
                    cx.start(yes);
                    cx.inst(Inst::SetCase { address: at, case }, n);
                    cx.jump(join, &[], n);
                    cx.start(no);
                }
                cx.inst(
                    Inst::SetCase {
                        address: at,
                        case: last,
                    },
                    n,
                );
                if send && let Some((net_ty, net)) = cx.net_case_field(ty, at, 4, 0, n) {
                    let tag = cx.value(k);
                    cx.inst(
                        Inst::Convert {
                            result: tag,
                            flag: None,
                            operand: written,
                            mode: ConvertMode::Truncate,
                        },
                        n,
                    );
                    cx.runtime_net_error(net, net_ty, tag, code, n);
                }
                cx.jump(join, &[], n);
                cx.start(join);
            },
        )
    }

    fn runtime_tls_result(
        &mut self,
        to: AddressId,
        ty: TyId,
        key: ValueId,
        protocol: ValueId,
        words: &[ValueId],
        n: NodeId,
    ) {
        let u = Ty::Scalar(Scalar::U64);
        let Some(pair) = self.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ty, n)
        else {
            return;
        };
        for direction in 1..=2 {
            let bit = self.konst(u, direction, n).unwrap();
            let half = self.value(u);
            self.inst(
                Inst::Binary {
                    result: half,
                    op: BinaryOp::BitOr,
                    left: key,
                    right: bit,
                    fail: None,
                },
                n,
            );
            self.runtime_store(pair, &[Step::Field(direction as u32 - 1)], u, half, n);
        }
        let Some((ty, handshake)) = self.net_field(ty, pair, 2, n) else {
            return;
        };
        let (_, selected) = self.net_field(ty, handshake, 0, n).unwrap();
        let (_, peer) = self.net_field(ty, handshake, 1, n).unwrap();
        for (i, word) in words.iter().enumerate() {
            self.runtime_store(peer, &[Step::Field(i as u32)], u, *word, n);
        }
        let none = self.runtime_compare(u, protocol, 0, n).unwrap();
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(none, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: selected,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        let one = self.konst(u, 1, n).unwrap();
        let index = self.value(u);
        self.inst(
            Inst::IntArith {
                result: index,
                flag: None,
                op: ArithOp::Sub,
                mode: ArithMode::Wrapping,
                left: protocol,
                right: one,
            },
            n,
        );
        self.inst(
            Inst::SetCase {
                address: selected,
                case: 1,
            },
            n,
        );
        self.runtime_store(
            selected,
            &[Step::CaseField { case: 1, field: 0 }],
            u,
            index,
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
    }
}
