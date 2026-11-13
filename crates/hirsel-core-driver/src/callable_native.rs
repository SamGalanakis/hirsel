// The shared core-source harness checks and runs the callable cases without a
// second fixture core or a separate runtime convention.
fn captured_formal_regions(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-region-captured.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn captured_formal_regions_cranelift(){captured_formal_regions(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn captured_formal_regions_llvm(){captured_formal_regions(Backend::Llvm);}

fn map_churn(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let core=dir.path().join("core");std::fs::create_dir(&core).unwrap();
    for file in std::fs::read_dir(root.join("core/core")).unwrap() {
        let path=file.unwrap().path();
        if path.extension().is_some_and(|e| e == "hirsel") {std::fs::copy(&path,core.join(path.file_name().unwrap())).unwrap();}
    }
    let map=core.join("map.hirsel");let mut source=std::fs::read_to_string(&map).unwrap();
    source.push_str(include_str!("../../../core/front/fixtures/map-test-capacity.hirsel"));std::fs::remove_file(&map).unwrap();std::fs::write(map,source).unwrap();
    let mut request=source_request(include_str!("../../../core/front/fixtures/map-churn.hirsel"),dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&core,package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();run_clean(&built.executable,0);
    ordered_case(backend, include_str!("../../../core/front/cases/body/map-churn-owners.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn map_churn_cranelift(){map_churn(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn map_churn_llvm(){map_churn(Backend::Llvm);}
fn bytes_callable(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-bytes.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn bytes_callable_cranelift(){bytes_callable(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn bytes_callable_llvm(){bytes_callable(Backend::Llvm);}

fn map_string_words(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/map-string-words.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn map_string_words_cranelift(){map_string_words(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn map_string_words_llvm(){map_string_words(Backend::Llvm);}

fn callable_item_conversions(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-item-conversions.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn callable_item_conversions_cranelift(){callable_item_conversions(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn callable_item_conversions_llvm(){callable_item_conversions(Backend::Llvm);}

fn captured_callback(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-captured-callback.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn captured_callback_cranelift(){captured_callback(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn captured_callback_llvm(){captured_callback(Backend::Llvm);}

fn aggregate_callback(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-aggregate.hirsel"));
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-aggregate-captured.hirsel"));
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-aggregate-overwrite.hirsel"));
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-aggregate-viewed.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn aggregate_callback_cranelift(){aggregate_callback(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn aggregate_callback_llvm(){aggregate_callback(Backend::Llvm);}

fn aggregate_owned_projection(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-aggregate-owned-projection.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn aggregate_owned_projection_cranelift(){aggregate_owned_projection(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn aggregate_owned_projection_llvm(){aggregate_owned_projection(Backend::Llvm);}

fn owning_callable_scoped_views(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/callable-own-scoped.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn owning_callable_scoped_views_cranelift(){owning_callable_scoped_views(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn owning_callable_scoped_views_llvm(){owning_callable_scoped_views(Backend::Llvm);}

fn slots_needs_drop(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let core=dir.path().join("core");std::fs::create_dir(&core).unwrap();
    for file in std::fs::read_dir(root.join("core/core")).unwrap() {
        let path=file.unwrap().path();
        if path.extension().is_some_and(|e|e=="hirsel") {std::fs::copy(&path,core.join(path.file_name().unwrap())).unwrap();}
    }
    let file=core.join("list.hirsel");
    let mut source=std::fs::read_to_string(&file).unwrap();
    source.push_str(include_str!("../../../core/front/fixtures/slots-needs-drop.hirsel"));std::fs::remove_file(&file).unwrap();std::fs::write(file,source).unwrap();
    let mut request=source_request("pub fn main()->Result[(),Exit] {if list.needs_drop_law() {Ok(())}else{Error(Exit(status:1))}}",dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&core,package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let built=product.build(&DefPath::from_full("app@1/main/main").unwrap(),&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn slots_needs_drop_cranelift(){slots_needs_drop(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn slots_needs_drop_llvm(){slots_needs_drop(Backend::Llvm);}

fn map_clone(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/map-clone.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn map_clone_cranelift(){map_clone(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn map_clone_llvm(){map_clone(Backend::Llvm);}

fn invocation_region_headers(backend:Backend) {
 ordered_case(backend,include_str!("../../../core/front/cases/body/callable-closure-header-anchor-gap.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn invocation_region_headers_cranelift(){invocation_region_headers(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn invocation_region_headers_llvm(){invocation_region_headers(Backend::Llvm);}

fn scoped_result_regions(backend:Backend) {
 ordered_case(backend,include_str!("../../../core/front/cases/body/callable-scoped-region-result.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn scoped_result_regions_cranelift(){scoped_result_regions(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn scoped_result_regions_llvm(){scoped_result_regions(Backend::Llvm);}
