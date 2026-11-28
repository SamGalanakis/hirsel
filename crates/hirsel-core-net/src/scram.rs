//! TLS-6 transcript validation shared by the host and the evaluator.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{digest, hmac, pbkdf2};
use std::{borrow::Cow, num::NonZeroU32};
use stringprep::tables;
use subtle::ConstantTimeEq;
use unicode_normalization::UnicodeNormalization;

/// The stream's RFC 5929 server-end-point value. An empty value denotes absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    bytes: [u8; 64],
    len: usize,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
        }
    }
}

impl Binding {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if !matches!(bytes.len(), 0 | 32 | 48 | 64) {
            return None;
        }
        let mut binding = Self::default();
        binding.bytes[..bytes.len()].copy_from_slice(bytes);
        binding.len = bytes.len();
        Some(binding)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    Transcript = 2,
    Iterations = 3,
    Binding = 4,
}

pub struct Transcript<'a> {
    header: &'a [u8],
    binding: &'a [u8],
    bare: &'a [u8],
    server: &'a [u8],
    nonce: &'a [u8],
    salt: Vec<u8>,
    iterations: NonZeroU32,
}

fn attribute<'a>(field: Option<&'a [u8]>, name: &[u8]) -> Option<&'a [u8]> {
    field?.strip_prefix(name)
}

fn nonce_valid(nonce: &[u8]) -> bool {
    !nonce.is_empty() && nonce.iter().all(|b| matches!(b, b'!'..=b'~') && *b != b',')
}

fn sasl_name_valid(name: &[u8]) -> bool {
    if std::str::from_utf8(name).is_err() || name.contains(&0) {
        return false;
    }
    name.split(|&b| b == b'=')
        .skip(1)
        .all(|escape| escape.starts_with(b"2C") || escape.starts_with(b"3D"))
}

fn extension_valid(field: &[u8]) -> bool {
    matches!(field, [name, b'=', value @ ..] if name.is_ascii_alphabetic()
        && !value.is_empty() && !value.contains(&0) && std::str::from_utf8(value).is_ok())
}

fn parse_transcript<'a>(
    client: &'a [u8],
    server: &'a [u8],
) -> Option<(&'a [u8], &'a [u8], &'a [u8], Vec<u8>, u64)> {
    let mut client_fields = client.splitn(3, |&b| b == b',');
    let flag = client_fields.next()?;
    if !matches!(flag, b"n" | b"y") {
        let cb = flag.strip_prefix(b"p=")?;
        if cb.is_empty()
            || !cb
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return None;
        }
    }
    if client_fields.next()? != b"" {
        return None;
    }
    let bare = client_fields.next()?;
    let mut bare_fields = bare.split(|&b| b == b',');
    let name = attribute(bare_fields.next(), b"n=")?;
    let client_nonce = attribute(bare_fields.next(), b"r=")?;
    if !sasl_name_valid(name) || !nonce_valid(client_nonce) || bare_fields.next().is_some() {
        return None;
    }
    let mut server_fields = server.split(|&b| b == b',');
    let nonce = attribute(server_fields.next(), b"r=")?;
    let salt = STANDARD
        .decode(attribute(server_fields.next(), b"s=")?)
        .ok()?;
    let count = attribute(server_fields.next(), b"i=")?;
    if !nonce_valid(nonce)
        || !nonce.starts_with(client_nonce)
        || nonce.len() <= client_nonce.len()
        || salt.is_empty()
        || !(1..=10).contains(&count.len())
        || (count.len() > 1 && count[0] == b'0')
        || !count.iter().all(u8::is_ascii_digit)
        || !server_fields.all(extension_valid)
    {
        return None;
    }
    let count = count.iter().fold(0, |n, b| n * 10 + u64::from(*b - b'0'));
    Some((flag, bare, nonce, salt, count))
}

impl<'a> Transcript<'a> {
    pub fn parse(client: &'a [u8], server: &'a [u8], binding: &'a Binding) -> Result<Self, Error> {
        let (flag, bare, nonce, salt, count) =
            parse_transcript(client, server).ok_or(Error::Transcript)?;
        if !(4096..=1_000_000).contains(&count) {
            return Err(Error::Iterations);
        }
        let binding = match flag {
            b"n" | b"y" => &[][..],
            b"p=tls-server-end-point" if !binding.bytes().is_empty() => binding.bytes(),
            _ => return Err(Error::Binding),
        };
        Ok(Self {
            header: &client[..flag.len() + 2],
            binding,
            bare,
            server,
            nonce,
            salt,
            iterations: NonZeroU32::new(count as u32).unwrap(),
        })
    }

    pub fn iterations(&self) -> u32 {
        self.iterations.get()
    }

    pub fn final_len(&self) -> usize {
        let binding_len = self.header.len() + self.binding.len();
        2 + binding_len.div_ceil(3) * 4 + 3 + self.nonce.len() + 3 + 44
    }

