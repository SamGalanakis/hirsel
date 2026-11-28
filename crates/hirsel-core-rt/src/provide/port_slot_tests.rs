use super::*;

#[test]
fn bounded_ports_reuse_twenty_slots_and_retire_their_words() {
    let mut files = crate::host_file::Files::default();
    let mut context = Context::new(16 << 20);
    context.host_files = &raw mut files;
    context.limit_enter(1 << 20).unwrap();
    let registry = &files.registry;
    let cells = &registry.run_grants().cells;
    let mut previous = Vec::new();
    for _ in 0..128 {
        context.limit_enter(65536).unwrap();
        let payer = context.payer().address();
        let mut current = Vec::new();
        for _ in 0..4 {
            // SAFETY: the test owns the context, registry and current limit.
            let ports = unsafe {
                [
                    hirsel_core_provide_stdout(&mut context, 64, 0, 0),
                    hirsel_core_provide_stderr(&mut context, 64, 0, 0),
                    hirsel_core_provide_dir(&mut context, 64, 0),
                    hirsel_core_provide_net(&mut context, 64, 0),
                    hirsel_core_provide_exec(&mut context, 64, 0),
                ]
            };
            for port in ports {
                assert_ne!(port.value, 0);
                current.push((
                    port.value,
                    port.written,
                    registry.file_provider(port.written),
                    registry.file_handle(port.written),
                ));
            }
        }
        assert_eq!(lock(&cells.0).slots.len(), 20);
        for (word, served, provider, generation) in previous.drain(..) {
            assert!(retired(unsafe { &*registry.grant_provider(word) }));
            assert!(retired(&registry.file_provider(served)));
            assert!(!grant_live(&provider, generation));
            let mut next = empty_request(PORT_NEXT, generation, 0);
            // SAFETY: the run pins the reused slot. This is the previous
            // generation's resolved request, as a parked server would hold.
            assert_eq!(
                unsafe { (provider.resume)(provider.state, &mut next, 1) }
                    .outcome
                    .error,
                WITHDRAWN.error
            );
        }
        context.limit_exit();
        let table = lock(&cells.0);
        assert!(table.charged.is_empty() && table.words.is_empty());
        assert!(!lock(&BY_LIMIT).contains_key(&payer));
        previous = current;
    }
    context.limit_exit();
}
