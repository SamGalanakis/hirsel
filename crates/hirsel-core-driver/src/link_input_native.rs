
/// A release build's link holds no descriptor on any part it links: the
/// linker reads them as members of one bundle (`link_input::bundle_objects`).
/// Bug caught: a build that holds a descriptor per part from its check or
/// write to the link needs as many as it has parts (Checkbox: 7,724 for
/// 7,716 parts), past a service's soft limit of 1,024.
#[test]
#[ignore = "explicit native integration target"]
fn release_link_holds_no_descriptor_on_its_parts() {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7)) }\n";
    let dir = tempfile::tempdir().unwrap();
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    let parts = options.parts.clone();
    let held = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&held);
    AFTER_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |paths: &[PathBuf]| {
            let mut open = BTreeMap::<PathBuf, usize>::new();
            for fd in std::fs::read_dir("/proc/self/fd").unwrap() {
                if let Ok(target) = std::fs::read_link(fd.unwrap().path()) {
                    *open.entry(target).or_default() += 1;
                }
            }
            seen.borrow_mut().extend(
                paths
                    .iter()
                    .filter(|path| path.starts_with(&parts))
                    .map(|path| (path.clone(), open.get(path).copied())),
            );
        }));
    });
    let (_, _, status) = reach_build(&record_check(APP, &core_sources(), dir.path(), "store"), &options);
    AFTER_CHECK.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(status, Some(7));
    let held = held.take();
    assert!(!held.is_empty(), "the link checked release parts");
    for (part, descriptors) in held {
        assert_eq!(descriptors, None, "descriptors open on {}", part.display());
    }
}
