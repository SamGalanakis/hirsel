//! Current-spec explanations, extracted from docs/spec when the compiler is built.
//! There is no separately authored guidance or fallback to the old language catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Rule {
    pub id: &'static str,
    pub title: &'static str,
    pub spec_file: &'static str,
    pub anchor: &'static str,
    /// First prose paragraph of the rule, with source wrapping removed; Markdown retained.
    pub statement: &'static str,
}
include!(concat!(env!("OUT_DIR"), "/catalogue.rs"));
/// All current rules, sorted by ID. Suitable for explain --list and JSON serialization.
pub fn rules() -> &'static [Rule] {
    RULES
}
/// Exact rule lookup; unknown IDs have no fabricated explanation.
pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES
        .binary_search_by_key(&id, |r| r.id)
        .ok()
        .map(|i| &RULES[i])
}
#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;
