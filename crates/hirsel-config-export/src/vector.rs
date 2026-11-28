//! Prepare a generated vector package using only checked source intervals.
use PathPart::{Field, Index, Key};
use hirsel_config_schema::{Config, Edit, Limits, PathPart, pkg_v1, quote, read_bounded};
use hirsel_data::decode;
use std::{error::Error, fs::File, path::Path};

type Failure = Box<dyn Error>;

fn apply(source: &mut String, edit: Option<Edit>) {
    if let Some(edit) = edit {
        source.replace_range(
            edit.span.start as usize..edit.span.end as usize,
            &edit.replacement,
        );
    }
}

pub fn compose(package: &Path, vector: &Path, directory: &Path) -> Result<String, Failure> {
    let directory = directory
        .to_str()
        .filter(|_| directory.is_absolute())
        .ok_or("expected an absolute vector directory")?;
    let limits = Limits::default();
    let mut base = String::from_utf8(read_bounded(File::open(package)?)?)?;
    let mut spec = String::from_utf8(read_bounded(File::open(vector)?)?)?;
    let schemas = [pkg_v1::VectorTests::schema()];
    let checked = pkg_v1::VectorTests::read(spec.as_bytes(), limits)?;
    for (i, grant) in checked.tests.grants.iter().enumerate() {
        if let pkg_v1::Grant::Dir(path) = grant {
            if Some(path.as_str()) != Path::new(directory).file_name().and_then(|n| n.to_str()) {
                return Err(format!("unexpected vector directory {path:?}").into());
            }
            let doc = decode(spec.as_bytes(), &schemas, limits)?;
            let at = [
                Field("tests".into()),
                Field("grants".into()),
                Index(i),
                Index(0),
            ];
            apply(&mut spec, doc.replace(&at, &quote(directory), limits)?);
        }
    }
    let doc = decode(spec.as_bytes(), &schemas, limits)?;
    let base_schema = [pkg_v1::Package::schema()];
    let tests = doc
        .literal(&[Field("tests".into())])
        .ok_or("missing vector tests")?;
    // The generated package substitutes its vector suite for the original
    // suite, including any comments belonging to the old suite.
    let original = decode(base.as_bytes(), &base_schema, limits)?;
    let field = Field("tests".into());
    if let Some(span) = original.literal_span(std::slice::from_ref(&field)) {
        base.replace_range(
            span.start as usize..span.end as usize,
            &format!("Some({tests})"),
        );
    } else {
        apply(
            &mut base,
            original.insert(&[], field, &format!("Some({tests})"), limits)?,
        );
    }
    for (alias, _) in checked.dependencies {
        let parent = [Field("dependencies".into())];
        let key = Key(alias);
        let path = [parent[0].clone(), key.clone()];
        let original = decode(base.as_bytes(), &base_schema, limits)?;
        if original.at(&path).is_some_and(|v| Some(v) != doc.at(&path)) {
            return Err("vector dependency conflicts with the package".into());
        }
        let literal = doc.literal(&path).ok_or("missing vector dependency")?;
        apply(&mut base, original.insert(&parent, key, literal, limits)?);
    }
    decode(base.as_bytes(), &base_schema, limits)?;
    Ok(base)
}
