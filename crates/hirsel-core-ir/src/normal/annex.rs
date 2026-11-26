//! A body's annex: its positions and hints, in the body's row orders.
use super::*;

/// A parameter of the checked entry declaration, before lowering to a word.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntryParameter {
    pub ty: String,
    pub name: String,
}

impl EntryParameter {
    pub fn foreign_interface(&self) -> Option<&str> {
        self.ty
            .strip_prefix("core@1/core/Foreign[")
            .and_then(|ty| ty.strip_suffix(']'))
    }
}

/// A body's source positions and hints, indexed by canonical function number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Annex(
    pub Vec<Option<FunctionAnnex>>,
    /// The entry's ordered parameter types and startup diagnostic names.
    pub Option<Vec<EntryParameter>>,
    /// A program entry's error description (PROG-3), opaque to the IR.
    pub Option<Vec<u8>>,
);

impl Wire for EntryParameter {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        self.ty.write(out);
        self.name.write(out);
    }

    fn read(input: &mut Read<'_, '_>) -> DecodeResult<Self> {
        Ok(Self {
            ty: Wire::read(input)?,
            name: Wire::read(input)?,
        })
    }
}

impl Annex {
    pub fn encode(&self) -> Vec<u8> {
        let empty = Module::default();
        let mut order = ModuleOrder::new(&empty);
        let mut out = Write::module(&mut order);
        out.bytes.extend_from_slice(ANNEX_MAGIC);
        out.count(self.0.len());
        for function in &self.0 {
            function.is_some().write(&mut out);
            if let Some(function) = function {
                function.instructions.write(&mut out);
                function.terminators.write(&mut out);
            }
        }
        for function in self.0.iter().flatten() {
            function.decisions.write(&mut out);
            function.cold_calls.write(&mut out);
            function.cold_edges.write(&mut out);
            out.count(function.callable_parameters.len());
            for &parameter in &function.callable_parameters {
                out.count(parameter as usize);
            }
        }
        self.1.write(&mut out);
        self.2.is_some().write(&mut out);
        if let Some(error) = &self.2 {
            out.count(error.len());
            out.raw(error);
        }
        out.bytes
    }

    /// Read the annex sections and check their shape against the decoded body.
    pub fn decode(bytes: &[u8], body: &Module, budget: &Debit<'_>) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, ANNEX_MAGIC, budget)?;
        let len = input.count()?;
        if len != body.functions.len() {
            return Err(DecodeError::Malformed("annex does not match body"));
        }
        let mut functions = input.read_vec(len, |input| {
            bool::read(input)?
                .then(|| {
                    Ok(FunctionAnnex {
                        instructions: Wire::read(input)?,
                        terminators: Wire::read(input)?,
                        decisions: vec![],
                        cold_calls: vec![],
                        cold_edges: vec![],
                        callable_parameters: vec![],
                    })
                })
                .transpose()
        })?;
        for function in functions.iter_mut().flatten() {
            function.decisions = Wire::read(&mut input)?;
            function.cold_calls = Wire::read(&mut input)?;
            function.cold_edges = Wire::read(&mut input)?;
            let len = input.count()?;
            function.callable_parameters = input.read_vec(len, Read::index)?;
        }
        let parameters: Option<Vec<EntryParameter>> = Wire::read(&mut input)?;
        let error = bool::read(&mut input)?
            .then(|| {
                let n = input.count()?;
                input.budget.charge_bytes(n)?;
                Ok(input.take(n)?.to_vec())
            })
            .transpose()?;
        input.end()?;
        let parameters_match = (error.is_none() || parameters.is_some())
            && parameters.as_ref().is_none_or(|parameters| {
                body.functions.first().is_some_and(|entry| {
                    parameters.len() == body.signatures[entry.signature.index()].params.len()
                })
            });
        let matches = parameters_match
            && functions.len() == body.functions.len()
            && functions
                .iter()
                .zip(&body.functions)
                .all(|(annex, function)| match (annex, &function.body) {
                    (None, None) => true,
                    (Some(annex), Some(body)) => {
                        let params = body.blocks[0].params.get(&body.pools.values).unwrap();
                        annex.instructions.len() == body.insts.len()
                            && annex.terminators.len() == body.terminators.len()
                            && annex.decisions.len() == body.terminators.len()
                            && annex
                                .decisions
                                .iter()
                                .zip(&annex.terminators)
                                .zip(&body.terminators)
                                .all(|((&decision, site), terminator)| {
                                    !decision
                                        || (site.is_some()
                                            && matches!(
                                                terminator,
                                                Terminator::Branch { .. }
                                                    | Terminator::Switch { .. }
                                            ))
                                })
                            && annex.cold_calls.len() == body.calls.len()
                            && annex.cold_edges.len() == body.edges.len()
                            && annex.callable_parameters.windows(2).all(|v| v[0] < v[1])
                            && annex.callable_parameters.iter().all(|&v| {
                                params.contains(&ValueId(v)) && body.values[v as usize] == Ty::Ptr
                            })
                            && annex
                                .instructions
                                .iter()
                                .chain(&annex.terminators)
                                .flatten()
                                .all(|site| site.line != 0 && site.column != 0)
                    }
                    _ => false,
                });
        if !matches {
            return Err(DecodeError::Malformed("annex does not match body"));
        }
        Ok(Self(functions, parameters, error))
    }

    pub fn hash(&self) -> ContentHash {
        ContentHash::of_parts(ANNEX_HASH_DOMAIN, [&self.encode()[..]])
    }
}

/// A function's annex with its blocks, calls and edges taken in the given
/// row orders and its values renumbered by `value`.
pub(super) fn annex_in_order(
    module: &Module,
    body: &Body,
    blocks: impl Iterator<Item = usize> + Clone,
    value: impl Fn(usize) -> u32,
    calls: impl Iterator<Item = usize>,
    edges: impl Iterator<Item = usize>,
    sources: &SourcePositions<'_>,
) -> FunctionAnnex {
    let site = |anchor| retained::resolve_site(&module.items, sources, anchor);
    let mut callable_parameters: Vec<_> = body
        .callable_parameters
        .iter()
        .map(|parameter| value(parameter.index()))
        .collect();
    callable_parameters.sort_unstable();
    callable_parameters.dedup();
    let blocks = blocks.map(|row| &body.blocks[row]);
    retained::decided(FunctionAnnex {
        instructions: blocks
            .clone()
            .flat_map(|block| {
                PoolRange::new(block.insts.start, block.insts.len)
                    .get(&body.inst_anchors)
                    .unwrap()
                    .iter()
                    .copied()
                    .map(site)
            })
            .collect(),
        terminators: blocks.clone().map(|block| site(block.anchor)).collect(),
        decisions: blocks.map(|block| block.decision).collect(),
        cold_calls: calls.map(|row| body.calls[row].cold).collect(),
        cold_edges: edges.map(|row| body.edges[row].cold).collect(),
        callable_parameters,
    })
}
