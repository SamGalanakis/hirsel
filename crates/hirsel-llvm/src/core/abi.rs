//! Linux C result classification, adapted from rustc_target's callconv/{x86_64,
//! aarch64}.rs. SysV merges eightbyte classes; AAPCS64 uses homogeneous floating
//! aggregates or integer carriers, and otherwise the dedicated sret register.
//! Only FORM's naturally aligned scalar fields occur.
use super::*;
use hirsel_core_ir::native_layout::{
    CallLayouts, OUTCOME_OK, OUTCOME_OTHER, OUTCOME_RAISED, OkResult, ScalarRecord,
};

pub(super) struct Return {
    pub target: hirsel_core_layout::Target,
    /// A CoreIR function's own convention, which every caller shares (status
    /// appended), rather than a C one.
    pub internal: bool,
    pub ty: Type,
    pub storage: Type,
    pub indirect: bool,
    pub fields: Vec<Ty>,
    pub offsets: Vec<u64>,
    pub regs: Vec<Type>,
    pub reg_stride: u64,
    pub record: Option<ScalarRecord>,
    /// A body's private `Result` convention: the `Ok` leaves and an outcome byte
    /// in registers, any other case through the cold pointer (`Local::COLD`).
    pub ok: Option<OkResult>,
}

/// The private symbol suffix of a body: `.ok` for the `Result` register
/// convention, `.infallible` for a body without status, none for the public form.
pub(super) fn private_suffix(has_status: bool, ok: bool) -> &'static str {
    if ok {
        ".ok"
    } else if has_status {
        ""
    } else {
        ".infallible"
    }
}
pub(super) fn extension(target: hirsel_core_layout::Target, t: Ty) -> Extension {
    if target == hirsel_core_layout::Target::Aarch64Linux
        || size(t) >= 4
        || t.is_float()
        || t == Ty::Ptr
    {
        Extension::None
    } else if t.int().is_some_and(|x| x.0) {
        Extension::Sign
    } else {
        Extension::Zero
    }
}
impl Return {
    pub fn internal_on_target(target: hirsel_core_layout::Target, fields: &[Ty]) -> Self {
        Self::core(target, fields, true)
    }

    /// A CoreIR function's result convention: its fields, then its status byte
    /// when it may fail (an `.infallible` body has none).
    pub fn core(target: hirsel_core_layout::Target, fields: &[Ty], has_status: bool) -> Self {
        let mut fields = fields.to_vec();
        if has_status {
            fields.push(Ty::Scalar(Scalar::U8));
        }
        Self {
            internal: true,
            ..Self::on_target(target, &fields)
        }
    }

    pub fn for_signature(
        target: Target,
        sig: &Signature,
        layouts: &CallLayouts,
        has_status: bool,
    ) -> Self {
        Self::core_record(target, &sig.results, layouts.result(sig), has_status)
    }

    /// The convention a body's own symbol and its direct callers share: the
    /// `Result` register form when the body has one, else [`Self::for_signature`].
    pub fn private(
        target: Target,
        sig: &Signature,
        layouts: &CallLayouts,
        has_status: bool,
        body: bool,
    ) -> Self {
        match layouts.ok_result(sig).filter(|_| body) {
            Some(ok) => Self {
                record: None,
                ok: Some(ok),
                ..Self::core_record(target, &[], Some(ok.ok), true)
            },
            None => Self::for_signature(target, sig, layouts, has_status || !body),
        }
    }

    pub fn core_record(
        target: Target,
        fields: &[Ty],
        record: Option<ScalarRecord>,
        has_status: bool,
    ) -> Self {
        let Some(record) = record else {
            return Self::core(target, fields, has_status);
        };
        let fields: Vec<_> = record
            .fields()
            .iter()
            .map(|&(_, t)| t)
            .chain(has_status.then_some(Ty::Scalar(Scalar::U8)))
            .collect();
        let regs: Vec<_> = fields.iter().copied().map(ty).collect();
        let ty = if regs.len() == 1 {
            regs[0].clone()
        } else {
            Type::structure(regs.clone())
        };
        Self {
            target,
            internal: true,
            ty,
            storage: Type::Void,
            indirect: false,
            fields,
            offsets: vec![],
            regs,
            reg_stride: 0,
            record: Some(record),
            ok: None,
        }
    }

