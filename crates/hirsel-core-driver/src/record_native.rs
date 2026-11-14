fn record_inputs(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let mut options=native_options(backend,dir.path());
    let mut driver=Driver::default();
    let source="pub fn main() -> Result[(), Exit] { Error(Exit(status: value())) }\nfn value() -> u8 { 7 }\n";
    let product=driver.check(source_request(source,dir.path()),&mut Meter::new(false)).unwrap();
    let cold=product.build(&entry,&options).unwrap();assert_eq!(cold.compiled,2);assert!(cold.linked);run_clean(&cold.executable,7);
    let warm=Driver::default().check(source_request(source,dir.path()),&mut Meter::new(false)).unwrap();assert!(warm.rechecked.is_empty());
    let built=warm.build(&entry,&options).unwrap();assert_eq!(built.compiled,0);assert!(!built.linked);
    std::fs::remove_file(&built.executable).unwrap();
    let restored=warm.build(&entry,&options).unwrap();assert_eq!(restored.compiled,0);assert!(!restored.linked);run_clean(&restored.executable,7);
    std::fs::write(&restored.executable,b"replaced user-visible executable").unwrap();
    let restored=warm.build(&entry,&options).unwrap();assert_eq!(restored.compiled,0);assert!(!restored.linked);run_clean(&restored.executable,7);
    let body=source.replace("{ 7 }","{ 8 }");
    let product=driver.check(source_request(&body,dir.path()),&mut Meter::new(false)).unwrap();assert_eq!(product.rechecked.len(),1);
    let edited=product.build(&entry,&options).unwrap();assert_eq!(edited.compiled,1);assert!(edited.linked);run_clean(&edited.executable,8);
    let signature=body.replace("value()","value(8u8)").replace("fn value(8u8)","fn value(n: u8)").replace("{ 8 }","{ n }");
    let product=driver.check(source_request(&signature,dir.path()),&mut Meter::new(false)).unwrap();assert_eq!(product.rechecked.len(),2);
    let edited=product.build(&entry,&options).unwrap();assert_eq!(edited.compiled,2);assert!(edited.linked);run_clean(&edited.executable,8);

    fn quote(path:&Path)->String {format!("'{}'",path.display().to_string().replace('\'',"'\\''"))}
    let original=options.cc.clone();
    let selected=command(Command::new(&original).arg("-print-prog-name=ld.lld")).unwrap();
    let selected=String::from_utf8(selected).unwrap();
    let original_ld=if selected.trim().contains('/') {PathBuf::from(selected.trim())} else {
        let paths=std::env::var_os("PATH").unwrap();
        std::env::split_paths(&paths).map(|p|p.join(selected.trim())).find(|p|p.is_file()).unwrap()
    };
    let bin=dir.path().join("bin");std::fs::create_dir(&bin).unwrap();
    let ld=bin.join("ld.lld");
    let ld_script=format!("#!/bin/sh\nexec {} \"$@\"\n",quote(&original_ld));
    std::fs::write(&ld,&ld_script).unwrap();std::fs::set_permissions(&ld,std::fs::Permissions::from_mode(0o755)).unwrap();
    let cc=dir.path().join("cc");
    let script=format!("#!/bin/sh\nif [ \"$1\" = -print-prog-name=ld.lld ]; then printf '%s\\n' {}; exit; fi\nexec {} -B{} \"$@\"\n",quote(&ld),quote(&original),quote(&bin));
    std::fs::write(&cc,&script).unwrap();std::fs::set_permissions(&cc,std::fs::Permissions::from_mode(0o755)).unwrap();
    let version=command(Command::new(&cc).arg("--version")).unwrap();
    assert_eq!(version,command(Command::new(&original).arg("--version")).unwrap());
    options.cc=cc.clone();
    let changed=product.build(&entry,&options).unwrap();assert_eq!(changed.compiled,0);assert!(changed.linked,"different C driver, same version");
    assert!(!product.build(&entry,&options).unwrap().linked);
    let new_cc=dir.path().join("replacement-cc");std::fs::write(&new_cc,format!("{script}# replacement\n")).unwrap();std::fs::set_permissions(&new_cc,std::fs::Permissions::from_mode(0o755)).unwrap();std::fs::rename(new_cc,&cc).unwrap();
    assert_eq!(version,command(Command::new(&cc).arg("--version")).unwrap());
    let changed=product.build(&entry,&options).unwrap();assert_eq!(changed.compiled,0);assert!(changed.linked,"replaced C driver, same version");
    let same_cc=dir.path().join("same-cc");std::fs::write(&same_cc,std::fs::read(&cc).unwrap()).unwrap();std::fs::set_permissions(&same_cc,std::fs::Permissions::from_mode(0o755)).unwrap();std::fs::rename(same_cc,&cc).unwrap();
    let unchanged=product.build(&entry,&options).unwrap();assert_eq!(unchanged.compiled,0);assert!(!unchanged.linked,"identical C driver bytes must reuse");
    let new_ld=bin.join("replacement-ld");std::fs::write(&new_ld,format!("{ld_script}# replacement\n")).unwrap();std::fs::set_permissions(&new_ld,std::fs::Permissions::from_mode(0o755)).unwrap();std::fs::rename(new_ld,&ld).unwrap();
    let changed=product.build(&entry,&options).unwrap();assert_eq!(changed.compiled,0);assert!(changed.linked,"replaced link editor");run_clean(&changed.executable,8);
    std::fs::remove_file(ld).unwrap();
    let refused=product.build(&entry,&options).err().unwrap().to_string();assert!(refused.contains("tool selection"),"{refused}");
    options.cc=original.clone();
    let runtime=dir.path().join("runtime.a");std::fs::write(&runtime,std::fs::read(&options.runtime).unwrap()).unwrap();options.runtime=runtime.clone();
    let c=dir.path().join("extra.c");let object=dir.path().join("extra.o");std::fs::write(&c,"void hirsel_driver_cache_probe(void) {}\n").unwrap();
    command(Command::new(&original).arg("-c").arg(c).arg("-o").arg(&object)).unwrap();
    command(Command::new("ar").arg("rcs").arg(runtime).arg(object)).unwrap();
    let changed=product.build(&entry,&options).unwrap();assert_eq!(changed.compiled,2);assert!(changed.linked,"changed runtime bytes");run_clean(&changed.executable,8);
}
#[test]
#[ignore="explicit native integration target"]
fn executable_tool_runtime_records_cranelift() {record_inputs(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn executable_tool_runtime_records_llvm() {record_inputs(Backend::Llvm);}
