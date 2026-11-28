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
        let mut module = image(k);
        let second = hirsel_mod::Entry {
            identity: ContentHash([91; 32]),
            ..module.entries[0]
        };
        module.entries.push(second);
        let bytes = module.encode();
        loader.admit_module(&bytes).unwrap();
        loader.image(&bytes, true).unwrap()
    };
    let a = install(10);
    let old = a.resolve::<Leaf>(&IDENTITY).unwrap();
    assert!(matches!(
        a.resolve::<Other>(&IDENTITY),
        Err(Refusal::NoEntry)
    ));
    let original = a
        .code::<Leaf>(&IDENTITY)
        .unwrap()
        .register([old.candidate()])
        .unwrap();
    let first = slot.activate_checkout(0, &original).unwrap();
    drop(original);
    drop(a);
    let b = install(20);
    let new = b.resolve::<Leaf>(&IDENTITY).unwrap();
    let sibling = b.resolve::<Leaf>(&ContentHash([91; 32])).unwrap();
    let code = b.code::<Leaf>(&IDENTITY).unwrap();
    assert!(matches!(
        code.register([new.candidate(), sibling.candidate(), old.candidate()]),
        Err(Refusal::NoEntry)
    ));
    let selected = code
        .register([new.candidate(), sibling.candidate()])
        .unwrap();
    let reversed = code
        .register([sibling.candidate(), new.candidate()])
        .unwrap();
    let (answer, second) = worker
        .dispatch_entry(&first, 0, &old, |entry| {
            let second = slot.activate_checkout(first.version(), &selected).unwrap();
            // SAFETY: the callback owns the original generation through the call.
            (unsafe { entry(1) }, second)
        })
        .unwrap()
        .unwrap();
    assert_eq!(answer, 12);
    assert_eq!(
        worker.dispatch_entry(&first, 0, &old, |_| ()),
        Err(Refusal::NoEntry)
    );
    assert_eq!(
        worker.dispatch_entry(&second, 0, &old, |_| ()),
        Err(Refusal::NoEntry)
    );
    // A second checkout of B can have a different selection or grant binding.
    let third = slot.activate_checkout(second.version(), &reversed).unwrap();
    assert_eq!(
        worker.dispatch_entry(&second, 0, &new, |_| ()),
        Err(Refusal::NoEntry)
    );
    for ordinal in [0, 2] {
        assert_eq!(
            worker.dispatch_entry(&third, ordinal, &new, |_| ()),
            Err(Refusal::NoEntry)
        );
    }
    // SAFETY: the selected leaf belongs to B.
    assert_eq!(
        worker.dispatch_entry(&third, 1, &new, |f| unsafe { f(1) }),
        Ok(Some(22))
    );
    assert_eq!(
        worker.dispatch_registered(&third, 0, |_: Other| unreachable!()),
        Err::<Option<()>, _>(Refusal::NoEntry)
    );
    assert_eq!(
        worker.dispatch_registered(&second, 0, |_: Leaf| unreachable!()),
        Err::<Option<()>, _>(Refusal::NoEntry)
    );
    // A repeated executable can serve two registrations with different bound arguments.
    let repeated = code.register([new.candidate(), new.candidate()]).unwrap();
    let fourth = slot.activate_checkout(third.version(), &repeated).unwrap();
    for ordinal in [0, 1] {
        assert_eq!(
            worker.dispatch_entry(&fourth, ordinal, &new, |f| unsafe { f(1) }),
            Ok(Some(22))
        );
    }
    let empty = code.register([]).unwrap();
    let fifth = slot.activate_checkout(fourth.version(), &empty).unwrap();
    assert_eq!(
        worker.dispatch_registered(&fifth, 0, |_: Leaf| ()),
        Err(Refusal::NoEntry)
    );
    drop((first, second, third, fourth, fifth, repeated, empty));
    drop((b, code, selected, reversed));
    drop(slot);
    worker.park();
    assert_eq!((loader.installed(), loader.retired()), (0, 0));
}
