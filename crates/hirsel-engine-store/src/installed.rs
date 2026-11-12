//! An installed package binds its discovery inputs and complete product closure.
//! This is ordinary untrusted content. It never imports verifier authority.
use super::*;
mod image;
mod semantic;
use hirsel_syntax::{FileContext, PackageId};
pub use semantic::{InstalledDefinition, InstalledDefinitionKey, InstalledDefinitionOwnership};
pub use semantic::{
    InstalledSemanticInput, InstalledSpecializations, SEMANTIC_BODY, SEMANTIC_HEADER,
    SEMANTIC_INPUT, SEMANTIC_SOURCE,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PACKAGE: Domain = Domain {
    kind: 0x4600_0039,
    schema: 5,
};
fn address() -> Address {
    Address(ContentHash::of_bytes(b"hirsel.installed-package.v5"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(
        root: &Path,
    ) -> (
        hirsel_artifact_store::Store,
        ContentHash,
        PackageId,
        Reference,
    ) {
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        let compiler = ContentHash::of_bytes(b"compiler");
        let package = PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let reference = store
            .put(
                Domain {
                    kind: 77,
                    schema: 1,
                },
                b"product",
            )
            .unwrap();
        let row = Address(ContentHash::of_bytes(b"row"));
        let action = Action(ContentHash::of_bytes(b"action"));
        store.associate(row, action, reference).unwrap();
        let records = InstallationRecords {
            rows: [(row.0, (action, reference))].into(),
            contents: [(reference.output.0, reference)].into(),
        };
        let installed = InstalledPackage::new(
            compiler,
            package.clone(),
            b"manifest".to_vec(),
            vec![InstalledSource {
                path: "/absent/source.hir".into(),
                logical: "std@1/source.hir".into(),
                context: FileContext {
                    package: package.clone(),
                    modules: vec![],
                },
                text: "pub fn value()->u8{1u8}".into(),
            }],
            vec![],
            InstalledInputs::default(),
        );
        installed.publish(&store, &records).unwrap();
        (store, compiler, package, reference)
    }
    /// The attack's schedule: the index skips one section write of a new
    /// publication and admits everything after it, header and association
    /// included. Publication must fail and the root keep serving the
    /// installation it had, rather than associate a header whose rows section
    /// never reached storage.
    #[test]
    fn a_skipped_section_write_fails_publication_and_keeps_the_old_header() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let old = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package
            .identity;
        let next = (*InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package)
            .clone();
        // A new product, so the next image's rows section is new content.
        let product = store
            .put(
                Domain {
                    kind: 77,
                    schema: 1,
                },
                b"next product",
            )
            .unwrap();
        let mut records = records_of(&next);
        records.rows.insert(
            ContentHash::of_bytes(b"next row"),
            (Action(ContentHash::of_bytes(b"next action")), product),
        );
        records.contents.insert(product.output.0, product);
        // The rows section is the image's first write.
        store.skip_index_publications_for_test(1);
        next.publish(&store, &records).unwrap_err();
        let fresh = hirsel_artifact_store::Store::new(directory.path().to_owned());
        let served = InstalledPackage::open(&fresh, &package, compiler).unwrap();
        assert_eq!(served.package.identity, old);
    }
    /// The rows and contents an opened package lists, to republish it.
    fn records_of(installed: &InstalledPackage) -> InstallationRecords {
        InstallationRecords {
            rows: installed
                .rows
                .iter()
                .map(|(address, row)| (*address, (row.action, row.reference)))
                .collect(),
            contents: installed
                .contents
                .iter()
                .map(|reference| (reference.output.0, *reference))
                .collect(),
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn required_installed_content_survives_request_publication_pressure() {
        const CHILD: &str = "HIRSEL_INSTALLED_BACKING_PRESSURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "ulimit -n 128 && exec \"$@\"", "installed-backing-law"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "installed::tests::required_installed_content_survives_request_publication_pressure",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(&directory.path().join("installed"));
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        let sources = installed.package.source_records().unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let request = RequestStore::with_scratch(
            Arc::new(DirStore::new(directory.path().join("local"))),
            budget.reserve(1 << 20).unwrap(),
            directory.path(),
            8 << 20,
        )
        .unwrap();
        let operation = request.operation_scope();
        let domain = Domain {
            kind: 78,
            schema: 1,
        };
        let mut expected = Vec::new();
        for value in 0..160u32 {
            let mut bytes = vec![7; 32 << 10];
            bytes[..4].copy_from_slice(&value.to_le_bytes());
            let reference = request.put_content(domain, &bytes).unwrap();
            expected.push((reference, value));
        }
        // Every publication fits its existing RAM/disk allowance. Keeping those
        // required outputs cannot consume the descriptors needed to read an
        // installed record. The package's original payload is the oracle.
        assert_eq!(&*sources.content(reference, 4096).unwrap(), b"product");
        drop(operation);
        for (reference, value) in expected {
            let bytes = request.get_content(reference).unwrap();
            assert_eq!(&bytes[..4], &value.to_le_bytes());
            assert!(bytes[4..].iter().all(|byte| *byte == 7));
        }
        drop(request);
        assert_eq!(budget.reserved(), 0);
        drop(sources);
        drop(installed);
        // Actual missing backing remains an explicit failure at demand and
        // installation audit; descriptor pressure never makes it absent-OK.
        std::fs::remove_file(store.output_path(reference)).unwrap();
        let reopened = InstalledPackage::open(&store.for_operation(), &package, compiler).unwrap();
        assert!(
            reopened
                .package
                .source_records()
                .unwrap()
                .content(reference, 4096)
                .unwrap_err()
                .contains("missing installed semantic content")
        );
        assert!(InstalledPackage::verify_store(store.root()).is_err());
        let records = records_of(&reopened.package);
        assert!(
            reopened
                .package
                .as_ref()
                .clone()
                .publish(&store, &records)
                .is_err()
        );
    }
    #[test]
    fn install_and_audit_reject_stale_corrupt_and_partial_products() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _reference) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package;
        drop(installed);
        assert!(
            InstalledPackage::open(&store, &package, ContentHash::default())
                .unwrap_err()
                .contains("compiler identity mismatch")
        );
        let row = store.lookup(address()).unwrap();
        // The header alone defines the installation: the cache index row at
        // the same address is never consulted, so losing it changes nothing.
        let product = Address(ContentHash::of_bytes(b"row"));
        let listed = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .records
            .lookup(product)
            .unwrap();
        std::fs::remove_file(store.association_path(product).unwrap()).unwrap();
        InstalledPackage::verify_store(directory.path()).unwrap();
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        assert_eq!(installed.records.lookup(product), Some(listed));
        // A listed row whose content is gone is partial, and a publication
        // that would list it fails without replacing the current header.
        let content = store.output_path(listed.reference);
        let kept = std::fs::read(&content).unwrap();
        std::fs::remove_file(&content).unwrap();
        let error = InstalledPackage::verify_store(directory.path()).unwrap_err();
        assert!(error.contains("partial"), "{error}");
        let original_root = store.lookup(address()).unwrap();
        assert!(
            installed
                .package
                .as_ref()
                .clone()
                .publish(&store, &records_of(&installed.package))
                .is_err()
        );
        assert_eq!(store.lookup(address()), Some(original_root));
        std::fs::write(&content, kept).unwrap();
        InstalledPackage::verify_store(directory.path()).unwrap();
        // The header is small enough to share a pack: damage its own payload.
        let path = store.output_path(row.reference);
        let payload = store.get_owned(row.reference).unwrap();
        let mut damaged = std::fs::read(&path).unwrap();
        let at = damaged
            .windows(payload.len())
            .position(|bytes| bytes == &*payload)
            .unwrap();
        damaged[at + payload.len() - 1] ^= 1;
        std::fs::write(path, damaged).unwrap();
        // A new process, without this one's record of checked ranges.
        let fresh = hirsel_artifact_store::Store::new(directory.path().to_owned());
        assert!(
            InstalledPackage::open(&fresh, &package, compiler)
                .unwrap_err()
                .contains("corrupt")
        );
    }
    #[test]
    fn unused_corruption_is_deferred_until_demand_and_blocks_install_publication() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let path = store.output_path(reference);
        let mut damaged = std::fs::read(&path).unwrap();
        let payload = damaged
            .windows(b"product".len())
            .position(|bytes| bytes == b"product")
            .unwrap();
        damaged[payload] ^= 1;
        std::fs::write(&path, damaged).unwrap();
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        assert!(installed.records.content(reference).is_none());
        assert!(InstalledPackage::verify_store(directory.path()).is_err());
        let records = records_of(&installed.package);
        let old_root = store.lookup(address()).unwrap();
        assert!(
            installed
                .package
                .as_ref()
                .clone()
                .publish(&store, &records)
                .is_err()
        );
        assert_eq!(store.lookup(address()), Some(old_root));
    }
    /// An image with definitions `a.one` and `a.two` in `a.hir` and `b.three`
    /// in `b.hir`, written by a trusted test publisher: its descriptor for
    /// `a.two` names another compiler when `bad`. Construction would refuse
    /// that descriptor, so the publisher writes the sections directly.
    fn imaged(
        root: &Path,
        bad: bool,
    ) -> (
        hirsel_artifact_store::Store,
        ContentHash,
        PackageId,
        (Address, Association),
        InstalledPackage,
    ) {
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        let compiler = ContentHash::of_bytes(b"compiler");
        let package = PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let manifest = b"manifest".to_vec();
        let mut records = InstallationRecords::default();
        let mut definitions = BTreeMap::new();
        for (module, name, file) in [
            ("a", "one", "a.hir"),
            ("a", "two", "a.hir"),
            ("b", "three", "b.hir"),
        ] {
            let path = DefPath(hirsel_syntax::ItemPath {
                package: package.clone(),
                modules: vec![module.into()],
                name: name.into(),
            });
            let text = format!("pub fn {name}()->u8{{1u8}}");
            let put = |domain, part: &str| {
                store
                    .put(domain, format!("{name} {part}").as_bytes())
                    .unwrap()
            };
            let source = store.put(SEMANTIC_SOURCE, text.as_bytes()).unwrap();
            let ownership = InstalledDefinitionOwnership {
                header: put(SEMANTIC_HEADER, "header"),
                body: put(SEMANTIC_BODY, "body"),
                semantic: put(SEMANTIC_BODY, "semantic"),
                source,
                source_logical: format!("std@1/{file}").into(),
                source_digest: ContentHash::of_bytes(text.as_bytes()),
                source_bytes: source.len,
                source_base: 0,
            };
            let record = put(SEMANTIC_INPUT, "input");
            let mut key = InstalledDefinitionKey {
                compiler,
                config: ContentHash::default(),
                manifest: ContentHash::of_bytes(&manifest),
                definition: path.clone(),
                specialization: vec![],
                source: ownership.source_digest,
                implementation: ContentHash::default(),
                dependencies: ContentHash::default(),
            };
            records
                .rows
                .insert(key.address().0, (Action(record.output.0), record));
            for reference in [
                record,
                ownership.header,
                ownership.body,
                ownership.semantic,
                source,
            ] {
                records.contents.insert(reference.output.0, reference);
            }
            if bad && name == "two" {
                key.compiler = ContentHash::of_bytes(b"another compiler");
            }
            definitions.insert(
                path,
                InstalledDefinition {
                    key,
                    record,
                    ownership,
                },
            );
        }
        let source = |file: &str| InstalledSource {
            path: format!("/absent/{file}").into(),
            logical: format!("std@1/{file}").into(),
            context: FileContext {
                package: package.clone(),
                modules: vec![],
            },
            text: String::new(),
        };
        let mut installed = InstalledPackage::new(
            compiler,
            package.clone(),
            manifest,
            vec![source("a.hir"), source("b.hir")],
            vec![],
            InstalledInputs::default(),
        );
        installed.rows = records_of_rows(&records);
        installed.contents = records.contents.values().copied().collect();
        let checked = image::check(
            &image::Context {
                package: &installed.package,
                compiler,
                manifest: ContentHash::of_bytes(&installed.manifest),
                rows: &installed.rows,
                contents: &installed.contents,
            },
            &definitions,
        );
        assert_eq!(checked.is_err(), bad, "{checked:?}");
        let header = image::build(&store, &mut installed, &definitions).unwrap();
        let row = Association {
            action: Action(header.output.0),
            reference: header,
        };
        store.associate(address(), row.action, header).unwrap();
        (store, compiler, package, (address(), row), installed)
    }
    fn records_of_rows(records: &InstallationRecords) -> Arc<[(ContentHash, Association)]> {
        records
            .rows
            .iter()
            .map(|(address, (action, reference))| {
                (
                    *address,
                    Association {
                        action: *action,
                        reference: *reference,
                    },
                )
            })
            .collect()
    }
    /// Flip one byte of `reference`'s stored payload, wherever it is packed.
    fn damage(store: &hirsel_artifact_store::Store, reference: Reference) {
        let path = store.output_path(reference);
        let payload = store.get_owned(reference).unwrap();
        let mut damaged = std::fs::read(&path).unwrap();
        let at = damaged
            .windows(payload.len())
            .position(|bytes| bytes == &*payload)
            .unwrap();
        damaged[at] ^= 1;
        std::fs::write(path, damaged).unwrap();
    }
    fn item(package: &PackageId, module: &str, name: &str) -> DefPath {
        DefPath(hirsel_syntax::ItemPath {
            package: package.clone(),
            modules: vec![module.into()],
            name: name.into(),
        })
    }
    /// The header authenticates every section by reference. With the header
    /// association untouched, a changed byte in a definition chunk is refused
    /// when a definition in that chunk is first demanded, while the other
    /// chunk loads; a changed byte in a section open reads refuses the open.
    #[test]
    fn a_changed_section_byte_is_refused_at_its_first_use() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, header, installed) = imaged(directory.path(), false);
        let sections = installed.sections.clone().unwrap();
        damage(&store, sections.chunks[1]);
        // A new process, without this one's record of checked ranges.
        let fresh = || hirsel_artifact_store::Store::new(directory.path().to_owned());
        let opened =
            InstalledPackage::open_header(&fresh(), Some(header), &package, compiler).unwrap();
        // Membership reads the index only: catalog enumerations ask it of
        // every source path, and must neither decode nor meet a bad chunk.
        let sources = opened.package.source_records().unwrap();
        assert!(sources.installs(&item(&package, "b", "three")));
        assert!(!sources.installs(&item(&package, "b", "absent")));
        let one = opened
            .package
            .definition(&item(&package, "a", "one"))
            .unwrap()
            .unwrap();
        assert_eq!(one.key.definition, item(&package, "a", "one"));
        let refused = opened
            .package
            .definition(&item(&package, "b", "three"))
            .unwrap_err();
        assert!(
            refused.contains("corrupt installed definition chunk"),
            "{refused}"
        );
        assert!(
            opened
                .package
                .definition(&item(&package, "b", "absent"))
                .unwrap()
                .is_none()
        );
        damage(&store, sections.index);
        let refused =
            InstalledPackage::open_header(&fresh(), Some(header), &package, compiler).unwrap_err();
        assert!(
            refused.contains("corrupt installed package section"),
            "{refused}"
        );
    }
    /// An inconsistent descriptor nobody demands no longer refuses the open;
    /// the one that is demanded is refused on that first load, and the others,
    /// in its chunk and outside it, load.
    #[test]
    fn one_bad_descriptor_is_refused_at_its_first_load_and_the_others_load() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, header, _) = imaged(directory.path(), true);
        let opened =
            InstalledPackage::open_header(&store, Some(header), &package, compiler).unwrap();
        let refused = opened
            .package
            .definition(&item(&package, "a", "two"))
            .unwrap_err();
        assert!(
            refused.contains("invalid installed definition descriptor"),
            "{refused}"
        );
        for (module, name) in [("a", "one"), ("b", "three")] {
            let definition = opened
                .package
                .definition(&item(&package, module, name))
                .unwrap();
            assert_eq!(
                definition.unwrap().key.definition,
                item(&package, module, name)
            );
        }
    }
    /// The image of `header` with one section rewritten by `edit`, under a new
    /// header that the root now associates.
    fn rewritten(
        store: &hirsel_artifact_store::Store,
        header: (Address, Association),
        section: fn(&mut image::Sections) -> &mut Reference,
        edit: impl FnOnce(&mut [u8]),
    ) -> (Address, Association) {
        let reference = image::republish(store, header.1.reference, |sections| {
            let reference = section(sections);
            let mut bytes = store.get_owned(*reference).unwrap().to_vec();
            edit(&mut bytes);
            *reference = store.put(reference.domain, &bytes).unwrap();
        });
        let row = Association {
            action: Action(reference.output.0),
            reference,
        };
        store.associate(address(), row.action, reference).unwrap();
        (address(), row)
    }
    /// Lookups binary-search the rows, contents and fingerprint order, so an
    /// authenticated image that misorders one is refused at open rather than
    /// answering "absent" for what it lists. An index entry that points into
    /// another definition's bytes refuses its own path; the definition it
    /// points at still loads.
    #[test]
    fn a_misordered_or_misdirected_section_is_refused_never_reported_absent() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, header, _) = imaged(directory.path(), false);
        let open = |header| InstalledPackage::open_header(&store, Some(header), &package, compiler);
        let swap = |width: usize, at: usize| {
            move |bytes: &mut [u8]| {
                let (first, second) = bytes[at..at + 2 * width].split_at_mut(width);
                first.swap_with_slice(second);
            }
        };
        // Three definitions: three rows, and the order after three entries.
        for (section, edit) in [
            (
                (|s| &mut s.rows) as fn(&mut image::Sections) -> &mut Reference,
                swap(112, 8),
            ),
            (|s| &mut s.contents, swap(48, 8)),
            (|s| &mut s.index, swap(4, 8 + 3 * 44)),
        ] {
            let refused = open(rewritten(&store, header, section, edit)).unwrap_err();
            assert!(
                refused.contains("invalid installed package section"),
                "{refused}"
            );
        }
        // Entries are in path order: a.one, a.two, b.three. Point a.one's
        // chunk, offset and length at a.two's descriptor.
        let misdirected = rewritten(
            &store,
            header,
            |s| &mut s.index,
            |bytes| {
                bytes.copy_within(8 + 44 + 32..8 + 2 * 44, 8 + 32);
            },
        );
        let opened = open(misdirected).unwrap();
        let refused = opened
            .package
            .definition(&item(&package, "a", "one"))
            .unwrap_err();
        assert!(
            refused.contains("invalid installed definition index"),
            "{refused}"
        );
        let two = opened
            .package
            .definition(&item(&package, "a", "two"))
            .unwrap()
            .unwrap();
        assert_eq!(two.key.definition, item(&package, "a", "two"));
    }
    #[test]
    fn demanded_expanded_content_refuses_before_allocating_and_remains_usable() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        // Empty fields cost eight transport bytes each, but every decoded field
        // owns a Vec header. The independently granted transport-sized decode
        // reservation cannot hold those headers.
        let expected = vec![Vec::<u8>::new(); 8192];
        let encoded = bincode::serialize(&expected).unwrap();
        let reference = store
            .put(
                Domain {
                    kind: 79,
                    schema: 1,
                },
                &encoded,
            )
            .unwrap();
        let mut records = records_of(&installed.package);
        records.contents.insert(reference.output.0, reference);
        let package_image = installed.package.as_ref().clone();
        // The open operation's batch holds this store's writes pending; end
        // it so the content and the new image reach published storage.
        drop(installed);
        package_image.publish(&store, &records).unwrap();
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(2 << 20);
        let _transport = budget.reserve(encoded.len() as u64).unwrap();
        let small = budget.reserve(encoded.len() as u64).unwrap();
        let bytes = installed.records.content(reference).unwrap();
        let (requirement, scanned) = test_alloc::measured(|| {
            hirsel_product_codec::DecodeRequirement::<hirsel_product_codec::RunContent>::open(bytes)
                .unwrap()
        });
        assert_eq!(scanned.allocations, 0);
        let (refused, allocations) = test_alloc::measured(|| requirement.decode_content(&small));
        assert!(
            refused.is_none(),
            "valid transport bytes require decoded-container admission"
        );
        assert_eq!(
            allocations.allocations, 0,
            "demand refuses before allocating semantic containers"
        );
        assert!(allocations.peak <= small.bytes() as isize);
        let large = budget.reserve(1 << 20).unwrap();
        let requirement =
            hirsel_product_codec::DecodeRequirement::<hirsel_product_codec::RunContent>::open(
                installed.records.content(reference).unwrap(),
            )
            .unwrap();
        let (decoded, allocations) =
            test_alloc::measured(|| requirement.decode_content(&large).unwrap());
        assert_eq!(decoded, expected);
        assert!(allocations.peak <= large.bytes() as isize);
    }
    #[test]
    fn operation_pins_the_manifest_and_rows_across_atomic_publication() {
        let directory = tempfile::tempdir().unwrap();
        let (writer, compiler, package, first) = fixture(directory.path());
        let reader = DirStore::new(directory.path().join("cache"))
            .with_read_only_root(directory.path().to_owned());
        let package_owner = reader.installed_package(&package, compiler).unwrap();
        let previous = OpenedInstallation {
            records: package_owner
                .records
                .upgrade()
                .expect("the provider pins direct library reads too"),
            package: package_owner,
        };
        let row = Address(ContentHash::of_bytes(b"row"));
        let second = writer.put(first.domain, b"replacement").unwrap();
        let action = Action(ContentHash::of_bytes(b"new action"));
        writer.associate(row, action, second).unwrap();
        let records = InstallationRecords {
            rows: [(row.0, (action, second))].into(),
            contents: [(second.output.0, second)].into(),
        };
        previous
            .package
            .as_ref()
            .clone()
            .publish(&writer, &records)
            .unwrap();
        let next = InstalledPackage::open(&writer.for_operation(), &package, compiler).unwrap();
        assert_ne!(previous.package.identity, next.package.identity);
        assert_eq!(previous.records.lookup(row).unwrap().reference, first);
        assert_eq!(&*previous.records.content(first).unwrap(), b"product");
        assert_eq!(
            next.records.lookup(row).unwrap(),
            Association {
                action,
                reference: second
            }
        );
        assert_eq!(&*next.records.content(second).unwrap(), b"replacement");
        // A separately damaged index row cannot supply consumed metadata: the
        // header's row is the one returned, and the installation stays valid.
        let path = writer.association_path(row).unwrap();
        let mut damaged = std::fs::read(&path).unwrap();
        let offset = damaged
            .windows(action.0.0.len())
            .position(|bytes| bytes == action.0.0)
            .unwrap();
        damaged[offset] ^= 1;
        std::fs::write(path, damaged).unwrap();
        let reopened = InstalledPackage::open(&writer.for_operation(), &package, compiler).unwrap();
        assert_eq!(
            reopened.records.lookup(row),
            Some(Association {
                action,
                reference: second
            })
        );
        InstalledPackage::verify_store(directory.path()).unwrap();
    }
    #[test]
    fn installed_source_owner_keeps_published_references_without_query_cache_access() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let key = EntryKey(
            ContentHash::of_bytes(b"source"),
            ContentHash::default(),
            hirsel_query_products::QueryKind::Parse,
        );
        let first = store
            .put(super::super::entry_domain(key.2), b"original source")
            .unwrap();
        store
            .associate(Address(key.0), Action(first.output.0), first)
            .unwrap();
        let installed = (*InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package)
            .clone();
        let mut records = records_of(&installed);
        records.rows.insert(key.0, (Action(first.output.0), first));
        records.contents.insert(first.output.0, first);
        installed.publish(&store, &records).unwrap();
        let owner = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package
            .source_records()
            .unwrap();
        let changed = store
            .put(super::super::entry_domain(key.2), b"replacement source")
            .unwrap();
        store
            .associate(Address(key.0), Action(changed.output.0), changed)
            .unwrap();
        assert_eq!(&*owner.get(&key).unwrap(), b"original source");
        assert!(
            owner
                .get(&EntryKey(
                    key.0,
                    key.1,
                    hirsel_query_products::QueryKind::Modes
                ))
                .is_none()
        );
        // A later run rewrites the cache index at the same address; the
        // installation stays valid and keeps resolving its own row.
        InstalledPackage::verify_store(directory.path()).unwrap();
        let reopened = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package
            .source_records()
            .unwrap();
        assert_eq!(&*reopened.get(&key).unwrap(), b"original source");
    }
    #[test]
    fn duplicate_content_checks_preserve_full_reference_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package;
        for invalid in [
            Reference {
                len: reference.len + 1,
                ..reference
            },
            Reference {
                domain: Domain {
                    schema: reference.domain.schema + 1,
                    ..reference.domain
                },
                ..reference
            },
        ] {
            let mut changed = (*installed).clone();
            changed.contents = changed.contents.iter().copied().chain([invalid]).collect();
            assert!(
                changed
                    .check_records(&store)
                    .unwrap_err()
                    .contains("corrupt package product content")
            );
        }
    }
    #[test]
    fn operation_shares_installed_inputs_and_revalidates_at_the_next_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let provider = DirStore::new(directory.path().join("cache"))
            .with_read_only_root(directory.path().to_owned());
        let operation = provider.for_operation().unwrap();
        let first = operation.installed_package(&package, compiler).unwrap();
        let second = operation.installed_package(&package, compiler).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(
            operation
                .installed_package(&package, ContentHash::default())
                .unwrap_err()
                .contains("compiler identity mismatch")
        );
        let held = operation.get_content(reference).unwrap();
        assert_eq!(&*held, b"product");
        let path = store.output_path(reference);
        let original = std::fs::read(&path).unwrap();
        let mut damaged = original.clone();
        *damaged.last_mut().unwrap() ^= 1;
        let replacement = directory.path().join("replacement");
        std::fs::write(&path, damaged).unwrap();
        assert_eq!(&*held, b"product");
        assert!(operation.get_content(reference).is_none());
        let next = provider.for_operation().unwrap();
        next.installed_package(&package, compiler).unwrap();
        assert!(next.get_content(reference).is_none());
        assert!(
            InstalledPackage::verify_store(directory.path())
                .unwrap_err()
                .contains("corrupt")
        );
        std::fs::write(&replacement, original).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        std::fs::remove_file(
            store
                .association_path(Address(ContentHash::of_bytes(b"row")))
                .unwrap(),
        )
        .unwrap();
        let next = provider.for_operation().unwrap();
        next.installed_package(&package, compiler).unwrap();
        assert!(next.get_content(reference).is_some());
        // Restored content revalidates; the index row is not part of it.
        InstalledPackage::verify_store(directory.path()).unwrap();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledSource {
    pub path: PathBuf,
    pub logical: PathBuf,
    pub context: FileContext,
    /// Text is the validation input of the Parse products and their anchors.
    pub text: String,
}

