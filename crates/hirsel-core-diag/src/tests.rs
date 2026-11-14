use super::*;
#[path = "catalogue_source.rs"]
mod source;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use syn::visit::{self, Visit};
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
#[test]
fn catalogue_is_current_spec_with_no_separate_guidance() {
    let expected = source::read(&repo().join("docs/spec")).unwrap();
    assert_eq!(rules().len(), expected.len());
    for (actual, expected) in rules().iter().zip(expected) {
        assert_eq!(
            (
                actual.id,
                actual.title,
                actual.spec_file,
                actual.anchor,
                actual.statement
            ),
            (
                expected.id.as_str(),
                expected.title.as_str(),
                expected.spec_file.as_str(),
                expected.anchor.as_str(),
                expected.statement.as_str()
            )
        );
        assert_eq!(rule(actual.id), Some(actual));
    }
    assert_eq!(rule("LANG-7").unwrap().title, "Names");
    assert_eq!(rule("LANG-7").unwrap().anchor, "lang-7-names");
    assert_eq!(rule("NOT-A-RULE"), None);
    let json = serde_json::to_value(rule("LANG-7").unwrap()).unwrap();
    assert_eq!(json["spec_file"], "docs/spec/language.md");
}
#[test]
fn headings_ignore_examples_and_paragraphs_preserve_spec_words() {
    let entries = source::extract("docs/spec/example.md", "# Intro\n### TEST-1: A `thing`\n\n```hirsel\n### FAKE-9: Not a rule\n```\n\nThe first\nparagraph.\n\nLater detail.\n### TEST-2: More\n\nOther statement.\n").unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.id.as_str(), e.anchor.as_str(), e.statement.as_str()))
            .collect::<Vec<_>>(),
        [
            ("TEST-1", "test-1-a-thing", "The first paragraph."),
            ("TEST-2", "test-2-more", "Other statement.")
        ]
    );
    assert!(source::extract("x", "### TEST-1: Empty\n\n```\ncode\n```\n").is_err());
    assert!(source::extract("x", "### TEST-1: Broken\n\n```\nnever closed\n").is_err());
}
fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("test")
            || a.path().is_ident("cfg")
                && a.parse_args::<syn::Path>()
                    .is_ok_and(|p| p.is_ident("test"))
    })
}
#[derive(Default)]
struct References {
    ids: BTreeSet<String>,
    dir: PathBuf,
}
impl References {
    fn string(&mut self, text: &str) {
        if source::rule_id(text) {
            self.ids.insert(text.into());
        }
        // Rule IDs must be literal members of a finite catalogue, not formatted IDs.
        assert!(
            !text.split_once('-').is_some_and(|(p, n)| !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_uppercase())
                && n.starts_with('{')),
            "constructed diagnostic rule ID: {text}"
        );
    }
    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(g) => self.tokens(g.stream()),
                proc_macro2::TokenTree::Literal(l) => {
                    if let Ok(s) = syn::parse2::<syn::LitStr>(proc_macro2::TokenStream::from(
                        proc_macro2::TokenTree::Literal(l),
                    )) {
                        self.string(&s.value());
                    }
                }
                _ => {}
            }
        }
    }
    fn file(&mut self, path: &Path) {
        let tree = syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap();
        let dir = if matches!(
            path.file_name().unwrap().to_str().unwrap(),
            "lib.rs" | "main.rs" | "mod.rs"
        ) {
            path.parent().unwrap().to_owned()
        } else {
            path.parent().unwrap().join(path.file_stem().unwrap())
        };
        let old = std::mem::replace(&mut self.dir, dir);
        self.visit_file(&tree);
        self.dir = old;
    }
}
impl<'ast> Visit<'ast> for References {
    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        self.string(&s.value());
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.tokens(m.tokens.clone());
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if !test_only(&f.attrs) {
            visit::visit_item_fn(self, f);
        }
    }
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if test_only(&m.attrs) {
            return;
        }
        if let Some((_, items)) = &m.content {
            let old = self.dir.clone();
            self.dir.push(m.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.dir = old;
        } else {
            let flat = self.dir.join(format!("{}.rs", m.ident));
            let path = if flat.exists() {
                flat
            } else {
                self.dir.join(m.ident.to_string()).join("mod.rs")
            };
            self.file(&path);
        }
    }
}
#[test]
fn every_production_diagnostic_rule_exists_in_the_catalogue() {
    let mut references = References::default();
    for name in [
        "hirsel-core-syntax",
        "hirsel-core-resolve",
        "hirsel-core-front",
    ] {
        for root in ["lib.rs", "main.rs"] {
            let path = repo().join("crates").join(name).join("src").join(root);
            if path.exists() {
                references.file(&path);
            }
        }
    }
    assert!(!references.ids.is_empty());
    let unknown = references
        .ids
        .iter()
        .filter(|id| rule(id).is_none())
        .collect::<Vec<_>>();
    assert!(
        unknown.is_empty(),
        "production rules absent from current spec: {unknown:?}"
    );
    println!(
        "{} current-spec rules; {} production diagnostic rule IDs",
        rules().len(),
        references.ids.len()
    );
}
#[test]
fn production_walk_excludes_test_modules_and_includes_macro_literals() {
    let tree = syn::parse_file("fn f(){refuse(\"LANG-7\");} macro_rules! r {()=>{ refuse(\"OWN-5\") }} #[cfg(test)] mod tests { fn f(){refuse(\"FAKE-999\");} }").unwrap();
    let mut refs = References::default();
    refs.visit_file(&tree);
    assert_eq!(
        refs.ids.into_iter().collect::<Vec<_>>(),
        ["LANG-7", "OWN-5"]
    );
}
