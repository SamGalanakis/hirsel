//! Structural codec makers and their encoder, decoder and schema functions.
//! Each instance names the type closures it reads.
use super::key_glue::{Code, signature_id};
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecOp {
    /// `() -> Codec[T]`: the maker, the tuple row's codec hook.
    Of,
    /// The encoder `fn(T, mut Bytes) -> Result[(), EncodeError]`.
    Encode,
    /// The decoder `fn(Bytes, mut u64) -> Result[T, DecodeError]`.
    Decode,
    /// The describer `fn(mut Schema, Codec[T]) -> Result[(), EncodeError]`.
    Describe,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Format {
    Value,
    Key,
    Desc,
}

const FORMATS: [(Format, [&str; 4]); 3] = [
    (
        Format::Value,
        [
            "$tuple_of",
            "$tuple_encode",
            "$tuple_decode",
            "$tuple_describe",
        ],
    ),
    (
        Format::Key,
        ["$key_of", "$key_encode", "$key_decode", "$key_describe"],
    ),
    (
        Format::Desc,
        ["$desc_of", "$desc_encode", "$desc_decode", "$desc_describe"],
    ),
];
const OPS: [CodecOp; 4] = [
    CodecOp::Of,
    CodecOp::Encode,
    CodecOp::Decode,
    CodecOp::Describe,
];

/// The rows a tuple codec's operations read, which are its key's arguments in this
/// order: the tuple, `Codec[T]`, `Result[T, DecodeError]`, `Result[(), EncodeError]`, `Bytes`,
/// `Schema`, then per component its row, `Codec` and `Result[_, DecodeError]`. Naming the
/// components keeps their types in the key's closures, where a field alone would not
/// (a String field is the `seq u8` a Bytes field is).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rows {
    pub format: Format,
    pub tuple: TypeId,
    pub codec: TypeId,
    pub decoded: TypeId,
    pub written: TypeId,
    pub bytes: TypeId,
    pub schema: TypeId,
    pub parts: Vec<Part>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Part {
    pub ty: TypeId,
    pub codec: TypeId,
    pub decoded: TypeId,
}
impl Rows {
    pub fn arguments(&self) -> Vec<TypeId> {
        let mut xs = vec![
            self.tuple,
            self.codec,
            self.decoded,
            self.written,
            self.bytes,
            self.schema,
        ];
        for p in &self.parts {
            xs.extend([p.ty, p.codec, p.decoded]);
        }
        xs
    }
    fn from(format: Format, xs: &[TypeId]) -> Option<Self> {
        let (head, parts) = xs.split_at_checked(6)?;
        if parts.len() % 3 != 0 {
            return None;
        }
        Some(Self {
            format,
            tuple: head[0],
            codec: head[1],
            decoded: head[2],
            written: head[3],
            bytes: head[4],
            schema: head[5],
            parts: parts
                .chunks(3)
                .map(|p| Part {
                    ty: p[0],
                    codec: p[1],
                    decoded: p[2],
                })
                .collect(),
        })
    }
}

impl CodecOp {
    fn named(definition: &DefPath) -> Option<(Self, Format)> {
        let d = &definition.0;
        if d.package.name != "std" || d.modules != ["codec"] {
            return None;
        }
        FORMATS.iter().find_map(|(format, names)| {
            names
                .iter()
                .position(|n| *n == d.name)
                .map(|i| (OPS[i], *format))
        })
    }

    pub fn definition(self, of: &DefPath, format: Format) -> DefPath {
        let mut d = of.clone();
        let (_, names) = FORMATS.iter().find(|(mode, _)| *mode == format).unwrap();
        d.0.name = names[self as usize].into();
        d
    }
    /// The operation's instance at `rows`.
    pub(super) fn instance(self, of: &DefPath, rows: &Rows) -> InstanceKey {
        InstanceKey {
            definition: self.definition(of, rows.format),
            arguments: rows.arguments(),
            dictionaries: vec![],
        }
    }
    /// The tuple codec operation instance `key` names, and the rows it reads.
    pub fn of(key: &InstanceKey) -> Option<(Self, Rows)> {
        let (op, format) = Self::named(&key.definition)?;
        Some((op, Rows::from(format, &key.arguments)?))
    }
    /// The signature of this operation at a value row and its codec rows, by address:
    /// the encoder's, decoder's and describer's are a callable's (an environment
    /// pointer first), so a codec's field calls them as it calls any other.
    fn place(
        self,
        module: &Module,
        value: TypeId,
        codec: TypeId,
        decoded: TypeId,
        rows: &Rows,
        format: Format,
    ) -> Signature {
        let view = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::View,
        };
        let out = |ty| Param::Address {
            place: scalar_abi::mut_place(&module.types, ty),
            loan: Loan::Mut,
        };
        let env = Param::Value(Ty::Ptr);
        let (params, dest) = match self {
            Self::Of => (vec![], Some(codec)),
            Self::Encode => (
                vec![env, view(value), out(rows.bytes)],
                (format == Format::Value).then_some(rows.written),
            ),
            Self::Decode => (
                vec![
                    env,
                    view(rows.bytes),
                    Param::Address {
                        place: PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                        loan: Loan::Mut,
                    },
                ],
                Some(decoded),
            ),
            Self::Describe if format == Format::Value => {
                (vec![env, out(rows.schema), view(codec)], Some(rows.written))
            }
            Self::Describe => (vec![env, out(rows.schema)], Some(rows.written)),
        };
        Signature::new(params, vec![], dest)
    }
    /// Which parameters follow their rows' carriage: the viewed value, bytes or codec.
    pub fn abi(self, format: Format) -> scalar_abi::SignatureAbi {
        scalar_abi::SignatureAbi {
            params: match self {
                Self::Of => vec![],
                Self::Encode | Self::Decode => vec![false, true, false],
                Self::Describe if format == Format::Value => vec![false, false, true],
                Self::Describe => vec![false, false],
            },
            result: self != Self::Encode || format == Format::Value,
        }
    }
    pub fn signature(self, module: &Module, rows: &Rows) -> Signature {
        self.place(
            module,
            rows.tuple,
            rows.codec,
            rows.decoded,
            rows,
            rows.format,
        )
    }
    pub(super) fn sealed(
        self,
        module: &Module,
        value: TypeId,
        codec: TypeId,
        decoded: TypeId,
        rows: &Rows,
        format: Format,
    ) -> Signature {
        let mut sig = self.place(module, value, codec, decoded, rows, format);
        scalar_abi::Plan::signature(module, &mut sig, &self.abi(format));
        sig
    }
}

