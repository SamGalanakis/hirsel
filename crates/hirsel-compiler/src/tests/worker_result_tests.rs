use super::{Refusal, caps, refusal};

#[test]
fn explicit_tool_refusal_is_distinct_from_worker_termination() {
    assert_eq!(
        refusal(
            "echo '\"ToolRefusal\"'; echo 'link tool refused' >&2; exit 2",
            caps()
        ),
        Refusal::ToolRefusal("link tool refused\n".into())
    );
}

#[test]
fn capped_worker_emits_the_selected_typed_exports() {
    use super::{Caps, Request, Service, Target, real_worker_path, request};
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(real_worker_path(), dir.path().into(), Caps::HOST, 1, 0);
    let files = [("main.hirsel", b"pub fn left(take v: u64) -> u64 { v + 1u64 }\npub fn right(take v: u64) -> u64 { v + 2u64 }\npub fn main() -> Result[(), Exit] { Ok(()) }".as_slice())];
    let request = Request {
        files: &files,
        ..request(Target::X86_64Linux)
    };
    let program = service.compile(&request).unwrap();
    let root = hirsel_mod::Module::parse(&program.bytes).unwrap();
    assert_eq!(root.entries().count(), 1);
    assert_eq!(
        root.entries().next().unwrap().descriptor,
        hirsel_mod::NO_DESCRIPTOR
    );
    let names = ["local/app@1/main/left", "local/app@1/main/right"];
    let built = service.compile_exports(&request, &names).unwrap();
    let module = hirsel_mod::Module::parse(&built.bytes).unwrap();
    let entries: Vec<_> = module.entries().collect();
    assert_eq!(entries.len(), names.len());
    assert_eq!(entries[0].signature, entries[1].signature);
    let zero = 0u64.to_le_bytes();
    for name in names {
        let id = hirsel_core_ids::ContentHash::of_parts(
            "hirsel.core-symbol.v1",
            [name.as_bytes(), &zero, &zero],
        );
        let row = entries
            .iter()
            .find(|row| row.identity == id)
            .expect("selected entry");
        let descriptor = module.descriptor(row).unwrap().unwrap();
        let source_identity = format!("\x01{}(take u64)->u64", name.replace("local/", "local%2F"));
        assert_eq!(descriptor.identity, source_identity.as_bytes());
        assert!(descriptor.revision.is_some());
    }
    let reversed = service
        .compile_exports(&request, &[names[1], names[0]])
        .unwrap();
    assert_eq!(built, reversed);
    assert!(matches!(
        service.compile_exports(&request, &[names[0], "local/app@1/main/absent"]),
        Err(Refusal::Unsupported(_))
    ));
}
