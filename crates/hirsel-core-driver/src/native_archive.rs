//! Native static libraries use the same checked units as executables.
use crate::NativeStop;
use hirsel_core_ids::{ContentHash, launch};
use hirsel_core_ir::{FuncId, Identity, Module};

pub(super) fn describe(
    units: Vec<(&Identity, std::borrow::Cow<'_, Module>, FuncId)>,
    entry: &Identity,
    launch: &hirsel_core_ir::release_recipe::Launch,
    revisions: impl IntoIterator<Item = (ContentHash, ContentHash)>,
) -> Result<Vec<u8>, NativeStop> {
    use hirsel_core_eval::{Nans, Outcome};
    use hirsel_core_layout::{Layout, case_of, compute};

    let error =
        |reason: &dyn std::fmt::Display| NativeStop::Entry(format!("evaluator Describe: {reason}"));
    let (module, function) =
        hirsel_core_materialize::join_units(units, entry).map_err(|e| error(&e))?;
    let signature = &module.signatures[module.functions[function.index()].signature.index()];
    let result = signature.dest.ok_or_else(|| error(&"main has no Result"))?;
    let layouts = compute(&module.types).map_err(|e| error(&format!("{e:?}")))?;
    let Layout::Fixed(layout) = &layouts[result.index()].layout else {
        return Err(error(&"main has no fixed Result layout"));
    };
    let mut destination = vec![0; layout.size as usize];
    let args = (0..signature.params.len() as u64).collect::<Vec<_>>();
    let revisions: hirsel_core_eval::Revisions = revisions.into_iter().collect();
    let entry_revision = revisions.get(&entry.digest()).map(|revision| revision.0);
    let metadata = launch::Metadata::source(&launch.target, launch.n_place, true)
        .with_launch_identity(entry_revision, true);
    let report = hirsel_core_eval::run_entry(
        &module,
        function,
        &args,
        launch.root_limit,
        Nans::Canonical,
        Some(&mut destination),
        1_000_000_000,
        &revisions,
        false,
        &[],
        Some(&metadata),
        None,
    )
    .0;
    if !matches!(report.outcome, Outcome::Returned(_)) {
        return Err(error(&format_args!("{:?}", report.outcome)));
    }
    let success = layout.tag.is_none_or(|tag| {
        let mut word = [0; 8];
        let offset = tag.offset as usize;
        let size = tag.width as usize;
        word[..size].copy_from_slice(&destination[offset..offset + size]);
        case_of(&tag, u64::from_le_bytes(word)) == 0
    });
    if !success {
        return match crate::sdk::place_error(&report.stdout) {
            Ok(problem) => Err(error(&problem)),
            Err(_) => Err(error(&"entry returned an error")),
        };
    }
    let result = crate::sdk::Describe::read(&report.stdout).map_err(|e| error(&e))?;
    let manifest = crate::sdk::manifest_json(&result).map_err(|e| error(&e))?;
    hirsel_core_ids::embed::binding_projection(manifest.as_bytes()).map_err(|e| error(&e))?;
    Ok(manifest.into_bytes())
}

use crate::native::{Link, NativeOptions, command};
use crate::tool_inputs::LinkRecipe;
use hirsel_core_ids::embed::{EXPORTS_V1, HEADER_V1};
use hirsel_core_layout::{Arch, ObjectFormat, Target};
use object::read::archive::ArchiveFile;
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget, SymbolIndex};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::tool_inputs::tool_dir;

fn tool_identity(link: &Link) -> io::Result<[ContentHash; 2]> {
    let dir = tool_dir(link)?;
    let mut hashes = [ContentHash([0; 32]); 2];
    for (key, tool) in hashes.iter_mut().zip(["llvm-ar", "llvm-objcopy"]) {
        *key = crate::tool_inputs::tool_digest(&dir.join(tool))?;
    }
    Ok(hashes)
}

