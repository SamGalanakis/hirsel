//! Binary encoding shared by compiler identities and store records.
//!
//! Keys and records use fixed-width little-endian integers. Published table
//! rows and reach elements use tagged little-endian variable-width integers.
//! Both layouts are type-directed: field names and type metadata are absent.

use serde::{Deserialize, Serialize, de, ser};
use std::io::{self, Write};

pub fn write_fixed(out: &mut impl Write, value: &(impl Serialize + ?Sized)) -> io::Result<()> {
    write::<false>(out, value)
}

pub fn fixed_size(value: &(impl Serialize + ?Sized)) -> io::Result<u64> {
    #[derive(Default)]
    struct Size(u64);
    impl Write for Size {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io::Error::other("record size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut size = Size::default();
    write_fixed(&mut size, value)?;
    Ok(size.0)
}

pub fn read_fixed<T: de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    read::<false, T>(bytes)
}

pub fn write_compact(out: &mut impl Write, value: &(impl Serialize + ?Sized)) -> io::Result<()> {
    write::<true>(out, value)
}

pub fn read_compact<T: de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    read::<true, T>(bytes)
}

fn write<const COMPACT: bool>(
    out: &mut impl Write,
    value: &(impl Serialize + ?Sized),
) -> io::Result<()> {
    value
        .serialize(&mut Encoder::<_, COMPACT>(out))
        .map_err(|error| error.0)
}

fn read<const COMPACT: bool, T: de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    let mut decoder = Decoder::<COMPACT>(bytes);
    let value = T::deserialize(&mut decoder).map_err(|error| error.0)?;
    if !decoder.0.is_empty() {
        return Err(invalid("trailing record bytes").0);
    }
    Ok(value)
}

#[derive(Debug)]
struct Error(io::Error);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(io::Error::other(message.to_string()))
    }
}

impl de::Error for Error {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        invalid(message.to_string())
    }
}

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Error {
    Error(io::Error::new(io::ErrorKind::InvalidData, message))
}

type Result<T = ()> = std::result::Result<T, Error>;

struct Encoder<W, const COMPACT: bool>(W);

impl<W: Write, const COMPACT: bool> Encoder<W, COMPACT> {
    fn bytes(&mut self, bytes: &[u8]) -> Result {
        self.0.write_all(bytes).map_err(Error)
    }

    fn varint(&mut self, value: u128) -> Result {
        if value <= 250 {
            return self.bytes(&[value as u8]);
        }
        if let Ok(value) = u16::try_from(value) {
            self.tagged(251, value.to_le_bytes())
        } else if let Ok(value) = u32::try_from(value) {
            self.tagged(252, value.to_le_bytes())
        } else if let Ok(value) = u64::try_from(value) {
            self.tagged(253, value.to_le_bytes())
        } else {
            self.tagged(254, value.to_le_bytes())
        }
    }

    fn tagged<const N: usize>(&mut self, tag: u8, payload: [u8; N]) -> Result {
        self.bytes(&[tag])?;
        self.bytes(&payload)
    }
}

macro_rules! serialize_number {
    ($method:ident, $ty:ty) => {
        fn $method(self, value: $ty) -> Result {
            self.bytes(&value.to_le_bytes())
        }
    };
    ($method:ident, $ty:ty, unsigned) => {
        fn $method(self, value: $ty) -> Result {
            if COMPACT {
                self.varint(value as u128)
            } else {
                self.bytes(&value.to_le_bytes())
            }
        }
    };
    ($method:ident, $ty:ty, $unsigned:ty) => {
        fn $method(self, value: $ty) -> Result {
            if COMPACT {
                let zigzag = ((value << 1) ^ (value >> (<$ty>::BITS - 1))) as $unsigned;
                self.varint(zigzag as u128)
            } else {
                self.bytes(&value.to_le_bytes())
            }
        }
    };
}

