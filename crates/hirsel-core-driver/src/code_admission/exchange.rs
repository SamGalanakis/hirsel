use super::{
    AdmissionError, AdmissionLimits, Limit, ObjectKey, ObjectKind, check_hash, decode_rows,
    listed_objects, open_object, read_recipe, read_verified,
};
use crate::code_store::CodeStore;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::address::Group;
use hirsel_core_ir::release_recipe::Recipe;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

/// A connection accepted on the host's filesystem-authorized control listener.
/// The host must restrict access to the listener to principals that can attest a
/// complete release recipe, including its grant bindings and annexes.
pub struct ControlConnection(UnixStream);

impl ControlConnection {
    /// Accept on the host's control listener. The filesystem permissions of
    /// that listener are the authority check for a raw recipe request.
    pub fn accept(control: &UnixListener) -> io::Result<Self> {
        control.accept().map(|(stream, _)| Self(stream))
    }

    /// Recover the stream for the host's result or deploy preview.
    pub fn into_stream(self) -> io::Result<UnixStream> {
        self.0.set_read_timeout(None)?;
        self.0.set_write_timeout(None)?;
        Ok(self.0)
    }
}

// The request is R, recipe length, recipe bytes. Need is a count and that many
// typed keys. The sender answers with typed keys, lengths and bytes, then a
// zero tag. Counts and lengths on the socket are little-endian u64.
pub(super) struct Exchange<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
    received: u64,
}

impl<'a> Exchange<'a> {
    pub(super) fn new(connection: &'a mut ControlConnection, deadline: Instant) -> Self {
        Self::on(&mut connection.0, deadline)
    }

    fn on(stream: &'a mut UnixStream, deadline: Instant) -> Self {
        Self {
            stream,
            deadline,
            received: 0,
        }
    }

    pub(super) fn recipe(
        &mut self,
        max_bytes: usize,
    ) -> Result<(ContentHash, Vec<u8>), AdmissionError> {
        let root = ContentHash(self.read_array()?);
        let length = u64::from_le_bytes(self.read_array()?);
        let length = usize::try_from(length)
            .ok()
            .filter(|&length| length <= max_bytes)
            .ok_or(AdmissionError::Limit(super::Limit::RecipeBytes))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| AdmissionError::Limit(super::Limit::RecipeBytes))?;
        let mut scratch = [0; 8192];
        while bytes.len() < length {
            let chunk = (length - bytes.len()).min(scratch.len());
            let n = self.read(&mut scratch[..chunk])?;
            if n == 0 {
                return Err(AdmissionError::Disconnected);
            }
            bytes.extend_from_slice(&scratch[..n]);
        }
        self.check_deadline()?;
        Ok((root, bytes))
    }

    pub(super) fn need(
        &mut self,
        count: usize,
        keys: impl Iterator<Item = ObjectKey>,
    ) -> Result<(), AdmissionError> {
        self.write_frame(&(count as u64).to_le_bytes())?;
        for key in keys {
            self.write_frame(&key_frame(key))?;
        }
        Ok(())
    }

    pub(super) fn next_key(&mut self) -> Result<Option<ObjectKey>, AdmissionError> {
        let [tag] = self.read_array()?;
        let kind = match tag {
            0 => return Ok(None),
            1 => ObjectKind::Group,
            2 => ObjectKind::Body,
            3 => ObjectKind::Annex,
            _ => return Err(AdmissionError::Malformed("object frame tag")),
        };
        Ok(Some(ObjectKey {
            kind,
            hash: ContentHash(self.read_array()?),
        }))
    }

    pub(super) fn length(&mut self) -> Result<u64, AdmissionError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    pub(super) fn object(
        &mut self,
        key: ObjectKey,
        len: u64,
        max_bytes: u64,
        output: &mut impl Write,
    ) -> Result<(), AdmissionError> {
        if len > max_bytes - self.received {
            return Err(AdmissionError::Limit(super::Limit::ObjectBytes));
        }
        let hash = ContentHash::of_stream(key.kind.domain(), len, |hasher| {
            let mut remaining = len;
            let mut scratch = [0; 8192];
            while remaining != 0 {
                let chunk = remaining.min(scratch.len() as u64) as usize;
                let n = self.read(&mut scratch[..chunk])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                self.received += n as u64;
                remaining -= n as u64;
                output.write_all(&scratch[..n])?;
                hasher.write_all(&scratch[..n])?;
            }
            Ok(())
        })?;
        self.check_deadline()?;
        check_hash(hash, key.hash)
    }

    #[cfg(test)]
    pub(super) fn received_bytes(&self) -> u64 {
        self.received
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], AdmissionError> {
        let mut bytes = [0; N];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn write_frame(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.stream.set_write_timeout(Some(self.remaining()?))?;
            match self.stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        self.remaining().map(|_| ())
    }

    fn remaining(&self) -> io::Result<std::time::Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(io::ErrorKind::TimedOut.into())
    }

    fn check_deadline(&self) -> Result<(), AdmissionError> {
        self.remaining()?;
        Ok(())
    }
}

