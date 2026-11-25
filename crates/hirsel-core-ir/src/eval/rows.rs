use super::*;

impl Machine<'_> {
    /// SEM-4, MEM-24: the rows of a List or Columns carrier at `at`. A Columns block
    /// holds column k at the sum of the columns before it, each `capacity × stride`
    /// rounded up to 8; a List is its one column. Rows are cleaned in order, each
    /// row's columns in field order, and the block is refunded as List storage of
    /// the row.
    pub(super) fn drop_rows(&mut self, t: &mut Task, at: u64, ty: &FieldTy, row: &FieldTy) -> bool {
        let block = self.load(at, 8);
        let length = self.load(at + 8, 8);
        let capacity = self.load(at + 16, 8);
        let list = matches!(ty, FieldTy::Seq(_));
        let what = if list {
            "list storage"
        } else {
            "columns storage"
        };
        if self.stride(row) == 0 {
            // SEM-4: no element storage, nothing to clean (capacity 2^64 - 1).
            return false;
        }
        if capacity != 0 {
            self.edge(block, ty, what);
        }
        let strides = match list {
            true => vec![(row.clone(), self.stride(row))],
            false => hirsel_core_layout::column_strides(&self.m.types, &self.recipes, row),
        };
        let mut columns = Vec::new();
        let mut base = block;
        for (column, stride) in strides {
            columns.push((column, base, stride));
            base += (capacity * stride).next_multiple_of(8);
        }
        let mut failed = false;
        for i in 0..length {
            for (column, base, stride) in &columns {
                self.drop_marked(base + i * stride, *stride as usize);
                failed |= self.drop_field(t, base + i * stride, column);
            }
        }
        // Capacity 0 has no storage: empty, or static data the carrier does not own
        // (core.md "Literals").
        if capacity != 0 {
            let element = field_size(&self.m.types, &self.recipes, row).2;
            let size = (!list).then_some(base - block);
            self.free_as(block, charge::seq(capacity, element), size, what);
        }
        failed
    }
}