    /// A CoreIR value argument as caller and callee type it.
    pub fn value_parameter(&self, t: Ty) -> Parameter {
        Parameter::extended(ty(t), extension(self.target, t))
    }

    pub fn parameters(
        &self,
        sig: &Signature,
        layouts: &CallLayouts,
        facts: bool,
        extent: impl Fn(PlaceTy) -> u64,
    ) -> (Vec<Parameter>, Vec<Local>) {
        let mut params = vec![];
        let mut locals = vec![];
        if self.indirect {
            params.push(Parameter::sret(self.storage.clone()));
            locals.push(Local::SRET);
        }
        params.push(Type::Ptr.into());
        locals.push(Local::CONTEXT);
        if sig.dest.is_some() && self.record.is_none() {
            params.push(Parameter {
                noalias: facts,
                ..Parameter::plain(Type::Ptr)
            });
            locals.push(if self.ok.is_some() {
                Local::COLD
            } else {
                Local::DEST
            });
        }
        for (i, p) in sig.params.iter().enumerate() {
            if let Some(record) = layouts.parameter(*p) {
                for (j, &(_, t)) in record.fields().iter().enumerate() {
                    params.push(self.value_parameter(t));
                    locals.push(Local::ParamField(i, j));
                }
                continue;
            }
            params.push(match p {
                Param::Value(t) => self.value_parameter(*t),
                Param::Address { loan, place } if facts => Parameter {
                    noalias: true,
                    // FORM address arguments lend a live place throughout the
                    // call. Its readable extent lets argument promotion load
                    // fields even when a Step failure precedes their first use.
                    dereferenceable: extent(*place),
                    // Loan::View freezes the place for the whole call, including
                    // cleanup. Shared read-only aliases may still overlap.
                    readonly: *loan == Loan::View,
                    ..Parameter::plain(Type::Ptr)
                },
                Param::Address { .. } => Type::Ptr.into(),
            });
            locals.push(Local::Param(i));
        }
        params.push(Type::I64.into());
        locals.push(Local::CALLER_SITE);
        (params, locals)
    }

    pub fn signature(&self, params: Vec<Parameter>) -> FnSig {
        FnSig {
            result: self.ty.clone(),
            // An internal result in one integer register narrower than a word
            // (a packed payload and status, as i40) is zero-extended by its
            // callee on every target, so no caller re-masks the high bits; the
            // definition, its declarations and every call site take this one sig.
            extension: match (self.internal, &self.ty, self.fields.as_slice()) {
                (true, Type::Int(bits), _) if *bits < 64 => Extension::Zero,
                (true, ..) => Extension::None,
                (false, _, [t]) => extension(self.target, *t),
                (false, ..) => Extension::None,
            },
            nonnull: false,
            params,
        }
    }

