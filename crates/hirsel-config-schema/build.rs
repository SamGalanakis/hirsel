use hirsel_data::{Body, Descriptor, Field, Fields, Scalar, Shape};
use std::{collections::BTreeSet, env, fmt::Write, fs, path::PathBuf};

const MODULES: &[(&str, &[&str], bool)] = &[
    ("pkg_v1", &["Package", "Workspace", "VectorTests"], true),
    ("user_v1", &["Config"], false),
    ("host_v1", &["Admission", "Group", "Decision"], true),
    ("exec_v1", &["Policy"], false),
    ("product_v1", &["Product"], true),
    ("toolchain_v1", &["Packages"], false),
    ("workloads_v1", &["Config"], true),
];
fn main() {
    println!("cargo:rerun-if-changed=schemas");
    let dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let base = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("schemas");
    let grants = fs::read_to_string(base.join("grants.hirsel")).unwrap();
    let mut out = String::new();
    let mut entries = String::new();
    let mut catalog = Vec::new();
    for &(module, roots, with_grants) in MODULES {
        let mut source = fs::read_to_string(base.join(format!("{module}.hirsel"))).unwrap();
        if with_grants {
            source.push_str(&grants)
        }
        writeln!(out, "pub mod {module} {{use super::*;").unwrap();
        let mut seen = BTreeSet::new();
        for &name in roots {
            catalog.push(format!("<{module}::{name} as Config>::schema()"));
            let schema = Descriptor::lower(module, &source, name)
                .unwrap_or_else(|e| panic!("{module}.{name}: {e}"));
            let file = format!("{module}.{name}.json");
            fs::write(dir.join(&file), schema.bytes()).unwrap();
            for nominal in schema.types() {
                if !seen.insert(nominal.name.clone()) {
                    continue;
                }
                let n = &nominal.name;
                match &nominal.body {
                    Body::Record(fields) => {
                        let decl = field_decls(&schema, fields, true);
                        let values = field_values(&schema, fields);
                        writeln!(out,"#[derive(Clone,Debug,PartialEq)] pub struct {n} {{{decl}}}
                            impl {n} {{fn project(v:&Value)->Option<Self>{{ Some(Self{{{values}}}) }} }}").unwrap()
                    }
                    Body::Sum(cases) => {
                        let mut decl = String::new();
                        let mut arms = String::new();
                        for case in cases {
                            let c = &case.name;
                            let (d, v) = match &case.fields {
                                Fields::Positional(types) if types.is_empty() => {
                                    (String::new(), String::new())
                                }
                                Fields::Positional(types) => {
                                    let decl = types
                                        .iter()
                                        .map(|t| rust_type(&schema, t))
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    let vals = types
                                        .iter()
                                        .enumerate()
                                        .map(|(i, t)| {
                                            format!(
                                                "{}?",
                                                project(&schema, t, &format!("p.get({i})?"))
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    (format!("({decl})"), format!("({vals})"))
                                }
                                Fields::Labelled(fields) => (
                                    format!("{{{}}}", field_decls(&schema, fields, false)),
                                    format!("{{{}}}", field_values(&schema, fields)),
                                ),
                            };
                            writeln!(decl, "{c}{d},").unwrap();
                            writeln!(arms, "{c:?}=>Self::{c}{v},").unwrap();
                        }
                        writeln!(out,"#[derive(Clone,Debug,PartialEq)] pub enum {n} {{{decl}}}
                            impl {n} {{fn project(v:&Value)->Option<Self>{{ let v_case=v.get(\"case\")?.as_str()?; let p=v.get(\"fields\")?; let v=p;
                            let _=v; Some(match v_case {{ {arms} _=>return None }}) }} }}").unwrap();
                    }
                }
            }
            writeln!(out,"impl crate::Config for {name} {{
                fn schema()->&'static Descriptor {{
                    static S:std::sync::OnceLock<Descriptor>=std::sync::OnceLock::new();
                    S.get_or_init(||Descriptor::embedded(include_bytes!(concat!(env!(\"OUT_DIR\"),\"/{file}\")),{:?}).unwrap())
                }}
                fn from_value(v:&Value)->Option<Self>{{Self::project(v)}}
            }}",schema.digest()).unwrap();
            writeln!(
                entries,
                "\"{module}.{name}\"=>Some(<{module}::{name} as Config>::schema()),"
            )
            .unwrap();
        }
        out.push_str("}\n");
    }
    writeln!(
        out,
        "pub fn schema(root:&str)->Option<&'static Descriptor>{{match root{{{entries}_=>None}}}}"
    )
    .unwrap();
    writeln!(
        out,
        "pub fn catalog() -> Vec<&'static Descriptor> {{ vec![{}] }}",
        catalog.join(",")
    )
    .unwrap();
    fs::write(dir.join("schemas.rs"), out).unwrap();
}
fn field_decls(schema: &Descriptor, fields: &[Field], public: bool) -> String {
    fields
        .iter()
        .map(|f| {
            format!(
                "{} r#{}:{},",
                if public { "pub" } else { "" },
                f.name,
                rust_type(schema, &f.shape)
            )
        })
        .collect()
}
fn field_values(schema: &Descriptor, fields: &[Field]) -> String {
    fields
        .iter()
        .map(|f| {
            format!(
                "r#{}:{}?,",
                f.name,
                project(schema, &f.shape, &format!("v.get({:?})?", f.name))
            )
        })
        .collect()
}
fn rust_type(s: &Descriptor, t: &Shape) -> String {
    match t {
        Shape::Scalar(Scalar::Bytes) => "Vec<u8>".into(),
        Shape::Scalar(n) => n.name().into(),
        Shape::Unit => "()".into(),
        Shape::Named(i) => s.types()[*i].name.clone(),
        Shape::List(v) => format!("Vec<{}>", rust_type(s, v)),
        Shape::Map(v) => format!("Vec<(String,{})>", rust_type(s, v)),
        Shape::Option(v) => format!("Option<{}>", rust_type(s, v)),
        Shape::Tuple(ts) => format!(
            "({})",
            ts.iter()
                .map(|t| format!("{},", rust_type(s, t)))
                .collect::<String>()
        ),
    }
}
fn project(s: &Descriptor, t: &Shape, v: &str) -> String {
    match t {
        Shape::Scalar(Scalar::U64 | Scalar::I64 | Scalar::F32 | Scalar::F64) => {
            format!("{v}.as_str()?.parse().ok()")
        }
        Shape::Scalar(Scalar::String) => format!("{v}.as_str().map(str::to_owned)"),
        Shape::Scalar(Scalar::Bytes) => format!("sequence({v},|x|x.as_u64()?.try_into().ok())"),
        Shape::Scalar(Scalar::Bool) => format!("{v}.as_bool()"),
        Shape::Scalar(Scalar::Char) => format!("{v}.as_str()?.chars().next()"),
        Shape::Scalar(Scalar::U8 | Scalar::U16 | Scalar::U32) => {
            format!("{v}.as_u64()?.try_into().ok()")
        }
        Shape::Scalar(_) => format!("{v}.as_i64()?.try_into().ok()"),
        Shape::Unit => format!("{v}.is_null().then_some(())"),
        Shape::Named(i) => format!("{}::project({v})", s.types()[*i].name),
        Shape::List(k) | Shape::Map(k) | Shape::Option(k) => {
            let f = match t {
                Shape::List(_) => "sequence",
                Shape::Map(_) => "pairs",
                _ => "optional",
            };
            format!("{f}({v},|x|{})", project(s, k, "x"))
        }
        Shape::Tuple(ts) => format!(
            "Some(({}))",
            ts.iter()
                .enumerate()
                .map(|(i, t)| format!("{}?,", project(s, t, &format!("{v}.get({i})?"))))
                .collect::<String>()
        ),
    }
}
