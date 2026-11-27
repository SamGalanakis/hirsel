//! Where a counted drain keeps a suspended fixed block's frame (node-bytes-1076).
//!
//! A fixed block (cleanup class [`Cleanup::Fixed`](crate::Cleanup)) carries only
//! its payer word in front of the payload. When its counted cleanup yields a
//! descendant at a suspension point, the drain writes two words into payload
//! words that cleanup will never read again: *A*, the address of the point's
//! static continuation record, and *B*, the next frame link. A point inside an
//! inline array also keeps the full cursor in an index word, because the record
//! is shared by every element. All three offsets are fixed and payload-relative,
//! and none lies inside an array that is not yet finished, so the payload base
//! is always *A*'s address minus *A*'s offset. A type short of words at some
//! point gets private spare words after its value; they are physical only and
//! never charged (MEM-24).
//!
//! Points and steps follow [`DropRecipe`](crate::DropRecipe)'s counted visit order:
//! a `Box`, `Rc`, `Arc`, `Seq` or `Map` field is one step, a `Seq` of zero-sized
//! elements none, and a row with a finalizer takes one step for its finalizer
//! (step 0 of the row) before its fields. A resource is one step that hands the
//! resource itself to the drain: its storage holds no frame word, since its
//! frame lives in its native state. The fixed block of a `Box`, `Rc` or `Arc`
//! of a resource holds that one pointer, so its frames are a single last point.

use crate::{FieldTy, Layout, RecipeRow, Row, Tag, TypeRef, cleanup_class, field_size};

/// One place where cleanup of a fixed value yields a descendant, in visit order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    /// The cursor after the yield: steps taken, counting the yielded field. For a
    /// point inside an array, the value for every enclosing index 0; the index
    /// word holds the real one.
    pub cursor: u64,
    /// Payload-relative offset of the record word.
    pub a: u64,
    /// Payload-relative offset of the next-frame word.
    pub b: u64,
    /// Payload-relative offset of the full cursor, for a point inside an array.
    pub index: Option<u64>,
    /// No cleanup step follows the yield on this path, so once the descendant
    /// is done the block is finished and the drain frees it without resuming
    /// its glue. Never set inside an array.
    pub last: bool,
}

/// The suspension points of one fixed type and the spare words its blocks need.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frames {
    pub points: Vec<Point>,
    /// Private 8-byte words after round8(size), on every block of the type.
    pub spares: u64,
}

impl Frames {
    /// Native payload bytes of a block of this type that holds `bytes` of value.
    pub fn native_bytes(&self, bytes: u64) -> u64 {
        if self.spares == 0 {
            bytes
        } else {
            bytes.next_multiple_of(8) + 8 * self.spares
        }
    }
}

enum Node {
    Empty,
    /// A one-step field at `at`: `yields` when its cleanup hands over a block,
    /// `live` the byte ranges read while it is still to come.
    Leaf {
        at: u64,
        live: Vec<(u64, u64)>,
        yields: Option<bool>,
    },
    Fields(Vec<Node>),
    Variant(Tag, Vec<Node>),
    Array {
        at: u64,
        n: u64,
        stride: u64,
        elem: Box<Node>,
    },
}

impl Node {
    fn steps(&self) -> u64 {
        match self {
            Node::Empty => 0,
            Node::Leaf { .. } => 1,
            Node::Fields(f) => f.iter().map(Node::steps).sum(),
            Node::Variant(_, c) => c.iter().map(Node::steps).max().unwrap_or(0),
            Node::Array { n, elem, .. } => n * elem.steps(),
        }
    }
}

