//! Wire operations for the fields of a derived codec.
use super::{Body, Gen, Ty};
use std::fmt::Write;

impl<'a> Gen<'a> {
    /// Small leaf records have one exact size and need no constructed value to validate.
    pub(super) fn scalar_record(&self) -> Option<u64> {
        let Body::Record(fields) = &self.decl.body else {
            return None;
        };
        if self.decl.via.is_some() || fields.len() > 8 {
            return None;
        }
        fields
            .iter()
            .filter(|f| !f.removed)
            .try_fold(0, |sum, f| match f.ty {
                Ty::Scalar(name, n) if name != "string" => Some(sum + n),
                _ => None,
            })
    }

    pub(super) fn validator(&mut self, out: &mut String) {
        let Body::Record(fields) = &self.decl.body else {
            return;
        };
        let mut body = String::new();
        for field in fields.iter().filter(|f| !f.removed) {
            let read = self.take(&field.ty);
            writeln!(body, "    let _ = {read}").unwrap();
        }
        body.push_str("    Ok(())\n");
        let header = format!(
            "fn {}$validate(b$: Bytes, mut at$: u64) -> Result[(), {}DecodeError]",
            self.decl.name, self.c
        );
        self.function(out, &header, &body);
    }

    pub(super) fn put(&mut self, ty: &Ty<'a>, v: &str) -> String {
        let c = self.c;
        let mut stmt = match *ty {
            Ty::Scalar(x, _) => format!("{c}put_{x}({v}, mut out$)"),
            Ty::List("u8", _) => format!("{c}put_bytes({v}, mut out$)"),
            Ty::List(e, source) => format!("{c}put_list({v}, {}, mut out$)", self.of_at(e, source)),
            Ty::Option(e) => format!("{c}put_option({v}, {}, mut out$)", self.of(e)),
            Ty::Map(k, w) => format!("{c}put_map({v}, {}, {}, mut out$)", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}put_result({v}, {}, {}, mut out$)",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}put_with({v}, {}, mut out$)", self.of(t)),
        };
        if !matches!(*ty, Ty::Scalar(name, _) if name != "string") {
            stmt.push('?');
        }
        stmt
    }
    pub(super) fn take(&mut self, ty: &Ty<'a>) -> String {
        let c = self.c;
        match *ty {
            Ty::Scalar(x, _) => format!("{c}take_{x}(b$, mut at$)?"),
            Ty::List("u8", _) => format!("{c}take_bytes(b$, mut at$)?"),
            Ty::List(e, source) => format!("{c}take_list(b$, mut at$, {})?", self.of_at(e, source)),
            Ty::Option(e) => format!("{c}take_option(b$, mut at$, {})?", self.of(e)),
            Ty::Map(k, w) => format!("{c}take_map(b$, mut at$, {}, {})?", self.of(k), self.of(w)),
            Ty::Result(t, e) => format!(
                "{c}take_result(b$, mut at$, {}, {})?",
                self.of(t),
                self.of(e)
            ),
            Ty::With(t) => format!("{c}take_with(b$, mut at$, {})?", self.of(t)),
        }
    }
}
