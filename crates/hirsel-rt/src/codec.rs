//! The canonical SER-2 codec over native layouts (runtime ABI 9; LIR schema revision 29), as
//! the whole envelope or its bare payload (`Codec::form`, runtime ABI 109).
//!
//! `hirsel_rt_v2_codec_encode` and `_decode` walk a compiler-emitted codec program
//! (`hirsel_rt_v2.h`, "The canonical SER-2 codec") that pairs each schema node with the layout
//! of its values. Both walks are iterative over runtime-owned work stacks, so no native stack
//! depth follows the data. Decoding validates the envelope and the whole payload before it
//! allocates anything, then builds fresh blocks in reading order; nothing partially decoded
//! exists. Error classes and their order are the reference codec's (`hirsel_types::wire`).
//! Past the reference's budgets (bytes, and values: one per collection element, two per map
//! entry) a decode is `DecodeError::Limit` and an encode stops environmentally.
pub mod access;
mod borrowed;
pub(crate) mod value_access;

use crate::alloc::{
    allocate_sequence, hirsel_rt_v2_alloc, payload, sequence_elements_offset, sequence_slice_start,
};
use crate::stop::{Stop, Stopped};
use crate::{DOMAIN_HEAP, Header, TaskCtx, fault};
use std::collections::BTreeSet;
use std::ffi::c_void;

/// `hirsel_rt_v2_codec`.
#[repr(C)]
pub struct Codec {
    pub descriptor: *const u8,
    pub descriptor_length: u64,
    pub program: *const u64,
    pub program_length: u64,
    /// The checks a decode's `CHECKED` nodes call (runtime ABI 23), `check_count` of them.
    pub checks: *const CheckThunk,
    pub check_count: u64,
    /// What the bytes are (runtime ABI 109): `FORM_ENVELOPE` or `FORM_PAYLOAD`.
    pub form: u64,
}

/// `Codec::form`: the canonical SER-2 envelope around the payload.
pub const FORM_ENVELOPE: u64 = 0;
/// `Codec::form`: exactly the envelope's payload section, with no descriptor to match.
pub const FORM_PAYLOAD: u64 = 1;

/// A codec's form, read from its record; any other word is a stale or foreign record,
/// which faults rather than reading the bytes with another walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Form {
    Envelope,
    Payload,
}

/// `hirsel_rt_v2_check_thunk`: a checked opaque type's check (SER-1) on the borrowed value
/// at `value`, writing its verdict, 0 or 1, at `verdict`. Returns 0, or 1 with the check's
/// failure recorded.
pub type CheckThunk =
    unsafe extern "C" fn(ctx: *mut TaskCtx, value: *const u8, verdict: *mut u8) -> u8;

pub const CODEC_VERSION: u64 = 6;
pub const NODE_SCALAR: u64 = 0;
pub const NODE_TEXT: u64 = 1;
pub const NODE_FIELDS: u64 = 2;
pub const NODE_VARIANT: u64 = 3;
pub const NODE_BOX: u64 = 4;
pub const NODE_SEQUENCE: u64 = 5;
pub const NODE_REPEAT: u64 = 6;
pub const NODE_MAP: u64 = 7;
pub const NODE_SET: u64 = 8;
pub const NODE_FLAT_TEXT: u64 = 9;
pub const NODE_FRAMED: u64 = 10;
pub const NODE_FLAT_FIELDS: u64 = 11;
pub const NODE_FLAT_VARIANT: u64 = 12;
pub const NODE_FLAT_SEQUENCE: u64 = 13;
pub const NODE_SEQUENTIAL: u64 = 14;
pub const NODE_COLUMNS: u64 = 15;
pub const NODE_PATH: u64 = 16;
pub const NODE_CHECKED: u64 = 17;
pub const NO_NODE: u64 = u64::MAX;

const MAGIC: &[u8; 4] = b"HSC1";
const WIRE_VERSION: u32 = 1;
/// The reference codec's budgets (`hirsel_types::wire::Limits::default`): past them a decode
/// is `DecodeError::Limit`, and an encode stops operationally (`budget`).
const LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const LIMIT_VALUES: u64 = 1_000_000;

/// `DecodeError`'s ordinals (core-library.md).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DecodeError {
    Version = 0,
    SchemaMismatch = 1,
    Truncated = 2,
    Trailing = 3,
    Scalar = 4,
    Utf8 = 5,
    Tag = 6,
    Length = 7,
    Duplicate = 8,
    Order = 9,
    /// A checked opaque's check rejected a decoded representation (SER-1).
    Invalid = 10,
    /// The input exceeds the decode budgets, `LIMIT_BYTES` or `LIMIT_VALUES` (SER-2).
    Limit = 11,
}

#[derive(Clone, Copy)]
struct Text {
    utf8: bool,
    owner: u64,
    start: u64,
    length: u64,
    index: u32,
}

#[derive(Clone, Copy)]
struct Elements {
    index: u32,
    align: u64,
    offset: u64,
    stride: u64,
}

#[derive(Clone, Copy)]
struct Container {
    set: bool,
    index: u32,
    size: u64,
    align: u64,
    entries_at: u64,
    index_at: u64,
    entries: Elements,
    index_index: u32,
    index_align: u64,
    index_stride: u64,
    key: usize,
    key_at: u64,
    value: Option<usize>,
    value_at: u64,
    map: MapStorage,
}
#[derive(Clone, Copy)]
struct MapStorage {
    hashes_at: u64,
    live_at: u64,
    tag_at: u64,
    tag_form: u64,
    tag_bit: u64,
}
impl MapStorage {
    unsafe fn live(self, at: *const u8) -> bool {
        unsafe {
            let tag = at.add(self.tag_at as usize);
            match self.tag_form {
                0 => tag.cast::<u32>().read_unaligned() == 1,
                1 => word(tag) & (1 << self.tag_bit) != 0,
                2 => word(tag) != 0,
                _ => fault("a map entry tag form"),
            }
        }
    }
    unsafe fn publish(self, at: *mut u8) {
        unsafe {
            let tag = at.add(self.tag_at as usize);
            match self.tag_form {
                0 => tag.cast::<u32>().write_unaligned(1),
                1 => tag.cast::<u64>().write_unaligned(1 << self.tag_bit),
                2 => {} // the decoded non-null payload supplies the niche tag
                _ => fault("a map entry tag form"),
            }
        }
    }
}

/// One program node, read in place from the compiler-emitted words: a codec program is a
/// compile-time constant of its type, so no call parses, builds or caches it. Field lists
/// are `(offset, node)` word pairs and flat field lists node words.
#[derive(Clone, Copy)]
enum Node<'a> {
    Scalar(Scalar),
    Text(Text),
    Fields(&'a [u64]),
    Variant {
        niche: bool,
        tag: u64,
        nullary: u32,
        arms: Arms<'a>,
    },
    Box {
        index: u32,
        size: u64,
        align: u64,
        payload: usize,
        /// A source `box`, one value of the decode budget; a variable flat block is not.
        boxed: bool,
    },
    Sequence {
        elements: Elements,
        element: usize,
    },
    Repeat {
        count: u64,
        stride: u64,
        element: usize,
    },
    Container(Container),
    FlatText {
        utf8: bool,
    },
    Framed(usize),
    FlatFields(&'a [u64]),
    FlatVariant(Arms<'a>),
    FlatSequence {
        offset: u64,
        stride: u64,
        element: usize,
    },
    Sequential {
        index: u32,
        align: u64,
        offset: u64,
        element: usize,
    },
    /// `columns` holds `(prefix, stride, node)` word triples.
    Columns {
        index: u32,
        align: u64,
        data: u64,
        quantum: u64,
        element: usize,
        columns: &'a [u64],
    },
    /// A core Path's run-context identity, on the wire as its text.
    Path,
    /// A checked opaque type: its representation `value` at the same address, and the
    /// check a decode calls on it afterwards.
    Checked {
        check: usize,
        value: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scalar {
    Bool,
    Integer(usize),
    F32,
    F64,
    /// Four bytes holding a Unicode scalar value.
    Char,
}

impl Scalar {
    fn width(self) -> usize {
        match self {
            Scalar::Bool => 1,
            Scalar::Integer(n) => n,
            Scalar::F32 | Scalar::Char => 4,
            Scalar::F64 => 8,
        }
    }
}

/// A variant's arms in place: `count` field lists of `width`-word entries, each its length
/// then its entries, followed (ordinary variants) by one Packed extent per arm.
#[derive(Clone, Copy)]
struct Arms<'a> {
    words: &'a [u64],
    count: u64,
    width: usize,
}

impl<'a> Arms<'a> {
    /// The first arm's fields and the words after it.
    fn split(self, words: &'a [u64]) -> (&'a [u64], &'a [u64]) {
        let n = *words
            .first()
            .unwrap_or_else(|| fault("a codec program ends inside a node"));
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_mul(self.width))
            .and_then(|n| n.checked_add(1))
            .filter(|&end| end <= words.len())
            .unwrap_or_else(|| fault("a codec program ends inside a node"));
        (&words[1..end], &words[end..])
    }
    /// Arm `ordinal`'s fields, or None outside the variant.
    fn get(self, ordinal: u32) -> Option<&'a [u64]> {
        if u64::from(ordinal) >= self.count {
            return None;
        }
        let mut rest = self.words;
        for _ in 0..ordinal {
            rest = self.split(rest).1;
        }
        Some(self.split(rest).0)
    }
    /// Every arm's node ids, in order.
    fn nodes(self) -> impl Iterator<Item = impl Iterator<Item = usize> + 'a> {
        let width = self.width;
        let mut rest = self.words;
        (0..self.count).map(move |_| {
            let (fields, after) = self.split(rest);
            rest = after;
            fields
                .chunks_exact(width)
                .map(move |c| c[width - 1] as usize)
        })
    }
    /// Arm `ordinal`'s Packed extent.
    fn extent(self, ordinal: u32) -> Option<u64> {
        if u64::from(ordinal) >= self.count {
            return None;
        }
        let mut rest = self.words;
        for _ in 0..self.count {
            rest = self.split(rest).1;
        }
        Some(
            *rest
                .get(ordinal as usize)
                .unwrap_or_else(|| fault("a codec program ends inside a node")),
        )
    }
}