fn plan_type(shapes: &[Row], recipes: &[RecipeRow], t: TypeRef, at: u64) -> Node {
    let r = &recipes[t.index()];
    let (Some(d), Layout::Fixed(f)) = (&r.drop, &r.layout) else {
        return Node::Empty;
    };
    let rows: Vec<Node> = d
        .fields
        .iter()
        .map(|row| {
            Node::Fields(
                row.iter()
                    .map(|fd| plan_field(shapes, recipes, &fd.ty, at + fd.offset))
                    .collect(),
            )
        })
        .collect();
    let fields = match f.tag {
        Some(tag) => Node::Variant(
            Tag {
                offset: at + tag.offset,
                ..tag
            },
            rows,
        ),
        None => rows.into_iter().next().unwrap_or(Node::Empty),
    };
    // A finalizer is step 0 of its row and never yields; it may read the whole
    // row, so no frame word goes there before it has run (FAIL-8).
    if d.finalizer.is_some() {
        Node::Fields(vec![
            Node::Leaf {
                at,
                live: vec![(at, f.size)],
                yields: None,
            },
            fields,
        ])
    } else {
        fields
    }
}

fn plan_field(shapes: &[Row], recipes: &[RecipeRow], ty: &FieldTy, at: u64) -> Node {
    let fixed =
        |edge: &FieldTy| cleanup_class(shapes, recipes, edge) != Some(crate::Cleanup::Terminal);
    // Cleaned whole by its ordinary glue in one step that never yields: the
    // pointer (or handle word, or callable pair) is all the drain reads.
    let whole = |len: u64| Node::Leaf {
        at,
        live: vec![(at, len)],
        yields: None,
    };
    match ty {
        // The resource pointer is read once, to hand the resource over.
        FieldTy::Inline(t) if shapes[t.index()].repr == crate::Repr::Resource => Node::Leaf {
            at,
            live: vec![(at, 8)],
            yields: Some(false),
        },
        FieldTy::Inline(t) => plan_type(shapes, recipes, *t, at),
        FieldTy::FrozenBytes => whole(crate::SEQ_CARRIER),
        FieldTy::Seq(_) | FieldTy::Columns(_) if crate::storage_free(shapes, ty) => Node::Empty,
        FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) => Node::Leaf {
            at,
            live: vec![(at, 8)],
            yields: fixed(ty).then_some(false),
        },
        // A counted drain frees terminal storage from its pointer and capacity
        // alone (cranelift resume.rs `enqueue_buffer`, llvm `enqueue_collection`);
        // its length is never read there, and frames exist only in such drains.
        FieldTy::Seq(_) | FieldTy::Columns(_) | FieldTy::Map(..) if !fixed(ty) => Node::Leaf {
            at,
            live: vec![(at, 8), (at + 16, 8)],
            yields: None,
        },
        FieldTy::Seq(_) | FieldTy::Columns(_) | FieldTy::Map(..) => Node::Leaf {
            at,
            live: vec![(at, crate::SEQ_CARRIER)],
            yields: Some(true),
        },
        FieldTy::Array(e, n) => {
            let elem = plan_field(shapes, recipes, e, at);
            if elem.steps() == 0 {
                Node::Empty
            } else {
                Node::Array {
                    at,
                    n: *n,
                    stride: field_size(shapes, recipes, e).0,
                    elem: Box::new(elem),
                }
            }
        }
        FieldTy::Handle(crate::Handle::Capability) => Node::Empty,
        // A runtime holder hands its cleanup over to the drain; only the handle
        // word is read, and its record keeps the holder's own frame.
        FieldTy::Handle(
            crate::Handle::Held
            | crate::Handle::Receiver
            | crate::Handle::Task
            | crate::Handle::Latest,
        ) => Node::Leaf {
            at,
            live: vec![(at, 8)],
            yields: Some(false),
        },
        FieldTy::Handle(_) | FieldTy::Shared(_) => whole(8),
        // An owned closure hands its environment, when it has one, to the drain
        // under the descriptor the environment names; only that pointer is read.
        FieldTy::OwnFn => Node::Leaf {
            at,
            live: vec![(at + 8, 8)],
            yields: Some(false),
        },
        FieldTy::Scalar(_)
        | FieldTy::RegionRef(_)
        | FieldTy::Fn
        | FieldTy::Borrow
        | FieldTy::View(_)
        | FieldTy::Locator(_) => Node::Empty,
        // Compact data is plain bytes inside its blob: nothing to clean (MEM-9).
        FieldTy::Text
        | FieldTy::FixedSpan(_)
        | FieldTy::SeqSpan(_)
        | FieldTy::IndexedSpan(_)
        | FieldTy::Link(_) => Node::Empty,
    }
}

