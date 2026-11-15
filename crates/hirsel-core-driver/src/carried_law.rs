//! Every unit that carries a body defines it weak under its code identity, and the
//! linker keeps one copy. That is sound only if the name decides the code: a test
//! records each reached body under its code identity, and one identity must name one
//! Core IR body. A body is encoded apart from its module: a type is its closure hash,
//! a function is its code identity, an instance is its identity, an item is its path,
//! and a signature or static is its contents. Anchors are compared like any field.
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::{FuncId, Module};
use serde::ser::{self, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Each code identity recorded so far, with the encoding of its body and the
/// instance that carried it first, and the first identity found naming two bodies.
#[derive(Default)]
pub(crate) struct Carried {
    first: BTreeMap<ContentHash, (ContentHash, String)>,
    pub conflict: Option<String>,
}
impl Carried {
    /// Records every body `module` defines.
    pub fn add(&mut self, module: &Module) {
        let mut canon = Canon {
            module,
            out: Vec::new(),
            closures: HashMap::new(),
            codes: HashMap::new(),
        };
        for (i, function) in module.functions.iter().enumerate() {
            if function.body.is_none() {
                continue;
            }
            canon.out.clear();
            function.serialize(&mut canon).expect("code encoding");
            let code = ContentHash::of_parts("hirsel.core-carried-law", [&canon.out[..]]);
            let name = canon.code(i as u32);
            let (first, by) = self
                .first
                .entry(name)
                .or_insert_with(|| (code, module.render_instance(function.instance)));
            if *first != code && self.conflict.is_none() {
                self.conflict = Some(format!(
                    "code identity {} names two bodies: one carried with {by}, another with {}",
                    hirsel_core_ir::symbol(name),
                    module.render_instance(function.instance),
                ));
            }
        }
    }
}

struct Canon<'a> {
    module: &'a Module,
    out: Vec<u8>,
    closures: HashMap<u32, ContentHash>,
    codes: HashMap<u32, ContentHash>,
}
#[derive(Debug)]
struct Error(String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}
type R = Result<(), Error>;

impl Canon<'_> {
    fn closure(&mut self, row: u32) -> ContentHash {
        let m = self.module;
        *self
            .closures
            .entry(row)
            .or_insert_with(|| m.closure(hirsel_core_ir::TypeId(row)).hash)
    }
    /// [`Module::code_identity`], with each argument row's closure taken once.
    fn code(&mut self, f: u32) -> ContentHash {
        if let Some(&code) = self.codes.get(&f) {
            return code;
        }
        let m = self.module;
        let code = match m.code_variants.get(&FuncId(f)) {
            Some(&code) => code,
            None => self.identity(m.functions[f as usize].instance.0).digest(),
        };
        self.codes.insert(f, code);
        code
    }
    fn identity(&mut self, instance: u32) -> hirsel_core_ir::Identity {
        let key = &self.module.instances[instance as usize];
        hirsel_core_ir::Identity {
            definition: key.definition.clone(),
            dictionaries: key.dictionaries.clone(),
            closures: key.arguments.iter().map(|t| self.closure(t.0)).collect(),
        }
    }
    fn bytes(&mut self, tag: u8, bytes: &[u8]) {
        self.out.push(tag);
        self.out
            .extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.out.extend_from_slice(bytes);
    }
    /// A module row, by what it means rather than by its index.
    fn row(&mut self, name: &str, index: u32) -> Option<R> {
        let m = self.module;
        let i = index as usize;
        Some(match name {
            "TypeId" | "TypeRef" => {
                let hash = self.closure(index);
                self.bytes(1, &hash.0);
                Ok(())
            }
            "FuncId" | "FuncRef" => {
                let code = self.code(index);
                self.bytes(2, &code.0);
                Ok(())
            }
            "InstanceId" => {
                let digest = self.identity(index).digest();
                self.bytes(3, &digest.0);
                Ok(())
            }
            "ItemId" => {
                self.bytes(4, m.items[i].to_string().as_bytes());
                Ok(())
            }
            "SigId" => {
                self.out.push(5);
                m.signatures[i].serialize(self)
            }
            "StaticId" => {
                self.out.push(6);
                m.statics[i].serialize(self)
            }
            _ => return None,
        })
    }
}

