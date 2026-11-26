use super::*;
use hirsel_core_ir::normal::EntryParameter;

const U8_ERROR_DESCRIPTION: &[u8] = &[0, 0, 0, 0, 1, 6];

impl Fixture {
    pub(super) fn body(&mut self, name: &str, params: &str, externs: &str, code: &str) -> BodyRef {
        self.body_with_entry_parameters(name, params, externs, code, vec![])
    }

    pub(super) fn body_with_entry_parameters(
        &mut self,
        name: &str,
        params: &str,
        externs: &str,
        code: &str,
        entry_parameters: Vec<EntryParameter>,
    ) -> BodyRef {
        let block_params = if params == "(u64)" { "(v0: u64)" } else { "" };
        let module = hirsel_core_ir::text::parse(&format!(
            "core v1\nsig g0 = {params} -> ()\ninst n0 = app@1/main/{name} [] []\n             fn f0 = n0 g0 {{\nb0{block_params}:\n{code}\n}}\n{externs}"
        ))
        .unwrap();
        let sources = hirsel_core_ir::source::SourceMap::default();
        let (body, mut annex) = normal::encode_with_annex(&module, FuncId(0), &sources.positions());
        if identity(name) == self.recipe.entry {
            annex.1 = Some(entry_parameters);
            annex.2 = Some(U8_ERROR_DESCRIPTION.to_vec());
        }
        let row = BodyRef {
            body: self.object(ObjectKind::Body, body.bytes),
            annex: self.object(ObjectKind::Annex, annex.encode()),
        };
        self.recipe.bodies.push(row);
        self.recipe.bodies.sort_by_key(|row| row.body.hash);
        row
    }
}
