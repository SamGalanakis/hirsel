use super::*;
use crate::tests::{core_dir, inputs_in};
use hirsel_core_ids::PackageId;

const GRANTS: &str = include_str!("../../hirsel-config-schema/schemas/grants.hirsel");

#[test]
fn compiler_and_bootstrap_descriptors_agree() {
    let sets: &[(&str, &str, &[&str], bool)] = &[
        (
            "pkg_v1",
            include_str!("../../hirsel-config-schema/schemas/pkg_v1.hirsel"),
            &["Package", "Workspace"],
            true,
        ),
        (
            "user_v1",
            include_str!("../../hirsel-config-schema/schemas/user_v1.hirsel"),
            &["Config"],
            false,
        ),
        (
            "host_v1",
            include_str!("../../hirsel-config-schema/schemas/host_v1.hirsel"),
            &["Group", "Admission", "Decision"],
            true,
        ),
        (
            "exec_v1",
            include_str!("../../hirsel-config-schema/schemas/exec_v1.hirsel"),
            &["Policy"],
            false,
        ),
        (
            "toolchain_v1",
            include_str!("../../hirsel-config-schema/schemas/toolchain_v1.hirsel"),
            &["Packages"],
            false,
        ),
        (
            "workloads_v1",
            include_str!("../../hirsel-config-schema/schemas/workloads_v1.hirsel"),
            &["Config"],
            true,
        ),
        (
            "product_v1",
            include_str!("../../hirsel-config-schema/schemas/product_v1.hirsel"),
            &["Product"],
            true,
        ),
        (
            "orders_v1",
            include_str!("../../hirsel-config-schema/fixtures/orders_v1.hirsel"),
            &["Site"],
            false,
        ),
    ];
    for &(module, text, roots, grants) in sets {
        let source = if grants {
            format!("{text}\n{GRANTS}")
        } else {
            text.to_string()
        };
        let (index, modules) = inputs_in(&source, &core_dir(), module);
        assert!(
            index.diagnostics.is_empty(),
            "{module}: {:?}",
            index.diagnostics
        );
        let program = Program::declare(&index, &modules);
        let app = modules.last().unwrap().id;
        let issues: Vec<_> = program
            .refusals
            .iter()
            .filter(|r| r.primary.module == app)
            .collect();
        assert!(issues.is_empty(), "{module}: {issues:?}");
        for &root in roots {
            let path = DefPath(hirsel_core_ids::ItemPath {
                package: PackageId {
                    name: "app".into(),
                    version: "1".into(),
                },
                modules: vec![module.into()],
                name: root.into(),
            });
            let compiler = program.data_descriptor(&path, module).unwrap();
            let bootstrap = Descriptor::lower(module, &source, root).unwrap();
            let embedded = if module == "orders_v1" {
                &bootstrap
            } else {
                hirsel_config_schema::schema(&format!("{module}.{root}")).unwrap()
            };
            assert_eq!(compiler.bytes(), embedded.bytes(), "{module}.{root}");
            assert_eq!(compiler.digest(), embedded.digest());
            if module == "orders_v1" {
                let doc = hirsel_data::decode(
                    include_bytes!("../../hirsel-config-schema/fixtures/site.hcfg"),
                    &[&compiler],
                    Default::default(),
                )
                .unwrap();
                assert_eq!(doc.value["retries"], "3");
            }
        }
    }
}