impl<W: Write, const COMPACT: bool> ser::Serializer for &mut Encoder<W, COMPACT> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    serialize_number!(serialize_u8, u8);
    serialize_number!(serialize_u16, u16, unsigned);
    serialize_number!(serialize_u32, u32, unsigned);
    serialize_number!(serialize_u64, u64, unsigned);
    serialize_number!(serialize_u128, u128, unsigned);
    serialize_number!(serialize_i8, i8);
    serialize_number!(serialize_i16, i16, u16);
    serialize_number!(serialize_i32, i32, u32);
    serialize_number!(serialize_i64, i64, u64);
    serialize_number!(serialize_i128, i128, u128);
    serialize_number!(serialize_f32, f32);
    serialize_number!(serialize_f64, f64);

    fn serialize_bool(self, value: bool) -> Result {
        self.serialize_u8(u8::from(value))
    }

    fn serialize_char(self, value: char) -> Result {
        self.bytes(value.encode_utf8(&mut [0; 4]).as_bytes())
    }

    fn serialize_str(self, value: &str) -> Result {
        self.serialize_bytes(value.as_bytes())
    }

    fn serialize_bytes(self, value: &[u8]) -> Result {
        self.serialize_u64(value.len() as u64)?;
        self.bytes(value)
    }

    fn serialize_none(self) -> Result {
        self.serialize_u8(0)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result {
        self.serialize_u8(1)?;
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result {
        Ok(())
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result {
        self.serialize_unit()
    }

    fn serialize_unit_variant(self, _: &'static str, index: u32, _: &'static str) -> Result {
        self.serialize_u32(index)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        index: u32,
        _: &'static str,
        value: &T,
    ) -> Result {
        self.serialize_u32(index)?;
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self> {
        let len = len.ok_or_else(|| ser::Error::custom("record sequence or map needs a length"))?;
        self.serialize_u64(len as u64)?;
        Ok(self)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self> {
        self.serialize_seq(len)
    }

    fn serialize_tuple(self, _: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        index: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self> {
        self.serialize_u32(index)?;
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self> {
        self.serialize_tuple_variant(name, index, variant, len)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

macro_rules! serialize_fields {
    ($trait:ident, $($method:ident($($field:ident: $ty:ty)?)),+) => {
        impl<W: Write, const COMPACT: bool> ser::$trait for &mut Encoder<W, COMPACT> {
            type Ok = ();
            type Error = Error;

            $(
                fn $method<T: Serialize + ?Sized>(&mut self, $($field: $ty,)? value: &T) -> Result {
                    value.serialize(&mut **self)
                }
            )+

            fn end(self) -> Result {
                Ok(())
            }
        }
    };
}

serialize_fields!(SerializeSeq, serialize_element());
serialize_fields!(SerializeTuple, serialize_element());
serialize_fields!(SerializeTupleStruct, serialize_field());
serialize_fields!(SerializeTupleVariant, serialize_field());
serialize_fields!(SerializeMap, serialize_key(), serialize_value());
serialize_fields!(SerializeStruct, serialize_field(_name: &'static str));
serialize_fields!(SerializeStructVariant, serialize_field(_name: &'static str));

struct Decoder<'de, const COMPACT: bool>(&'de [u8]);

impl<'de, const COMPACT: bool> Decoder<'de, COMPACT> {
    fn take(&mut self, len: usize) -> Result<&'de [u8]> {
        let (bytes, rest) = self
            .0
            .split_at_checked(len)
            .ok_or_else(|| Error(io::ErrorKind::UnexpectedEof.into()))?;
        self.0 = rest;
        Ok(bytes)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (bytes, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or_else(|| Error(io::ErrorKind::UnexpectedEof.into()))?;
        self.0 = rest;
        Ok(*bytes)
    }

    fn varint(&mut self, width: usize) -> Result<u128> {
        let [tag] = self.take_array()?;
        match tag {
            0..=250 => Ok(u128::from(tag)),
            251 if width >= 2 => Ok(u16::from_le_bytes(self.take_array()?).into()),
            252 if width >= 4 => Ok(u32::from_le_bytes(self.take_array()?).into()),
            253 if width >= 8 => Ok(u64::from_le_bytes(self.take_array()?).into()),
            254 if width >= 16 => Ok(u128::from_le_bytes(self.take_array()?)),
            _ => Err(invalid("invalid compact integer tag")),
        }
    }

    fn len(&mut self) -> Result<usize> {
        usize::try_from(u64::deserialize(self)?)
            .map_err(|_| invalid("record length overflows usize"))
    }

    fn bytes(&mut self) -> Result<&'de [u8]> {
        let len = self.len()?;
        self.take(len)
    }

    fn string(&mut self) -> Result<&'de str> {
        std::str::from_utf8(self.bytes()?).map_err(invalid)
    }
}

macro_rules! deserialize_number {
    ($method:ident, $visit:ident, $ty:ty) => {
        fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            visitor.$visit(<$ty>::from_le_bytes(self.take_array()?))
        }
    };
    ($method:ident, $visit:ident, $ty:ty, unsigned) => {
        fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let value = if COMPACT {
                self.varint(size_of::<$ty>())? as $ty
            } else {
                <$ty>::from_le_bytes(self.take_array()?)
            };
            visitor.$visit(value)
        }
    };
    ($method:ident, $visit:ident, $ty:ty, $unsigned:ty) => {
        fn $method<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
            let value = if COMPACT {
                let zigzag = self.varint(size_of::<$unsigned>())? as $unsigned;
                ((zigzag >> 1) as $ty) ^ -((zigzag & 1) as $ty)
            } else {
                <$ty>::from_le_bytes(self.take_array()?)
            };
            visitor.$visit(value)
        }
    };
}

impl<'de, const COMPACT: bool> de::Deserializer<'de> for &mut Decoder<'de, COMPACT> {
    type Error = Error;

    deserialize_number!(deserialize_u8, visit_u8, u8);
    deserialize_number!(deserialize_u16, visit_u16, u16, unsigned);
    deserialize_number!(deserialize_u32, visit_u32, u32, unsigned);
    deserialize_number!(deserialize_u64, visit_u64, u64, unsigned);
    deserialize_number!(deserialize_u128, visit_u128, u128, unsigned);
    deserialize_number!(deserialize_i8, visit_i8, i8);
    deserialize_number!(deserialize_i16, visit_i16, i16, u16);
    deserialize_number!(deserialize_i32, visit_i32, i32, u32);
    deserialize_number!(deserialize_i64, visit_i64, i64, u64);
    deserialize_number!(deserialize_i128, visit_i128, i128, u128);
    deserialize_number!(deserialize_f32, visit_f32, f32);
    deserialize_number!(deserialize_f64, visit_f64, f64);

    fn deserialize_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value> {
        Err(invalid("record decoding needs a known type"))
    }

    serde::forward_to_deserialize_any! { identifier ignored_any }

    fn deserialize_bool<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match u8::deserialize(self)? {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            _ => Err(invalid("invalid record boolean")),
        }
    }

    fn deserialize_char<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = match self.0.first() {
            Some(0x00..=0x7f) => 1,
            Some(0xc2..=0xdf) => 2,
            Some(0xe0..=0xef) => 3,
            Some(0xf0..=0xf4) => 4,
            None => return Err(Error(io::ErrorKind::UnexpectedEof.into())),
            _ => return Err(invalid("invalid record character")),
        };
        let text = std::str::from_utf8(self.take(len)?).map_err(invalid)?;
        visitor.visit_char(text.chars().next().expect("one UTF-8 character"))
    }

    fn deserialize_str<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_str(self.string()?)
    }

    fn deserialize_string<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_string(self.string()?.to_owned())
    }

    fn deserialize_bytes<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_bytes(self.bytes()?)
    }

    fn deserialize_byte_buf<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_byte_buf(self.bytes()?.to_vec())
    }

    fn deserialize_option<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match u8::deserialize(&mut *self)? {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(self),
            _ => Err(invalid("invalid record option tag")),
        }
    }

    fn deserialize_unit<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.len()?;
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple<V: de::Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Fields {
            decoder: self,
            remaining: len,
        })
    }

    fn deserialize_tuple_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.len()?;
        visitor.visit_map(Fields {
            decoder: self,
            remaining: len,
        })
    }

    fn deserialize_struct<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: de::Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_enum(self)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

