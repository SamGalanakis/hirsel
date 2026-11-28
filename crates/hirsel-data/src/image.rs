//! Construction into unlinked blocks. Both engines admit the owned blocks and
//! relocate their pointers before copying the complete result to its destination.
use super::decoded::{host_memory, storage};
use super::guest::{Construction, Field, Plan};
use super::*;
use hirsel_core_layout::{case_word, charge, map_hash, map_index as index};

pub struct BlockImage {
    pub bytes: Vec<u8>,
    pub align: u64,
    pub charge: u64,
    pub graph: bool,
    pub parent: Option<(usize, usize)>,
    pub address: u64,
}

#[derive(Default)]
pub struct Image {
    pub blocks: Vec<BlockImage>,
}

type Result<T> = std::result::Result<T, DataError>;
type Pay<'a> = dyn FnMut(u64) -> Result<()> + 'a;
type Place = (usize, usize);

impl Image {
    pub fn prepare(
        descriptor: &Descriptor,
        plan: &Plan,
        bytes: &[u8],
        limits: Limits,
        key: map_hash::PreparedKey,
        pay: &mut Pay<'_>,
    ) -> Result<Self> {
        let root = &plan.nodes[plan.result];
        let mut out = Builder {
            image: Image { blocks: Vec::new() },
            plan,
            key,
            pay,
        };
        out.block(None, root.size, root.align, Some(0), false)?;
        match crate::read::guest_with_budget(bytes, descriptor, limits, out.pay) {
            Ok(value) => {
                let fields = out.case((0, 0), plan.result, "Ok");
                out.value((0, fields[0].offset as usize), fields[0].node, &value)?;
            }
            Err(e) if matches!(e.kind, ErrorKind::HostMemory | ErrorKind::Capacity { .. }) => {
                return Err(e);
            }
            Err(e) => {
                let field = &out.case((0, 0), plan.result, "Error")[0];
                out.error((0, field.offset as usize), field.node, &e)?;
            }
        }
        Ok(out.image)
    }
}

struct Builder<'s, 'a> {
    image: Image,
    plan: &'s Plan,
    key: map_hash::PreparedKey,
    pay: &'a mut Pay<'a>,
}

fn add((block, off): Place, n: u64) -> Place {
    (block, off + n as usize)
}

