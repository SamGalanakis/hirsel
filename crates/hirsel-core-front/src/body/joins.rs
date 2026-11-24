//! Join edges and their result ownership.
use super::*;

impl Cx<'_, '_, '_> {
    pub(super) fn new_join(&mut self) -> usize {
        self.joins.push(Join::default());
        self.joins.len() - 1
    }
    pub(super) fn new_result_join(&mut self) -> usize {
        let j = self.new_join();
        self.joins[j].destination = self.expression_destination(self.node);
        j
    }
    /// Leave the current block for join `j`, whose parameters carry a value of `ty`.
    pub(super) fn goto(&mut self, j: usize, ty: TyId, v: Option<ValueId>, n: NodeId) {
        if !self.live() {
            return;
        }
        let blk = match self.joins[j].block {
            Some(b) => b,
            None => {
                let tys: Vec<Ty> = self.ir_ty(ty).into_iter().collect();
                let b = self.new_block(&tys);
                self.joins[j].block = Some(b);
                b
            }
        };
        if let Some(id) = self.drop_id(ty)
            && v.is_some()
        {
            debug_assert!(
                {
                    let held = self.owners.iter().any(|owner| {
                    matches!(owner, cleanup::Owner::Slot(slot)
                        if matches!(self.var(*slot).avail, Avail::Live | Avail::Maybe)
                        && matches!(self.var(*slot).binding, Binding::Value(Some(p)) if Some(p) == v))
                });
                    !(held && self.temp_index(v).is_none())
                },
                "owning join of a live local's value"
            );
            self.consume(v, n);
            self.joins[j].drop = Some(id);
        }
        self.join_descriptor(j, v);
        self.joins[j].anchors = self.anchors.union(self.joins[j].anchors, self.anchors.last);
        let value = match (self.joins[j].destination, v) {
            (Some(to), Some(from)) if self.pointer_address(from) != Some(to) => {
                let PlaceTy::Declared(id) = self.b.addresses[to.index()].ty else {
                    unreachable!("only aggregate results have a destination");
                };
                self.copy_to(to, from, id, n);
                Some(self.pointer(to, n))
            }
            _ => v,
        };
        let args: Vec<ValueId> = value.into_iter().collect();
        self.jump(blk, &args, n);
    }
    /// Continue at join `j`, if any edge reached it; its parameter is the joined value.
    pub(super) fn land(&mut self, j: usize) -> Option<ValueId> {
        let blk = self.joins[j].block?;
        self.start(blk);
        let v = self.block_params(blk).first().copied();
        if let (Some(v), Some(to)) = (v, self.joins[j].destination) {
            self.note_pointer(v, to);
        }
        let anchors = self.joins[j].anchors;
        self.anchors.last = anchors;
        if anchors != 0 {
            self.anchors.expressions.insert(self.node, anchors);
            if let Some(v) = v {
                self.anchors.values.insert(v, anchors);
            }
        }
        if let (Some(value), Some(id)) = (v, self.joins[j].descriptor) {
            self.descriptors.values.insert(value, id);
        }
        if let (Some(id), Some(value), true) = (self.joins[j].drop, v, self.live()) {
            self.owners.push(cleanup::Owner::Temp(cleanup::Temp {
                value,
                source: self.node,
                id,
                avail: Avail::Live,
                flag: None,
                reached: 0,
            }));
            self.epoch += 1;
        }
        v
    }
}