/// A yield site with the path to it: the chosen case of each enclosing variant
/// and whether an array encloses it.
struct Site {
    rank: u64,
    /// Bytes `[at, at + live)` of the yielded field still read while its
    /// descendant drains: a buffer's data pointer and length.
    keep: (u64, u64),
    cases: Vec<usize>,
    array: Option<(u64, u64)>,
}

fn sites(
    node: &Node,
    rank: u64,
    cases: &mut Vec<usize>,
    array: Option<(u64, u64)>,
    out: &mut Vec<Site>,
) {
    match node {
        Node::Empty => {}
        Node::Leaf { at, yields, .. } => {
            if let Some(buffer) = *yields {
                out.push(Site {
                    rank,
                    keep: (*at, if buffer { 16 } else { 0 }),
                    cases: cases.clone(),
                    array,
                });
            }
        }
        Node::Fields(fields) => {
            let mut r = rank;
            for f in fields {
                sites(f, r, cases, array, out);
                r += f.steps();
            }
        }
        Node::Variant(_, rows) => {
            for (i, row) in rows.iter().enumerate() {
                cases.push(i);
                sites(row, rank, cases, array, out);
                cases.pop();
            }
        }
        Node::Array {
            at,
            n,
            stride,
            elem,
        } => {
            let outer = array.unwrap_or((*at, n * stride));
            sites(elem, rank, cases, Some(outer), out);
        }
    }
}

/// Mark the bytes cleanup may still read when it resumes after `site`.
fn live(node: &Node, rank: u64, site: &Site, depth: &mut usize, on_path: bool, bytes: &mut [bool]) {
    let mark = |at: u64, len: u64, bytes: &mut [bool]| {
        bytes[at as usize..(at + len) as usize].fill(true);
    };
    let end = rank + node.steps();
    match node {
        Node::Empty => {}
        Node::Leaf { live: ranges, .. } => {
            if rank > site.rank || (rank == site.rank && !on_path) {
                for &(at, len) in ranges {
                    mark(at, len, bytes);
                }
            }
        }
        Node::Fields(fields) => {
            let mut r = rank;
            for f in fields {
                let here = on_path && r <= site.rank && site.rank < r + f.steps();
                live(f, r, site, depth, here, bytes);
                r += f.steps();
            }
        }
        Node::Variant(tag, rows) => {
            mark(tag.offset, tag.width, bytes);
            if on_path {
                let case = site.cases[*depth];
                *depth += 1;
                live(&rows[case], rank, site, depth, true, bytes);
                *depth -= 1;
            } else {
                for row in rows {
                    live(row, rank, site, depth, false, bytes);
                }
            }
        }
        Node::Array {
            at,
            n,
            stride,
            elem,
        } => {
            if on_path || end > site.rank {
                mark(*at, n * stride, bytes);
            } else {
                for i in 0..*n {
                    tags(elem, i * stride, bytes);
                }
            }
        }
    }
}

/// Whether cleanup still has a step to take on `site`'s path after it yields.
fn later(node: &Node, rank: u64, site: &Site, depth: &mut usize) -> bool {
    match node {
        Node::Empty | Node::Leaf { .. } => false,
        Node::Fields(fields) => {
            let mut r = rank;
            for f in fields {
                if r > site.rank && f.steps() > 0 {
                    return true;
                }
                if r <= site.rank && site.rank < r + f.steps() && later(f, r, site, depth) {
                    return true;
                }
                r += f.steps();
            }
            false
        }
        Node::Variant(_, rows) => {
            let case = site.cases[*depth];
            *depth += 1;
            let more = later(&rows[case], rank, site, depth);
            *depth -= 1;
            more
        }
        Node::Array { .. } => true,
    }
}

