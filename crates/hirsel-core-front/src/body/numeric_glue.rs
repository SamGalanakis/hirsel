//! Numeric core calls in a rigid body retain an operation and type recipes.
//! Instances emit those operations at the call site using published scalar carriage.
use super::*;
use hirsel_core_layout::Body as ShapeBody;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NumericOp {
    Arithmetic(ArithMode, ArithOp),
    Narrow,
    TryNarrow,
}
impl NumericOp {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "narrow" => Self::Narrow,
            "try_narrow" => Self::TryNarrow,
            "checked_add" => Self::Arithmetic(ArithMode::Checked, ArithOp::Add),
            "checked_sub" => Self::Arithmetic(ArithMode::Checked, ArithOp::Sub),
            "checked_mul" => Self::Arithmetic(ArithMode::Checked, ArithOp::Mul),
            "checked_div" => Self::Arithmetic(ArithMode::Checked, ArithOp::Div),
            "wrapping_add" => Self::Arithmetic(ArithMode::Wrapping, ArithOp::Add),
            "wrapping_sub" => Self::Arithmetic(ArithMode::Wrapping, ArithOp::Sub),
            "wrapping_mul" => Self::Arithmetic(ArithMode::Wrapping, ArithOp::Mul),
            "saturating_add" => Self::Arithmetic(ArithMode::Saturating, ArithOp::Add),
            "saturating_sub" => Self::Arithmetic(ArithMode::Saturating, ArithOp::Sub),
            "saturating_mul" => Self::Arithmetic(ArithMode::Saturating, ArithOp::Mul),
            _ => return None,
        })
    }
    pub(super) fn optional(self) -> bool {
        matches!(
            self,
            Self::TryNarrow | Self::Arithmetic(ArithMode::Checked, _)
        )
    }
}

#[derive(Clone, Copy)]
pub(super) struct NumericAbi {
    function: FuncId,
    rows: [TypeId; 2],
    result: TyId,
    result_row: TypeId,
}
impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn generic_numeric(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let g = &self.p.fns[f];
        let Some(op) = NumericOp::named(&g.path.0.name) else {
            if self.live() {
                self.gap(n, "this core integer operation is not built");
            }
            return None;
        };
        let key = (f, tys[0], tys.get(1).copied().unwrap_or(ERROR));
        let abi = if let Some(&abi) = self.numeric_functions.get(&key) {
            abi
        } else {
            let result = subst(&mut self.t, g.result, tys);
            let result_row = self.e.type_row(self.p, &mut self.t, result)?;
            let mut rows = [TypeId(0); 2];
            let mut params = Vec::with_capacity(g.params.len());
            for (i, p) in g.params.iter().enumerate() {
                let ty = subst(&mut self.t, p.ty, tys);
                rows[i] = self.e.type_row(self.p, &mut self.t, ty)?;
                params.push(Param::Address {
                    place: PlaceTy::Declared(rows[i]),
                    loan: loan(p.mode),
                });
            }
            let definition = DefPath::core(&format!("int.${}", g.path.0.name));
            let function = self.e.add_function_abi(
                InstanceKey {
                    definition,
                    types: tys.iter().map(|&ty| self.name(ty)).collect(),
                    dictionaries: vec![],
                },
                Signature {
                    params,
                    results: vec![],
                    dest: Some(result_row),
                    never: false,
                },
                scalar_abi::SignatureAbi {
                    params: vec![true; g.params.len()],
                    result: true,
                },
            );
            self.e
                .generic_calls
                .entry(function)
                .or_insert_with(|| GenericCall {
                    operation: None,
                    arguments: tys
                        .iter()
                        .map(|&ty| NameRecipe::of(self.p, &self.t, ty))
                        .collect(),
                    dictionaries: vec![],
                });
            let abi = NumericAbi {
                function,
                rows,
                result,
                result_row,
            };
            self.numeric_functions.insert(key, abi);
            abi
        };
        if !self.live() {
            return None;
        }
        let mut args = [Arg::Value(ValueId(0)); 3];
        let out = self.new_slot(PlaceTy::Declared(abi.result_row), n);
        args[0] = Arg::Address(out);
        for (i, &value) in vals.iter().enumerate() {
            let ty = subst(&mut self.t, g.params[i].ty, tys);
            let rep = self.rep(ty).ok()?;
            let address = if matches!(rep, Rep::Place(_)) {
                self.address(
                    AddressBase::Value(value?),
                    &[],
                    PlaceTy::Declared(abi.rows[i]),
                    n,
                )
            } else {
                let at = self.new_slot(PlaceTy::Declared(abi.rows[i]), n);
                if let Rep::Scalar(ty) = rep {
                    let field = self.address(
                        AddressBase::Address(at),
                        &[Step::Field(0)],
                        PlaceTy::Value(ty),
                        n,
                    );
                    self.inst(
                        Inst::Store {
                            address: field,
                            value: value?,
                        },
                        n,
                    );
                }
                at
            };
            args[i + 1] = Arg::Address(address);
        }
        let signature = self.e.module.functions[abi.function.index()].signature;
        let args = PoolRange::push(
            &mut self.b.pools.args,
            args[..vals.len() + 1].iter().copied(),
        );
        let fail = self.fail_to(n);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(abi.function),
            signature,
            args,
            results: PoolRange::EMPTY,
            fail,
        });
        self.numeric_sites.push(scalar_abi::NumericSite {
            call,
            next_block: self.b.blocks.len() as u32,
            op,
        });
        self.inst(Inst::Call { call }, n);
        match self.rep(abi.result).ok()? {
            Rep::Scalar(ty) => {
                let field = self.address(
                    AddressBase::Address(out),
                    &[Step::Field(0)],
                    PlaceTy::Value(ty),
                    n,
                );
                Some(self.read_bits(field, ty, n))
            }
            Rep::Place(_) => {
                let value = self.pointer(out, n);
                self.temp(abi.result, Some(value));
                Some(value)
            }
            Rep::None => None,
        }
    }
}

fn scalar(module: &Module, row: TypeId) -> Scalar {
    match module.types[row.index()].carried {
        Carried::Value(Ty::Scalar(s)) => s,
        _ => unreachable!("a closed integer row carries its scalar"),
    }
}
pub(super) fn payload(module: &Module, row: TypeId) -> (Scalar, Vec<Step>) {
    let ShapeBody::Variant(cases) = &module.types[row.index()].body else {
        unreachable!("an optional numeric result is the core Option variant");
    };
    let mut steps = vec![Step::CaseField { case: 1, field: 0 }];
    let s = match cases[1].fields[0].ty {
        FieldTy::Scalar(s) => s,
        FieldTy::Inline(ty) => {
            steps.push(Step::Field(0));
            scalar(module, TypeId(ty.0))
        }
        _ => unreachable!("a numeric Option payload is a scalar or its inline row"),
    };
    (s, steps)
}