struct Fields<'a, 'de, const COMPACT: bool> {
    decoder: &'a mut Decoder<'de, COMPACT>,
    remaining: usize,
}

impl<'de, const COMPACT: bool> de::SeqAccess<'de> for Fields<'_, 'de, COMPACT> {
    type Error = Error;

    fn next_element_seed<T: de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(&mut *self.decoder).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

impl<'de, const COMPACT: bool> de::MapAccess<'de> for Fields<'_, 'de, COMPACT> {
    type Error = Error;

    fn next_key_seed<K: de::DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        de::SeqAccess::next_element_seed(self, seed)
    }

    fn next_value_seed<V: de::DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        seed.deserialize(&mut *self.decoder)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}

impl<'de, const COMPACT: bool> de::EnumAccess<'de> for &mut Decoder<'de, COMPACT> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: de::DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self)> {
        let index = u32::deserialize(&mut *self)?;
        Ok((
            seed.deserialize(de::value::U32Deserializer::new(index))?,
            self,
        ))
    }
}

impl<'de, const COMPACT: bool> de::VariantAccess<'de> for &mut Decoder<'de, COMPACT> {
    type Error = Error;

    fn unit_variant(self) -> Result {
        Ok(())
    }

    fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: de::Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        de::Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V: de::Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.tuple_variant(fields.len(), visitor)
    }
}