/// `(offset, node)` pairs of a field list, in order.
fn pairs(fields: &[u64]) -> impl DoubleEndedIterator<Item = (u64, usize)> + '_ {
    fields.chunks_exact(2).map(|c| (c[0], c[1] as usize))
}

/// Node ids of a flat field list, in order.
fn ids(fields: &[u64]) -> impl DoubleEndedIterator<Item = usize> + ExactSizeIterator + '_ {
    fields.iter().map(|&f| f as usize)
}

/// The emitted program words with their header read. Node ids index the start table;
/// a malformed program is an invariant fault where it is read.
#[derive(Clone, Copy)]
struct Program<'a> {
    words: &'a [u64],
    count: usize,
    out_size: u64,
    root: usize,
    out: usize,
    bytes: usize,
    /// The root layout's descriptor index, which a decode releases a value at when a
    /// check rejects it; None for a program without checked nodes.
    root_descriptor: Option<u32>,
}

/// A cursor over one node's words; a malformed program is an invariant fault.
struct Words<'a> {
    words: &'a [u64],
    at: usize,
    count: usize,
}

impl<'a> Words<'a> {
    fn next(&mut self) -> u64 {
        let word = *self
            .words
            .get(self.at)
            .unwrap_or_else(|| fault("a codec program ends inside a node"));
        self.at += 1;
        word
    }
    fn node(&mut self) -> usize {
        let node = self.next();
        if node >= self.count as u64 {
            fault("a codec program names a node outside it");
        }
        node as usize
    }
    fn index(&mut self) -> u32 {
        u32::try_from(self.next()).unwrap_or_else(|_| fault("a codec descriptor index overflows"))
    }
    /// The next `n` entries of `width` words.
    fn take(&mut self, n: u64, width: usize) -> &'a [u64] {
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_mul(width))
            .and_then(|n| n.checked_add(self.at))
            .filter(|&end| end <= self.words.len())
            .unwrap_or_else(|| fault("a codec program ends inside a node"));
        let words = &self.words[self.at..end];
        self.at = end;
        words
    }
    fn fields(&mut self, width: usize) -> &'a [u64] {
        let n = self.next();
        self.take(n, width)
    }
    fn rest(&self) -> &'a [u64] {
        &self.words[self.at.min(self.words.len())..]
    }
    fn elements(&mut self) -> Elements {
        Elements {
            index: self.index(),
            align: self.next(),
            offset: self.next(),
            stride: self.next(),
        }
    }
}

const HEADER: usize = 7;

impl<'a> Program<'a> {
    /// The program `words` (version, node count, out size, root, out and bytes nodes, root
    /// descriptor, then each node's start).
    fn new(words: &'a [u64]) -> Self {
        let header = words
            .get(..HEADER)
            .unwrap_or_else(|| fault("a codec program ends inside its header"));
        if header[0] != CODEC_VERSION {
            fault("a codec program of another version");
        }
        let count = usize::try_from(header[1])
            .ok()
            .filter(|&count| count <= words.len() - HEADER)
            .unwrap_or_else(|| fault("codec node count"));
        let node = |word: u64| {
            if word >= count as u64 {
                fault("a codec program names a node outside it");
            }
            word as usize
        };
        Program {
            words,
            count,
            out_size: header[2],
            root: node(header[3]),
            out: node(header[4]),
            bytes: node(header[5]),
            root_descriptor: match header[6] {
                NO_NODE => None,
                index => Some(
                    u32::try_from(index)
                        .unwrap_or_else(|_| fault("a codec descriptor index overflows")),
                ),
            },
        }
    }

    /// Node `id`'s words, from its kind on.
    fn words(self, id: usize) -> &'a [u64] {
        if id >= self.count {
            fault("a codec program names a node outside it");
        }
        usize::try_from(self.words[HEADER + id])
            .ok()
            .and_then(|start| self.words.get(start..))
            .filter(|node| !node.is_empty())
            .unwrap_or_else(|| fault("codec node offset"))
    }

    /// A scalar node's code, which keeps integer signedness for access-only value
    /// ordering. Wire bytes are unchanged.
    fn scalar_code(self, id: usize) -> u64 {
        match self.words(id) {
            [NODE_SCALAR, code, ..] => *code,
            _ => fault("a scalar code of a node that is not a scalar"),
        }
    }

    fn node(self, id: usize) -> Node<'a> {
        let mut w = Words {
            words: self.words(id),
            at: 0,
            count: self.count,
        };
        match w.next() {
            NODE_SCALAR => Node::Scalar(match w.next() {
                0 => Scalar::Bool,
                1 | 6 => Scalar::Integer(1),
                2 | 7 => Scalar::Integer(2),
                3 | 8 => Scalar::Integer(4),
                4 | 9 => Scalar::Integer(8),
                5 | 10 => Scalar::Integer(16),
                11 => Scalar::F32,
                12 => Scalar::F64,
                13 => Scalar::Char,
                _ => fault("an unknown codec scalar"),
            }),
            NODE_TEXT => Node::Text(Text {
                utf8: w.next() != 0,
                owner: w.next(),
                start: w.next(),
                length: w.next(),
                index: w.index(),
            }),
            NODE_FIELDS => Node::Fields(w.fields(2)),
            NODE_VARIANT => {
                let niche = match w.next() {
                    0 => false,
                    1 => true,
                    _ => fault("an unknown codec tag form"),
                };
                let tag = w.next();
                let nullary = u32::try_from(w.next()).unwrap_or_else(|_| fault("niche arm"));
                let count = w.next();
                if niche && (count != 2 || nullary > 1) {
                    fault("a codec niche that is not two arms");
                }
                Node::Variant {
                    niche,
                    tag,
                    nullary,
                    arms: Arms {
                        words: w.rest(),
                        count,
                        width: 2,
                    },
                }
            }
            NODE_BOX => Node::Box {
                index: w.index(),
                size: w.next(),
                align: w.next(),
                payload: w.node(),
                boxed: w.next() != 0,
            },
            NODE_SEQUENCE => Node::Sequence {
                elements: w.elements(),
                element: w.node(),
            },
            NODE_REPEAT => Node::Repeat {
                count: w.next(),
                stride: w.next(),
                element: w.node(),
            },
            kind @ (NODE_MAP | NODE_SET) => {
                let (index, size, align, entries_at, index_at) =
                    (w.index(), w.next(), w.next(), w.next(), w.next());
                let entries = w.elements();
                let (index_index, index_align, index_stride) = (w.index(), w.next(), w.next());
                let key = w.node();
                let key_at = w.next();
                let value = match w.next() {
                    NO_NODE => None,
                    node if node < self.count as u64 => Some(node as usize),
                    _ => fault("a codec program names a node outside it"),
                };
                let value_at = w.next();
                if (kind == NODE_SET) != value.is_none() {
                    fault("a codec container whose entries disagree with its kind");
                }
                let map = MapStorage {
                    hashes_at: w.next(),
                    live_at: w.next(),
                    tag_at: w.next(),
                    tag_form: w.next(),
                    tag_bit: w.next(),
                };
                Node::Container(Container {
                    map,
                    set: kind == NODE_SET,
                    index,
                    size,
                    align,
                    entries_at,
                    index_at,
                    entries,
                    index_index,
                    index_align,
                    index_stride,
                    key,
                    key_at,
                    value,
                    value_at,
                })
            }
            NODE_FLAT_TEXT => Node::FlatText {
                utf8: w.next() != 0,
            },
            NODE_FRAMED => Node::Framed(w.node()),
            NODE_FLAT_FIELDS => Node::FlatFields(w.fields(1)),
            NODE_FLAT_VARIANT => {
                let count = w.next();
                Node::FlatVariant(Arms {
                    words: w.rest(),
                    count,
                    width: 1,
                })
            }
            NODE_FLAT_SEQUENCE => Node::FlatSequence {
                offset: w.next(),
                stride: w.next(),
                element: w.node(),
            },
            NODE_SEQUENTIAL => Node::Sequential {
                index: w.index(),
                align: w.next(),
                offset: w.next(),
                element: w.node(),
            },
            NODE_COLUMNS => {
                let (index, align, data, quantum, element) =
                    (w.index(), w.next(), w.next(), w.next(), w.node());
                Node::Columns {
                    index,
                    align,
                    data,
                    quantum,
                    element,
                    columns: w.fields(3),
                }
            }
            NODE_PATH => Node::Path,
            NODE_CHECKED => Node::Checked {
                check: usize::try_from(w.next())
                    .unwrap_or_else(|_| fault("a codec check index overflows")),
                value: w.node(),
            },
            _ => fault("an unknown codec node kind"),
        }
    }

    /// The Bytes node's slice layout.
    fn bytes(self) -> Text {
        match self.node(self.bytes) {
            Node::Text(text @ Text { utf8: false, .. }) => text,
            _ => fault("a codec program whose bytes node is not Bytes"),
        }
    }

    /// A variant node's arm `ordinal`, after writing its tag at `at`.
    unsafe fn write_arm(self, node: usize, at: *mut u8, ordinal: u32) -> &'a [u64] {
        let Node::Variant {
            niche,
            tag,
            nullary,
            arms,
        } = self.node(node)
        else {
            fault("a codec arm of a node that is not a variant");
        };
        let arm = arms
            .get(ordinal)
            .unwrap_or_else(|| fault("a codec arm outside its variant"));
        // A niche's nullary arm is the null word the zeroed value already holds;
        // its dataful arm's first field writes the pointer.
        if !niche {
            unsafe { write(at.add(tag as usize), &ordinal.to_le_bytes()) };
        } else if ordinal != nullary && arm.is_empty() {
            fault("a codec niche whose dataful arm has no field");
        }
        arm
    }
}

