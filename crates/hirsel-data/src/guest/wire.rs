//! Internal compiler-to-runtime representation. Counts precede lists so an
//! engine can reserve each allocation fallibly. The carrier is static UTF-8.
use super::{Construction, Field, Node, Plan};
use crate::{
    self as data, Body, Case, Descriptor, Fields, Nominal, Scalar, Shape,
    decoded::{host_memory, storage},
};
use hirsel_core_layout::{Encoding, Tag};
use serde_json::Value;
use std::fmt::Write;

type Result<T> = std::result::Result<T, data::DataError>;

struct Reader<'a>(&'a str);
impl<'a> Reader<'a> {
    fn word(&mut self) -> u64 {
        let (digits, rest) = self.0.split_once('\n').unwrap();
        self.0 = rest;
        digits.parse().unwrap()
    }
    fn text(&mut self) -> &'a str {
        let n = self.word() as usize;
        let (text, rest) = self.0.split_at(n);
        self.0 = rest;
        text
    }
    fn string(&mut self) -> Result<String> {
        let text = self.text();
        let mut s = String::new();
        s.try_reserve_exact(text.len()).map_err(|_| host_memory())?;
        s.push_str(text);
        Ok(s)
    }
    fn list<T>(&mut self, read: impl Fn(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let n = self.word() as usize;
        let mut out = storage(n)?;
        for _ in 0..n {
            out.push(read(self)?);
        }
        Ok(out)
    }
}
struct Writer(String);
impl Writer {
    fn word(&mut self, n: u64) {
        writeln!(self.0, "{n}").unwrap();
    }
    fn text(&mut self, s: &str) {
        self.word(s.len() as u64);
        self.0.push_str(s);
    }
    fn list<T>(&mut self, xs: &[T], write: impl Fn(&T, &mut Self)) {
        self.word(xs.len() as u64);
        for x in xs {
            write(x, self);
        }
    }
}

