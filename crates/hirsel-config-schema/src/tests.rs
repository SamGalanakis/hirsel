use super::*;
const MEMBER: &[u8] = include_bytes!("../fixtures/app/hirsel.hcfg");
const WORKSPACE: &[u8] = include_bytes!("../fixtures/hirsel.hcfg");
const GROUP: &[u8] = include_bytes!("../fixtures/group.hcfg");
const CONFIG: &[u8] = include_bytes!("../fixtures/config.hcfg");
#[test]
fn proposal_roots_and_app_edits() {
    use hirsel_data::{Descriptor, PathPart, decode};
    let limits = Limits::default();
    let pkg = pkg_v1::Package::read(MEMBER, limits).unwrap();
    assert_eq!(pkg.sources, [("main.hirsel".into(), "main".into())]);
    assert!(
        matches!(&pkg.dependencies[0].1,pkg_v1::Dependency::Path{path,..} if path=="../ffi_laws")
    );
    assert_eq!(
        pkg_v1::Workspace::read(WORKSPACE, limits).unwrap().members,
        ["app", "ffi_laws"]
    );
    let private = user_v1::Config::read(CONFIG, limits).unwrap();
    assert_eq!(private.default_context.as_deref(), Some("lab"));
    assert!(matches!(
        private.contexts[0].1,
        user_v1::Context::Host {
            principal: user_v1::Principal::Uid(1001),
            ..
        }
    ));
    let group = host_v1::Group::read(GROUP, limits).unwrap();
    assert_eq!(group.memory_bytes, Some(268435456));
    assert_eq!(group.grants, [host_v1::Grant::Clock]);
    let s = Descriptor::lower(
        "orders_v1",
        include_str!("../fixtures/orders_v1.hirsel"),
        "Site",
    )
    .unwrap();
    let source = include_str!("../fixtures/site.hcfg")
        .replace("    timeout:", "    // author comment\n    timeout:");
    let doc = decode(source.as_bytes(), &[&s], limits).unwrap();
    assert_eq!(doc.value["retries"], "3");
    let labels = PathPart::Field("labels".into());
    let sample = [labels.clone(), PathPart::Key("role".into())];
    assert!(
        doc.replace(&sample, "\"writer\"", limits)
            .unwrap()
            .is_none()
    );
    let edited = doc.replace(&sample, "\"reader\"", limits).unwrap().unwrap();
    let mut result = source.clone();
    result.replace_range(
        edited.span.start as usize..edited.span.end as usize,
        &edited.replacement,
    );
    assert_eq!(result, source.replace("\"writer\"", "\"reader\""));
    let insert = doc
        .insert(&[labels], PathPart::Key("rack".into()), "\"blue\"", limits)
        .unwrap()
        .unwrap();
    let at = source.find("(\"region\"").unwrap();
    assert_eq!(
        (insert.span.start as usize, insert.span.end as usize),
        (at, at)
    );
    assert_eq!(insert.replacement, "(\"rack\", \"blue\"), ");
}