struct Index(Option<u32>);
impl ser::Serializer for &mut Index {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = ser::Impossible<(), Error>;
    type SerializeTuple = ser::Impossible<(), Error>;
    type SerializeTupleStruct = ser::Impossible<(), Error>;
    type SerializeTupleVariant = ser::Impossible<(), Error>;
    type SerializeMap = ser::Impossible<(), Error>;
    type SerializeStruct = ser::Impossible<(), Error>;
    type SerializeStructVariant = ser::Impossible<(), Error>;
    fn serialize_u32(self, v: u32) -> R {
        self.0 = Some(v);
        Ok(())
    }
    fn serialize_bool(self, _: bool) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_i8(self, _: i8) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_i16(self, _: i16) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_i32(self, _: i32) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_i64(self, _: i64) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_u8(self, _: u8) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_u16(self, _: u16) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_u64(self, _: u64) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_f32(self, _: f32) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_f64(self, _: f64) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_char(self, _: char) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_str(self, _: &str) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_bytes(self, _: &[u8]) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_none(self) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_unit(self) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_unit_struct(self, _: &'static str) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, _: &T) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> R {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(ser::Error::custom("a row index is a u32"))
    }
}

macro_rules! scalars {
    ($($method:ident: $ty:ty),*) => {$(
        fn $method(self, v: $ty) -> R {
            self.out.extend_from_slice(&v.to_le_bytes());
            Ok(())
        }
    )*};
}
impl<'b> ser::Serializer for &'b mut Canon<'_> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    scalars!(serialize_i8: i8, serialize_i16: i16, serialize_i32: i32, serialize_i64: i64,
        serialize_u8: u8, serialize_u16: u16, serialize_u32: u32, serialize_u64: u64,
        serialize_f32: f32, serialize_f64: f64);
    fn serialize_bool(self, v: bool) -> R {
        self.out.push(v as u8);
        Ok(())
    }
    fn serialize_char(self, v: char) -> R {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> R {
        self.bytes(7, v.as_bytes());
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> R {
        self.bytes(8, v);
        Ok(())
    }
    fn serialize_none(self) -> R {
        self.out.push(0);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> R {
        self.out.push(1);
        v.serialize(self)
    }
    fn serialize_unit(self) -> R {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> R {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, i: u32, _: &'static str) -> R {
        self.serialize_u32(i)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, name: &'static str, v: &T) -> R {
        let mut index = Index(None);
        if v.serialize(&mut index).is_ok()
            && let Some(row) = index.0.and_then(|i| self.row(name, i))
        {
            return row;
        }
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        v: &T,
    ) -> R {
        self.serialize_u32(i)?;
        v.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, Error> {
        let len = len.ok_or_else(|| ser::Error::custom("a sequence has a length"))?;
        self.serialize_u64(len as u64)?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(i)?;
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self, Error> {
        self.serialize_seq(len)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Error> {
        Ok(self)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        i: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(i)?;
        Ok(self)
    }
}
macro_rules! compound {
    ($($trait:ident :: $method:ident),*) => {$(
        impl ser::$trait for &mut Canon<'_> {
            type Ok = ();
            type Error = Error;
            fn $method<T: ?Sized + Serialize>(&mut self, v: &T) -> R {
                v.serialize(&mut **self)
            }
            fn end(self) -> R {
                Ok(())
            }
        }
    )*};
}
compound!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field
);
impl ser::SerializeMap for &mut Canon<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, v: &T) -> R {
        v.serialize(&mut **self)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> R {
        v.serialize(&mut **self)
    }
    fn end(self) -> R {
        Ok(())
    }
}
impl ser::SerializeStruct for &mut Canon<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> R {
        v.serialize(&mut **self)
    }
    fn end(self) -> R {
        Ok(())
    }
}
impl ser::SerializeStructVariant for &mut Canon<'_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, v: &T) -> R {
        v.serialize(&mut **self)
    }
    fn end(self) -> R {
        Ok(())
    }
}