impl<'s> Builder<'s, '_> {
    fn block(
        &mut self,
        parent: Option<Place>,
        size: u64,
        align: u64,
        charge: Option<u64>,
        graph: bool,
    ) -> Result<usize> {
        let charge = charge.ok_or_else(host_memory)?;
        (self.pay)(size.checked_add(96).ok_or_else(host_memory)?)?;
        let mut bytes = storage(usize::try_from(size).map_err(|_| host_memory())?)?;
        bytes.resize(size as usize, 0);
        self.image
            .blocks
            .try_reserve(1)
            .map_err(|_| host_memory())?;
        let index = self.image.blocks.len();
        self.image.blocks.push(BlockImage {
            bytes,
            align,
            charge,
            graph,
            parent,
            address: 0,
        });
        Ok(index)
    }
    fn write(&mut self, (b, i): Place, bytes: &[u8]) {
        self.image.blocks[b].bytes[i..i + bytes.len()].copy_from_slice(bytes);
    }
    fn word(&mut self, at: Place, size: u64, word: u64) {
        self.write(at, &word.to_le_bytes()[..size as usize]);
    }
    fn length(&mut self, at: Place, n: u64) {
        self.word(add(at, 8), 8, n);
        self.word(add(at, 16), 8, n);
    }
    fn bytes(&mut self, at: Place, bytes: &[u8]) -> Result<()> {
        let n = bytes.len() as u64;
        if n != 0 {
            let b = self.block(Some(at), n, 1, charge::seq(n, 1), false)?;
            self.write((b, 0), bytes);
        }
        self.length(at, n);
        Ok(())
    }
    fn case(&mut self, at: Place, node: usize, name: &str) -> &'s [Field] {
        let Construction::Sum { tag, cases } = &self.plan.nodes[node].body else {
            unreachable!()
        };
        let (i, (_, fields)) = cases
            .iter()
            .enumerate()
            .find(|(_, c)| c.0 == name)
            .expect("case in compiler plan");
        if let Some(tag) = tag
            && let Some(word) = case_word(tag, i as u32)
        {
            self.word(add(at, tag.offset), tag.width, word);
        }
        fields
    }
    fn fields(&mut self, at: Place, fields: &[Field], members: &Members) -> Result<()> {
        for (i, f) in fields.iter().enumerate() {
            let value = match members {
                Members::Positional(v) => &v[i],
                Members::Labelled(v) => &v[i].1,
            };
            self.value(add(at, f.offset), f.node, value)?;
        }
        Ok(())
    }
    fn seq(
        &mut self,
        at: Place,
        node: usize,
        n: usize,
        mut fill: impl FnMut(&mut Self, Place, usize, usize) -> Result<()>,
    ) -> Result<()> {
        let Construction::List { item, graph } = self.plan.nodes[node].body else {
            unreachable!()
        };
        let child = &self.plan.nodes[item];
        let len = n as u64;
        if n != 0 && child.size != 0 {
            let physical = len.checked_mul(child.size).ok_or_else(host_memory)?;
            let b = self.block(
                Some(at),
                physical,
                child.align,
                charge::seq(len, child.logical),
                graph,
            )?;
            for i in 0..n {
                fill(self, (b, i * child.size as usize), item, i)?;
            }
        }
        self.length(at, len);
        if child.size == 0 {
            self.word(add(at, 16), 8, u64::MAX);
        }
        Ok(())
    }
    fn value(&mut self, at: Place, node: usize, val: &Decoded) -> Result<()> {
        use Construction as C;
        let target = &self.plan.nodes[node];
        match (&target.body, val) {
            (C::Unit, _) => (),
            (C::Text, Decoded::Default(v)) => self.bytes(at, v.as_str().unwrap().as_bytes())?,
            (C::Text, Decoded::Scalar(v)) => self.bytes(at, v.as_str().unwrap().as_bytes())?,
            (C::Scalar(ty), Decoded::Default(v)) => self.word(at, target.size, bits(*ty, v)),
            (C::Scalar(ty), Decoded::Scalar(v)) => self.word(at, target.size, bits(*ty, v)),
            (C::Record(fields), Decoded::Members(m)) => self.fields(at, fields, m)?,
            (C::Sum { .. }, Decoded::Case(name, m)) => {
                let fields = self.case(at, node, name);
                self.fields(at, fields, m)?;
            }
            (C::List { .. }, Decoded::Members(Members::Positional(xs))) => {
                self.seq(at, node, xs.len(), |b, at, item, i| {
                    b.value(at, item, &xs[i])
                })?
            }
            (C::List { .. }, Decoded::Bytes(bytes)) => self.bytes(at, bytes)?,
            (C::List { .. }, Decoded::Default(v)) => {
                let bytes = v.as_array().unwrap();
                self.seq(at, node, bytes.len(), |b, at, _, i| {
                    b.word(at, 1, bytes[i].as_u64().unwrap());
                    Ok(())
                })?;
            }
            (C::Map { .. }, Decoded::Map(pairs)) => self.map(at, node, pairs)?,
            _ => unreachable!("schema and construction plan disagree"),
        }
        Ok(())
    }
    fn map(&mut self, at: Place, node: usize, pairs: &[(String, Decoded)]) -> Result<()> {
        let Construction::Map {
            item,
            stride,
            align,
            meta,
            key,
            value,
        } = self.plan.nodes[node].body
        else {
            unreachable!()
        };
        let n = pairs.len() as u64;
        if n == 0 {
            return Ok(());
        }
        let layout = index::IndexLayout::new(n, stride).ok_or_else(host_memory)?;
        let b = self.block(
            Some(at),
            layout.physical,
            align,
            charge::map(n, 24, self.plan.nodes[item].logical),
            true,
        )?;
        self.length(at, n);
        let header = (b, layout.header as usize);
        let mask = layout.groups - 1;
        let wide = n > index::WIDE_CAPACITY;
        self.word(add(header, index::LIVE_COUNT), 8, n);
        self.word(add(header, index::GROUP_MASK), 8, mask);
        self.word(
            add(header, index::MODE),
            8,
            index::STRONG | if wide { index::WIDE } else { 0 },
        );
        let group = |g: u64| add(header, index::HEADER_BYTES + g * layout.group_bytes);
        for g in 0..layout.groups {
            self.write(group(g), &[index::EMPTY; index::GROUP_WIDTH]);
        }
        for (i, (name, v)) in pairs.iter().enumerate() {
            let entry = (b, i * stride as usize);
            let mut hash = self.key.state();
            hash.bytes(name.as_bytes());
            let p = hash.finish();
            let mut g = p & mask;
            for probe in 1..=layout.groups {
                let (_, off) = group(g);
                if let Some(lane) = self.image.blocks[b].bytes[off..off + 16]
                    .iter()
                    .position(|&x| x == index::EMPTY)
                {
                    self.word((b, off + lane), 1, p >> index::TAG_SHIFT);
                    let width = if wide { 8 } else { 4 };
                    self.word((b, off + 16 + lane * width), width as u64, i as u64);
                    break;
                }
                g = g.wrapping_add(probe) & mask;
            }
            self.word(add(entry, meta), 8, p | index::LIVE_BIT);
            self.bytes(add(entry, key), name.as_bytes())?;
            self.value(add(entry, value), item, v)?;
        }
        Ok(())
    }
    fn error(&mut self, at: Place, node: usize, e: &DataError) -> Result<()> {
        let Construction::Record(fields) = &self.plan.nodes[node].body else {
            unreachable!()
        };
        for f in fields {
            let dest = add(at, f.offset);
            match f.name.as_str() {
                "reason" => {
                    let (case, why) = match e.kind {
                        ErrorKind::Invalid(s) => ("Invalid", s),
                        ErrorKind::Limit(s) => ("Limit", s),
                        _ => unreachable!(),
                    };
                    let arg = &self.case(dest, f.node, case)[0];
                    self.bytes(add(dest, arg.offset), why.as_bytes())?;
                }
                "path" => self.seq(dest, f.node, e.path.len(), |b, at, _, i| {
                    b.bytes(at, e.path[i].as_bytes())
                })?,
                "primary" => self.span(dest, f.node, e.span),
                "related" => {
                    if let Some(span) = e.related {
                        let arg = &self.case(dest, f.node, "Some")[0];
                        self.span(add(dest, arg.offset), arg.node, span);
                    } else {
                        self.case(dest, f.node, "None");
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }
    fn span(&mut self, at: Place, node: usize, span: Span) {
        let Construction::Record(fs) = &self.plan.nodes[node].body else {
            unreachable!()
        };
        self.word(add(at, fs[0].offset), 8, u64::from(span.start));
        self.word(add(at, fs[1].offset), 8, u64::from(span.end));
    }
}
fn bits(ty: Scalar, v: &Value) -> u64 {
    match ty {
        Scalar::Bool => u64::from(v.as_bool().unwrap()),
        Scalar::Char => u64::from(v.as_str().unwrap().chars().next().unwrap()),
        Scalar::F32 => u64::from(v.as_str().unwrap().parse::<f32>().unwrap().to_bits()),
        Scalar::F64 => v.as_str().unwrap().parse::<f64>().unwrap().to_bits(),
        Scalar::I64 => v.as_str().unwrap().parse::<i64>().unwrap() as u64,
        Scalar::U64 => v.as_str().unwrap().parse().unwrap(),
        _ => v.as_i64().unwrap() as u64,
    }
}
