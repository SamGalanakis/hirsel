//! Binary encoding shared by compiler identities and store records.
//!
//! Keys and records use fixed-width little-endian integers. Published table
//! rows and reach elements use little-endian variable-width integers.

use bincode_next::{config, enc::write::SizeWriter, serde as binary};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Write};

fn fixed() -> impl config::Config {
    config::legacy()
}

fn compact() -> impl config::Config {
    config::legacy().with_variable_int_encoding()
}

pub fn write_fixed(out: &mut impl Write, value: &(impl Serialize + ?Sized)) -> io::Result<()> {
    binary::encode_into_std_write(value, out, fixed())
        .map(|_| ())
        .map_err(io::Error::other)
}

pub fn fixed_size(value: &(impl Serialize + ?Sized)) -> io::Result<u64> {
    let mut writer = SizeWriter::default();
    binary::encode_into_writer(value, &mut writer, fixed()).map_err(io::Error::other)?;
    Ok(writer.bytes_written as u64)
}

pub fn read_fixed<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    read(bytes, fixed())
}

pub fn write_compact(out: &mut impl Write, value: &(impl Serialize + ?Sized)) -> io::Result<()> {
    binary::encode_into_std_write(value, out, compact())
        .map(|_| ())
        .map_err(io::Error::other)
}

pub fn read_compact<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    read(bytes, compact())
}

fn read<T: DeserializeOwned>(bytes: &[u8], config: impl config::Config) -> io::Result<T> {
    let (value, read) = binary::decode_from_slice(bytes, config).map_err(io::Error::other)?;
    if read != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing record bytes",
        ));
    }
    Ok(value)
}