/// The complete loader footprint of the Precompute that produced an
/// installation: every observed operation, as (kind, path), and the producing
/// compiler's key over those observations and every setting that changes its
/// products. A later Precompute replays the operations and compares the key.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledInputs {
    pub operations: Vec<(u8, PathBuf)>,
    pub key: ContentHash,
}

#[derive(Debug)]
/// One manifest generation and immutable, verified snapshots of demanded records.
/// The header alone defines the installation: a row resolves to its listed
/// content, checked on read, never through the root's mutable cache index.
/// A later Precompute into the same root rewrites that index freely; this
/// installation stays valid until the next header replaces it.
pub(crate) struct InstalledRecords {
    pub(crate) root: PathBuf,
    store: hirsel_artifact_store::Store,
    // Keep the physical index generation pinned until this operation releases it.
    _scope: hirsel_artifact_store::Batch,
    rows: Arc<[(ContentHash, Association)]>,
    contents: Arc<[Reference]>,
}
impl InstalledRecords {
    pub(crate) fn lookup(&self, address: Address) -> Option<Association> {
        let index = self
            .rows
            .binary_search_by_key(&address.0, |(key, _)| *key)
            .ok()?;
        Some(self.rows[index].1)
    }
    pub(crate) fn content(&self, reference: Reference) -> Option<Bytes> {
        self.contents.binary_search(&reference).ok()?;
        self.store.get_owned(reference).ok()
    }
    pub fn metadata_bytes(&self) -> usize {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + 2 * std::mem::size_of::<usize>()
                + self.root.as_os_str().len()
                + 2 * self.store.root().as_os_str().len(),
        );
        // The rows and contents are the package's, charged with it.
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
/// Immutable parse references survive their operation without retaining its
/// complete integrity closure. Reads after release validate the pinned reference.
#[derive(Debug)]
pub struct InstalledSourceRecords {
    pub installation: ContentHash,
    store: hirsel_artifact_store::Store,
    rows: BTreeMap<ContentHash, Reference>,
    operation: std::sync::Weak<InstalledRecords>,
    definitions: Option<Arc<image::Definitions>>,
}
impl InstalledSourceRecords {
    pub fn get(&self, key: &EntryKey) -> Option<Bytes> {
        if key.2 != hirsel_query_products::QueryKind::Parse {
            return None;
        }
        let reference = *self.rows.get(&key.0)?;
        match self.operation.upgrade() {
            Some(operation) => operation.content(reference),
            None => self.store.get_owned(reference).ok(),
        }
    }
    pub fn metadata_bytes(&self) -> usize {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + std::mem::size_of::<InstalledRecords>()
                + 4 * std::mem::size_of::<usize>()
                + self.store.root().as_os_str().len(),
        );
        weight.tree::<ContentHash, Reference>(self.rows.len());
        // The definitions are the package's, charged with it.
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub compiler: ContentHash,
    /// Identity of the verified installation header, including all actions and references.
    pub identity: ContentHash,
    pub package: PackageId,
    pub manifest: Vec<u8>,
    pub sources: Vec<InstalledSource>,
    pub dependencies: Vec<PackageId>,
    pub inputs: InstalledInputs,
    /// The image's sections, once published or opened.
    sections: Option<image::Sections>,
    /// Physical action rows, sorted by address, and every content they and
    /// the definitions require, sorted.
    rows: Arc<[(ContentHash, Association)]>,
    contents: Arc<[Reference]>,
    definitions: Option<Arc<image::Definitions>>,
    records: std::sync::Weak<InstalledRecords>,
    sources_owner: Option<Arc<InstalledSourceRecords>>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenedInstallation {
    pub(crate) package: Arc<InstalledPackage>,
    pub(crate) records: Arc<InstalledRecords>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallationRecords {
    pub rows: BTreeMap<ContentHash, (Action, Reference)>,
    pub contents: BTreeMap<ContentHash, Reference>,
}

impl InstalledPackage {
    pub fn source_records(&self) -> Option<Arc<InstalledSourceRecords>> {
        self.sources_owner.clone()
    }
    pub fn operation_metadata_bytes(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>());
        weight.add(self.package.name.capacity() + self.package.version.capacity());
        weight.add(self.manifest.capacity());
        weight.add(std::mem::size_of_val(&*self.rows));
        weight.add(std::mem::size_of_val(&*self.contents));
        if let Some(sections) = &self.sections {
            weight.add(sections.references().count() * std::mem::size_of::<Reference>());
        }
        // Reserved up front for every chunk the request may come to demand.
        if let Some(definitions) = &self.definitions {
            weight.add(definitions.bytes());
        }
        weight.add(self.sources.capacity() * std::mem::size_of::<InstalledSource>());
        for source in &self.sources {
            weight.add(source.path.as_os_str().len() + source.logical.as_os_str().len());
            weight.add(source.text.capacity());
            weight.add(
                source.context.package.name.capacity() + source.context.package.version.capacity(),
            );
            weight.add(source.context.modules.capacity() * std::mem::size_of::<String>());
            for module in &source.context.modules {
                weight.add(module.capacity());
            }
        }
        if let Some(records) = self.records.upgrade() {
            weight.add(records.metadata_bytes());
        }
        if let Some(sources) = &self.sources_owner {
            weight.add(sources.metadata_bytes());
        }
        weight.bytes()
    }
    pub fn new(
        compiler: ContentHash,
        package: PackageId,
        manifest: Vec<u8>,
        sources: Vec<InstalledSource>,
        dependencies: Vec<PackageId>,
        inputs: InstalledInputs,
    ) -> Self {
        Self {
            compiler,
            identity: ContentHash::default(),
            package,
            manifest,
            sources,
            dependencies,
            inputs,
            sections: None,
            rows: Arc::new([]),
            contents: Arc::new([]),
            definitions: None,
            records: Default::default(),
            sources_owner: None,
        }
    }
    pub(crate) fn publish(
        mut self,
        store: &hirsel_artifact_store::Store,
        records: &InstallationRecords,
    ) -> std::io::Result<(Address, hirsel_artifact_store::Association)> {
        // Without a retention scope the store reports a skipped index
        // publication as success; with one, any write this publication makes
        // that the index does not admit fails it.
        let _retention = store.retention_scope()?;
        let mut records = records.clone();
        let definitions = self.index_definitions(store, &mut records)?;
        self.rows = records
            .rows
            .iter()
            .map(|(address, (action, reference))| {
                (
                    *address,
                    Association {
                        action: *action,
                        reference: *reference,
                    },
                )
            })
            .collect();
        self.contents = records
            .contents
            .values()
            .copied()
            .chain(self.rows.iter().map(|(_, row)| row.reference))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if self.rows.is_empty() || self.contents.is_empty() {
            return Err(std::io::Error::other(
                "package precomputation published no products",
            ));
        }
        image::check(
            &image::Context {
                package: &self.package,
                compiler: self.compiler,
                manifest: ContentHash::of_bytes(&self.manifest),
                rows: &self.rows,
                contents: &self.contents,
            },
            &definitions,
        )
        .map_err(std::io::Error::other)?;
        let reference = image::build(store, &mut self, &definitions)?;
        // Associate last, after reading back the header and everything it
        // lists through published storage: a fresh store holds none of this
        // one's pending bytes. The header's one association is the commit:
        // until it lands, the previous header and every content and section
        // it lists stay intact, since content is immutable.
        hirsel_artifact_store::Store::new(store.root().to_owned())
            .verify_records(
                std::iter::empty(),
                self.contents
                    .iter()
                    .copied()
                    .chain(self.rows.iter().map(|(_, row)| row.reference))
                    .chain(self.sections.iter().flat_map(image::Sections::references))
                    .chain([reference]),
            )
            .map_err(|error| {
                std::io::Error::other(format!("incomplete package publication: {error}"))
            })?;
        let action = Action(reference.output.0);
        store.associate(address(), action, reference)?;
        Ok((
            address(),
            hirsel_artifact_store::Association { action, reference },
        ))
    }
    fn check_records(
        &self,
        store: &hirsel_artifact_store::Store,
    ) -> Result<hirsel_artifact_store::VerifiedRecords, String> {
        // Rows, contents and sections are the header's; each must reach
        // present, intact content. Open already checked the sections it reads,
        // and publication wrote them itself.
        store
            .verify_records(
                std::iter::empty(),
                self.contents
                    .iter()
                    .copied()
                    .chain(self.rows.iter().map(|(_, row)| row.reference))
                    .chain(self.sections.iter().flat_map(image::Sections::unread)),
            )
            .map_err(|error| format!("partial or corrupt package product content or row: {error}"))
    }
    /// The producing inputs of the installation published at `root`, only when
    /// its header opens checked under `compiler` and every recorded row and
    /// content is present and intact. This reads; it never writes the root.
    pub fn current_inputs(root: &Path, compiler: ContentHash) -> Result<InstalledInputs, String> {
        if !root.join("index/MANIFEST").is_file() {
            return Err("no installed package".into());
        }
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        let _scope = store.batch();
        let installed = Self::read(&store)?;
        if installed.compiler != compiler {
            return Err("stale installed package: compiler identity mismatch".into());
        }
        installed.check_records(&store)?;
        Ok(installed.inputs)
    }
    /// Audit the complete published installation, including unused products.
    pub fn verify_store(root: &Path) -> Result<(), String> {
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        Self::audit(&store).map(|_| ())
    }
    pub(crate) fn audit(
        store: &hirsel_artifact_store::Store,
    ) -> Result<hirsel_artifact_store::VerifiedRecords, String> {
        let _scope = store.batch();
        let installed = Self::read(store)?;
        installed.check_records(store)
    }
    fn read(store: &hirsel_artifact_store::Store) -> Result<Self, String> {
        Self::read_header(store, None)
    }
    /// Read the installation `header` names, or without one the root's
    /// current header. A header a registration authenticated is opened as
    /// is, never reselected from the root.
    fn read_header(
        store: &hirsel_artifact_store::Store,
        header: Option<(Address, hirsel_artifact_store::Association)>,
    ) -> Result<Self, String> {
        let (at, row) = header
            .or_else(|| Some((address(), store.lookup_owned(address())?)))
            .ok_or("missing installed package product")?;
        if at != address() {
            return Err("invalid installed package header address".into());
        }
        if row.reference.domain != PACKAGE || row.action != Action(row.reference.output.0) {
            return Err("invalid installed package domain or action".into());
        }
        let bytes = store
            .get_owned(row.reference)
            .ok()
            .ok_or("corrupt installed package product")?;
        let mut installed = image::read(store, &bytes)?;
        installed.identity = row.reference.output.0;
        if installed.sources.is_empty()
            || installed.rows.is_empty()
            || installed.contents.is_empty()
            || installed
                .sources
                .iter()
                .any(|source| source.context.package != installed.package)
        {
            return Err("incomplete installed package interface".into());
        }
        // Std currently has no package dependencies. Refuse a new interface
        // until its dependency closure can be attached explicitly.
        if !installed.dependencies.is_empty() {
            return Err("installed package dependency products are unavailable".into());
        }
        Ok(installed)
    }
    #[cfg(test)]
    pub(crate) fn open(
        store: &hirsel_artifact_store::Store,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<OpenedInstallation, String> {
        Self::open_header(store, None, package, compiler)
    }
    /// Open the installation `header` names; see `read_header`.
    pub(crate) fn open_header(
        store: &hirsel_artifact_store::Store,
        header: Option<(Address, hirsel_artifact_store::Association)>,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<OpenedInstallation, String> {
        hirsel_intern::timings::count(hirsel_intern::timings::Count::InstalledPackageOpens);
        let read = || -> Result<OpenedInstallation, String> {
            let scope = store.batch();
            let installed = Self::read_header(store, header)?;
            if installed.compiler != compiler {
                return Err("stale installed package: compiler identity mismatch".into());
            }
            if installed.package != *package {
                return Err("installed package identity mismatch".into());
            }
            Self::attach(store, scope, installed)
        };
        read().map_err(|error| format!("installed products {}: {error}", store.root().display()))
    }
    /// The records of whatever installation `store`'s header names, for an
    /// ordinary read-only reader that has not selected a package. Its rows
    /// resolve only through this header, never through the root's index.
    pub(crate) fn open_records(
        store: &hirsel_artifact_store::Store,
        header: Option<(Address, hirsel_artifact_store::Association)>,
    ) -> Result<Arc<InstalledRecords>, String> {
        let scope = store.batch();
        Ok(Self::attach(store, scope, Self::read_header(store, header)?)?.records)
    }
    fn attach(
        store: &hirsel_artifact_store::Store,
        scope: hirsel_artifact_store::Batch,
        mut installed: Self,
    ) -> Result<OpenedInstallation, String> {
        let records = Arc::new(InstalledRecords {
            root: store.root().to_owned(),
            store: store.clone(),
            _scope: scope,
            rows: installed.rows.clone(),
            contents: installed.contents.clone(),
        });
        installed.sources_owner = Some(Arc::new(InstalledSourceRecords {
            installation: installed.identity,
            store: store.clone(),
            rows: installed
                .rows
                .iter()
                .filter(|(_, row)| {
                    row.reference.domain
                        == super::entry_domain(hirsel_query_products::QueryKind::Parse)
                })
                .map(|(address, row)| (*address, row.reference))
                .collect(),
            operation: Arc::downgrade(&records),
            definitions: installed.definitions.clone(),
        }));
        installed.records = Arc::downgrade(&records);
        Ok(OpenedInstallation {
            package: Arc::new(installed),
            records,
        })
    }
}
