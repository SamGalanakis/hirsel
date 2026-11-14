//! Linux C result classification, adapted from rustc_target's callconv/{x86_64,
//! aarch64}.rs. SysV merges eightbyte classes; AAPCS64 uses homogeneous floating
//! aggregates or integer carriers, and otherwise the dedicated sret register.
//! Only FORM's naturally aligned scalar fields occur.
use super::*;

pub(super) fn view_input(sig: &Signature) -> bool {
    sig.params.iter().any(|p| {
        matches!(
            p,
            Param::Address {
                loan: Loan::View,
                ..
            }
        )
    })
}

pub(super) struct Return {
    pub target: hirsel_core_layout::Target,
    pub ty: Type,
    pub storage: Type,
    pub indirect: bool,
    pub fields: Vec<Ty>,
    pub offsets: Vec<u64>,
    pub regs: Vec<Type>,
    pub reg_stride: u64,
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
        let mut fields = fields.to_vec();
        fields.push(Ty::Scalar(Scalar::U8));
        Self::on_target(target, &fields)
    }

    /// A CoreIR value argument as caller and callee type it.
    pub fn value_parameter(&self, t: Ty) -> Parameter {
        Parameter::extended(ty(t), extension(self.target, t))
    }

    pub fn parameters(&self, sig: &Signature, facts: bool) -> (Vec<Parameter>, Vec<Local>) {
        let mut params = vec![];
        let mut locals = vec![];
        if self.indirect {
            params.push(Parameter::sret(self.storage.clone()));
            locals.push(Local::SRET);
        }
        params.push(Type::Ptr.into());
        locals.push(Local::CONTEXT);
        if sig.dest.is_some() {
            params.push(Parameter {
                noalias: facts,
                ..Parameter::plain(Type::Ptr)
            });
            locals.push(Local::DEST);
        }
        for (i, p) in sig.params.iter().enumerate() {
            params.push(match p {
                Param::Value(t) => self.value_parameter(*t),
                Param::Address { loan, .. } if facts => Parameter {
                    noalias: true,
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
            extension: match self.fields.as_slice() {
                [t] => extension(self.target, *t),
                _ => Extension::None,
            },
            nonnull: false,
            params,
        }
    }

    #[cfg(all(test, not(hirsel_lazy_core)))]
    pub fn new(fields: &[Ty]) -> Self {
        Self::on_target(hirsel_core_layout::Target::X86_64Linux, fields)
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
            ty,
            storage,
            indirect,
            fields: fields.to_vec(),
            offsets,
            regs,
            reg_stride,
        }
    }
}

fn memcpy(to: &Value, from: &Value, bytes: Value) -> [CArg; 4] {
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
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &memcpy(&shadow, original, int(24)),
        );
        shadow
    }
    fn shadow_copy_back(&mut self, original: &Value, shadow: &Value) {
        // The callee may change the carrier even when it returns failure.
        // These word copies transfer no ownership, marks, drops or charges.
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &memcpy(original, shadow, int(24)),
        );
        self.runtime_void("llvm.lifetime.end.p0", &[ptr(shadow.clone())]);
    }
    /// The arguments of a direct or indirect CoreIR call, after the context.
    fn call_arguments(
        &mut self,
        site: &CallSite,
        destination: bool,
    ) -> (Vec<CArg>, Option<(Value, Value)>) {
        let mut args = vec![ptr(CONTEXT)];
        let shadow_index = self.shadow_argument(site);
        let mut carrier_shadow = None;
        for (i, a) in site
            .args
            .get(&self.body.pools.args)
            .unwrap()
            .iter()
            .enumerate()
        {
            args.push(if destination && i == 0 {
                ptr(Local::DEST)
            } else {
                match *a {
                    Arg::Address(a) => {
                        let p = self.address(a);
                        if shadow_index == Some(i) {
                            let shadow = self.shadow_copy_in(&p);
                            carrier_shadow = Some((p, shadow.clone()));
                            ptr(shadow)
                        } else {
                            ptr(p)
                        }
                    }
                    Arg::Value(v) => {
                        let x = self.numeric(v);
                        (self.abi.value_parameter(self.vty(v)), x)
                    }
                }
            });
        }
        (args, carrier_shadow)
    }
    fn callee(&mut self, callee: Callee, has_status: bool) -> Value {
        match callee {
            Callee::Direct(f) => Value::global(format!(
                "{}{}",
                symbol(self.module, f),
                if has_status { "" } else { ".infallible" }
            )),
            Callee::Indirect(v) => self.numeric(v),
            Callee::Runtime(_) => unreachable!(),
        }
    }
    pub(super) fn return_call(&mut self, site: &CallSite, ending: &super::forward::Ending) -> bool {
        let has_status = match site.callee {
            Callee::Direct(f) => self.failures.may_fail[f.index()],
            Callee::Indirect(_) => true,
            Callee::Runtime(_) => return false,
        };
        let sig = &self.module.signatures[site.signature.index()];
        let abi = if has_status {
            Return::internal_on_target(self.abi.target, &sig.results)
        } else {
            Return::on_target(self.abi.target, &sig.results)
        };
        if has_status && !self.has_status {
            return false;
        }
        let (mut args, carrier_shadow) = self.call_arguments(site, ending.destination);
        let callee = self.callee(site.callee, has_status);
        let site_word = self.call_site_word(site.callee);
        args.push(word(site_word));
        if carrier_shadow.is_some() || abi.fields != self.abi.fields {
            // An infallible private callee has no status; the public caller
            // still returns its constant zero. Its destination can be forwarded
            // even though that return convention prevents a tail jump.
            let values = self.invoke(&abi, &callee, args);
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
            args.insert(0, (Parameter::sret(abi.storage.clone()), Local::SRET.into()));
        }
        let fn_sig = abi.signature(args.iter().map(|(p, _)| p.clone()).collect());
        let values: Vec<_> = args.into_iter().map(|(_, v)| v).collect();
        let attrs = CallAttrs {
            tail: ending.frame_safe,
            ..CallAttrs::default()
        };
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
    pub(super) fn return_failure(&mut self) {
        let abi_ty = self.abi.ty.clone();
        if self.abi.fields.len() == 1 {
            self.ret(&abi_ty, &int(1));
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
        let has_status = match site.callee {
            Callee::Direct(f) => self.failures.may_fail[f.index()],
            _ => true,
        };
        let abi = if has_status {
            Return::internal_on_target(self.abi.target, &sig.results)
        } else {
            Return::on_target(self.abi.target, &sig.results)
        };
        let (mut args, carrier_shadow) = self.call_arguments(site, false);
        let callee = self.callee(site.callee, has_status);
        let site_word = self.call_site_word(site.callee);
        args.push(word(site_word));
        let mut values = self.invoke(&abi, &callee, args);
        if let Some((original, shadow)) = carrier_shadow {
            self.shadow_copy_back(&original, &shadow);
        }
        if has_status {
            let status = values.pop().unwrap();
            let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
            self.branch_failure(&failed, site.fail, None);
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
    pub(super) fn invoke(&mut self, abi: &Return, callee: &Value, mut args: Vec<CArg>) -> Vec<Value> {
        let scratch = if abi.fields.len() > 1 {
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
        let r = self.call(&fn_sig, callee, &arguments, &CallAttrs::default());
        let mut values = vec![];
        if abi.fields.len() == 1 {
            values.push(r);
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

    pub(super) fn infallible_wrapper(
        &mut self,
        name: &str,
        sig: &Signature,
        facts: bool,
        debug: Option<(usize, usize)>,
    ) {
        self.debug_location = debug.map(|(_, location)| location);
        self.next = 0;
        self.has_status = true;
        self.abi = Return::internal_on_target(self.abi.target, &sig.results);
        let (params, locals) = self.abi.parameters(sig, facts);
        let attrs = FnAttrs {
            view_input: view_input(sig),
            subprogram: debug.map(|(scope, _)| scope),
            ..FnAttrs::default()
        };
        let signature = self.abi.signature(params);
        self.sink.begin_function(name, &signature, &locals, &attrs);
        let mut args = vec![ptr(CONTEXT)];
        if sig.dest.is_some() {
            args.push(ptr(Local::DEST));
        }
        for (i, p) in sig.params.iter().enumerate() {
            args.push(match p {
                Param::Value(t) => (self.abi.value_parameter(*t), Local::Param(i).into()),
                Param::Address { .. } => ptr(Local::Param(i)),
            });
        }
        args.push(word(Local::CALLER_SITE));
        let values = self.invoke(
            &Return::on_target(self.abi.target, &sig.results),
            &Value::global(format!("{name}.infallible")),
            args,
        );
        self.return_values(&values);
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
