//! Tree-sitter language and editor queries for Hirsel.
use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_hirsel_data() -> *const ();
    fn tree_sitter_hirsel() -> *const ();
}

/// Pass to `tree_sitter::Parser::set_language` after converting into a `Language`.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_hirsel) };

pub const DATA_LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_hirsel_data) };

pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");
pub const LOCALS_QUERY: &str = include_str!("../../queries/locals.scm");
pub const TAGS_QUERY: &str = include_str!("../../queries/tags.scm");
pub const INJECTIONS_QUERY: &str = include_str!("../../queries/injections.scm");

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Query;

    #[test]
    fn exported_queries_compile() {
        let language = LANGUAGE.into();
        for (name, source) in [
            ("highlights", HIGHLIGHTS_QUERY),
            ("locals", LOCALS_QUERY),
            ("tags", TAGS_QUERY),
            ("injections", INJECTIONS_QUERY),
        ] {
            Query::new(&language, source).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }
}