/// The program, form, descriptor and checks of the compiler-emitted static codec at `codec`.
unsafe fn program<'a>(codec: *const Codec) -> (Program<'a>, Form, &'a [u8], &'a [CheckThunk]) {
    unsafe {
        let codec = &*codec;
        let form = match codec.form {
            FORM_ENVELOPE => Form::Envelope,
            FORM_PAYLOAD => Form::Payload,
            _ => fault("a codec of an unknown form"),
        };
        let words = std::slice::from_raw_parts(codec.program, codec.program_length as usize);
        let descriptor =
            std::slice::from_raw_parts(codec.descriptor, codec.descriptor_length as usize);
        let checks = if codec.check_count == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(codec.checks, codec.check_count as usize)
        };
        (Program::new(words), form, descriptor, checks)
    }
}

unsafe fn read(at: *const u8, n: usize) -> &'static [u8] {
    unsafe { std::slice::from_raw_parts(at, n) }
}
unsafe fn write(at: *mut u8, bytes: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len()) }
}
unsafe fn word(at: *const u8) -> u64 {
    unsafe { at.cast::<u64>().read_unaligned() }
}
unsafe fn pointer(at: *const u8) -> *mut Header {
    unsafe { at.cast::<*mut Header>().read_unaligned() }
}
unsafe fn set_pointer(at: *mut u8, block: *mut Header) {
    unsafe { at.cast::<*mut Header>().write_unaligned(block) }
}

/// The bytes a String or Bytes slice at `at` shows.
unsafe fn text_bytes<'a>(text: &Text, at: *const u8) -> &'a [u8] {
    unsafe {
        let owner = pointer(at.add(text.owner as usize));
        let start = word(at.add(text.start as usize));
        let length = word(at.add(text.length as usize));
        if start
            .checked_add(length)
            .is_none_or(|end| end > isize::MAX as u64)
        {
            fault("a codec slice range overflows");
        }
        std::slice::from_raw_parts(payload(owner).add(8 + start as usize), length as usize)
    }
}

/// A fresh byte sequence holding `bytes`, written as a whole slice at `at`.
unsafe fn write_text(ctx: *mut TaskCtx, text: &Text, at: *mut u8, bytes: &[u8]) {
    unsafe {
        let owner = allocate_sequence(ctx, text.index, bytes.len() as u64, 1, 1);
        write(
            payload(owner).add(sequence_elements_offset(1) as usize),
            bytes,
        );
        set_pointer(at.add(text.owner as usize), owner);
        write(
            at.add(text.start as usize),
            &sequence_slice_start(1).to_le_bytes(),
        );
        write(
            at.add(text.length as usize),
            &(bytes.len() as u64).to_le_bytes(),
        );
    }
}

/// The stop of an encode past `hirsel_types::wire::Limits::default`'s byte or value budget: the run ends
/// environmentally, as the interpreter's operational limit ends it.
fn budget() -> Stop {
    Stop::Environmental(hirsel_run_schema::Environmental::CodecLimit)
}

/// Each node's fewest wire bytes for one program (`u64::MAX` for a node with no finite
/// value), as `hirsel_types::wire` computes them: the least fixpoint from "no finite
/// value", so a recursive schema gets its shortest finite encoding. Every decode reads its
/// root's, so the sizes of the last few programs decoded are kept: a caller alternating a
/// key and a value codec computes each once.
#[derive(Default)]
struct Minimum {
    /// (program words address, sizes), most recently computed last.
    programs: Vec<(usize, Vec<u64>)>,
}

/// How many programs' minimum sizes `Minimum` keeps.
const MINIMUM_PROGRAMS: usize = 8;

impl Minimum {
    fn of(&mut self, program: Program<'_>) -> &[u64] {
        let key = program.words.as_ptr() as usize;
        let found = self
            .programs
            .iter()
            .position(|(k, sizes)| *k == key && sizes.len() == program.count);
        let index = match found {
            Some(index) => index,
            None => {
                let mut sizes = if self.programs.len() == MINIMUM_PROGRAMS {
                    self.programs.remove(0).1
                } else {
                    Vec::new()
                };
                fixpoint(program, &mut sizes);
                self.programs.push((key, sizes));
                self.programs.len() - 1
            }
        };
        &self.programs[index].1
    }
}

