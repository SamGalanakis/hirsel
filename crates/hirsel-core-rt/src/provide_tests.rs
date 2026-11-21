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
    let mut cell = Cell::Port(Mutex::new(port));
    let state = (&raw mut cell).cast::<u8>();
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
    let Cell::Port(port) = &cell else {
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
        let Cell::Port(port) = &cell else {
            unreachable!()
        };
        lock(port).calls.clear();
    }
}