impl Read for Exchange<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.stream.set_read_timeout(Some(self.remaining()?))?;
            match self.stream.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
}

/// Offer a recipe and stream only objects named by the host's one Need.
/// The caller explicitly attests the complete recipe rooted at `root`.
pub fn send_release(
    stream: &mut UnixStream,
    store: &CodeStore,
    root: ContentHash,
    limits: &AdmissionLimits,
) -> Result<(), AdmissionError> {
    let deadline = super::deadline(limits)?;
    let mut budget = super::budget(limits, deadline);
    let bytes = read_recipe(store, root, limits.recipe_bytes, &budget)?;
    let len = bytes.len() as u64;
    let mut recipe = Recipe::decode(&bytes, &mut budget)?;
    let mut listing = listed_objects(&recipe, limits, &mut budget)?;
    let mut exchange = Exchange::on(stream, deadline);
    exchange.write_frame(&root.0)?;
    exchange.write_frame(&len.to_le_bytes())?;
    exchange.write_frame(&bytes)?;
    let count = exchange.length()?;
    if count > listing.len() as u64 || count > limits.outstanding as u64 {
        return Err(AdmissionError::Limit(Limit::Outstanding));
    }
    for _ in 0..count {
        let key = exchange
            .next_key()?
            .ok_or(AdmissionError::Malformed("end inside Need"))?;
        let row = listing
            .get_mut(&key)
            .filter(|row| !row.requested)
            .ok_or(AdmissionError::Unsolicited(key))?;
        row.requested = true;
    }

    recipe.groups.retain(|group| {
        listing[&ObjectKey {
            kind: ObjectKind::Group,
            hash: group.hash,
        }]
            .requested
    });
    let groups = decode_rows(&recipe.groups, &mut budget, |group, budget| {
        let file = open_object(store, ObjectKind::Group, group.hash)?;
        let bytes = read_verified(file, ObjectKind::Group, group, budget)?;
        Ok(Group::decode(&bytes, budget)?)
    })?;
    let order = super::closure::send_order(&recipe, &groups, &mut budget)?;
    for (&key, row) in &listing {
        if row.requested && key.kind != ObjectKind::Group {
            send_object(&mut exchange, store, key, row.len)?;
        }
    }
    for i in order {
        let object = recipe.groups[i];
        send_object(
            &mut exchange,
            store,
            ObjectKey {
                kind: ObjectKind::Group,
                hash: object.hash,
            },
            object.len,
        )?;
    }
    exchange.write_frame(&[0])?;
    Ok(())
}

fn send_object(
    exchange: &mut Exchange<'_>,
    store: &CodeStore,
    key: ObjectKey,
    length: u64,
) -> Result<(), AdmissionError> {
    let file = open_object(store, key.kind, key.hash)?;
    let mut remaining = length;
    if file.metadata().map_err(AdmissionError::Io)?.len() != length {
        return Err(AdmissionError::Hash(key.hash));
    }
    let mut file = file.take(length);
    exchange.write_frame(&key_frame(key))?;
    exchange.write_frame(&length.to_le_bytes())?;
    let mut scratch = [0; 8192];
    while remaining != 0 {
        let n = file.read(&mut scratch).map_err(AdmissionError::Io)?;
        if n == 0 {
            return Err(AdmissionError::Hash(key.hash));
        }
        exchange.write_frame(&scratch[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

fn key_frame(key: ObjectKey) -> [u8; 33] {
    let mut frame = [0; 33];
    frame[0] = tag(key.kind);
    frame[1..].copy_from_slice(&key.hash.0);
    frame
}

fn tag(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Group => 1,
        ObjectKind::Body => 2,
        ObjectKind::Annex => 3,
        ObjectKind::Recipe => unreachable!("recipes are in the request header"),
    }
}