pub(super) fn toolchain_image(
    options: &NativeOptions,
    image: ContentHash,
) -> io::Result<ContentHash> {
    if options.output_kind == hirsel_core_layout::OutputKind::Executable {
        Ok(image)
    } else {
        super::key(
            "hirsel.archive-toolchain.v3",
            &(image, tool_identity(&options.link)?),
        )
    }
}

fn build_error(error: impl std::fmt::Display) -> NativeStop {
    NativeStop::Link(format!("static library: {error}"))
}

struct Member<'a> {
    name: String,
    bytes: &'a [u8],
    runtime: bool,
}

fn members<'a>(label: &str, bytes: &'a [u8], runtime: bool) -> Result<Vec<Member<'a>>, NativeStop> {
    ArchiveFile::parse(bytes)
        .map_err(build_error)?
        .members()
        .map(|member| {
            let member = member.map_err(build_error)?;
            Ok(Member {
                name: format!("{label}({})", String::from_utf8_lossy(member.name())),
                bytes: member.data(bytes).map_err(build_error)?,
                runtime,
            })
        })
        .collect()
}

type SymbolSet = BTreeSet<Vec<u8>>;
type Imports<'a> = BTreeMap<Vec<u8>, &'a str>;

fn object_symbols<'a>(
    member: &Member<'a>,
) -> Result<(object::File<'a>, std::collections::HashSet<SymbolIndex>), NativeStop> {
    let object = object::File::parse(member.bytes)
        .map_err(|e| build_error(format_args!("{}: {e}", member.name)))?;
    let referenced = object
        .sections()
        .flat_map(|section| section.relocations())
        .flat_map(|(_, relocation)| {
            let target = match relocation.target() {
                RelocationTarget::Symbol(symbol) => Some(symbol),
                _ => None,
            };
            target.into_iter().chain(relocation.subtractor())
        })
        .collect();
    Ok((object, referenced))
}

fn symbols<'a>(members: &'a [Member<'_>]) -> Result<(SymbolSet, Imports<'a>), NativeStop> {
    let mut definitions = BTreeSet::new();
    let mut imports = BTreeMap::new();
    for member in members {
        let (object, referenced) = object_symbols(member)?;
        for symbol in object.symbols().filter(ObjectSymbol::is_global) {
            let name = symbol.name_bytes().map_err(build_error)?;
            if name.is_empty() {
                continue;
            }
            if symbol.is_undefined() {
                if referenced.contains(&symbol.index()) && (!member.runtime || !symbol.is_weak()) {
                    imports.entry(name.to_vec()).or_insert(member.name.as_str());
                }
            } else {
                definitions.insert(name.to_vec());
            }
        }
    }
    Ok((definitions, imports))
}

/// Use the same rename map for a definition and all its references. Pass
/// objcopy only the rows this member uses. Native emitters can declare names
/// without referring to them. Remove those unused undefined symbol entries.
fn rewrite(
    member: &Member<'_>,
    definitions: &SymbolSet,
    names: &Names<'_>,
    tool: &Path,
    work: &Path,
    output: &str,
) -> Result<(), NativeStop> {
    let (object, referenced) = object_symbols(member)?;
    let mut rename = Vec::new();
    let mut used = BTreeSet::new();
    let mut unused = BTreeSet::new();
    for symbol in object.symbols().filter(ObjectSymbol::is_global) {
        let name = symbol.name_bytes().map_err(build_error)?;
        if name.is_empty() {
            continue;
        }
        if symbol.is_undefined() && !referenced.contains(&symbol.index()) {
            unused.insert(name);
        } else {
            used.insert(name);
        }
    }
    for &name in &used {
        if definitions.contains(name) {
            rename.extend_from_slice(name);
            rename.push(b' ');
            rename.extend_from_slice(&names.renamed(name));
            rename.push(b'\n');
        }
    }
    let mut strip = Vec::new();
    for &name in unused.difference(&used) {
        strip.extend_from_slice(name);
        strip.push(b'\n');
    }
    fs::write(work.join("symbols"), rename)?;
    fs::write(work.join("unused"), strip)?;
    fs::write(work.join(output), member.bytes)?;
    command(
        crate::tool_inputs::tool_command(tool)
            .current_dir(work)
            .args(["--redefine-syms=symbols", "--strip-symbols=unused", output]),
    )?;
    Ok(())
}

