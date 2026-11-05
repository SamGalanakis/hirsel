use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;

#[test]
fn headers_compose_module_declarations_once() {
    with_compiler_stack(|| {
        for count in [16, 64, 256] {
            let source = (0..count)
                .map(|i| format!("fn f{i}(a: u8) -> u8 {{ a }}\n"))
                .collect::<String>();
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let before =
                hirsel_intern::work::read(hirsel_intern::work::Counter::ModuleDeclarationRows);
            let source_bytes = source.len() as u64;
            let bytes_before =
                hirsel_intern::work::read(hirsel_intern::work::Counter::SourceTextBytes);
            let file = engine.sources.add("headers.hir", source).unwrap();
            let parsed = engine.parse_file(file, &default_context()).unwrap();
            assert!(parsed.diagnostics.is_empty());
            assert_eq!(parsed.value.items.len(), count as usize);
            for id in &parsed.value.items {
                let path = &id.item.definition;
                let resolved = engine.resolve_definition(path).unwrap();
                assert!(resolved.diagnostics.is_empty());
                let signature = engine.item_signature(path).unwrap();
                assert!(signature.diagnostics.is_empty());
            }
            let inspected =
                hirsel_intern::work::read(hirsel_intern::work::Counter::ModuleDeclarationRows)
                    - before;
            let exposed = hirsel_intern::work::read(hirsel_intern::work::Counter::SourceTextBytes)
                - bytes_before;
            // SourceFile::text also serves span slicing: exposing a source
            // does not scan all of it. Allow at most eight full-text accesses
            // per header across parsing, indexing and signature resolution.
            assert!(
                exposed <= 8 * count * source_bytes,
                "{count} headers exposed {exposed} source bytes, input {source_bytes}"
            );
            assert!(
                inspected <= 8 * count,
                "{count} headers inspected {inspected} module declaration rows"
            );
        }
    });
}

/// Indexing a file updates the rows of its own declarations; the module input
/// is composed once, when resolution first reads it. Publishing every row
/// again for each file made loading a package quadratic in its files.
#[test]
fn files_index_their_own_declarations_once() {
    with_compiler_stack(|| {
        for files in [8, 32, 128] {
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let before =
                hirsel_intern::work::read(hirsel_intern::work::Counter::ModuleDeclarationRows);
            for f in 0..files {
                let source = (0..4)
                    .map(|i| format!("fn f{f}_{i}(a: u8) -> u8 {{ a }}\n"))
                    .collect::<String>();
                let file = engine.sources.add(format!("m{f}.hir"), source).unwrap();
                engine.parse_file(file, &default_context()).unwrap();
            }
            let path = |name: &str| {
                let mut path = entry_path(&default_context());
                path.0.name = name.into();
                path
            };
            assert!(engine.resolve_definition(&path("f0_0")).is_ok());
            let rows =
                hirsel_intern::work::read(hirsel_intern::work::Counter::ModuleDeclarationRows)
                    - before;
            let items = 4 * files;
            assert!(
                rows <= 3 * items,
                "{files} files of {items} items composed {rows} declaration rows"
            );
        }
    });
}
