//! A definition's checked body, kept with its result.
use super::*;
/// A body and its result are kept together: CoreIR ids refer only to this module.
/// Callee rows are signature-only imports, so a callee-body edit does not alter it.
///
/// A product read back from its record holds only its head; its IR is read
/// through `get_body` on the first [`DefinitionProduct::ir`] demand.
pub struct DefinitionProduct {
    pub definition: DefPath,
    pub check: CoreCheck,
    ir: std::sync::OnceLock<Module>,
    /// Where a record-backed product's IR is read from on demand.
    ir_source: Option<IrSource>,
    /// The hash of `ir`'s serialization: its body record's module part,
    /// set when the record is framed.
    ir_hash: ContentHash,
    pub key: DefinitionKey,
    pub anchors: BTreeMap<ItemAt, DefPath>,
    pub incomplete: Vec<Incomplete>,
    /// The body record's address: the content hash of this product as published.
    pub record: ContentHash,
}
/// The store a record-backed product's IR is read from, and for an image
/// body the reader its published descriptors decode through.
pub(crate) struct IrSource {
    pub store: Store,
    pub reader: Option<Arc<hirsel_core_ids::published::Reader>>,
}
/// The hash a body record's module part would have, for a module no
/// record carries: an instance's materialized body, serialized into
/// `scratch`, which a caller reuses across modules. A stream into the
/// hasher costs a dispatched write per encoded field.
pub(crate) fn ir_hash(module: &Module, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
    scratch.clear();
    encoding::write_fixed(scratch, module)?;
    Ok(Store::ir_hash(scratch))
}

/// The checked header's only optional suffix is its nonempty computed-read set.
pub(crate) fn decode_head(bytes: &[u8]) -> Option<(DiskCheck, BTreeSet<DefPath>)> {
    let (disk, suffix) = encoding::read_fixed_prefix(bytes).ok()?;
    let computed = if suffix.is_empty() {
        BTreeSet::new()
    } else {
        let names: BTreeSet<DefPath> = decode(suffix)?;
        if names.is_empty() {
            return None;
        }
        names
    };
    Some((disk, computed))
}

impl DefinitionProduct {
    /// A freshly checked body. Its record is framed at publication
    /// (`frame`), which sets `ir_hash` and `record`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fresh(
        definition: DefPath,
        check: CoreCheck,
        ir: Module,
        key: DefinitionKey,
        anchors: BTreeMap<ItemAt, DefPath>,
        incomplete: Vec<Incomplete>,
    ) -> Self {
        Self {
            definition,
            check,
            ir: ir.into(),
            ir_source: None,
            ir_hash: ContentHash::default(),
            key,
            anchors,
            incomplete,
            record: ContentHash::default(),
        }
    }
    /// The body record (`DiskCheck`): its head, then the serialization of
    /// the module the emitter has, encoded once into the envelope. The
    /// module part's hash is `ir_hash`.
    pub(crate) fn frame(&mut self) -> io::Result<Vec<u8>> {
        let head = encode(&DiskCheckRef::of(self)?)?;
        let ir = self.ir.get().expect("a fresh product holds its IR");
        thread_local! {
            static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        let (record, ir_hash, bytes) = SCRATCH.with_borrow_mut(|scratch| {
            scratch.clear();
            {
                hirsel_core_front::cost_scope!("encoding", "canonical key encoding");
                encoding::write_fixed(scratch, ir)?;
            }
            Ok::<_, io::Error>(Store::frame_body(&head, scratch))
        })?;
        self.record = record;
        self.ir_hash = ir_hash;
        Ok(bytes)
    }
    /// A published body record's head read back under its address
    /// (`Store::get_body_head`). Its IR is read from `source` on demand.
    pub(crate) fn from_head(
        head: &crate::store::Head,
        record: ContentHash,
        decode: impl FnOnce(&[u8]) -> Option<(DiskCheck, BTreeSet<DefPath>)>,
        source: IrSource,
    ) -> Option<Self> {
        let (disk, computed) = decode(head.head())?;
        let [nodes, uses, places, calls, types, bytes] = disk.counts;
        Some(Self {
            record,
            definition: disk.definition,
            anchors: (0u32..)
                .zip(disk.anchors)
                .map(|(i, path)| ((ANCHORED, i), path))
                .collect(),
            key: disk.key,
            incomplete: disk.incomplete,
            ir_hash: head.ir_hash,
            ir: std::sync::OnceLock::new(),
            ir_source: Some(source),
            check: CoreCheck {
                refusals: disk.refusals,
                gaps: vec![],
                function: disk.function.map(hirsel_core_ir::FuncId),
                template: disk.template,
                flow: disk.flow,
                constructor_types: disk.constructor_types,
                constructor_candidates: disk.constructor_candidates,
                computed,
                counts: hirsel_core_front::Counts {
                    nodes,
                    uses,
                    places,
                    calls,
                    types,
                    bytes,
                },
            },
        })
    }
    /// The body's module. A record-backed product reads it once, through
    /// `get_body`, the only IR reader. A record whose IR fails its check
    /// behind the head this product was read from is removed, so the next
    /// request misses it and checks the body again (a library image's needs
    /// a rebuild); this request refuses with the record's path.
    pub fn ir(&self) -> io::Result<&Module> {
        if let Some(ir) = self.ir.get() {
            return Ok(ir);
        }
        let source = self
            .ir_source
            .as_ref()
            .expect("a product without its IR has a record");
        let ir = source
            .store
            .get_body(self.record)?
            .filter(|body| body.ir_hash == self.ir_hash)
            .and_then(|body| match &source.reader {
                Some(reader) => reader.decode(|| decode::<Module>(body.ir())),
                None => decode::<Module>(body.ir()),
            });
        let Some(ir) = ir else {
            source.store.remove_damaged(self.record)?;
            let next = match source.reader {
                None => "the next request checks the body again",
                Some(_) => "the library image needs a rebuild",
            };
            return Err(io::Error::other(format!(
                "the body record {} of `{}` is damaged behind its head; it is removed, and {next}",
                source.store.path(self.record).display(),
                self.definition
            )));
        };
        Ok(self.ir.get_or_init(|| ir))
    }
    /// The hash of `ir`'s serialization, as its record's module part.
    pub fn ir_hash(&self) -> ContentHash {
        self.ir_hash
    }
}