fn audit(
    definitions: &BTreeSet<Vec<u8>>,
    imports: BTreeMap<Vec<u8>, &str>,
    recipe: &LinkRecipe,
    link: &Link,
) -> Result<(), NativeStop> {
    let mut allowed = BTreeSet::new();
    let installed_kit = recipe
        .linker
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("link/kit"));
    for file in &recipe.files {
        if file.path == recipe.linker
            || (matches!(link, Link::Installed(_))
                && !installed_kit
                    .as_ref()
                    .is_some_and(|kit| file.path.starts_with(kit)))
        {
            continue;
        }
        let bytes = fs::read(&file.path)?;
        // The resolved platform kit includes CRT definitions such as
        // __dso_handle and archives that supply the unwinder.
        if file
            .path
            .extension()
            .is_some_and(|ext| ext == "tbd" || ext == "a" || ext == "o")
            || object::File::parse(bytes.as_slice())
                .is_ok_and(|object| object.kind() == object::ObjectKind::Dynamic)
        {
            crate::foreign_archive::input_definitions(&bytes, &mut |name| {
                allowed.insert(name.to_vec());
                Ok(())
            })
            .ok_or_else(|| {
                build_error(format_args!(
                    "invalid link kit stub {}",
                    file.path.display()
                ))
            })??;
        }
    }
    for (name, member) in imports {
        if !definitions.contains(&name) && !allowed.contains(&name) {
            return Err(build_error(format_args!(
                "UnlistedImport({}, {member})",
                String::from_utf8_lossy(&name)
            )));
        }
    }
    Ok(())
}

struct Names<'a> {
    target: Target,
    build: &'a str,
}

impl Names<'_> {
    fn source(&self, name: &str) -> String {
        format!("{}{name}", self.target.symbol_prefix())
    }

    fn renamed(&self, symbol: &[u8]) -> Vec<u8> {
        let prefix = self.target.symbol_prefix().as_bytes();
        let mut name = self.source(&format!("hirsel_{}_", self.build)).into_bytes();
        name.extend_from_slice(symbol.strip_prefix(prefix).unwrap_or(symbol));
        name
    }

    fn anchor(&self) -> String {
        format!("hirsel_embed_v1_anchor_{}", self.build)
    }
}