fn fixpoint(program: Program<'_>, min: &mut Vec<u64>) {
    min.clear();
    min.resize(program.count, u64::MAX);
    let sum = |min: &[u64], nodes: &mut dyn Iterator<Item = usize>| {
        nodes.fold(0u64, |total, node| total.saturating_add(min[node]))
    };
    let arms = |min: &[u64], arms: Arms<'_>| {
        arms.nodes()
            .map(|mut arm| sum(min, &mut arm))
            .min()
            .map_or(u64::MAX, |arm| arm.saturating_add(4))
    };
    loop {
        let mut changed = false;
        for id in 0..program.count {
            let size = match program.node(id) {
                Node::Scalar(scalar) => scalar.width() as u64,
                Node::Text(_) | Node::FlatText { .. } | Node::Path => 8,
                Node::Fields(fields) => sum(min, &mut pairs(fields).map(|(_, f)| f)),
                Node::FlatFields(fields) => sum(min, &mut ids(fields)),
                Node::Variant { arms: a, .. } | Node::FlatVariant(a) => arms(min, a),
                Node::Box { payload: child, .. }
                | Node::Framed(child)
                | Node::Checked { value: child, .. } => min[child],
                Node::Repeat { count, element, .. } => match count {
                    0 => 0,
                    count => count.saturating_mul(min[element]),
                },
                Node::Sequence { .. }
                | Node::FlatSequence { .. }
                | Node::Sequential { .. }
                | Node::Columns { .. }
                | Node::Container(_) => 8,
            };
            if size < min[id] {
                min[id] = size;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// A count of `n` elements of at least `each` wire bytes that the bytes left cannot hold
/// is truncated input, decided before any element is visited or budgeted.
fn satisfiable(input: &Input<'_>, n: u64, each: u64) -> Result<(), DecodeError> {
    let left = (input.bytes.len() - input.at) as u128;
    if n > 0 && u128::from(n).saturating_mul(u128::from(each)) > left {
        return Err(DecodeError::Truncated);
    }
    Ok(())
}

fn nan32(bits: u32) -> bool {
    bits & 0x7fff_ffff > 0x7f80_0000
}
fn nan64(bits: u64) -> bool {
    bits & 0x7fff_ffff_ffff_ffff > 0x7ff0_0000_0000_0000
}

/// Runtime-owned work storage one encode or decode borrows. It is kept per thread and
/// reused, so a call allocates only its result; a nested call (a check that encodes or
/// decodes) finds it taken and uses fresh storage.
#[derive(Default)]
struct Scratch {
    out: Vec<u8>,
    put: Vec<Put>,
    /// Open sets: where each one's elements begin, and where its element starts begin
    /// in `marks`.
    sets: Vec<(usize, usize)>,
    marks: Vec<usize>,
    ranges: Vec<(usize, usize)>,
    sorted: Vec<u8>,
    extent: Vec<usize>,
    check: Vec<Check>,
    build: Vec<Build>,
    flat: Vec<u8>,
    flat_jobs: Vec<Job>,
    content: Vec<u8>,
    inline: Vec<(usize, usize)>,
    /// Recently decoded programs' per-node minimum wire sizes, kept across calls.
    minimum: Minimum,
}

/// Scratch storage past this many bytes is released rather than kept for the next call.
const RETAIN: usize = 1 << 20;

fn keep<T>(v: &mut Vec<T>) {
    v.clear();
    if v.capacity() * std::mem::size_of::<T>() > RETAIN {
        *v = Vec::new();
    }
}

impl Scratch {
    fn reset(&mut self) {
        keep(&mut self.out);
        keep(&mut self.put);
        keep(&mut self.sets);
        keep(&mut self.marks);
        keep(&mut self.ranges);
        keep(&mut self.sorted);
        keep(&mut self.extent);
        keep(&mut self.check);
        keep(&mut self.build);
        keep(&mut self.flat);
        keep(&mut self.flat_jobs);
        keep(&mut self.content);
        keep(&mut self.inline);
    }
}

thread_local! {
    static SCRATCH: std::cell::Cell<Option<Box<Scratch>>> = const { std::cell::Cell::new(None) };
}

fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    let mut scratch = SCRATCH.take().unwrap_or_default();
    let result = f(&mut scratch);
    scratch.reset();
    SCRATCH.set(Some(scratch));
    result
}

/// The active native extent of a pointer-free flat value. Framed fields make
/// variable children self delimiting; no maximum Packed storage enters a walk.
unsafe fn flat_extent(
    program: Program<'_>,
    node: usize,
    at: *const u8,
    work: &mut Vec<usize>,
) -> u64 {
    let mut extent = 0u64;
    let base = work.len();
    work.push(node);
    while work.len() > base {
        let node = work.pop().unwrap_or_else(|| unreachable!());
        let cursor = at.wrapping_add(extent as usize);
        let n = unsafe {
            match program.node(node) {
                Node::Scalar(scalar) => scalar.width() as u64,
                Node::FlatText { .. } | Node::Framed(_) | Node::FlatSequence { .. } => word(cursor)
                    .checked_add(8)
                    .unwrap_or_else(|| fault("flat extent overflow")),
                Node::FlatFields(fields) => {
                    work.extend(ids(fields).rev());
                    0
                }
                Node::Fields(fields) => {
                    work.extend(pairs(fields).rev().map(|(_, child)| child));
                    0
                }
                Node::FlatVariant(arms) => {
                    let ordinal = cursor.cast::<u32>().read_unaligned();
                    let arm = arms
                        .get(ordinal)
                        .unwrap_or_else(|| fault("flat extent tag"));
                    work.extend(ids(arm).rev());
                    4
                }
                Node::Variant { tag, arms, .. } => {
                    let ordinal = cursor.add(tag as usize).cast::<u32>().read_unaligned();
                    arms.extent(ordinal)
                        .unwrap_or_else(|| fault("Packed extent tag"))
                }
                Node::Repeat { count, stride, .. } => count
                    .checked_mul(stride)
                    .unwrap_or_else(|| fault("flat repeat extent")),
                Node::Checked { value, .. } => {
                    work.push(value);
                    0
                }
                _ => fault("a non-flat codec node in a flat extent"),
            }
        };
        extent = extent
            .checked_add(n)
            .unwrap_or_else(|| fault("flat extent overflow"));
    }
    extent
}

unsafe fn flat_fields(
    program: Program<'_>,
    fields: &[u64],
    at: *const u8,
    work: &mut Vec<Put>,
    extent: &mut Vec<usize>,
) {
    let start = work.len();
    let mut cursor = at;
    for child in ids(fields) {
        work.push(Put::Visit(child, cursor));
        cursor =
            cursor.wrapping_add(unsafe { flat_extent(program, child, cursor, extent) } as usize);
    }
    work[start..].reverse();
}

/// Rebuild pointer-free ordinary Packed storage, respecting field offsets,
/// native padding and its tag-selected active extent, appended to `out`. This is
/// separate from FlatFormatV0: only scalar leaves are copied from the validated wire.
fn inline_decode(
    program: Program<'_>,
    root: usize,
    input: &mut Input<'_>,
    out: &mut Vec<u8>,
    work: &mut Vec<(usize, usize)>,
    flat: &mut Vec<u8>,
    flat_jobs: &mut Vec<Job>,
) {
    let base = out.len();
    work.clear();
    work.push((root, base));
    let checked = |r: Result<u128, DecodeError>| r.unwrap_or_else(|_| fault("Packed codec reread"));
    while let Some((node, at)) = work.pop() {
        match program.node(node) {
            Node::Scalar(scalar) => {
                let n = scalar.width();
                out.resize(out.len().max(at + n), 0);
                out[at..at + n].copy_from_slice(&checked(input.number(n)).to_le_bytes()[..n]);
            }
            Node::Fields(fields) => work.extend(
                pairs(fields)
                    .rev()
                    .map(|(offset, child)| (child, at + offset as usize)),
            ),
            Node::Checked { value, .. } => work.push((value, at)),
            Node::Variant {
                niche, tag, arms, ..
            } => {
                if niche {
                    fault("a pointer niche in a pointer-free Packed element");
                }
                let ordinal = checked(input.number(4)) as u32;
                let extent = arms
                    .extent(ordinal)
                    .unwrap_or_else(|| fault("Packed codec reread"))
                    as usize;
                out.resize(out.len().max(at + extent), 0);
                let tag = at + tag as usize;
                out[tag..tag + 4].copy_from_slice(&ordinal.to_le_bytes());
                let arm = arms
                    .get(ordinal)
                    .unwrap_or_else(|| fault("Packed codec reread"));
                work.extend(
                    pairs(arm)
                        .rev()
                        .map(|(offset, child)| (child, at + offset as usize)),
                );
            }
            Node::Repeat {
                count,
                stride,
                element,
            } => work.extend(
                (0..count)
                    .rev()
                    .map(|i| (element, at + (i * stride) as usize)),
            ),
            Node::FlatFields(_)
            | Node::FlatVariant(_)
            | Node::Framed(_)
            | Node::FlatText { .. }
            | Node::FlatSequence { .. } => {
                flat.clear();
                flat_decode(program, node, input, flat, flat_jobs);
                out.resize(out.len().max(at + flat.len()), 0);
                out[at..at + flat.len()].copy_from_slice(flat);
            }
            _ => fault("a reference-bearing node in a pointer-free Packed element"),
        }
    }
}

enum Job {
    Visit(usize),
    Elements(usize, u64),
    Prefix(usize),
}

/// Transcode validated wire data into private FlatFormatV0 framing, appended to `out`.
/// The work stack, prefix patches and output are runtime owned, including for nested lists.
fn flat_decode(
    program: Program<'_>,
    root: usize,
    input: &mut Input<'_>,
    out: &mut Vec<u8>,
    work: &mut Vec<Job>,
) {
    work.clear();
    work.push(Job::Visit(root));
    let checked = |r: Result<u128, DecodeError>| r.unwrap_or_else(|_| fault("flat codec reread"));
    while let Some(job) = work.pop() {
        let node = match job {
            Job::Visit(node) => node,
            Job::Prefix(at) => {
                let length = (out.len() - at - 8) as u64;
                out[at..at + 8].copy_from_slice(&length.to_le_bytes());
                continue;
            }
            Job::Elements(node, count) => {
                if count > 0 {
                    work.push(Job::Elements(node, count - 1));
                    work.push(Job::Visit(node));
                }
                continue;
            }
        };
        match program.node(node) {
            Node::Scalar(scalar) => {
                let n = scalar.width();
                out.extend_from_slice(&checked(input.number(n)).to_le_bytes()[..n]);
            }
            Node::FlatText { .. } => {
                let n = checked(input.number(8)) as u64;
                out.extend_from_slice(&n.to_le_bytes());
                out.extend_from_slice(input.take(n).unwrap_or_else(|_| fault("flat text reread")));
            }
            Node::Framed(child) => {
                work.push(Job::Prefix(out.len()));
                out.extend_from_slice(&[0; 8]);
                work.push(Job::Visit(child));
            }
            Node::FlatFields(fields) => work.extend(ids(fields).rev().map(Job::Visit)),
            // Zero-size elements keep their ordinary layout identity: their
            // fields have no packing offsets or bytes to reconstruct.
            Node::Fields(fields) => work.extend(pairs(fields).rev().map(|(_, f)| Job::Visit(f))),
            Node::Checked { value, .. } => work.push(Job::Visit(value)),
            Node::FlatVariant(arms) => {
                let ordinal = checked(input.number(4)) as u32;
                out.extend_from_slice(&ordinal.to_le_bytes());
                let arm = arms
                    .get(ordinal)
                    .unwrap_or_else(|| fault("flat variant reread"));
                work.extend(ids(arm).rev().map(Job::Visit));
            }
            Node::FlatSequence {
                offset,
                stride: _,
                element,
            } => {
                let count = checked(input.number(8)) as u64;
                work.push(Job::Prefix(out.len()));
                out.extend_from_slice(&[0; 8]);
                if offset == 16 {
                    out.extend_from_slice(&count.to_le_bytes());
                }
                work.push(Job::Elements(element, count));
            }
            Node::Repeat { count, element, .. } => work.push(Job::Elements(element, count)),
            _ => fault("a non-flat codec node in flat reconstruction"),
        }
    }
}

// ----- Encoding --------------------------------------------------------------------------

enum Put {
    Visit(usize, *const u8),
    /// A source `box`: one value of the budget.
    Boxed,
    /// The next `count` elements of a sequence, `stride` apart from `at`.
    Elements {
        node: usize,
        at: *const u8,
        stride: u64,
        count: u64,
        set: bool,
    },
    Sequential {
        node: usize,
        at: *const u8,
        count: u64,
    },
    /// Rows `row..count` of the Columns node `node` over the block payload `base`.
    Rows {
        node: usize,
        base: *const u8,
        capacity: u64,
        row: u64,
        count: u64,
    },
    /// The next `count` map entries from `at`.
    Entries {
        container: Container,
        at: *const u8,
        count: u64,
    },
    /// The start of one element of the innermost set.
    Mark,
    /// The end of the innermost set: sort its elements' encodings.
    SetEnd,
    /// A decode's check pass: call check `check` on the checked value at `at`, after
    /// the value's own inner occurrences.
    Check(usize, *const u8),
}

/// What a decode's check pass found (SER-1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Verdict {
    /// Every check admitted its occurrence.
    Admitted,
    /// A check returned false.
    Rejected,
    /// A check failed, or its entry was refused; the failure is recorded.
    Failed,
}

/// Append the SER-2 payload of the value at `value` to `s.out`. With `checks`, a decode's
/// check pass over the value it reconstructed: it also calls each checked node's check,
/// inner occurrences first and otherwise in encoding order, and stops at the first check
/// that does not admit its occurrence (SER-1).
unsafe fn encode_payload(
    ctx: *mut TaskCtx,
    program: Program<'_>,
    value: *const u8,
    checks: Option<&[CheckThunk]>,
    s: &mut Scratch,
) -> Stopped<Verdict> {
    let Scratch {
        out,
        put: work,
        sets,
        marks,
        ranges,
        sorted,
        extent,
        ..
    } = s;
    let base = out.len();
    work.clear();
    work.push(Put::Visit(program.root, value));
    // The decoder's value count: one per element and per `box`, two per map entry.
    let mut values = 0u64;
    let mut charge = |n: u64| {
        values += n;
        if values > LIMIT_VALUES {
            Err(budget())
        } else {
            Ok(())
        }
    };
    while let Some(job) = work.pop() {
        if (out.len() - base) as u64 > LIMIT_BYTES {
            return Err(budget());
        }
        match job {
            Put::Check(check, at) => {
                let thunk = checks
                    .and_then(|checks| checks.get(check))
                    .unwrap_or_else(|| fault("a codec check outside the codec's checks"));
                let mut verdict = 0u8;
                // SAFETY: the check borrows the reconstructed value at `at`, of its
                // layout, and returns only after its ordinary cleanup.
                let status = unsafe { thunk(ctx, at, &mut verdict) };
                match (status, verdict) {
                    (crate::STATUS_OK, 1) => (),
                    (crate::STATUS_OK, 0) => return Ok(Verdict::Rejected),
                    (crate::STATUS_OK, _) => fault("a codec check verdict that is not a bool"),
                    // The check recorded its failure; the decode fails (FAIL-3).
                    _ => return Ok(Verdict::Failed),
                }
            }
            Put::Boxed => charge(1)?,
            Put::Mark => {
                if sets.is_empty() {
                    fault("a codec set mark outside a set");
                }
                marks.push(out.len());
            }
            Put::SetEnd => {
                let (start, first) = sets
                    .pop()
                    .unwrap_or_else(|| fault("a codec set end outside a set"));
                ranges.clear();
                let starts = &marks[first..];
                ranges.extend(
                    starts
                        .iter()
                        .zip(starts.iter().skip(1).chain([&out.len()]))
                        .map(|(&a, &b)| (a, b)),
                );
                marks.truncate(first);
                let bytes = &out[..];
                ranges.sort_unstable_by(|&(a, b), &(c, d)| bytes[a..b].cmp(&bytes[c..d]));
                sorted.clear();
                for &(a, b) in ranges.iter() {
                    sorted.extend_from_slice(&out[a..b]);
                }
                out.truncate(start);
                out.extend_from_slice(sorted);
            }
            Put::Elements {
                node,
                at,
                stride,
                count,
                set,
            } => {
                if count == 0 {
                    continue;
                }
                charge(1)?;
                work.push(Put::Elements {
                    node,
                    at: at.wrapping_add(stride as usize),
                    stride,
                    count: count - 1,
                    set,
                });
                work.push(Put::Visit(node, at));
                if set {
                    work.push(Put::Mark);
                }
            }
            Put::Sequential { node, at, count } => {
                if count > 0 {
                    charge(1)?;
                    let n = unsafe { flat_extent(program, node, at, extent) };
                    work.push(Put::Sequential {
                        node,
                        at: at.wrapping_add(n as usize),
                        count: count - 1,
                    });
                    work.push(Put::Visit(node, at));
                }
            }
            Put::Rows {
                node,
                base: block,
                capacity,
                row,
                count,
            } => {
                if row < count {
                    charge(1)?;
                    let Node::Columns { data, columns, .. } = program.node(node) else {
                        fault("codec rows of a node that is not columns");
                    };
                    work.push(Put::Rows {
                        node,
                        base: block,
                        capacity,
                        row: row + 1,
                        count,
                    });
                    for c in columns.chunks_exact(3).rev() {
                        let (prefix, stride, child) = (c[0], c[1], c[2] as usize);
                        work.push(Put::Visit(
                            child,
                            block.wrapping_add((data + capacity * prefix + row * stride) as usize),
                        ));
                    }
                }
            }
            Put::Entries {
                container,
                at,
                count,
            } => {
                if count == 0 {
                    continue;
                }
                work.push(Put::Entries {
                    container,
                    at: at.wrapping_add(container.entries.stride as usize),
                    count: count - 1,
                });
                if !unsafe { container.map.live(at) } {
                    continue;
                }
                charge(if container.set { 1 } else { 2 })?;
                if let Some(value) = container.value {
                    work.push(Put::Visit(
                        value,
                        at.wrapping_add(container.value_at as usize),
                    ));
                }
                work.push(Put::Visit(
                    container.key,
                    at.wrapping_add(container.key_at as usize),
                ));
                if container.set {
                    work.push(Put::Mark);
                }
            }
            Put::Visit(node, at) => {
                unsafe {
                    encode_node(
                        ctx,
                        program,
                        node,
                        at,
                        out,
                        work,
                        sets,
                        marks,
                        extent,
                        checks.is_some(),
                    )
                };
            }
        }
    }
    if (out.len() - base) as u64 > LIMIT_BYTES {
        return Err(budget());
    }
    Ok(Verdict::Admitted)
}

#[allow(clippy::too_many_arguments)]
unsafe fn encode_node(
    ctx: *mut TaskCtx,
    program: Program<'_>,
    node: usize,
    at: *const u8,
    out: &mut Vec<u8>,
    work: &mut Vec<Put>,
    sets: &mut Vec<(usize, usize)>,
    marks: &[usize],
    extent: &mut Vec<usize>,
    checking: bool,
) {
    unsafe {
        match program.node(node) {
            Node::Checked { check, value } => {
                if checking {
                    work.push(Put::Check(check, at));
                }
                work.push(Put::Visit(value, at));
            }
            Node::Scalar(Scalar::Bool) => out.push(u8::from(*at != 0)),
            Node::Scalar(Scalar::Integer(width)) => out.extend_from_slice(read(at, width)),
            Node::Scalar(Scalar::Char) => out.extend_from_slice(read(at, 4)),
            Node::Scalar(Scalar::F32) => {
                let bits = at.cast::<u32>().read_unaligned();
                let bits = if nan32(bits) { 0x7fc0_0000 } else { bits };
                out.extend_from_slice(&bits.to_le_bytes());
            }
            Node::Scalar(Scalar::F64) => {
                let bits = at.cast::<u64>().read_unaligned();
                let bits = if nan64(bits) {
                    0x7ff8_0000_0000_0000
                } else {
                    bits
                };
                out.extend_from_slice(&bits.to_le_bytes());
            }
            Node::Text(text) => {
                let bytes = text_bytes(&text, at);
                out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                out.extend_from_slice(bytes);
            }
            Node::Path => {
                let text = crate::paths::names(ctx).path_text(word(at));
                out.extend_from_slice(&(text.len() as u64).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
            }
            Node::Fields(fields) => {
                for (offset, child) in pairs(fields).rev() {
                    work.push(Put::Visit(child, at.add(offset as usize)));
                }
            }
            Node::Variant {
                niche,
                tag,
                nullary,
                arms,
            } => {
                let ordinal = if niche {
                    if word(at.add(tag as usize)) == 0 {
                        nullary
                    } else {
                        1 - nullary
                    }
                } else {
                    at.add(tag as usize).cast::<u32>().read_unaligned()
                };
                let arm = arms
                    .get(ordinal)
                    .unwrap_or_else(|| fault("a variant value outside its arms"));
                out.extend_from_slice(&ordinal.to_le_bytes());
                for (offset, child) in pairs(arm).rev() {
                    work.push(Put::Visit(child, at.add(offset as usize)));
                }
            }
            Node::Box {
                payload: p, boxed, ..
            } => {
                if boxed {
                    work.push(Put::Boxed);
                }
                work.push(Put::Visit(p, payload(pointer(at))));
            }
            Node::Sequence { elements, element } => {
                let block = payload(pointer(at));
                let count = word(block);
                out.extend_from_slice(&count.to_le_bytes());
                work.push(Put::Elements {
                    node: element,
                    at: block.add(elements.offset as usize),
                    stride: elements.stride,
                    count,
                    set: false,
                });
            }
            Node::Repeat {
                count,
                stride,
                element,
            } => work.push(Put::Elements {
                node: element,
                at,
                stride,
                count,
                set: false,
            }),
            Node::FlatText { .. } => {
                let length = word(at);
                out.extend_from_slice(&length.to_le_bytes());
                out.extend_from_slice(read(at.add(8), length as usize));
            }
            Node::Framed(child) => work.push(Put::Visit(child, at.add(8))),
            Node::FlatFields(fields) => flat_fields(program, fields, at, work, extent),
            Node::FlatVariant(arms) => {
                let ordinal = at.cast::<u32>().read_unaligned();
                out.extend_from_slice(&ordinal.to_le_bytes());
                let fields = arms
                    .get(ordinal)
                    .unwrap_or_else(|| fault("flat variant ordinal"));
                flat_fields(program, fields, at.add(4), work, extent);
            }
            Node::FlatSequence {
                offset,
                stride,
                element,
            } => {
                let count = if offset == 16 {
                    word(at.add(8))
                } else {
                    if stride == 0 {
                        fault("flat sequence without a count");
                    }
                    word(at) / stride
                };
                out.extend_from_slice(&count.to_le_bytes());
                work.push(Put::Sequential {
                    node: element,
                    at: at.add(offset as usize),
                    count,
                });
            }
            Node::Sequential {
                offset, element, ..
            } => {
                let p = payload(pointer(at));
                let count = word(p);
                out.extend_from_slice(&count.to_le_bytes());
                work.push(Put::Sequential {
                    node: element,
                    at: p.add(offset as usize),
                    count,
                });
            }
            Node::Columns { .. } => {
                let p = payload(pointer(at));
                let (count, capacity) = (word(p), word(p.add(8)));
                out.extend_from_slice(&count.to_le_bytes());
                work.push(Put::Rows {
                    node,
                    base: p,
                    capacity,
                    row: 0,
                    count,
                });
            }
            Node::Container(container) => {
                let container_at = payload(pointer(at));
                let entries = payload(pointer(container_at.add(container.entries_at as usize)));
                let count = word(entries);
                let live = word(container_at.add(container.map.live_at as usize));
                out.extend_from_slice(&live.to_le_bytes());
                let first = entries.add(container.entries.offset as usize);
                if container.set {
                    sets.push((out.len(), marks.len()));
                    work.push(Put::SetEnd);
                }
                work.push(Put::Entries {
                    container,
                    at: first,
                    count,
                });
            }
        }
    }
}

/// `codec.encode` (LIR schema revision 29): write a fresh Bytes slice of the SER-2 envelope of
/// the value at `value` at `out`. It cannot fail; past the reference's operational budgets the
/// run stops environmentally.
///
/// # Safety
/// `ctx` is a live run context, `codec` a compiler-emitted codec, `value` a live value of its
/// root node's layout and `out` writable storage of its Bytes layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_codec_encode(
    ctx: *mut TaskCtx,
    codec: *const Codec,
    value: *const c_void,
    out: *mut c_void,
) {
    // SAFETY: caller contract; the encoding's own storage is dropped before a stop.
    unsafe { crate::stop::check(ctx, encode(ctx, codec, value, out)) }
}

unsafe fn encode(
    ctx: *mut TaskCtx,
    codec: *const Codec,
    value: *const c_void,
    out: *mut c_void,
) -> Stopped<()> {
    unsafe {
        let (program, form, descriptor, _) = program(codec);
        let text = program.bytes();
        with_scratch(|s| {
            // An envelope encodes its payload in place after a patched length.
            if form == Form::Envelope {
                s.out.extend_from_slice(MAGIC);
                s.out.extend_from_slice(&WIRE_VERSION.to_le_bytes());
                s.out
                    .extend_from_slice(&(descriptor.len() as u64).to_le_bytes());
                s.out.extend_from_slice(descriptor);
                s.out.extend_from_slice(&[0; 8]);
            }
            let start = s.out.len();
            if encode_payload(ctx, program, value.cast(), None, s)? != Verdict::Admitted {
                fault("an encoding rejected by a check");
            }
            if form == Form::Envelope {
                let length = (s.out.len() - start) as u64;
                s.out[start - 8..start].copy_from_slice(&length.to_le_bytes());
            }
            if s.out.len() as u64 > LIMIT_BYTES {
                return Err(budget());
            }
            std::ptr::write_bytes(out.cast::<u8>(), 0, program.out_size as usize);
            write_text(ctx, &text, out.cast(), &s.out);
            Ok(())
        })
    }
}

// ----- Decoding --------------------------------------------------------------------------

struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, n: u64) -> Result<&'a [u8], DecodeError> {
        let end = (self.at as u64).checked_add(n).ok_or(DecodeError::Length)?;
        if end > self.bytes.len() as u64 {
            return Err(DecodeError::Truncated);
        }
        let bytes = &self.bytes[self.at..end as usize];
        self.at = end as usize;
        Ok(bytes)
    }
    fn number(&mut self, n: usize) -> Result<u128, DecodeError> {
        let mut bytes = [0; 16];
        bytes[..n].copy_from_slice(self.take(n as u64)?);
        Ok(u128::from_le_bytes(bytes))
    }
    fn length(&mut self) -> Result<u64, DecodeError> {
        Ok(self.number(8)? as u64)
    }
}