/// Every discriminant byte under a finished subtree, shifted by `shift`.
fn tags(node: &Node, shift: u64, bytes: &mut [bool]) {
    match node {
        Node::Variant(tag, rows) => {
            bytes[(tag.offset + shift) as usize..(tag.offset + shift + tag.width) as usize]
                .fill(true);
            for row in rows {
                tags(row, shift, bytes);
            }
        }
        Node::Fields(f) => f.iter().for_each(|n| tags(n, shift, bytes)),
        Node::Array {
            n, stride, elem, ..
        } => {
            for i in 0..*n {
                tags(elem, shift + i * stride, bytes);
            }
        }
        Node::Empty | Node::Leaf { .. } => {}
    }
}

/// The suspension points of fixed type `t` and its spare words. Empty for a
/// type whose cleanup never yields.
pub fn frames(shapes: &[Row], recipes: &[RecipeRow], t: TypeRef) -> Frames {
    let Layout::Fixed(fixed) = &recipes[t.index()].layout else {
        return Frames::default();
    };
    // A value of a resource type is its pointer.
    let (root, size) = if shapes[t.index()].repr == crate::Repr::Resource {
        (plan_field(shapes, recipes, &FieldTy::Inline(t), 0), 8)
    } else {
        (plan_type(shapes, recipes, t, 0), fixed.size)
    };
    let mut found = Vec::new();
    sites(&root, 0, &mut Vec::new(), None, &mut found);
    let words = size / 8;
    let mut chosen = Vec::new();
    let mut spares = 0;
    for site in &found {
        let mut bytes = vec![false; size as usize];
        live(&root, 0, site, &mut 0, true, &mut bytes);
        bytes[site.keep.0 as usize..(site.keep.0 + site.keep.1) as usize].fill(true);
        if let Some((at, len)) = site.array {
            bytes[at as usize..(at + len) as usize].fill(true);
        }
        let need = 2 + u64::from(site.array.is_some());
        let mut free: Vec<u64> = (0..words)
            .map(|w| w * 8)
            .filter(|&w| bytes[w as usize..w as usize + 8].iter().all(|b| !b))
            .take(need as usize)
            .collect();
        spares = spares.max(need - free.len() as u64);
        chosen.push((
            site.rank + 1,
            site.array.is_some(),
            site.array.is_none() && !later(&root, 0, site, &mut 0),
            std::mem::take(&mut free),
        ));
    }
    let spare = size.next_multiple_of(8);
    let points = chosen
        .into_iter()
        .map(|(cursor, in_array, last, mut free)| {
            let mut j = 0;
            while free.len() < 2 + usize::from(in_array) {
                free.push(spare + 8 * j);
                j += 1;
            }
            Point {
                cursor,
                a: free[0],
                b: free[1],
                index: in_array.then(|| free[2]),
                last,
            }
        })
        .collect();
    Frames { points, spares }
}

/// Uncharged spare words after the value in a block owned by `edge` (zero
/// unless the edge's cleanup is [`Cleanup::Fixed`](crate::Cleanup) or, for the
/// block holding a resource pointer, [`Cleanup::Resource`](crate::Cleanup)). A counted
/// node's value starts at 8, so for both owners the block's native bytes are
/// round8(bytes) + 8 * spares when spares is nonzero.
pub fn spare_words(shapes: &[Row], recipes: &[RecipeRow], edge: &FieldTy) -> u64 {
    match edge {
        FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t)
            if matches!(
                cleanup_class(shapes, recipes, edge),
                Some(crate::Cleanup::Fixed | crate::Cleanup::Resource)
            ) =>
        {
            frames(shapes, recipes, *t).spares
        }
        _ => 0,
    }
}
