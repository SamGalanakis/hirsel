use hirsel_syntax::DefPath;
#[derive(Clone, Debug, Default)]
pub struct NativeObservation {
    pub failure_rules: Vec<String>,
    pub sites: std::collections::BTreeMap<u64, (hirsel_syntax::DefId, hirsel_lir::ir::SiteRange)>,
    pub anchors: std::collections::BTreeMap<DefPath, Vec<hirsel_span::Anchor>>,
}