/// The payload of a complete envelope whose descriptor is `descriptor`.
fn envelope<'a>(bytes: &'a [u8], descriptor: &[u8]) -> Result<&'a [u8], DecodeError> {
    let mut input = Input { bytes, at: 0 };
    if input.take(4)? != MAGIC || input.number(4)? != u128::from(WIRE_VERSION) {
        return Err(DecodeError::Version);
    }
    let length = input.length()?;
    if input.take(length)? != descriptor {
        return Err(DecodeError::SchemaMismatch);
    }
    let length = input.length()?;
    let payload = input.take(length)?;
    if input.at != bytes.len() {
        return Err(DecodeError::Trailing);
    }
    Ok(payload)
}

enum Check {
    Visit(usize),
    /// The next `count` elements of a sequence or set (`key`: each one a set key).
    Elements {
        node: usize,
        count: u64,
        key: bool,
    },
    /// The next `count` map entries.
    Entries {
        key: usize,
        value: usize,
        count: u64,
    },
    KeyEnd {
        at: usize,
        set: bool,
    },
    CollectionStart,
    CollectionEnd,
}

#[derive(Default)]
struct Collection<'a> {
    keys: BTreeSet<&'a [u8]>,
    previous: Option<&'a [u8]>,
}

/// Validate a whole payload against the program, in the reference's reading order.
fn validate(
    program: Program<'_>,
    bytes: &[u8],
    work: &mut Vec<Check>,
    minimum: &mut Minimum,
) -> Result<(), DecodeError> {
    let mut input = Input { bytes, at: 0 };
    // A root or chosen arm with no finite encoding is truncated input: no bytes hold it.
    if minimum.of(program)[program.root] == u64::MAX {
        return Err(DecodeError::Truncated);
    }
    work.clear();
    work.push(Check::Visit(program.root));
    let mut collections: Vec<Collection<'_>> = Vec::new();
    // Each count charges its elements (a map's twice) before any is read, and each `box`
    // one.
    let mut values = 0u64;
    let mut charge = |n: u64| {
        if n > LIMIT_VALUES - values {
            return Err(DecodeError::Limit);
        }
        values += n;
        Ok(())
    };
    while let Some(job) = work.pop() {
        match job {
            Check::CollectionStart => collections.push(Collection::default()),
            Check::CollectionEnd => {
                collections.pop();
            }
            Check::KeyEnd { at, set } => {
                let key = &input.bytes[at..input.at];
                let collection = collections
                    .last_mut()
                    .unwrap_or_else(|| fault("a codec key outside a collection"));
                if set {
                    if let Some(previous) = collection.previous {
                        if previous == key {
                            return Err(DecodeError::Duplicate.into());
                        }
                        if previous > key {
                            return Err(DecodeError::Order.into());
                        }
                    }
                    collection.previous = Some(key);
                } else if !collection.keys.insert(key) {
                    return Err(DecodeError::Duplicate.into());
                }
            }
            Check::Elements { node, count, key } => {
                if count == 0 {
                    continue;
                }
                work.push(Check::Elements {
                    node,
                    count: count - 1,
                    key,
                });
                if key {
                    work.push(Check::KeyEnd {
                        at: input.at,
                        set: true,
                    });
                }
                work.push(Check::Visit(node));
            }
            Check::Entries { key, value, count } => {
                if count == 0 {
                    continue;
                }
                work.push(Check::Entries {
                    key,
                    value,
                    count: count - 1,
                });
                work.push(Check::Visit(value));
                work.push(Check::KeyEnd {
                    at: input.at,
                    set: false,
                });
                work.push(Check::Visit(key));
            }
            Check::Visit(node) => {
                match program.node(node) {
                    Node::Scalar(Scalar::Bool) => {
                        if input.number(1)? > 1 {
                            return Err(DecodeError::Scalar.into());
                        }
                    }
                    Node::Scalar(Scalar::Integer(width)) => {
                        input.take(width as u64)?;
                    }
                    Node::Scalar(Scalar::F32) => {
                        let bits = input.number(4)? as u32;
                        if nan32(bits) && bits != 0x7fc0_0000 {
                            return Err(DecodeError::Scalar.into());
                        }
                    }
                    Node::Scalar(Scalar::Char) => {
                        if char::from_u32(input.number(4)? as u32).is_none() {
                            return Err(DecodeError::Scalar.into());
                        }
                    }
                    Node::Scalar(Scalar::F64) => {
                        let bits = input.number(8)? as u64;
                        if nan64(bits) && bits != 0x7ff8_0000_0000_0000 {
                            return Err(DecodeError::Scalar.into());
                        }
                    }
                    Node::Text(Text { utf8, .. }) | Node::FlatText { utf8 } => {
                        let n = input.length()?;
                        let bytes = input.take(n)?;
                        if utf8 && std::str::from_utf8(bytes).is_err() {
                            return Err(DecodeError::Utf8.into());
                        }
                    }
                    // `path.of` admits any text.
                    Node::Path => {
                        let n = input.length()?;
                        if std::str::from_utf8(input.take(n)?).is_err() {
                            return Err(DecodeError::Utf8.into());
                        }
                    }
                    Node::Fields(fields) => {
                        work.extend(pairs(fields).rev().map(|(_, f)| Check::Visit(f)));
                    }
                    Node::Variant { arms, .. } => {
                        let tag = input.number(4)? as u32;
                        let arm = arms.get(tag).ok_or(DecodeError::Tag)?;
                        let sizes = minimum.of(program);
                        if pairs(arm).fold(0u64, |n, (_, f)| n.saturating_add(sizes[f])) == u64::MAX
                        {
                            return Err(DecodeError::Truncated);
                        }
                        work.extend(pairs(arm).rev().map(|(_, f)| Check::Visit(f)));
                    }
                    Node::Framed(child) | Node::Checked { value: child, .. } => {
                        work.push(Check::Visit(child))
                    }
                    Node::FlatFields(fields) => work.extend(ids(fields).rev().map(Check::Visit)),
                    Node::FlatVariant(arms) => {
                        let tag = input.number(4)? as u32;
                        let arm = arms.get(tag).ok_or(DecodeError::Tag)?;
                        let sizes = minimum.of(program);
                        if ids(arm).fold(0u64, |n, f| n.saturating_add(sizes[f])) == u64::MAX {
                            return Err(DecodeError::Truncated);
                        }
                        work.extend(ids(arm).rev().map(Check::Visit));
                    }
                    Node::Box { payload, boxed, .. } => {
                        if boxed {
                            charge(1)?;
                        }
                        work.push(Check::Visit(payload))
                    }
                    Node::Sequence { element, .. }
                    | Node::FlatSequence { element, .. }
                    | Node::Sequential { element, .. }
                    | Node::Columns { element, .. } => {
                        let count = input.length()?;
                        satisfiable(&input, count, minimum.of(program)[element])?;
                        charge(count)?;
                        work.push(Check::Elements {
                            node: element,
                            count,
                            key: false,
                        });
                    }
                    Node::Repeat { count, element, .. } => {
                        charge(count)?;
                        work.push(Check::Elements {
                            node: element,
                            count,
                            key: false,
                        });
                    }
                    Node::Container(container) => {
                        let count = input.length()?;
                        let sizes = minimum.of(program);
                        let each = container
                            .value
                            .map_or(0, |value| sizes[value])
                            .saturating_add(sizes[container.key]);
                        satisfiable(&input, count, each)?;
                        charge(count.saturating_mul(if container.set { 1 } else { 2 }))?;
                        work.push(Check::CollectionEnd);
                        work.push(match container.value {
                            None => Check::Elements {
                                node: container.key,
                                count,
                                key: true,
                            },
                            Some(value) => Check::Entries {
                                key: container.key,
                                value,
                                count,
                            },
                        });
                        work.push(Check::CollectionStart);
                    }
                }
            }
        }
    }
    if input.at != bytes.len() {
        return Err(DecodeError::Trailing.into());
    }
    Ok(())
}