/// A single definition fixes the read and write order of each aggregate.
trait Wire: Sized {
    fn read(r: &mut Reader<'_>) -> Result<Self>;
    fn write(&self, w: &mut Writer);
}
macro_rules! record {
    ($ty:ty { $($field:ident),* }) => {
        impl Wire for $ty {
            fn read(r:&mut Reader<'_>)->Result<Self> { Ok(Self { $($field: Wire::read(r)?),* }) }
            fn write(&self,w:&mut Writer) { $(self.$field.write(w);)* }
        }
    }
}
macro_rules! variants {
    ($ty:ty { $($i:literal => $case:ident $(($arg:ident))? $({$($field:ident),*})?),* }) => {
        impl Wire for $ty {
            fn read(r:&mut Reader<'_>)->Result<Self> {
                Ok(match r.word() {
                    $($i => Self::$case $( ({ let $arg = Wire::read(r)?; $arg }) )?
                        $({$($field: Wire::read(r)?),*})? ,)*
                    _=>unreachable!("compiler witness tag"),
                })
            }
            fn write(&self,w:&mut Writer) {
                match self { $(Self::$case $(($arg))? $({$($field),*})? => {
                    w.word($i); $($arg.write(w);)? $($($field.write(w);)*)?
                }),* }
            }
        }
    }
}
macro_rules! number {
    ($($t:ty),*) => { $(impl Wire for $t {
        fn read(r:&mut Reader<'_>)->Result<Self> { Ok(r.word() as Self) }
        fn write(&self,w:&mut Writer) { w.word(*self as u64); }
    })* }
}
number!(u64, u32, usize);
impl Wire for bool {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(r.word() != 0)
    }
    fn write(&self, w: &mut Writer) {
        w.word(u64::from(*self));
    }
}
impl Wire for String {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        r.string()
    }
    fn write(&self, w: &mut Writer) {
        w.text(self);
    }
}
impl<T: Wire> Wire for Vec<T> {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        r.list(T::read)
    }
    fn write(&self, w: &mut Writer) {
        w.list(self, T::write);
    }
}
impl<T: Wire> Wire for Box<T> {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        // The one-element allocation has the exact layout of T.
        let mut cell = storage(1)?;
        cell.push(T::read(r)?);
        let cell: Box<[T; 1]> = cell.try_into().ok().unwrap();
        // SAFETY: [T; 1] has the size and alignment of its only element.
        Ok(unsafe { Box::from_raw(Box::into_raw(cell).cast::<T>()) })
    }
    fn write(&self, w: &mut Writer) {
        (**self).write(w);
    }
}
impl<T: Wire> Wire for Option<T> {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        (r.word() != 0).then(|| T::read(r)).transpose()
    }
    fn write(&self, w: &mut Writer) {
        self.is_some().write(w);
        if let Some(x) = self {
            x.write(w);
        }
    }
}
impl<A: Wire, B: Wire> Wire for (A, B) {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok((A::read(r)?, B::read(r)?))
    }
    fn write(&self, w: &mut Writer) {
        self.0.write(w);
        self.1.write(w);
    }
}
impl Wire for Scalar {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self::parse(r.text()).unwrap())
    }
    fn write(&self, w: &mut Writer) {
        w.text(self.name());
    }
}
impl Wire for Value {
    fn read(r: &mut Reader<'_>) -> Result<Self> {
        Ok(match r.word() {
            0 => Self::Bool(bool::read(r)?),
            1 => Self::from(r.word() as i64),
            2 => Self::String(r.string()?),
            3 => Self::Array(r.list(Self::read)?),
            _ => unreachable!("compiler scalar default"),
        })
    }
    fn write(&self, w: &mut Writer) {
        match self {
            Self::Bool(b) => {
                w.word(0);
                b.write(w);
            }
            Self::Number(n) => {
                w.word(1);
                w.word(n.as_i64().unwrap() as u64);
            }
            Self::String(s) => {
                w.word(2);
                w.text(s);
            }
            Self::Array(a) => {
                w.word(3);
                a.write(w);
            }
            _ => unreachable!("schema admits scalar defaults only"),
        }
    }
}
record!(Nominal { module, name, body });
record!(Case { name, fields });
record!(data::Field {
    name,
    shape,
    default
});
variants!(Body {0=>Record(fields),1=>Sum(cases)});
variants!(Fields {0=>Labelled(fields),1=>Positional(shapes)});
variants!(Shape {0=>Scalar(scalar),1=>Unit,2=>List(item),3=>Map(item),4=>Option(item),5=>Tuple(fields),6=>Named(index)});
record!(Plan { nodes, result });
record!(Node {
    size,
    logical,
    align,
    body
});
record!(Field { offset, name, node });
record!(Tag {
    offset,
    width,
    encoding
});
variants!(Encoding {0=>Direct,1=>Niche {dataful,first,last,start}});
variants!(Construction {
    0=>Unit,1=>Scalar(scalar),2=>Text,3=>List{item,graph},
    4=>Map{item,stride,align,meta,key,value},5=>Record(fields),6=>Sum{tag,cases}
});

pub(super) fn encode(schema: &Descriptor, plan: &Plan) -> Vec<u8> {
    let mut w = Writer(String::new());
    w.text(std::str::from_utf8(&schema.bytes()).unwrap());
    w.text(schema.digest());
    w.text(schema.module());
    schema.types.write(&mut w);
    plan.write(&mut w);
    w.0.into_bytes()
}
pub(super) fn open(bytes: &[u8]) -> Result<(Descriptor, Plan)> {
    let mut r = Reader(std::str::from_utf8(bytes).expect("compiler witness"));
    let _canonical = r.text();
    let schema = Descriptor {
        digest: r.string()?,
        module: r.string()?,
        types: Vec::read(&mut r)?,
    };
    Ok((schema, Plan::read(&mut r)?))
}
