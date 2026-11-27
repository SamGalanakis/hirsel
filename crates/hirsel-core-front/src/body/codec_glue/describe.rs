use super::*;

impl Derive<'_> {
    /// `tuple(n)`, then each component's description, into `dest` as the encoder does.
    pub(super) fn describe_body(
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
        for (i, part) in rows.parts.iter().enumerate() {
            let codec = self.part_codec(c, i)?;
            let name = if rows.format == Format::Value {
                "schema_of"
            } else {
                "schema_key"
            };
            let params = vec![
                Param::Address {
                    place: scalar_abi::mut_place(&self.module.types, rows.schema),
                    loan: Loan::Mut,
                },
                Param::Address {
                    place: PlaceTy::Declared(part.codec),
                    loan: Loan::View,
                },
            ];
            let (describe, signature) = self.sibling(
                name,
                vec![part.ty],
                Signature::new(params, vec![], Some(rows.written)),
            );
            self.call(
                c,
                Callee::Direct(describe),
                signature,
                &[
                    Arg::Address(result),
                    Arg::Address(into),
                    Arg::Address(codec),
                ],
            );
            Self::unless_failed(c, result, failed);
        }
        Self::returns(c);
        c.start(failed);
        Self::returns(c);
        Ok(())
    }
}