fn export_member(names: &Names<'_>) -> Result<Vec<u8>, NativeStop> {
    use object::write::{Mangling, Object, Relocation, StandardSection, Symbol, SymbolSection};
    use object::{BinaryFormat, Endianness, RelocationFlags, SymbolFlags, SymbolKind, SymbolScope};
    let format = match names.target.object_format() {
        ObjectFormat::Elf => BinaryFormat::Elf,
        ObjectFormat::MachO => BinaryFormat::MachO,
    };
    let arch = match names.target.arch() {
        Arch::X86_64 => object::Architecture::X86_64,
        Arch::Aarch64 => object::Architecture::Aarch64,
    };
    let mut object = Object::new(format, arch, Endianness::Little);
    // Names already carry the target's exact object symbol prefix.
    object.set_mangling(Mangling::None);
    if let Some(apple) = names.target.apple() {
        let mut version = object::write::MachOBuildVersion::default();
        version.platform = object::macho::Platform(apple.macho_platform);
        version.minos = object::macho::Version(apple.macho_minos);
        version.sdk = version.minos;
        object.set_macho_build_version(version);
    }
    let constant = object.section_id(StandardSection::ReadOnlyData);
    let anchor = object.append_section_data(constant, &[0], 1);
    object.add_symbol(Symbol {
        name: names.source(&names.anchor()).into_bytes(),
        value: anchor,
        size: 1,
        kind: SymbolKind::Data,
        scope: SymbolScope::Dynamic,
        weak: false,
        section: SymbolSection::Section(constant),
        flags: SymbolFlags::None,
    });
    let text = object.section_id(StandardSection::Text);
    for &name in EXPORTS_V1 {
        let entry = match name {
            "hirsel_embed_v1_start" => "hirsel_archive_start",
            "hirsel_embed_v1_manifest" => "hirsel_archive_manifest",
            _ => name,
        };
        let destination = object.add_symbol(Symbol {
            name: names.renamed(names.source(entry).as_bytes()),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Unknown,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        });
        // A tail branch preserves the platform C ABI, including arguments
        // passed by address and multi-register return values.
        let (branch, alignment, delta, addend, flags): (&[u8], _, _, _, _) = match names.target {
            Target::X86_64Linux => (
                &[0xe9, 0, 0, 0, 0],
                1,
                1,
                -4,
                RelocationFlags::Elf {
                    r_type: object::elf::R_X86_64_PLT32,
                },
            ),
            Target::Aarch64Linux => (
                &[0, 0, 0, 0x14],
                4,
                0,
                0,
                RelocationFlags::Elf {
                    r_type: object::elf::R_AARCH64_JUMP26,
                },
            ),
            Target::Aarch64Darwin | Target::Aarch64Ios | Target::Aarch64IosSim => (
                &[0, 0, 0, 0x14],
                4,
                0,
                0,
                RelocationFlags::MachO {
                    r_type: object::macho::ARM64_RELOC_BRANCH26,
                    r_pcrel: true,
                    r_length: 2,
                },
            ),
        };
        let offset = object.append_section_data(text, branch, alignment);
        object.add_symbol(Symbol {
            name: names.source(name).into_bytes(),
            value: offset,
            size: branch.len() as u64,
            kind: SymbolKind::Text,
            scope: SymbolScope::Dynamic,
            weak: false,
            section: SymbolSection::Section(text),
            flags: SymbolFlags::None,
        });
        object
            .add_relocation(
                text,
                Relocation {
                    offset: offset + delta,
                    symbol: destination,
                    addend,
                    flags,
                },
            )
            .map_err(build_error)?;
    }
    object.write().map_err(build_error)
}

pub(super) fn archive(
    options: &NativeOptions,
    recipe: &LinkRecipe,
    work: &Path,
    build: &str,
    entry: &[u8],
    objects: &[Arc<[u8]>],
    foreign: &crate::foreign_archive::LinkArchives,
) -> Result<Vec<u8>, NativeStop> {
    let target = Target::from_triple(&options.target)
        .ok_or_else(|| build_error(format_args!("no archive target {}", options.target)))?;
    let names = Names { target, build };
    let runtime = crate::installation::read_runtime_archive(&options.runtime)?;
    let archives = foreign.read_archives()?;
    let mut inputs = vec![Member {
        name: "entry.o".into(),
        bytes: entry,
        runtime: false,
    }];
    inputs.extend(objects.iter().enumerate().map(|(i, bytes)| Member {
        name: format!("program-{i}.o"),
        bytes,
        runtime: false,
    }));
    for (label, bytes) in &archives {
        inputs.extend(members(label, bytes, false)?);
    }
    inputs.extend(members("runtime", &runtime, true)?);
    let (defined, imports) = symbols(&inputs)?;
    for &export in EXPORTS_V1 {
        if !defined.contains(names.source(export).as_bytes()) {
            return Err(build_error(format_args!(
                "embed runtime is missing {export}"
            )));
        }
    }
    audit(&defined, imports, recipe, &options.link)?;
    if !archives.is_empty() {
        let mut symbols = foreign.definitions(&hirsel_core_ir::decode::Debit::unlimited())?;
        symbols.check_tool_inputs(&options.runtime, recipe)?;
        for object in std::iter::once(entry).chain(objects.iter().map(AsRef::as_ref)) {
            symbols.check_link_input(object, "the generated code")?;
        }
        symbols.check_imports(&recipe.target)?;
    }

    fs::write(work.join("exports.o"), export_member(&names)?)?;
    let dir = tool_dir(&options.link)?;
    let mut list = String::from("exports.o\n");
    for (i, member) in inputs.iter().enumerate() {
        use std::fmt::Write;
        let name = format!("{i}.o");
        rewrite(
            member,
            &defined,
            &names,
            &dir.join("llvm-objcopy"),
            work,
            &name,
        )?;
        writeln!(list, "{name}").expect("writing to String");
    }
    fs::write(work.join("members.rsp"), list)?;
    let format = match target.object_format() {
        ObjectFormat::Elf => "--format=gnu",
        ObjectFormat::MachO => "--format=darwin",
    };
    command(
        crate::tool_inputs::tool_command(dir.join("llvm-ar"))
            .current_dir(work)
            .args(["rcsD", format, "product.a", "@members.rsp"]),
    )?;
    Ok(fs::read(work.join("product.a"))?)
}

