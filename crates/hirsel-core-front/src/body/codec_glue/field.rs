//! Direct calls from structural glue to the component codec selected by its maker.
use super::*;

impl Derive<'_> {
    /// Resolve a component through its type closure, as `codec.of` does.
    fn part_maker(&mut self, i: usize) -> Result<FuncId, String> {
        let part = self.rows.parts[i];
        let sig = signature_id(
            self.module,
            Signature::new(vec![], vec![], Some(part.codec)),
        );
        if self.rows.format == Format::Value {
            templates::codec_maker(self.module, self.of, part.ty, sig)
        } else {
            super::ordered_codec::maker(self.module, self.of, part.ty, sig)
        }
    }

    pub(super) fn part_codec(&mut self, c: &mut Code, i: usize) -> Result<AddressId, String> {
        let maker = self.part_maker(i)?;
        let signature = self.module.functions[maker.index()].signature;
        let at = Self::slot(c, self.rows.parts[i].codec);
        self.call(c, Callee::Direct(maker), signature, &[Arg::Address(at)]);
        Ok(at)
    }

    fn part_signature(&mut self, i: usize, op: CodecOp) -> Signature {
        let p = self.rows.parts[i];
        let format = if self.rows.format == Format::Value {
            Format::Value
        } else {
            Format::Key
        };
        op.sealed(self.module, p.ty, p.codec, p.decoded, self.rows, format)
    }

    pub(super) fn call_part(
        &mut self,
        c: &mut Code,
        i: usize,
        op: CodecOp,
        mut args: Vec<Arg>,
    ) -> Result<(), String> {
        let mut sig = self.part_signature(i, op);
        let ty = &self.module.types[self.rows.parts[i].ty.index()].name;
        let callee = if let Some(scalar) = templates::builtin_codec(ty)
            .and_then(|maker| maker.strip_prefix("of_"))
            .filter(|name| {
                self.rows.format == Format::Value
                    && !matches!(*name, "string" | "decode_error" | "never")
            }) {
            let mut definition = self.of.clone();
            let prefix = if op == CodecOp::Encode {
                "encode"
            } else {
                "take"
            };
            definition.0.name = format!("{prefix}_{scalar}");
            let key = InstanceKey {
                definition,
                arguments: vec![],
                dictionaries: vec![],
            };
            sig.params.remove(0);
            Callee::Direct(self.function_row(key, sig.clone()).0)
        } else {
            let codec = self.part_codec(c, i)?;
            let field = u32::from(op == CodecOp::Decode);
            let code = Self::field_fn(c, codec, field);
            args.insert(usize::from(sig.dest.is_some()), Arg::Value(Self::null(c)));
            Callee::Indirect(code)
        };
        let signature = signature_id(self.module, sig);
        self.call(c, callee, signature, &args);
        Ok(())
    }
    /// The code of function field `field` (0 encoder, 1 decoder, 2 describer) of the
    /// codec at `at`: the field is a `StaticFn`, its descriptor's first word the code.
    fn field_fn(c: &mut Code, at: AddressId, field: u32) -> ValueId {
        let f = Self::address(
            c,
            AddressBase::Address(at),
            &[Step::Field(field)],
            PlaceTy::Value(Ty::Ptr),
        );
        let descriptor = c.read_at(f, Ty::Ptr);
        let code = Self::address(
            c,
            AddressBase::Value(descriptor),
            &[],
            PlaceTy::Value(Ty::Ptr),
        );
        c.read_at(code, Ty::Ptr)
    }
}