    pub fn on_target(target: hirsel_core_layout::Target, fields: &[Ty]) -> Self {
        let mut offsets = vec![];
        let mut end = 0u64;
        let mut alignment = 1;
        for &t in fields {
            let n = size(t);
            alignment = alignment.max(n);
            end = end.next_multiple_of(n);
            offsets.push(end);
            end += n;
        }
        let extent = end.next_multiple_of(alignment);
        let storage = Type::bytes(extent.max(16));
        let homogeneous = target == hirsel_core_layout::Target::Aarch64Linux
            && (2..=4).contains(&fields.len())
            && fields[0].is_float()
            && fields.iter().all(|t| *t == fields[0]);
        let indirect = fields.len() > 1 && extent > 16 && !homogeneous;
        let regs = if fields.len() <= 1 {
            fields.iter().map(|t| ty(*t)).collect()
        } else if indirect {
            vec![]
        } else if homogeneous {
            fields.iter().map(|t| ty(*t)).collect()
        } else if target == hirsel_core_layout::Target::Aarch64Linux {
            if extent <= 8 {
                vec![Type::Int(extent as u32 * 8)]
            } else {
                vec![Type::I64, Type::I64]
            }
        } else {
            (0..end.div_ceil(8))
                .map(|chunk| {
                    let fs = fields
                        .iter()
                        .zip(&offsets)
                        .filter(|(_, off)| **off / 8 == chunk)
                        .collect::<Vec<_>>();
                    if fs.iter().any(|(t, _)| !t.is_float()) {
                        Type::Int((end - chunk * 8).min(8) as u32 * 8)
                    } else if fs.iter().any(|(t, _)| size(**t) == 8) {
                        Type::Double
                    } else if fs.len() == 2 {
                        Type::Vector(2, Box::new(Type::Float))
                    } else {
                        Type::Float
                    }
                })
                .collect::<Vec<_>>()
        };
        let ty = match regs.as_slice() {
            [] => Type::Void,
            [one] => one.clone(),
            _ => Type::structure(regs.clone()),
        };
        let reg_stride = if homogeneous { size(fields[0]) } else { 8 };
        Self {
            target,
            internal: false,
            ty,
            storage,
            indirect,
            fields: fields.to_vec(),
            offsets,
            regs,
            reg_stride,
            record: None,
            ok: None,
        }
    }
}

pub(super) fn memcpy(to: &Value, from: &Value, bytes: Value) -> [CArg; 4] {
    [
        ptr(to.clone()),
        ptr(from.clone()),
        word(bytes),
        arg(Type::I1, FALSE),
    ]
}