pub(super) fn deliver(path: &Path, build: &str, archive: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new(""));
    fs::create_dir_all(parent)?;
    let anchor = format!("hirsel_embed_v1_anchor_{build}");
    let header = format!(
        "#ifndef HIRSEL_PRODUCT_{build}_H\n#define HIRSEL_PRODUCT_{build}_H\n\n\
         #include \"hirsel_embed.h\"\n\n\
         #ifdef __cplusplus\nextern \"C\" {{\n#endif\n\
         extern const uint8_t {anchor};\n\
         #ifdef __cplusplus\n}}\n#endif\n\n\
         static inline void hirsel_embed_v1_require_anchor_{build}(void) {{\n    \
         (void)*(const volatile uint8_t *)&{anchor};\n}}\n\n#endif\n"
    );
    crate::executable::write_from(
        &parent.join("hirsel_embed.h"),
        HEADER_V1.as_bytes(),
        None,
        0o644,
    )?;
    crate::executable::write_from(
        &parent.join("hirsel_product.h"),
        header.as_bytes(),
        None,
        0o644,
    )?;
    crate::executable::write_from(path, archive, None, 0o644)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pack(
    entry: &crate::native_entry::Plan<'_>,
    objects: &[super::ObjectProduct],
    foreign: &crate::foreign_archive::LinkArchives,
    options: &NativeOptions,
    sources_digest: ContentHash,
    sources: impl FnOnce() -> io::Result<(Arc<hirsel_core_ir::source::SourceMap>, Option<ContentHash>)>,
    build: ContentHash,
    key: ContentHash,
    store: &crate::Store,
    recipe: &LinkRecipe,
    mut counters: crate::reach::ReachCounters,
    address: ContentHash,
    revisions: impl FnOnce() -> Result<BTreeMap<Identity, ContentHash>, NativeStop>,
    assembly: Option<(ContentHash, ContentHash)>,
) -> Result<super::Linked, NativeStop> {
    use super::{BuildProduct, Linked};
    let build_text = build.to_string();
    let prefix = &build_text[..16];
    let revision = hirsel_core_ir::code_revision(address);
    let compiled = objects.iter().filter(|o| o.compiled).count();
    let (bytes, linked) = match store.get(key)? {
        Some(bytes) => (bytes, false),
        None => {
            let references = objects.iter().collect::<Vec<_>>();
            let members = match super::link_input::object_members(&references, &mut counters)? {
                Ok(members) => members,
                Err(refused) => return Ok(Linked::Damaged(refused, compiled, counters)),
            };
            let (sources, sources_key) = sources()?;
            let target = Target::from_triple(&options.target)
                .ok_or_else(|| build_error(format_args!("no archive target {}", options.target)))?;
            let members = super::link_input::Members::new(members, target.object_format());
            let keys: Vec<_> = objects.iter().map(|o| (o.identity, o.check)).collect();
            let (runtime_sources, spawns_units) =
                crate::native_entry::runtime_sources(&sources, sources_key, &members, &keys)?;
            let revisions: Vec<_> = revisions()?
                .into_iter()
                .map(|(id, hash)| (hirsel_core_ir::revision_symbol(id.digest()), hash.0))
                .collect();
            let entry = entry
                .emit(
                    &options.target,
                    &runtime_sources,
                    sources_digest,
                    &revision.0,
                    &build.0,
                    &revisions,
                    (None, spawns_units, (&members, &keys)),
                )
                .map_err(build_error)?;
            let temp = store.scratch()?;
            let bytes = archive(
                options,
                recipe,
                temp.path(),
                prefix,
                &entry,
                &members,
                foreign,
            )?;
            store.put_keyed(key, &bytes)?;
            (bytes, true)
        }
    };
    let product = BuildProduct {
        private_variants: objects.iter().map(|o| o.private_variants).sum(),
        executable: options.output.clone(),
        identity: key,
        address,
        recipe: assembly.map(|(root, _)| root),
        assembly: None,
        revision,
        compiled,
        linked,
        observation_mode: false,
        debug_lines: options.debug_lines,
        reach: counters,
        coverage: None,
    };
    match deliver(&options.output, prefix, &bytes) {
        Ok(()) => Ok(Linked::Built(product)),
        Err(error) => Err(NativeStop::Output {
            product: Box::new(product),
            error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "run with the archive laws"]
    fn audit_only_exempts_optional_imports_from_the_runtime() {
        use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
        use object::{
            Architecture, BinaryFormat, Endianness, RelocationFlags, SymbolFlags, SymbolKind,
            SymbolScope,
        };
        let reference = |weak| {
            let mut object =
                Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
            let external = b"hirsel_archive_test_unlisted";
            let data = object.section_id(StandardSection::Data);
            let offset = object.append_section_data(data, &[0; 8], 8);
            let symbol = object.add_symbol(Symbol {
                name: external.to_vec(),
                value: 0,
                size: 0,
                kind: SymbolKind::Text,
                scope: SymbolScope::Unknown,
                weak,
                section: SymbolSection::Undefined,
                flags: SymbolFlags::None,
            });
            object
                .add_relocation(
                    data,
                    Relocation {
                        offset,
                        symbol,
                        addend: 0,
                        flags: RelocationFlags::Elf {
                            r_type: object::elf::R_X86_64_64,
                        },
                    },
                )
                .unwrap();
            object.write().unwrap()
        };
        let weak = reference(true);
        let strong = reference(false);
        let recipe = LinkRecipe {
            schema: 0,
            target: Target::X86_64Linux.triple().into(),
            linker: Path::new("ld.lld").into(),
            args: vec![],
            files: vec![],
            source_files: vec![],
            absent: vec![],
            identity: ContentHash([0; 32]),
        };
        let link = Link::Driver(Path::new("clang").into());
        let runtime = Member {
            name: "runtime.o".into(),
            bytes: &weak,
            runtime: true,
        };
        let check = |members: &[Member<'_>]| {
            let (definitions, imports) = symbols(members).unwrap();
            audit(&definitions, imports, &recipe, &link).map_err(|e| e.to_string())
        };
        let strong_runtime = Member {
            name: "runtime.o".into(),
            bytes: &strong,
            runtime: true,
        };
        assert!(
            check(&[strong_runtime])
                .unwrap_err()
                .contains("UnlistedImport(hirsel_archive_test_unlisted, runtime.o)")
        );
        assert!(check(std::slice::from_ref(&runtime)).is_ok());
        let foreign = Member {
            name: "foreign.o".into(),
            bytes: &weak,
            runtime: false,
        };
        assert!(
            check(&[runtime, foreign])
                .unwrap_err()
                .contains("UnlistedImport(hirsel_archive_test_unlisted, foreign.o)")
        );
    }
}