pub(super) fn is_glue(definition: &DefPath) -> bool {
    CodecOp::named(definition).is_some()
}

/// The row of `op` at `rows` in `module`, signature-only when it is new. `of` is any
/// definition of std.codec.
pub fn row(module: &mut Module, of: &DefPath, rows: &Rows, op: CodecOp) -> FuncId {
    let key = op.instance(of, rows);
    if let Some(i) = module
        .functions
        .iter()
        .position(|f| module.instances[f.instance.index()] == key)
    {
        return FuncId(i as u32);
    }
    let sealed = op.sealed(
        module,
        rows.tuple,
        rows.codec,
        rows.decoded,
        rows,
        rows.format,
    );
    let signature = signature_id(module, sealed);
    let instance = InstanceId(module.instances.len() as u32);
    module.instances.push(key);
    module.functions.push(Function {
        versioned: false,
        cold: false,
        instance,
        signature,
        body: None,
    });
    FuncId(module.functions.len() as u32 - 1)
}

/// The body of `op` at `rows`, alone: the operations and makers it calls or names are
/// signature-only rows, instances of their own.
pub fn derive(
    module: &mut Module,
    of: &DefPath,
    rows: &Rows,
    op: CodecOp,
) -> Result<FuncId, String> {
    let f = row(module, of, rows, op);
    if module.functions[f.index()].body.is_some() {
        return Ok(f);
    }
    let recipes = hirsel_core_layout::compute(&module.types)
        .map_err(|_| "the tuple codec's layout is not built".to_owned())?;
    let mut d = Derive {
        module,
        recipes,
        of,
        rows,
    };
    let place = op.signature(d.module, rows);
    let old = signature_id(d.module, place);
    let signature = &d.module.signatures[old.index()];
    let inputs = signature.params.len() + usize::from(signature.dest.is_some());
    let mut c = Code::new(&[Ty::Ptr; 4][..inputs]);
    let ps = c.body.blocks[0]
        .params
        .get(&c.body.pools.values)
        .unwrap()
        .to_vec();
    match op {
        CodecOp::Of => d.of_body(&mut c, ps[0])?,
        CodecOp::Encode => {
            if rows.format == Format::Value {
                d.encode_body(&mut c, Some(ps[0]), ps[2], ps[3])?;
            } else {
                d.encode_body(&mut c, None, ps[1], ps[2])?;
            }
        }
        CodecOp::Decode => d.decode_body(&mut c, ps[0], ps[2], ps[3])?,
        CodecOp::Describe => d.describe_body(&mut c, ps[0], ps[2])?,
    }
    order_instructions(&mut c.body);
    let mut sig = d.module.signatures[old.index()].clone();
    let plan = scalar_abi::Plan::signature(d.module, &mut sig, &op.abi(rows.format));
    let signature = signature_id(d.module, sig);
    let old_calls = c
        .body
        .calls
        .iter()
        .map(|call| scalar_abi::CallPlan::fixed(d.module, call.signature))
        .collect::<Vec<_>>();
    scalar_abi::rewrite(
        d.module,
        &d.recipes,
        &mut c.body,
        old,
        &plan,
        &old_calls,
        &[],
    );
    d.module.functions[f.index()].signature = signature;
    d.module.functions[f.index()].body = Some(c.body);
    Ok(f)
}

