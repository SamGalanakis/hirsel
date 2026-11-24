//! Owned destructuring (T3.7, FAIL-12): a pattern over an owned subject consumes it
//! and hands each owned field to its binding, its sub-pattern or its cleanup.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn pattern_count(&self, pat: Pat) -> u32 {
        if pat.order.len == 0 {
            pat.fields.len
        } else {
            pat.order.len
        }
    }
    pub(super) fn pattern_index(&self, pat: Pat, at: u32) -> u32 {
        if pat.order.len == 0 {
            at
        } else {
            self.patterns.order[(pat.order.start + at) as usize]
        }
    }
    /// An owned subject is consumed before its parts are bound: each owned field the
    /// pattern holds (bound, destructured further, or omitted) is an owner until its
    /// binding or pattern takes it, and a destructured resource's block is one, freed
    /// last. The omitted fields are then cleaned in declaration order, so a field drop
    /// that fails (FAIL-12) cleans the fields after it and the block, never the
    /// subject or a part already taken (T3.7).
    pub(super) fn destructure(
        &mut self,
        pat: Pat,
        case: Option<u32>,
        subject: Subject,
        mutable: bool,
    ) {
        if subject.place.is_none() {
            self.consume(subject.value, pat.site);
        }
        let mark = self.owner_mark();
        self.free_owner(subject.value, pat.ty, pat.site);
        let mut fields = vec![None; pat.fields.len as usize];
        // Begun last to first, so they are cleaned in declaration order.
        for i in (0..pat.fields.len).rev() {
            let child = self.patterns.fields[(pat.fields.start + i) as usize];
            if child == 0 || self.pat_head(child) == Head::Any {
                let ty = self.pattern_field_ty(pat.ty, case, i as usize);
                if self.drop_id(ty).is_some() {
                    let field = self.pattern_at(subject, case, i, ty, pat.site);
                    self.temp(ty, field.value);
                }
                continue;
            }
            let c = self.pat(child);
            let field = self.pattern_at(subject, case, i, c.ty, pat.site);
            if !c.viewed
                && matches!(
                    c.head,
                    Head::Product(_) | Head::Bind(_, Mode::View | Mode::Take)
                )
            {
                self.temp(c.ty, field.value);
            }
            fields[i as usize] = Some(field);
        }
        for at in 0..self.pattern_count(pat) {
            let i = self.pattern_index(pat, at);
            let child = self.patterns.fields[(pat.fields.start + i) as usize];
            if child == 0 || self.pat_head(child) == Head::Any {
                continue;
            }
            let Some(field) = fields[i as usize] else {
                continue;
            };
            self.consume(field.value, pat.site);
            self.bind_pattern(child, field, mutable);
        }
        self.end_owners(mark, None, true, pat.site);
    }
}
