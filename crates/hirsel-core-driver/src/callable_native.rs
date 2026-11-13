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