struct Derive<'a> {
    module: &'a mut Module,
    recipes: Vec<hirsel_core_layout::RecipeRow>,
    of: &'a DefPath,
    rows: &'a Rows,
}
impl Derive<'_> {
    fn address(c: &mut Code, base: AddressBase, steps: &[Step], ty: PlaceTy) -> AddressId {
        let steps = PoolRange::push(&mut c.body.pools.steps, steps.iter().copied());
        let id = AddressId(c.body.addresses.len() as u32);
        c.body.addresses.push(AddressInfo { base, steps, ty });
        c.inst(Inst::Address {
            address: id,
            fail: None,
        });
        id
    }
    fn slot(c: &mut Code, ty: TypeId) -> AddressId {
        let slot = SlotId(c.body.slots.len() as u32);
        c.body.slots.push(PlaceTy::Declared(ty));
        c.inst(Inst::StorageLive { slot });
        Self::address(c, AddressBase::Slot(slot), &[], PlaceTy::Declared(ty))
    }
    fn null(c: &mut Code) -> ValueId {
        let result = c.value(Ty::Ptr);
        c.inst(Inst::Const { result, bits: 0 });
        result
    }
    /// The place a component of row `ty` has in a record or a payload: a value place
    /// of the scalar a value-carried row is carried as, else the row.
    fn part_place(&self, ty: TypeId) -> PlaceTy {
        scalar_abi::mut_place(&self.module.types, ty)
    }
    fn call(&mut self, c: &mut Code, callee: Callee, signature: SigId, args: &[Arg]) {
        let args = PoolRange::push(&mut c.body.pools.args, args.iter().copied());
        let results = PoolRange::push(&mut c.body.pools.values, []);
        let call = CallId(c.body.calls.len() as u32);
        c.body.calls.push(CallSite {
            callee,
            signature,
            args,
            results,
            fail: None,
            cold: false,
        });
        c.inst(Inst::Call { call });
    }
    /// Continue where the variant at `at` is case 0; at case 1 go to `failed`.
    fn unless_failed(c: &mut Code, at: AddressId, failed: BlockId) {
        let case = c.value(Ty::Scalar(Scalar::U32));
        c.inst(Inst::Case {
            result: case,
            address: at,
        });
        let one = c.typed_constant(Scalar::U32, 1);
        let ok = c.compare(CompareOp::Ne, case, one);
        c.require(ok, failed);
    }
    fn returns(c: &mut Code) {
        let values = PoolRange::push(&mut c.body.pools.values, []);
        c.end(Terminator::Return(values));
    }
    /// Component `i`'s codec, made by its maker into a new slot.
    fn part_codec(&mut self, c: &mut Code, i: usize) -> Result<AddressId, String> {
        let part = self.rows.parts[i];
        let sig = signature_id(
            self.module,
            Signature::new(vec![], vec![], Some(part.codec)),
        );
        let maker = if self.rows.format == Format::Value {
            templates::codec_maker(self.module, self.of, part.ty, sig)?
        } else {
            super::ordered_codec::maker(self.module, self.of, part.ty, sig)?
        };
        let signature = self.module.functions[maker.index()].signature;
        let at = Self::slot(c, part.codec);
        self.call(c, Callee::Direct(maker), signature, &[Arg::Address(at)]);
        Ok(at)
    }
    /// The function field `field` (0 encoder, 1 decoder, 2 describer) of the codec at `at`.
    fn field_fn(c: &mut Code, at: AddressId, field: u32) -> ValueId {
        let f = Self::address(
            c,
            AddressBase::Address(at),
            &[Step::Field(field)],
            PlaceTy::Value(Ty::Ptr),
        );
        let result = c.value(Ty::Ptr);
        c.inst(Inst::Read {
            result,
            address: f,
            mode: ReadMode::Bits,
        });
        result
    }
    fn part_signature(&mut self, i: usize, op: CodecOp) -> SigId {
        let p = self.rows.parts[i];
        let format = if self.rows.format == Format::Value {
            Format::Value
        } else {
            Format::Key
        };
        let sig = op.sealed(self.module, p.ty, p.codec, p.decoded, self.rows, format);
        signature_id(self.module, sig)
    }

    fn of_body(&mut self, c: &mut Code, dest: ValueId) -> Result<(), String> {
        let mut least = c.constant(0);
        let mut empty = c.boolean(false);
        for i in 0..if self.rows.format == Format::Value {
            self.rows.parts.len()
        } else {
            0
        } {
            let at = self.part_codec(c, i)?;
            let n = Self::address(
                c,
                AddressBase::Address(at),
                &[Step::Field(3)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            );
            let n = c.read_at(n, Ty::Scalar(Scalar::U64));
            least = c.binary(BinaryOp::Add, least, n);
            let flag = Self::address(
                c,
                AddressBase::Address(at),
                &[Step::Field(4)],
                PlaceTy::Value(Ty::Scalar(Scalar::Bool)),
            );
            let flag = c.read_at(flag, Ty::Scalar(Scalar::Bool));
            let combined = c.value(Ty::Scalar(Scalar::Bool));
            c.inst(Inst::Binary {
                result: combined,
                op: BinaryOp::BitOr,
                left: empty,
                right: flag,
                fail: None,
            });
            empty = combined;
        }
        let to = Self::address(
            c,
            AddressBase::Value(dest),
            &[],
            PlaceTy::Declared(self.rows.codec),
        );
        for (field, op) in [CodecOp::Encode, CodecOp::Decode, CodecOp::Describe]
            .into_iter()
            .enumerate()
        {
            let function = row(self.module, self.of, self.rows, op);
            let result = c.value(Ty::Ptr);
            c.inst(Inst::FunctionRef { result, function });
            let at = Self::address(
                c,
                AddressBase::Address(to),
                &[Step::Field(field as u32)],
                PlaceTy::Value(Ty::Ptr),
            );
            c.inst(Inst::Store {
                address: at,
                value: result,
            });
        }
        if self.rows.format != Format::Value {
            Self::returns(c);
            return Ok(());
        }
        let at = Self::address(
            c,
            AddressBase::Address(to),
            &[Step::Field(3)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
        );
        c.inst(Inst::Store {
            address: at,
            value: least,
        });
        let at = Self::address(
            c,
            AddressBase::Address(to),
            &[Step::Field(4)],
            PlaceTy::Value(Ty::Scalar(Scalar::Bool)),
        );
        c.inst(Inst::Store {
            address: at,
            value: empty,
        });
        Self::returns(c);
        Ok(())
    }

    /// Each component by its encoder, into `dest` itself: the first refusal is the
    /// result, and the last component's `Ok` is the tuple's.
    fn encode_body(
        &mut self,
        c: &mut Code,
        dest: Option<ValueId>,
        value: ValueId,
        bytes: ValueId,
    ) -> Result<(), String> {
        let rows = self.rows;
        let outcome = dest.map(|dest| {
            (
                c.block(&[]),
                Self::address(
                    c,
                    AddressBase::Value(dest),
                    &[],
                    PlaceTy::Declared(rows.written),
                ),
            )
        });
        let tuple = Self::address(
            c,
            AddressBase::Value(value),
            &[],
            PlaceTy::Declared(rows.tuple),
        );
        let out = Self::address(
            c,
            AddressBase::Value(bytes),
            &[],
            scalar_abi::mut_place(&self.module.types, rows.bytes),
        );
        if rows.format == Format::Desc {
            let key = self.part_codec(c, 0)?;
            let (put, signature) = self.desc_helper(CodecOp::Encode);
            self.call(
                c,
                Callee::Direct(put),
                signature,
                &[Arg::Address(tuple), Arg::Address(key), Arg::Address(out)],
            );
            Self::returns(c);
            return Ok(());
        }
        for i in 0..rows.parts.len() {
            let codec = self.part_codec(c, i)?;
            let encoder = Self::field_fn(c, codec, 0);
            let env = Self::null(c);
            let part = rows.parts[i].ty;
            let mut args = vec![];
            if let Some((_, result)) = outcome {
                args.push(Arg::Address(result));
            }
            args.push(Arg::Value(env));
            match self.module.types[part.index()].carried {
                Carried::Nothing => {}
                Carried::Value(t) => {
                    let at = Self::address(
                        c,
                        AddressBase::Address(tuple),
                        &[Step::Field(i as u32)],
                        PlaceTy::Value(t),
                    );
                    args.push(Arg::Value(c.read_at(at, t)));
                }
                Carried::Address => {
                    let at = Self::address(
                        c,
                        AddressBase::Address(tuple),
                        &[Step::Field(i as u32)],
                        PlaceTy::Declared(part),
                    );
                    args.push(Arg::Address(at));
                }
            }
            args.push(Arg::Address(out));
            let signature = self.part_signature(i, CodecOp::Encode);
            self.call(c, Callee::Indirect(encoder), signature, &args);
            if let Some((failed, result)) = outcome {
                Self::unless_failed(c, result, failed);
            }
        }
        Self::returns(c);
        if let Some((failed, _)) = outcome {
            c.start(failed);
            Self::returns(c);
        }
        Ok(())
    }

    /// Each component by its decoder into its own slot. A refusal drops the components
    /// decoded before it and is the result; else the components move into the tuple.
    fn decode_body(
        &mut self,
        c: &mut Code,
        dest: ValueId,
        bytes: ValueId,
        at: ValueId,
    ) -> Result<(), String> {
        let rows = self.rows;
        let ShapeBody::Variant(cases) = &self.module.types[rows.decoded.index()].body else {
            return Err("the tuple codec's decode result is not a variant".into());
        };
        let Some(FieldTy::Inline(error)) = cases
            .get(1)
            .and_then(|c| c.fields.first())
            .map(|f| f.ty.clone())
        else {
            return Err("the tuple codec's decode refusal is not a DecodeError".into());
        };
        let error = TypeId(error.0);
        let ShapeBody::Record(fields) = self.module.types[rows.tuple.index()].body.clone() else {
            return Err("the tuple codec's row is not a tuple".into());
        };
        let result = Self::address(
            c,
            AddressBase::Value(dest),
            &[],
            PlaceTy::Declared(rows.decoded),
        );
        let input = Self::address(
            c,
            AddressBase::Value(bytes),
            &[],
            PlaceTy::Declared(rows.bytes),
        );
        let position = Self::address(
            c,
            AddressBase::Value(at),
            &[],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
        );
        if rows.format == Format::Desc {
            let key = self.part_codec(c, 0)?;
            let (take, signature) = self.desc_helper(CodecOp::Decode);
            self.call(
                c,
                Callee::Direct(take),
                signature,
                &[
                    Arg::Address(result),
                    Arg::Address(input),
                    Arg::Address(position),
                    Arg::Address(key),
                ],
            );
            Self::returns(c);
            return Ok(());
        }
        let mut decoded: Vec<AddressId> = vec![];
        for i in 0..rows.parts.len() {
            let part = rows.parts[i];
            let codec = self.part_codec(c, i)?;
            let decoder = Self::field_fn(c, codec, 1);
            let env = Self::null(c);
            let slot = Self::slot(c, part.decoded);
            let signature = self.part_signature(i, CodecOp::Decode);
            self.call(
                c,
                Callee::Indirect(decoder),
                signature,
                &[
                    Arg::Address(slot),
                    Arg::Value(env),
                    Arg::Address(input),
                    Arg::Address(position),
                ],
            );
            let failed = c.block(&[]);
            Self::unless_failed(c, slot, failed);
            let next = c.current();
            c.start(failed);
            for (j, &earlier) in decoded.iter().enumerate() {
                let ty = rows.parts[j].decoded;
                if self.recipes[ty.index()].drop.is_some() {
                    c.inst(Inst::Drop {
                        address: earlier,
                        ty,
                        fail: None,
                    });
                }
            }
            c.inst(Inst::SetCase {
                address: result,
                case: 1,
            });
            let to = Self::address(
                c,
                AddressBase::Address(result),
                &[Step::CaseField { case: 1, field: 0 }],
                PlaceTy::Declared(error),
            );
            let from = Self::address(
                c,
                AddressBase::Address(slot),
                &[Step::CaseField { case: 1, field: 0 }],
                PlaceTy::Declared(error),
            );
            let bytes = c.field_size(&FieldTy::Inline(TypeRef(error.0)));
            c.inst(Inst::Copy { to, from, bytes });
            Self::returns(c);
            c.start(next);
            decoded.push(slot);
        }
        c.inst(Inst::SetCase {
            address: result,
            case: 0,
        });
        let tuple = Self::address(
            c,
            AddressBase::Address(result),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(rows.tuple),
        );
        for (i, &slot) in decoded.iter().enumerate() {
            let size =
                hirsel_core_layout::field_size(&self.module.types, &self.recipes, &fields[i].ty).0;
            if size == 0 {
                continue;
            }
            let place = self.part_place(rows.parts[i].ty);
            let to = Self::address(
                c,
                AddressBase::Address(tuple),
                &[Step::Field(i as u32)],
                place,
            );
            let from = Self::address(
                c,
                AddressBase::Address(slot),
                &[Step::CaseField { case: 0, field: 0 }],
                place,
            );
            let bytes = c.field_size(&fields[i].ty);
            c.inst(Inst::Copy { to, from, bytes });
        }
        Self::returns(c);
        Ok(())
    }

    /// `tuple(n)`, then each component's description, into `dest` as the encoder does.
    fn describe_body(
        &mut self,
        c: &mut Code,
        dest: ValueId,
        schema: ValueId,
    ) -> Result<(), String> {
        let rows = self.rows;
        let failed = c.block(&[]);
        let result = Self::address(
            c,
            AddressBase::Value(dest),
            &[],
            PlaceTy::Declared(rows.written),
        );
        let into = Self::address(
            c,
            AddressBase::Value(schema),
            &[],
            scalar_abi::mut_place(&self.module.types, rows.schema),
        );
        if rows.format == Format::Desc {
            let (desc, signature) = self.desc_helper(CodecOp::Describe);
            self.call(c, Callee::Direct(desc), signature, &[Arg::Address(into)]);
        } else {
            let (tuple, signature) = self.schema_tuple();
            let n = c.constant(rows.parts.len() as u64);
            self.call(
                c,
                Callee::Direct(tuple),
                signature,
                &[Arg::Address(result), Arg::Address(into), Arg::Value(n)],
            );
            Self::unless_failed(c, result, failed);
        }
        for i in 0..rows.parts.len() {
            let codec = self.part_codec(c, i)?;
            let describer = Self::field_fn(c, codec, 2);
            let env = Self::null(c);
            let signature = self.part_signature(i, CodecOp::Describe);
            let mut args = vec![Arg::Address(result), Arg::Value(env), Arg::Address(into)];
            if rows.format == Format::Value {
                args.push(Arg::Address(codec));
            }
            self.call(c, Callee::Indirect(describer), signature, &args);
            Self::unless_failed(c, result, failed);
        }
        Self::returns(c);
        c.start(failed);
        Self::returns(c);
        Ok(())
    }
    fn desc_helper(&mut self, op: CodecOp) -> (FuncId, SigId) {
        let rows = self.rows;
        let part = rows.parts[0];
        let view = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::View,
        };
        let out = |ty| Param::Address {
            place: scalar_abi::mut_place(&self.module.types, ty),
            loan: Loan::Mut,
        };
        let offset = Param::Address {
            place: PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            loan: Loan::Mut,
        };
        let (name, arguments, params, dest) = match op {
            CodecOp::Encode => (
                "put_key_desc",
                vec![part.ty],
                vec![view(rows.tuple), view(part.codec), out(rows.bytes)],
                None,
            ),
            CodecOp::Decode => (
                "take_key_desc",
                vec![part.ty],
                vec![view(rows.bytes), offset, view(part.codec)],
                Some(rows.decoded),
            ),
            CodecOp::Describe => ("schema_desc", vec![], vec![out(rows.schema)], None),
            CodecOp::Of => unreachable!("descending key protocol"),
        };
        self.sibling(name, arguments, Signature::new(params, vec![], dest))
    }

    fn schema_tuple(&mut self) -> (FuncId, SigId) {
        let schema = Param::Address {
            place: scalar_abi::mut_place(&self.module.types, self.rows.schema),
            loan: Loan::Mut,
        };
        let params = vec![schema, Param::Value(Ty::Scalar(Scalar::U64))];
        let sig = Signature::new(params, vec![], Some(self.rows.written));
        self.sibling("schema_tuple", vec![], sig)
    }

    fn sibling(&mut self, name: &str, arguments: Vec<TypeId>, sig: Signature) -> (FuncId, SigId) {
        let mut definition = self.of.clone();
        definition.0.name = name.into();
        let key = InstanceKey {
            definition,
            arguments,
            dictionaries: vec![],
        };
        if let Some(i) = self
            .module
            .functions
            .iter()
            .position(|f| self.module.instances[f.instance.index()] == key)
        {
            return (FuncId(i as u32), self.module.functions[i].signature);
        }
        let signature = signature_id(self.module, sig);
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key);
        self.module.functions.push(Function {
            versioned: false,
            cold: false,
            instance,
            signature,
            body: None,
        });
        (FuncId(self.module.functions.len() as u32 - 1), signature)
    }
}

