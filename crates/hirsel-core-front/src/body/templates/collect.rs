//! Collect the substitution and signature recipes for a checked body or adapter.
use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenericTemplate {
    pub signatures: BTreeMap<SigId, scalar_abi::SignatureAbi>,
    pub parameters: Vec<Option<TypeId>>,
    pub copies: Vec<CopySize>,
    pub allocations: Vec<AllocationLayout>,
    pub numeric_sites: Vec<scalar_abi::NumericSite>,
    pub nested: BTreeMap<FuncId, GenericTemplate>,
    pub descriptor_adapter: Option<FuncId>,
    pub descriptors: BTreeMap<FuncId, crate::identity::DescriptorRecipe>,
    pub types: BTreeMap<TypeId, NameRecipe>,
    pub calls: BTreeMap<FuncId, GenericCall>,
    pub codecs: BTreeMap<TypeId, CodecRecipe>,
    pub key_codecs: BTreeMap<TypeId, (DefPath, codec_glue::Rows)>,
}

impl Cx<'_, '_, '_> {
    pub(in crate::body) fn take_template(&mut self, f: FuncId) -> GenericTemplate {
        let mut parameters = vec![];
        for i in 0..self.fd.generics.len() {
            let ty = self.t.mk(TyKind::Param(i as u32));
            parameters.push(self.e.type_row(self.p, &mut self.t, ty));
        }
        let body = self.e.module.functions[f.index()].body.as_ref().unwrap();
        let mut signatures = std::collections::BTreeMap::new();
        let ids = std::iter::once(self.e.source_signature(f))
            .chain(body.calls.iter().map(|c| c.signature))
            .chain(body.insts.iter().filter_map(|i| match i {
                Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                    Some(self.e.source_signature(*function))
                }
                _ => None,
            }));
        for id in ids {
            if let Some(abi) = self.e.source_signatures.abis.get(&id) {
                signatures.entry(id).or_insert_with(|| abi.clone());
            }
        }
        GenericTemplate {
            signatures,
            parameters,
            copies: std::mem::take(&mut self.copies),
            allocations: std::mem::take(&mut self.allocations),
            numeric_sites: std::mem::take(&mut self.numeric_sites),
            nested: std::mem::take(&mut self.nested),
            descriptor_adapter: None,
            descriptors: std::mem::take(&mut self.descriptor_recipes),
            types: self.e.generic_names.clone(),
            calls: self.e.generic_calls.clone(),
            codecs: self.e.generic_codecs.clone(),
            key_codecs: self.e.generic_key_codecs.clone(),
        }
    }
}
