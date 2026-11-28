//! A closed target layout built by the compiler. Only the compiler supplies this
//! input to FORM. The caller's document is always read separately by `decode`.
use super::{DataError, Descriptor};
mod wire;
use hirsel_core_layout::Tag;

pub struct Plan {
    pub nodes: Vec<Node>,
    /// Root row of Result[T, DataError] in the construction plan.
    pub result: usize,
}

pub struct Node {
    pub size: u64,
    pub logical: u64,
    pub align: u64,
    pub body: Construction,
}

pub enum Construction {
    Unit,
    Scalar(super::Scalar),
    Text,
    List {
        item: usize,
        graph: bool,
    },
    Map {
        item: usize,
        stride: u64,
        align: u64,
        meta: u64,
        key: u64,
        value: u64,
    },
    Record(Vec<Field>),
    Sum {
        tag: Option<Tag>,
        cases: Vec<(String, Vec<Field>)>,
    },
}

pub struct Field {
    pub offset: u64,
    pub name: String,
    pub node: usize,
}

impl Plan {
    pub fn witness(&self, schema: &Descriptor) -> Vec<u8> {
        wire::encode(schema, self)
    }

    /// The caller funds this compiler-owned representation before opening it.
    pub fn from_compiler(bytes: &[u8]) -> Result<(Descriptor, Self), DataError> {
        wire::open(bytes)
    }
}

/// Logical temporary storage for opening the compiler witness, including list
/// rows and decoded scalar defaults. The price is independent of the engine.
pub fn witness_charge(bytes: usize) -> u64 {
    (bytes as u64).saturating_mul(16)
}

/// Work for opening the static descriptor and scanning an admitted source.
pub fn steps(witness: usize, input: usize, limits: super::Limits) -> u64 {
    let source = if input <= limits.input_bytes {
        input as u64
    } else {
        0
    };
    (witness as u64)
        .div_ceil(32)
        .saturating_add(source)
        .saturating_add(1)
}