enum Build {
    Visit(usize, *mut u8),
    Elements {
        node: usize,
        at: *mut u8,
        stride: u64,
        count: u64,
    },
    Entries {
        container: Container,
        at: *mut u8,
        count: u64,
    },
}

/// A fresh zeroed heap block for `size` payload bytes.
unsafe fn block(ctx: *mut TaskCtx, index: u32, size: u64, align: u64) -> *mut Header {
    unsafe {
        let block = hirsel_rt_v2_alloc(ctx, index, size, align.max(8), DOMAIN_HEAP, 0);
        std::ptr::write_bytes(payload(block), 0, size as usize);
        block
    }
}

/// A fresh sequence block of `count` zeroed elements.
unsafe fn sequence(ctx: *mut TaskCtx, e: &Elements, count: u64) -> *mut Header {
    unsafe {
        let block = allocate_sequence(ctx, e.index, count, e.stride, e.align);
        std::ptr::write_bytes(
            payload(block).add(e.offset as usize),
            0,
            (count * e.stride) as usize,
        );
        block
    }
}

/// Build the validated payload's value at `out` (zeroed storage of the root node's layout).
/// Validation has succeeded, so every read succeeds.
unsafe fn materialize(
    ctx: *mut TaskCtx,
    program: Program<'_>,
    bytes: &[u8],
    out: *mut u8,
    s: &mut Scratch,
) {
    let Scratch {
        build: work,
        flat,
        flat_jobs,
        content,
        inline,
        ..
    } = s;
    let mut input = Input { bytes, at: 0 };
    let checked = |r: Result<u128, DecodeError>| r.unwrap_or_else(|_| fault("codec reread"));
    work.clear();
    work.push(Build::Visit(program.root, out));
    while let Some(job) = work.pop() {
        let (node, at) = match job {
            Build::Visit(node, at) => (node, at),
            Build::Elements {
                node,
                at,
                stride,
                count,
            } => {
                if count > 0 {
                    work.push(Build::Elements {
                        node,
                        at: at.wrapping_add(stride as usize),
                        stride,
                        count: count - 1,
                    });
                    work.push(Build::Visit(node, at));
                }
                continue;
            }
            Build::Entries {
                container,
                at,
                count,
            } => {
                if count > 0 {
                    unsafe {
                        container.map.publish(at);
                    }
                    work.push(Build::Entries {
                        container,
                        at: at.wrapping_add(container.entries.stride as usize),
                        count: count - 1,
                    });
                    if let Some(value) = container.value {
                        work.push(Build::Visit(
                            value,
                            at.wrapping_add(container.value_at as usize),
                        ));
                    }
                    work.push(Build::Visit(
                        container.key,
                        at.wrapping_add(container.key_at as usize),
                    ));
                }
                continue;
            }
        };
        unsafe {
            match program.node(node) {
                Node::Scalar(Scalar::Bool) => *at = checked(input.number(1)) as u8,
                Node::Scalar(scalar) => {
                    let n = scalar.width();
                    write(at, &checked(input.number(n)).to_le_bytes()[..n])
                }
                Node::Text(text) => {
                    let n = checked(input.number(8)) as u64;
                    let bytes = input.take(n).unwrap_or_else(|_| fault("codec reread"));
                    write_text(ctx, &text, at, bytes);
                }
                Node::Path => {
                    let n = checked(input.number(8)) as u64;
                    let bytes = input.take(n).unwrap_or_else(|_| fault("codec reread"));
                    let text = std::str::from_utf8(bytes).unwrap_or_else(|_| fault("codec reread"));
                    let id = crate::paths::names(ctx).paths.intern(text);
                    write(at, &id.to_le_bytes());
                }
                Node::Fields(fields) => {
                    for (offset, child) in pairs(fields).rev() {
                        work.push(Build::Visit(child, at.add(offset as usize)));
                    }
                }
                Node::Checked { value, .. } => {
                    if program.root_descriptor.is_none() {
                        fault("a codec program whose checks and root descriptor disagree");
                    }
                    work.push(Build::Visit(value, at))
                }
                Node::Variant { .. } => {
                    let ordinal = checked(input.number(4)) as u32;
                    for (offset, child) in pairs(program.write_arm(node, at, ordinal)).rev() {
                        work.push(Build::Visit(child, at.add(offset as usize)));
                    }
                }
                Node::Box {
                    index,
                    size,
                    align,
                    payload: p,
                    ..
                } => {
                    if size == NO_NODE {
                        flat.clear();
                        flat_decode(program, p, &mut input, flat, flat_jobs);
                        let b = block(ctx, index, flat.len() as u64, align);
                        write(payload(b), flat);
                        set_pointer(at, b);
                    } else {
                        let b = block(ctx, index, size, align);
                        set_pointer(at, b);
                        work.push(Build::Visit(p, payload(b)));
                    }
                }
                Node::Sequence { elements, element } => {
                    let count = checked(input.number(8)) as u64;
                    let b = sequence(ctx, &elements, count);
                    set_pointer(at, b);
                    work.push(Build::Elements {
                        node: element,
                        at: payload(b).add(elements.offset as usize),
                        stride: elements.stride,
                        count,
                    });
                }
                Node::Repeat {
                    count,
                    stride,
                    element,
                } => work.push(Build::Elements {
                    node: element,
                    at,
                    stride,
                    count,
                }),
                Node::FlatText { .. }
                | Node::Framed(_)
                | Node::FlatFields(_)
                | Node::FlatVariant(_)
                | Node::FlatSequence { .. } => {
                    flat.clear();
                    flat_decode(program, node, &mut input, flat, flat_jobs);
                    write(at, flat);
                }
                Node::Sequential {
                    index,
                    align,
                    offset,
                    element,
                } => {
                    let count = checked(input.number(8)) as u64;
                    content.clear();
                    for _ in 0..count {
                        inline_decode(
                            program, element, &mut input, content, inline, flat, flat_jobs,
                        );
                    }
                    let used = content.len() as u64;
                    let b = block(
                        ctx,
                        index,
                        offset
                            .checked_add(used)
                            .unwrap_or_else(|| fault("a validated codec extent overflows")),
                        align,
                    );
                    write(payload(b), &count.to_le_bytes());
                    write(payload(b).add(8), &used.to_le_bytes());
                    write(payload(b).add(offset as usize), content);
                    set_pointer(at, b);
                }
                Node::Columns {
                    index,
                    align,
                    data,
                    quantum,
                    columns,
                    ..
                } => {
                    let count = checked(input.number(8)) as u64;
                    let capacity = count
                        .checked_add(quantum - 1)
                        .unwrap_or_else(|| fault("a validated codec extent overflows"))
                        & !(quantum - 1);
                    let width: u64 = columns.chunks_exact(3).map(|c| c[1]).sum();
                    let size = capacity
                        .checked_mul(width)
                        .and_then(|n| data.checked_add(n))
                        .unwrap_or_else(|| fault("a validated codec extent overflows"));
                    let b = block(ctx, index, size, align);
                    write(payload(b), &count.to_le_bytes());
                    write(payload(b).add(8), &capacity.to_le_bytes());
                    set_pointer(at, b);
                    for row in (0..count).rev() {
                        for c in columns.chunks_exact(3).rev() {
                            let (prefix, stride, child) = (c[0], c[1], c[2] as usize);
                            work.push(Build::Visit(
                                child,
                                payload(b).add((data + capacity * prefix + row * stride) as usize),
                            ));
                        }
                    }
                }
                Node::Container(container) => {
                    let count = checked(input.number(8)) as u64;
                    let c = block(ctx, container.index, container.size, container.align);
                    let entries = sequence(ctx, &container.entries, count);
                    // An empty index is a valid unindexed container.
                    let index = allocate_sequence(
                        ctx,
                        container.index_index,
                        0,
                        container.index_stride,
                        container.index_align,
                    );
                    set_pointer(payload(c).add(container.entries_at as usize), entries);
                    set_pointer(payload(c).add(container.index_at as usize), index);
                    let map = container.map;
                    let hashes = allocate_sequence(
                        ctx,
                        container.index_index,
                        0,
                        container.index_stride,
                        container.index_align,
                    );
                    set_pointer(payload(c).add(map.hashes_at as usize), hashes);
                    write(payload(c).add(map.live_at as usize), &count.to_le_bytes());
                    set_pointer(at, c);
                    let first = payload(entries).add(container.entries.offset as usize);
                    work.push(Build::Entries {
                        container,
                        at: first,
                        count,
                    });
                }
            }
        }
    }
    if input.at != bytes.len() {
        fault("a codec rebuild that did not read its whole payload");
    }
}