impl Emitter {
    /// Gives the row of `ty`, when it is a tuple, its maker as its codec hook: the tuple
    /// codec's `$tuple_of` at the rows it reads. Each component's row is asked first, so
    /// a nested tuple has its own. A tuple holding a parameter gets the operation's
    /// generic row, which substitution instantiates with the row; a program without
    /// std.codec gives none.
    pub(super) fn tuple_codec(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId, row: TypeId) {
        if self.module.types[row.index()].codec.is_some() {
            return;
        }
        let Some((of, rows)) = self.tuple_codec_rows(p, t, ty) else {
            return;
        };
        let op = CodecOp::Of;
        let f = if has_param(t, ty) {
            let f = self.add_function_abi(
                InstanceKey {
                    definition: op.definition(&of, rows.format),
                    arguments: rows.arguments(),
                    dictionaries: vec![],
                },
                op.signature(&self.module, &rows),
                op.abi(rows.format),
            );
            self.generic_calls.entry(f).or_insert_with(|| GenericCall {
                dictionaries: vec![],
                operation: None,
            });
            f
        } else {
            self.glue_function(op.instance(&of, &rows), |m| {
                op.sealed(m, rows.tuple, rows.codec, rows.decoded, &rows, rows.format)
            })
        };
        std::sync::Arc::make_mut(&mut self.module.types[row.index()]).codec =
            Some(hirsel_core_layout::FuncRef(f.0));
    }
    pub(super) fn tuple_codec_rows(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        ty: TyId,
    ) -> Option<(DefPath, Rows)> {
        let TyKind::Tuple(xs) = t.kind(ty).clone() else {
            return None;
        };
        let protocol = p.tuple_codec_protocol()?;
        let rows_of = |e: &mut Self, t: &mut _, x: TyId| -> Option<(TypeId, TypeId, TypeId)> {
            let c = subst(t, protocol.codec, &[x]);
            let d = subst(t, protocol.decoded, &[x]);
            Some((
                e.type_row(p, t, x)?,
                e.type_row(p, t, c)?,
                e.type_row(p, t, d)?,
            ))
        };
        let Some((tuple, codec_row, decoded_row)) = rows_of(self, t, ty) else {
            return None;
        };
        let mut parts = vec![];
        for x in xs {
            let Some((ty, codec, decoded)) = rows_of(self, t, x) else {
                return None;
            };
            parts.push(Part { ty, codec, decoded });
        }
        let (Some(written), Some(bytes), Some(schema)) = (
            self.type_row(p, t, protocol.written),
            self.type_row(p, t, protocol.bytes),
            self.type_row(p, t, protocol.schema),
        ) else {
            return None;
        };
        Some((
            protocol.of.clone(),
            Rows {
                format: Format::Value,
                tuple,
                codec: codec_row,
                decoded: decoded_row,
                written,
                bytes,
                schema,
                parts,
            },
        ))
    }
}
