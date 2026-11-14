//! Observer boundaries for the post-promotion numerical NaN-map walk.
use crate::context::{Context, Module};
use std::fmt::Write;

fn transformed(source: &str) -> String {
    let context = Context::new();
    let module = Module::parse(&context, "numeric-nan", source).unwrap();
    module.verify().unwrap();
    module.defer_numerical_nan_maps();
    module.verify().unwrap();
    module.text()
}

#[test]
fn canonical_nan_maps_stop_at_observers_and_walk_every_cycle_exit() {
    let fixture = include_str!("../../tests/core_runtime/numeric-nan.ll");
    for source in [
        fixture.to_owned(),
        fixture
            .replace("double", "float")
            .replace("f64", "f32")
            .replace("i64", "i32"),
    ] {
        let ir = transformed(&source);
        for name in [
            "returned",
            "bits",
            "typed_store",
            "conversion",
            "call",
            "packing",
            "copy_sign",
            "cycle_store",
            "two_maps_cycle",
            "successive_cycle",
            "spectral",
            "compare",
            "fast_math",
            "fast_condition",
        ] {
            let body = ir
                .split(&format!("@{name}("))
                .nth(1)
                .unwrap()
                .split("\n}")
                .next()
                .unwrap();
            let expected = match name {
                "compare" => 0,
                "two_maps_cycle" => 2,
                _ => 1,
            };
            assert_eq!(
                body.matches(" = select ").count(),
                expected,
                "{name}: {body}"
            );
            if name == "spectral" {
                assert!(!body.contains("%m = select"), "{body}");
                assert!(body.contains("%m2 = select"), "{body}");
            }
        }
    }
}

#[test]
fn numerical_use_walk_grows_without_capping_its_observer_search() {
    for observer in [false, true] {
        let mut source = String::from(
            "define i1 @large(double %x, ptr %out) {\nentry:\n\
             %n = fcmp uno double %x, %x\n\
             %m = select i1 %n, double 0x7FF8000000000000, double %x\n\
             %v0 = fadd double %m, 1.0\n",
        );
        for i in 1..1024 {
            writeln!(source, "%v{i} = fadd double %v{}, 1.0", i - 1).unwrap();
        }
        if observer {
            source.push_str("store double %v1023, ptr %out\n");
        }
        source.push_str("%cmp = fcmp olt double %v1023, 1.0\nret i1 %cmp\n}\n");
        let ir = transformed(&source);
        assert_eq!(ir.contains("%m = select"), observer, "{ir}");
    }
}
