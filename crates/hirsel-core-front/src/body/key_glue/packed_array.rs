use super::*;

impl Derive<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn order_packed_array(
        &mut self,
        c: &mut Code,
        item: &FieldTy,
        blobs: [ValueId; 2],
        ats: [ValueId; 2],
        widths: [ValueId; 2],
        count: u64,
        different: BlockId,
    ) -> Result<[ValueId; 2], &'static str> {
        if count == 0 {
            return Ok(ats);
        }
        let limit = c.constant(count);
        let zero = c.constant(0);
        let body = c.block(&[Ty::Scalar(Scalar::U64); 3]);
        let done = c.block(&[]);
        // A nonempty fixed array always visits its first element.
        c.jump(body, &[zero, ats[0], ats[1]]);
        c.start(body);
        let params = c.body.blocks[body.index()]
            .params
            .get(&c.body.pools.values)
            .unwrap();
        let (index, cursors) = (params[0], [params[1], params[2]]);
        let ats = self.order_packed(c, item, blobs, cursors, widths, different)?;
        let one = c.constant(1);
        let next = c.binary(BinaryOp::Add, index, one);
        let more = c.compare(CompareOp::Lt, next, limit);
        let yes = c.edge(body, &[next, ats[0], ats[1]]);
        let no = c.edge(done, &[]);
        c.end(Terminator::Branch {
            condition: more,
            yes,
            no,
        });
        c.start(done);
        Ok(ats)
    }
}
