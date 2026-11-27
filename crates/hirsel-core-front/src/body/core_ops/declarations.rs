use super::*;

pub(crate) fn assert_declarations(p: &Program<'_>) {
    use std::collections::BTreeSet;
    let actual: BTreeSet<_> = p
        .fns
        .iter()
        .filter(|f| f.path.package_is("core", "1") && f.body.is_none())
        .map(|f| (f.path.0.modules[0].as_str(), f.path.0.name.as_str()))
        .collect();
    for entry in OPERATIONS {
        assert!(matches!(entry.class, "A" | "B" | "C"));
        if entry.private {
            assert!(
                entry.module == "string"
                    && matches!(
                        entry.name,
                        "of_bytes" | "to_bytes" | "view_string" | "shorten"
                    )
                    || entry.module == "compiler" && entry.name == "check"
                    || entry.module == "map"
                    || entry.module == "list" && entry.name.starts_with("slots_")
                    || matches!(entry.module, "deque" | "heap")
            );
        }
    }
    let recorded: BTreeSet<_> = OPERATIONS.iter().map(|e| (e.module, e.name)).collect();
    assert_eq!(
        recorded.len(),
        OPERATIONS.len(),
        "duplicate core operation binding"
    );
    assert_eq!(
        actual, recorded,
        "assign each bodiless core function to its owning lane"
    );
}

/// CAP-6: a kernel that observes outside its operands (the host, the standard streams,
/// the process arguments) is reached only through an operation with an authority
/// operand: a capability, `Args`, or a core resource made from one (CAP-7). The match
/// has no wildcard, so a new kernel is classified before it can bind.
pub(crate) fn assert_no_ambient_input(p: &Program<'_>) {
    fn ambient(k: Kernel) -> bool {
        match k {
            Kernel::CheckLibrary
            | Kernel::Stream(_)
            | Kernel::StreamTerminal(_)
            | Kernel::StreamSize(_)
            | Kernel::Host(_)
            | Kernel::ArgsLen
            | Kernel::ArgsGet
            | Kernel::Env(_) => true,
            Kernel::Media(op) => op != HostOp::ScheduleQuicInitial,
            // `net.class` reads only its address (NET-18).
            Kernel::Net(op) => op != HostOp::NetClass,
            // A virtual provider observes only run state the program made.
            Kernel::Provide(_)
            | Kernel::ReadOnlyDir
            | Kernel::CarrierMove
            | Kernel::CarrierView
            | Kernel::SecretSeal
            | Kernel::SecretRender
            | Kernel::StringShorten
            | Kernel::StringList(_)
            | Kernel::Unique
            | Kernel::NewOwner(_)
            | Kernel::Share(_)
            | Kernel::TryUnwrap(_)
            | Kernel::WithUnique(_)
            | Kernel::Unbox
            | Kernel::Spawn
            | Kernel::TrySpawn
            | Kernel::CheckCancel
            | Kernel::Cancel
            | Kernel::Join
            | Kernel::ChannelNew
            | Kernel::SpawnUnit
            | Kernel::ConnNew
            | Kernel::ConnClient
            | Kernel::SecretShare
            | Kernel::ConnSend
            | Kernel::ConnTrySend
            | Kernel::ConnReceive
            | Kernel::ConnTryReceive
            | Kernel::LatestNew
            | Kernel::LatestHandle
            | Kernel::LatestPublish
            | Kernel::LatestRead
            | Kernel::SharedNew
            | Kernel::SharedShare
            | Kernel::ChannelSender
            | Kernel::Send
            | Kernel::TrySend
            | Kernel::ReceiveUntil
            | Kernel::TryReceive
            | Kernel::Receive
            | Kernel::RootLimit
            | Kernel::CodeIdentity
            | Kernel::CodeRevision
            | Kernel::CodeCallable
            | Kernel::CodeDescribe
            | Kernel::Cell(_)
            | Kernel::Flow(_) => false,
        }
    }
    for (f, fd) in p.fns.iter().enumerate() {
        let Some(i) = p.core_operations[f] else {
            continue;
        };
        if i >= u16::MAX - 4 {
            continue;
        }
        let Lane::Runtime(kernel) = OPERATIONS[i as usize].lane else {
            continue;
        };
        if !ambient(kernel) {
            continue;
        }
        let authority = fd.params.iter().any(|param| match p.types.kind(param.ty) {
            TyKind::Adt(d, _) => {
                let decl = &p.decls[d.index()];
                crate::layout::handle(decl)
                    || decl.repr == crate::decls::Repr::Resource
                        && decl.path.package_is("core", "1")
            }
            _ => false,
        });
        assert!(
            authority,
            "CAP-6: {} observes outside its operands without an authority operand",
            fd.path
        );
    }
}