impl FunctionEmitter<'_> {
    fn shadow_argument(&self, site: &CallSite) -> Option<usize> {
        let Callee::Direct(f) = site.callee else {
            return None;
        };
        if self.shadow_callee != Some(f) {
            return None;
        }
        let sig = &self.module.signatures[site.signature.index()];
        let Some(Param::Address {
            place: PlaceTy::Declared(t),
            loan: Loan::Mut,
        }) = sig.params.first()
        else {
            return None;
        };
        if !hirsel_core_ir::roles::carrier(self.module, *t) || self.fixed(*t).size != 24 {
            return None;
        }
        Some(usize::from(sig.dest.is_some()))
    }
    fn shadow_copy_in(&mut self, original: &Value) -> Value {
        let shadow = self.temp(Type::bytes(24));
        self.runtime_void("llvm.lifetime.start.p0", &[ptr(shadow.clone())]);
        self.runtime_void("llvm.memcpy.p0.p0.i64", &memcpy(&shadow, original, int(24)));
        shadow
    }
    fn shadow_copy_back(&mut self, original: &Value, shadow: &Value) {
        // The callee may change the carrier even when it returns failure.
        // These word copies transfer no ownership, marks, drops or charges.
        self.runtime_void("llvm.memcpy.p0.p0.i64", &memcpy(original, shadow, int(24)));
        self.runtime_void("llvm.lifetime.end.p0", &[ptr(shadow.clone())]);
    }
    /// The arguments of a direct or indirect CoreIR call, after the context.
    pub(super) fn load_record(&mut self, record: ScalarRecord, address: &Value) -> Vec<Value> {
        record
            .fields()
            .iter()
            .map(|&(offset, t)| {
                let at = self.offset(address, offset);
                self.load(&ty(t), &at, 1)
            })
            .collect()
    }

    pub(super) fn store_record(&mut self, record: ScalarRecord, address: &Value, values: &[Value]) {
        for (&(offset, t), value) in record.fields().iter().zip(values) {
            let at = self.offset(address, offset);
            self.store(&ty(t), value, &at, 1);
        }
    }

    fn call_arguments(
        &mut self,
        site: &CallSite,
        layouts: &CallLayouts,
        result: Option<ScalarRecord>,
        forward: bool,
        cold: Option<Value>,
    ) -> (Vec<CArg>, Option<(Value, Value)>) {
        let sig = &self.module.signatures[site.signature.index()];
        let mut args = vec![ptr(CONTEXT)];
        let shadow_index = self.shadow_argument(site);
        let mut carrier_shadow = None;
        let actual = site.args.get(&self.body.pools.args).unwrap();
        let skip = usize::from(sig.dest.is_some());
        if skip != 0 && result.is_none() {
            let Arg::Address(dest) = actual[0] else {
                unreachable!()
            };
            let place = if let Some(cold) = cold {
                cold
            } else if forward {
                Local::DEST.into()
            } else {
                self.address(dest)
            };
            args.push(ptr(place));
        }
        for (i, (&a, &p)) in actual[skip..].iter().zip(&sig.params).enumerate() {
            let operand = match a {
                Arg::Address(a) => {
                    let pointer = self.address(a);
                    if let Some(record) = layouts.parameter(p) {
                        let values = self.load_record(record, &pointer);
                        args.extend(
                            record
                                .fields()
                                .iter()
                                .zip(values)
                                .map(|(&(_, t), x)| (self.abi.value_parameter(t), x)),
                        );
                        continue;
                    }
                    if shadow_index == Some(i + skip) {
                        let shadow = self.shadow_copy_in(&pointer);
                        carrier_shadow = Some((pointer, shadow.clone()));
                        ptr(shadow)
                    } else {
                        ptr(pointer)
                    }
                }
                Arg::Value(v) => {
                    let x = self.numeric(v);
                    (self.abi.value_parameter(self.vty(v)), x)
                }
            };
            args.push(operand);
        }
        (args, carrier_shadow)
    }
    /// How a call reaches its callee: its convention and whether it may raise.
    /// A direct call to a body uses the body's private symbol; a declaration, an
    /// indirect call and every export use the public form.
    fn callee_abi(&self, callee: Callee, sig: &Signature, layouts: &CallLayouts) -> (Return, bool) {
        let (has_status, body) = match callee {
            Callee::Direct(f) => (
                self.failures.may_fail[f.index()],
                self.module.functions[f.index()].body.is_some(),
            ),
            _ => (true, false),
        };
        (
            Return::private(self.abi.target, sig, layouts, has_status, body),
            has_status,
        )
    }
    fn callee(&mut self, callee: Callee, has_status: bool, ok: bool) -> Value {
        match callee {
            Callee::Direct(f) => {
                let name = self.items.function_symbol(f);
                let mut name = match self.level {
                    Some(level) if self.items.is_versioned(f) => versions::symbol(name, level),
                    _ if self.items.step_dispatch[f.index()].is_some() => step_free::checked(name),
                    _ => name.to_owned(),
                };
                name.push_str(private_suffix(has_status, ok));
                Value::global(name)
            }
            Callee::Indirect(v) => self.numeric(v),
            Callee::Runtime(_) => unreachable!(),
        }
    }
    pub(super) fn return_call(
        &mut self,
        site: &CallSite,
        ending: &super::forward::Ending,
        layouts: &CallLayouts,
    ) -> bool {
        if matches!(site.callee, Callee::Runtime(_)) {
            return false;
        }
        let sig = &self.module.signatures[site.signature.index()];
        let (abi, has_status) = self.callee_abi(site.callee, sig, layouts);
        if has_status && !self.has_status {
            return false;
        }
        // A `Result` register callee forwards only into the same convention: it
        // returns this body's outcome and writes any other case to its cold slot.
        // Any other pairing receives the result at an ordinary call.
        if (abi.ok.is_some() || self.abi.ok.is_some())
            && !(abi.ok.is_some()
                && self.abi.ok.is_some()
                && ending.destination
                && abi.fields == self.abi.fields)
        {
            return false;
        }
        let cold = abi.ok.map(|_| Value::Local(Local::COLD));
        let (mut args, carrier_shadow) =
            self.call_arguments(site, layouts, abi.record, ending.destination, cold);
        let callee = self.callee(site.callee, has_status, abi.ok.is_some());
        let site_word = self.call_site_word(site.callee);
        args.push(word(site_word));
        if carrier_shadow.is_some() || abi.fields != self.abi.fields {
            // An infallible private callee has no status; the public caller
            // still returns its constant zero. Its destination can be forwarded
            // even though that return convention prevents a tail jump.
            let values = self.invoke(&abi, &callee, args, &site_attrs(site));
            if let Some((original, shadow)) = carrier_shadow {
                self.shadow_copy_back(&original, &shadow);
            }
            if abi.fields != self.abi.fields {
                self.return_values(&values);
            } else {
                self.return_abi_values(&values);
            }
            return true;
        }
        if abi.indirect {
            args.insert(
                0,
                (Parameter::sret(abi.storage.clone()), Local::SRET.into()),
            );
        }
        let fn_sig = abi.signature(args.iter().map(|(p, _)| p.clone()).collect());
        let values: Vec<_> = args.into_iter().map(|(_, v)| v).collect();
        let attrs = CallAttrs {
            tail: ending.frame_safe,
            ..site_attrs(site)
        };
        if ending.frame_safe {
            // No argument points into the frame, so the scoped slots end
            // before the call and it stays in tail position.
            self.end_scoped();
            self.scoped_ended = true;
        }
        let result = self.call(&fn_sig, &callee, &values, &attrs);
        if abi.ty.is_void() {
            self.ret_void();
        } else {
            self.ret(&abi.ty, &result);
        }
        true
    }

    pub(super) fn return_values(&mut self, values: &[Value]) {
        let mut returned = values.to_vec();
        if self.has_status {
            returned.push(int(0));
        }
        self.return_abi_values(&returned);
    }
    pub(super) fn return_at(&mut self, block: usize, values: PoolRange<ValueId>) {
        self.step_refund();
        if (self.abi.ok.is_some() || self.abi.record.is_some())
            && self.body.parked_returns.contains(&BlockId(block as u32))
        {
            // The caller checks the continuation word before using the
            // result. Return defined register values without reading its place.
            self.ret(&self.abi.ty.clone(), &Value::Zero);
        } else if let Some(ok) = self.abi.ok {
            self.return_ok(ok);
        } else if let Some(record) = self.abi.record {
            let fields = self.load_record(record, &Local::DEST.into());
            self.return_values(&fields);
        } else {
            let values = values
                .get(&self.body.pools.values)
                .unwrap()
                .iter()
                .map(|v| self.numeric(*v))
                .collect::<Vec<_>>();
            self.return_values(&values);
        }
    }
    pub(super) fn return_failure(&mut self) {
        if !self.to_raised_exit() {
            self.emit_failure_return();
        }
    }
    pub(super) fn emit_failure_return(&mut self) {
        let abi_ty = self.abi.ty.clone();
        if self.abi.fields.len() == 1 {
            self.ret(&abi_ty, &int(1));
        } else if self.abi.record.is_some() || self.abi.ok.is_some() {
            let mut values = vec![Value::Zero; self.abi.fields.len() - 1];
            values.push(int(i128::from(OUTCOME_RAISED)));
            self.return_abi_values(&values);
        } else if self.abi.indirect {
            let storage = self.abi.storage.clone();
            let sret = Local::SRET.into();
            self.store(&storage, &Value::Zero, &sret, 8);
            let at = self.offset(&sret, *self.abi.offsets.last().unwrap());
            self.store(&Type::I8, &int(1), &at, 1);
            self.ret_void();
        } else {
            // The same little-endian ABI coercion as return_abi_values: every
            // result and padding byte is zero, except the final status byte.
            // Its integer class determines the register containing the one.
            let status = *self.abi.offsets.last().unwrap();
            let values: Vec<_> = self
                .abi
                .regs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let start = i as u64 * self.abi.reg_stride;
                    let value = if status >= start && status < start + self.abi.reg_stride {
                        int(1u64 << ((status - start) * 8))
                    } else {
                        Value::Zero
                    };
                    (t.clone(), value)
                })
                .collect();
            let value = if values.len() == 1 {
                values[0].1.clone()
            } else {
                Value::Struct {
                    packed: false,
                    fields: values,
                }
            };
            self.ret(&abi_ty, &value);
        }
    }
    fn return_abi_values(&mut self, values: &[Value]) {
        let abi_ty = self.abi.ty.clone();
        if self.abi.fields.len() <= 1 {
            if let Some(x) = values.first() {
                self.ret(&abi_ty, x);
            } else {
                self.ret_void();
            }
            return;
        }
        if self.abi.record.is_some() || self.abi.ok.is_some() {
            let regs = self.abi.regs.clone();
            let mut result = Value::Poison;
            for (i, (t, value)) in regs.iter().zip(values).enumerate() {
                result = self.emit(Operation::Insert(&abi_ty, &result, t, value, i as u32));
            }
            self.ret(&abi_ty, &result);
            return;
        }
        let storage = self.abi.storage.clone();
        let p = if self.abi.indirect {
            Local::SRET.into()
        } else {
            self.temp(storage.clone())
        };
        // Padding is defined, so register coercions never observe poison.
        self.store(&storage, &Value::Zero, &p, 8);
        for ((t, off), x) in self
            .abi
            .fields
            .clone()
            .iter()
            .zip(self.abi.offsets.clone())
            .zip(values)
        {
            let at = self.offset(&p, off);
            self.store(&ty(*t), x, &at, 1);
        }
        if self.abi.indirect {
            self.ret_void();
            return;
        }
        let regs = self.abi.regs.clone();
        let mut r = Value::Poison;
        for (i, t) in regs.iter().enumerate() {
            let at = self.offset(&p, i as u64 * self.abi.reg_stride);
            let x = self.load(t, &at, 1);
            r = if regs.len() == 1 {
                x
            } else {
                self.emit(Operation::Insert(&abi_ty, &r, t, &x, i as u32))
            };
        }
        self.ret(&abi_ty, &r);
    }
    pub(super) fn call_site(&mut self, site: &CallSite) -> Result<()> {
        if let Callee::Runtime(op) = site.callee {
            return self.runtime_call(op, site);
        }
        let sig = &self.module.signatures[site.signature.index()];
        let layouts = self.call_layouts.unwrap();
        let (abi, has_status) = self.callee_abi(site.callee, sig, layouts);
        let cold = abi.ok.map(|ok| self.cold_slot(ok));
        let (mut args, carrier_shadow) =
            self.call_arguments(site, layouts, abi.record, false, cold.clone());
        let callee = self.callee(site.callee, has_status, abi.ok.is_some());
        let site_word = self.call_site_word(site.callee);
        args.push(word(site_word));
        let mut values = self.invoke(&abi, &callee, args, &site_attrs(site));
        if let Some((original, shadow)) = carrier_shadow {
            self.shadow_copy_back(&original, &shadow);
        }
        if let (Some(ok), Some(cold)) = (abi.ok, cold) {
            let Arg::Address(dest) = site.args.get(&self.body.pools.args).unwrap()[0] else {
                unreachable!()
            };
            let dest = self.address(dest);
            self.receive_ok(ok, values, &cold, &dest, has_status, site.fail);
            return Ok(());
        }
        if has_status {
            let status = values.pop().unwrap();
            let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
            self.branch_failure(&failed, site.fail, None);
        }
        if let Some(record) = abi.record {
            let Arg::Address(dest) = site.args.get(&self.body.pools.args).unwrap()[0] else {
                unreachable!()
            };
            let dest = self.address(dest);
            self.store_record(record, &dest, &values);
        }
        for (v, x) in site
            .results
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(*v, &x);
        }
        Ok(())
    }
    pub(super) fn invoke(
        &mut self,
        abi: &Return,
        callee: &Value,
        mut args: Vec<CArg>,
        attrs: &CallAttrs,
    ) -> Vec<Value> {
        let registers = abi.record.is_some() || abi.ok.is_some();
        let scratch = if abi.fields.len() > 1 && !registers {
            Some(self.temp(abi.storage.clone()))
        } else {
            None
        };
        if abi.indirect {
            args.insert(
                0,
                (
                    Parameter::sret(abi.storage.clone()),
                    scratch.clone().unwrap(),
                ),
            );
        }
        let fn_sig = abi.signature(args.iter().map(|(p, _)| p.clone()).collect());
        let arguments: Vec<_> = args.into_iter().map(|(_, v)| v).collect();
        let r = self.call(&fn_sig, callee, &arguments, attrs);
        let mut values = vec![];
        if abi.fields.len() == 1 {
            values.push(r);
        } else if registers {
            values.extend((0..abi.fields.len()).map(|i| self.extract(&abi.ty, &r, i as u32)));
        } else if let Some(p) = scratch {
            if !abi.indirect {
                for (i, t) in abi.regs.iter().enumerate() {
                    let x = if abi.regs.len() == 1 {
                        r.clone()
                    } else {
                        self.extract(&abi.ty, &r, i as u32)
                    };
                    let at = self.offset(&p, i as u64 * abi.reg_stride);
                    self.store(t, &x, &at, 1);
                }
            }
            for (t, off) in abi.fields.iter().zip(&abi.offsets) {
                let at = self.offset(&p, *off);
                values.push(self.load(&ty(*t), &at, 1));
            }
        }
        values
    }

    /// A cold slot for a `Result` register callee's other case, live across the
    /// call only, so the caller's own destination never escapes.
    fn cold_slot(&mut self, ok: OkResult) -> Value {
        let slot = self.fresh();
        self.sink
            .alloca(slot, &Type::bytes(ok.ok.size), ok.align as u32);
        let slot = Value::Local(slot);
        self.runtime_void("llvm.lifetime.start.p0", &[ptr(slot.clone())]);
        slot
    }

    /// Receive a `Result` register call into `dest`: the `Ok` case from its
    /// leaves, a raised failure on the failure edge, and any other case from the
    /// cold slot, which then ends.
    fn receive_ok(
        &mut self,
        ok: OkResult,
        mut values: Vec<Value>,
        cold: &Value,
        dest: &Value,
        has_status: bool,
        fail: Option<BlockId>,
    ) {
        let outcome = values.pop().unwrap();
        let n = self.counter();
        let (hot, other, done) = (
            Label::new("ok_case", n),
            Label::new("other_case", n),
            Label::new("case_received", n),
        );
        let is_ok = self.cmp(
            Predicate::Eq,
            &Type::I8,
            &outcome,
            &int(i128::from(OUTCOME_OK)),
        );
        self.likely(&is_ok, hot, other);
        self.block(other);
        if has_status {
            let raised = self.cmp(
                Predicate::Eq,
                &Type::I8,
                &outcome,
                &int(i128::from(OUTCOME_RAISED)),
            );
            self.branch_failure(&raised, fail, None);
        }
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &memcpy(dest, cold, int(ok.ok.size)),
        );
        self.runtime_void("llvm.lifetime.end.p0", &[ptr(cold.clone())]);
        self.br(done);
        self.block(hot);
        self.runtime_void("llvm.lifetime.end.p0", &[ptr(cold.clone())]);
        self.set_case(&ok.tag, ok.case, dest);
        self.store_record(ok.ok, dest, &values);
        self.br(done);
        self.block(done);
    }

    /// Return the `Result` at `Local::DEST` in this body's register convention.
    pub(super) fn return_ok(&mut self, ok: OkResult) {
        let dest = Value::Local(Local::DEST);
        let n = self.counter();
        let (hot, other) = (Label::new("return_ok", n), Label::new("return_other", n));
        let case = self.case_value(&ok.tag, &dest);
        let is_ok = self.cmp(Predicate::Eq, &Type::I32, &case, &int(i128::from(ok.case)));
        self.likely(&is_ok, hot, other);
        self.block(other);
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &memcpy(&Local::COLD.into(), &dest, int(ok.ok.size)),
        );
        let mut values = vec![Value::Zero; self.abi.fields.len() - 1];
        values.push(int(i128::from(OUTCOME_OTHER)));
        self.return_abi_values(&values);
        self.block(hot);
        let mut values = self.load_record(ok.ok, &dest);
        values.push(int(i128::from(OUTCOME_OK)));
        self.return_abi_values(&values);
    }

    /// The public symbol of a body with a private convention (`private_suffix`):
    /// the status ABI, with its destination, that exports and indirect calls use.
    pub(super) fn public_wrapper(
        &mut self,
        name: &str,
        sig: &Signature,
        layouts: &CallLayouts,
        facts: bool,
        debug: Option<(usize, usize)>,
        has_status: bool,
    ) {
        self.debug_location = debug.map(|(_, location)| location);
        self.view_invariants.clear();
        // The wrapper has no slots of its own.
        self.scoped = hirsel_core_ir::extent::ScopedSlots::none(0);
        self.live = None;
        self.next = 0;
        self.has_status = true;
        self.abi = Return::for_signature(self.abi.target, sig, layouts, true);
        let (params, locals) = self
            .abi
            .parameters(sig, layouts, facts, |p| self.place_layout(p).0);
        let attrs = FnAttrs {
            linkage: Linkage::WeakOdr,
            subprogram: debug.map(|(scope, _)| scope),
            level: self.level,
            ..FnAttrs::default()
        };
        let signature = self.abi.signature(params);
        self.sink.begin_function(name, &signature, &locals, &attrs);
        let args = signature
            .params
            .into_iter()
            .zip(locals)
            .skip(usize::from(self.abi.indirect))
            .map(|(param, local)| (Parameter::extended(param.ty, param.extension), local.into()))
            .collect();
        let private = Return::private(self.abi.target, sig, layouts, has_status, true);
        let callee = Value::global(format!(
            "{name}{}",
            private_suffix(has_status, private.ok.is_some())
        ));
        let Some(ok) = private.ok else {
            let values = self.invoke(&private, &callee, args, &CallAttrs::default());
            self.return_values(&values);
            self.sink.end_function();
            return;
        };
        let cold = self.cold_slot(ok);
        let args = args
            .into_iter()
            .map(|(param, value)| {
                if value == Value::Local(Local::DEST) {
                    (param, cold.clone())
                } else {
                    (param, value)
                }
            })
            .collect();
        let values = self.invoke(&private, &callee, args, &CallAttrs::default());
        self.receive_ok(ok, values, &cold, &Local::DEST.into(), has_status, None);
        self.return_values(&[]);
        self.sink.end_function();
    }
}

/// Large C composites are stack byval on SysV and caller-owned copies passed
/// by pointer on AAPCS64. All runtime composites here are larger than16 bytes.
pub(super) fn composite_argument(target: Target, bytes: usize) -> Parameter {
    match target {
        Target::X86_64Linux => Parameter {
            byval: Some(Type::bytes(bytes as u64)),
            align: 8,
            ..Parameter::plain(Type::Ptr)
        },
        Target::Aarch64Linux => Type::Ptr.into(),
    }
}

/// A call the front marked cold (core-ir CallSite::cold, the growth path) is
/// cold and not inlined, so the inliner keeps the growth body out of the hot
/// caller; the branch into its block is weighted unlikely (`cold_edge`).
fn site_attrs(site: &CallSite) -> CallAttrs {
    CallAttrs {
        cold: site.cold,
        inline: if site.cold {
            Inline::Never
        } else {
            Inline::Default
        },
        ..CallAttrs::default()
    }
}
