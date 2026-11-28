use super::*;

#[test]
fn resolved_entry_stays_with_its_installation_through_replacement() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(config()).unwrap();
    let slot = loader.slot::<Leaf>();
    let mut worker = loader.worker();
    let install = |k| {
        let bytes = image(k).encode();
        loader.admit_module(&bytes).unwrap();
        loader.image(&bytes, true).unwrap()
    };
    let a = install(10);
    let old = a.resolve::<Leaf>(&IDENTITY).unwrap();
    assert!(matches!(
        a.resolve::<Other>(&IDENTITY),
        Err(Refusal::NoEntry)
    ));
    let first = slot
        .activate_checkout(0, a.code::<Leaf>(&IDENTITY).unwrap())
        .unwrap();
    drop(a);
    let b = install(20);
    let new = b.resolve::<Leaf>(&IDENTITY).unwrap();
    let (answer, second) = worker
        .dispatch_entry(&first, &old, |entry| {
            let second = slot
                .activate_checkout(first.version(), b.code::<Leaf>(&IDENTITY).unwrap())
                .unwrap();
            // SAFETY: the callback owns the original generation through the call.
            (unsafe { entry(1) }, second)
        })
        .unwrap()
        .unwrap();
    assert_eq!(answer, 12);
    assert_eq!(
        worker.dispatch_entry(&first, &old, |_| ()),
        Err(Refusal::NoEntry)
    );
    assert_eq!(
        worker.dispatch_entry(&second, &old, |_| ()),
        Err(Refusal::NoEntry)
    );
    // A second checkout of B can have a different selection or grant binding.
    let third = slot
        .activate_checkout(second.version(), b.code::<Leaf>(&IDENTITY).unwrap())
        .unwrap();
    assert_eq!(
        worker.dispatch_entry(&second, &new, |_| ()),
        Err(Refusal::NoEntry)
    );
    // SAFETY: the selected leaf belongs to B.
    assert_eq!(
        worker.dispatch_entry(&third, &new, |f| unsafe { f(1) }),
        Ok(Some(22))
    );
    drop((first, second, third));
    drop(b);
    drop(slot);
    worker.park();
    assert_eq!((loader.installed(), loader.retired()), (0, 0));
}
