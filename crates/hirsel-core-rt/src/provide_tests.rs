use super::*;

fn handle(kind: Kind, state: State) -> Handle {
    Handle {
        kind,
        state,
        write: None,
    }
}

/// A Dir port queues a call only on one of its own live objects of the
/// operation's kind (core.md, Provide: a forged or foreign handle is refused).
/// Anything else answers Denied (`IoError` tag 2) at issue and queues nothing,
/// so the server never sees an id it did not hand out.
#[test]
fn a_dir_port_refuses_every_handle_it_does_not_hold_live_for_the_op() {
    let mut port = Port {
        vocab: Vocab::Dir,
        ..Port::default()
    };
    port.handles.insert(0, handle(Kind::Dir, State::Live));
    port.handles.insert(5, handle(Kind::File, State::Live));
    port.handles.insert(6, handle(Kind::Entries, State::Live));
    port.handles.insert(7, handle(Kind::File, State::Closing));
    port.handles
        .insert(8, handle(Kind::File, State::Released(3)));
    port.next_id = 9;
    let mut cell = cells::test_slot(Cell::Port(Box::new(Mutex::new(port))));
    let state = (&raw mut *cell).cast::<u8>();
    let refused = [
        (OPEN, 99, "a forged id"),
        (READ, 99, "a forged id"),
        (READ, 0, "a File op on the root Dir"),
        (OPEN, 5, "a Dir op on a File"),
        (NEXT_ENTRIES, 5, "an Entries op on a File"),
        (READ, 6, "a File op on an Entries"),
        (WRITE, 7, "a closing File"),
        (READ, 8, "a released File"),
        (CLOSE, 8, "a released File"),
    ];
    for (token, &(operation, id, what)) in refused.iter().enumerate() {
        let mut request = empty_request(operation, id, 0);
        // SAFETY: `state` is the live cell; the request outlives the call.
        let step = unsafe { issue(state, &mut request, token as u64 + 1) };
        assert_eq!(step.kind, STEP_DONE, "{what}: op {operation} on {id}");
        assert_eq!(step.outcome.error.tag, 2, "{what}: op {operation} on {id}");
    }
    let Cell::Port(port) = &cell.cell else {
        unreachable!()
    };
    assert!(
        lock(port).calls.is_empty(),
        "a refused handle queued a call"
    );
    // The same ops on held objects of their kind are queued for the server.
    for (token, (operation, id)) in [(OPEN, 0), (READ, 5), (NEXT_ENTRIES, 6)]
        .into_iter()
        .enumerate()
    {
        let mut request = empty_request(operation, id, 0);
        // SAFETY: as above; the queued call is cleared before the request ends.
        let step = unsafe { issue(state, &mut request, 100 + token as u64) };
        assert_eq!(step.kind, STEP_PENDING, "op {operation} on {id}");
        let Cell::Port(port) = &cell.cell else {
            unreachable!()
        };
        lock(port).calls.clear();
    }
}

/// MEM-24: a port withdrawn when its limit ends settles every call, readies
/// its callers and parked servers, and frees its room. A `Call` the server
/// holds stays until the server drops it. Afterwards every operation fails
/// with Bounds (`WITHDRAWN`, which admission records as the cause) but a
/// close, which succeeds, and `next` fails the same way.
#[test]
fn a_withdrawn_port_settles_its_calls_and_answers_bounds_but_to_a_close() {
    let mut port = Port {
        vocab: Vocab::Dir,
        room: 1 << 20,
        ..Port::default()
    };
    port.handles.insert(0, handle(Kind::Dir, State::Live));
    port.handles.insert(5, handle(Kind::File, State::Live));
    port.used = 2 * HANDLE_ROOM;
    port.next_id = 6;
    port.servers.push((9, false));
    let mut cell = cells::test_slot(Cell::Port(Box::new(Mutex::new(port))));
    let state = (&raw mut *cell).cast::<u8>();
    let Cell::Port(port) = &cell.cell else {
        unreachable!()
    };
    let mut queued = empty_request(READ, 5, 0);
    let mut taken = empty_request(OPEN, 0, 0);
    // SAFETY: `state` is the live cell; the requests outlive their calls.
    unsafe {
        assert_eq!(issue(state, &mut queued, 1).kind, STEP_PENDING);
        assert_eq!(issue(state, &mut taken, 2).kind, STEP_PENDING);
    }
    let held = {
        let mut p = lock(port);
        let (&id, call) = p.calls.iter_mut().find(|(_, c)| c.token == 2).unwrap();
        (call.taken, call.held) = (true, true);
        id
    };
    let (mut tokens, retired) = lock(port).withdraw();
    tokens.sort_unstable();
    assert_eq!(
        tokens,
        [1, 2, 9],
        "both callers and the parked server are readied"
    );
    assert!(retired.is_empty());
    {
        let p = lock(port);
        assert!(p.withdrawn && p.sealed);
        assert_eq!(p.used, 0, "the room is free");
        assert!(p.handles.is_empty() && p.keys.is_empty() && p.servers.is_empty());
        let tag = WITHDRAWN.error.tag;
        assert!(
            p.calls
                .values()
                .all(|c| c.answer.is_some_and(|a| a.error.tag == tag))
        );
        assert!(p.calls.contains_key(&held), "the server's Call stays");
    }
    for (operation, id, tag) in [
        (READ, 5, WITHDRAWN.error.tag),
        (OPEN, 0, WITHDRAWN.error.tag),
        (CLOSE, 5, 0),
    ] {
        let mut request = empty_request(operation, id, 0);
        // SAFETY: as above.
        let step = unsafe { issue(state, &mut request, 20) };
        assert_eq!(step.kind, STEP_DONE, "op {operation} on {id}");
        assert_eq!(step.outcome.error.tag, tag, "op {operation} on {id}");
    }
    let mut next = empty_request(PORT_NEXT, 0, 0);
    // SAFETY: as above.
    let step = unsafe { issue(state, &mut next, 21) };
    assert_eq!(
        (step.kind, step.outcome.error.tag),
        (STEP_DONE, WITHDRAWN.error.tag)
    );
    lock(port).calls.clear();
}