    /// The returned proof is for the provider's send buffer. Report only the
    /// verdict of comparing the retained server signature to the final message.
    pub fn derive(&self, password: &[u8]) -> (Vec<u8>, [u8; 32]) {
        let password = normalize(password);
        let mut salted = [0; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            self.iterations,
            &self.salt,
            &password,
            &mut salted,
        );
        let salted = hmac::Key::new(hmac::HMAC_SHA256, &salted);
        let client = hmac::sign(&salted, b"Client Key");
        let stored = digest::digest(&digest::SHA256, client.as_ref());
        let server = hmac::sign(&salted, b"Server Key");

        let mut cbind = Vec::with_capacity(self.header.len() + self.binding.len());
        cbind.extend_from_slice(self.header);
        cbind.extend_from_slice(self.binding);
        let mut message = Vec::with_capacity(self.final_len());
        message.extend_from_slice(b"c=");
        message.extend_from_slice(STANDARD.encode(cbind).as_bytes());
        message.extend_from_slice(b",r=");
        message.extend_from_slice(self.nonce);

        let sign = |key: &[u8]| -> [u8; 32] {
            let key = hmac::Key::new(hmac::HMAC_SHA256, key);
            let mut auth = hmac::Context::with_key(&key);
            for part in [self.bare, b",", self.server, b",", &message] {
                auth.update(part);
            }
            auth.sign().as_ref().try_into().unwrap()
        };
        let signature = sign(server.as_ref());
        let mut proof = sign(stored.as_ref());
        for (proof, client) in proof.iter_mut().zip(client.as_ref()) {
            *proof ^= client;
        }
        message.extend_from_slice(b",p=");
        message.extend_from_slice(STANDARD.encode(proof).as_bytes());
        (message, signature)
    }
}

pub fn verify(signature: &[u8; 32], server_final: &[u8]) -> bool {
    let Some((encoded, rest)) = server_final
        .strip_prefix(b"v=")
        .and_then(<[u8]>::split_first_chunk::<44>)
    else {
        return false;
    };
    if !rest.is_empty()
        && !rest
            .strip_prefix(b",")
            .is_some_and(|ext| ext.split(|&b| b == b',').all(extension_valid))
    {
        return false;
    }
    let mut decoded = [0; 32];
    STANDARD.decode_slice(encoded, &mut decoded).ok() == Some(32)
        && bool::from(signature.ct_eq(&decoded))
}

fn normalize(password: &[u8]) -> Cow<'_, [u8]> {
    let Ok(text) = std::str::from_utf8(password) else {
        return Cow::Borrowed(password);
    };
    if text.is_ascii() {
        return Cow::Borrowed(password);
    }
    // PostgreSQL checks the mapped input for prohibited, unassigned and bidi
    // characters, and falls back to the raw password if mapping empties it.
    let mapped: String = text
        .chars()
        .filter_map(|c| {
            if tables::non_ascii_space_character(c) {
                Some(' ')
            } else if tables::commonly_mapped_to_nothing(c) {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    let prohibited = |c| {
        tables::ascii_control_character(c)
            || tables::non_ascii_control_character(c)
            || tables::private_use(c)
            || tables::non_character_code_point(c)
            || tables::surrogate_code(c)
            || tables::inappropriate_for_plain_text(c)
            || tables::inappropriate_for_canonical_representation(c)
            || tables::change_display_properties_or_deprecated(c)
            || tables::tagging_character(c)
            || tables::unassigned_code_point(c)
    };
    if mapped.is_empty()
        || mapped.contains(prohibited)
        || (mapped.contains(tables::bidi_r_or_al)
            && (mapped.contains(tables::bidi_l)
                || !mapped.starts_with(tables::bidi_r_or_al)
                || !mapped.ends_with(tables::bidi_r_or_al)))
    {
        return Cow::Borrowed(password);
    }
    Cow::Owned(mapped.nfkc().collect::<String>().into_bytes())
}

#[cfg(test)]
pub(crate) mod laws {
    use super::*;

    /// The password of every reviewed and discovered exchange.
    pub(crate) const PASSWORD: &[u8] = b"pencil";

    /// A SCRAM-SHA-256 exchange's owner predicate (the `net` decoder cohort,
    /// `ip::laws`). The input is four lines: the channel binding in hex, the
    /// client-first, server-first and server-final messages. An accepted
    /// exchange projects its iteration count and client-final message.
    pub(crate) fn exchange(bytes: &[u8]) -> Result<String, String> {
        let mut lines = bytes.split(|&byte| byte == b'\n');
        let (Some(binding), Some(client), Some(server), Some(last), None) = (
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
        ) else {
            return Err("frame".into());
        };
        let binding = std::str::from_utf8(binding)
            .ok()
            .and_then(hirsel_core_ids::hex::decode)
            .and_then(|binding| Binding::from_bytes(&binding))
            .ok_or("frame")?;
        let transcript =
            Transcript::parse(client, server, &binding).map_err(|error| format!("{error:?}"))?;
        let (message, signature) = transcript.derive(PASSWORD);
        assert_eq!(message.len(), transcript.final_len());
        if !verify(&signature, last) {
            return Err("ServerSignature".into());
        }
        let message = String::from_utf8(message).expect("the client-final message is text");
        Ok(format!(
            "iterations={} final={message}",
            transcript.iterations()
        ))
    }
}
