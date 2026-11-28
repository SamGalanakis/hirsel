use super::diagnostics::{self, Diagnostic};
use super::documents::Text;
use hirsel_config_schema::{
    Config, Descriptor, pkg_v1, product_v1, toolchain_v1, user_v1, workloads_v1,
};
use hirsel_core_syntax::ParseMode;
use hirsel_data::{ErrorKind, Limits};
use std::path::Path;

pub(super) fn is_config(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "hcfg")
}

pub(super) fn schemas(path: &Path) -> Vec<&'static Descriptor> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(crate::package_dir::MANIFEST) => {
            vec![pkg_v1::Package::schema(), pkg_v1::Workspace::schema()]
        }
        Some("config.hcfg") => vec![user_v1::Config::schema()],
        Some("product.hcfg") => vec![product_v1::Product::schema()],
        Some("toolchain-packages.hcfg") => vec![toolchain_v1::Packages::schema()],
        Some("workloads.hcfg") => vec![workloads_v1::Config::schema()],
        _ => hirsel_config_schema::catalog(),
    }
}

pub(super) fn check(path: &Path, text: &Text) -> Vec<Diagnostic> {
    let limits = Limits {
        diagnostics: Limits::default().collection,
        ..Limits::default()
    };
    let Err(error) = hirsel_data::decode(&text.bytes, &schemas(path), limits) else {
        return Vec::new();
    };
    let message = match error.kind {
        ErrorKind::Invalid("data syntax") => {
            let syntax = diagnostics::syntax(path, text, ParseMode::Data);
            if !syntax.is_empty() {
                return syntax;
            }
            "Invalid data syntax".to_owned()
        }
        ErrorKind::Invalid(message) if error.path.is_empty() => message.to_owned(),
        ErrorKind::Invalid(message) => format!("{}: {message}", error.path.join(".")),
        ErrorKind::Limit(limit) => format!("Configuration exceeded the {limit} limit"),
        kind => format!("Configuration resource refusal: {kind:?}"),
    };
    std::iter::once(error.span)
        .chain(error.additional)
        .map(|span| Diagnostic::data(path, span, message.clone(), error.related))
        .collect()
}