/// `codec.decode` (LIR schema revision 29): validate the Bytes slice at `bytes` completely,
/// then write `Ok` of fresh owned data or `Error` of its first `DecodeError` at `out` and return
/// `STATUS_OK`. Since LIR revision 69 it fails when a check fails or its entry is refused: the
/// decoded value is released whole first, `out` is left zeroed, and it returns
/// `STATUS_FAILURE` with the failure recorded (FAIL-3). Past the reference's operational budgets
/// the run stops environmentally.
///
/// # Safety
/// `ctx` is a live run context, `codec` a compiler-emitted codec, `bytes` a live Bytes value of
/// its Bytes node's layout and `out` writable storage of its result layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_codec_decode(
    ctx: *mut TaskCtx,
    codec: *const Codec,
    bytes: *const c_void,
    out: *mut c_void,
) -> u8 {
    // SAFETY: this semantic entry owns its task until it suspends or returns.
    let _role = unsafe { crate::tasks::role::Guard::new(ctx) };
    // SAFETY: caller contract; the decoder's own storage is dropped before a stop.
    unsafe { crate::stop::check(ctx, with_scratch(|s| decode(ctx, codec, bytes, out, s))) }
}

unsafe fn decode(
    ctx: *mut TaskCtx,
    codec: *const Codec,
    bytes: *const c_void,
    out: *mut c_void,
    s: &mut Scratch,
) -> Stopped<u8> {
    unsafe {
        let (program, form, descriptor, checks) = program(codec);
        let input = text_bytes(&program.bytes(), bytes.cast());
        let out = out.cast::<u8>();
        std::ptr::write_bytes(out, 0, program.out_size as usize);
        let payload = if input.len() as u64 > LIMIT_BYTES {
            Err(DecodeError::Limit)
        } else {
            match form {
                Form::Envelope => envelope(input, descriptor),
                Form::Payload => Ok(input),
            }
        };
        let checked = match payload {
            Ok(payload) => {
                validate(program, payload, &mut s.check, &mut s.minimum).map(|()| payload)
            }
            Err(invalid) => Err(invalid),
        };
        let checked = match checked {
            Ok(payload) => {
                let &[offset, _] = program.write_arm(program.out, out, 0) else {
                    fault("a codec result whose Ok arm is not one field");
                };
                let value = out.add(offset as usize);
                materialize(ctx, program, payload, value, s);
                match program.root_descriptor {
                    None => Ok(()),
                    // SER-1: every check admits its occurrence, or the value is released,
                    // here and only here; decoded types are inert, so no finalizer runs.
                    Some(index) => {
                        s.out.clear();
                        let verdict = encode_payload(ctx, program, value, Some(checks), s)?;
                        if verdict == Verdict::Admitted {
                            Ok(())
                        } else {
                            let mut cleanup =
                                std::mem::MaybeUninit::<crate::finalize::Cleanup>::uninit();
                            let status = crate::finalize::hirsel_rt_v2_finalize(
                                ctx,
                                index,
                                value,
                                std::ptr::null(),
                                cleanup.as_mut_ptr(),
                                0,
                                0,
                            );
                            if status != crate::STATUS_OK {
                                fault("a decoded value's release failed");
                            }
                            std::ptr::write_bytes(out, 0, program.out_size as usize);
                            if verdict == Verdict::Failed {
                                return Ok(crate::STATUS_FAILURE);
                            }
                            Err(DecodeError::Invalid)
                        }
                    }
                }
            }
            Err(invalid) => Err(invalid),
        };
        match checked {
            Ok(()) => (),
            Err(error) => {
                let &[offset, error_node] = program.write_arm(program.out, out, 1) else {
                    fault("a codec result whose Error arm is not one field");
                };
                let none =
                    program.write_arm(error_node as usize, out.add(offset as usize), error as u32);
                if !none.is_empty() {
                    fault("a DecodeError arm with fields");
                }
            }
        }
    }
    Ok(crate::STATUS_OK)
}
