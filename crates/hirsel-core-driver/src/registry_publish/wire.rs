use super::{Failure, METADATA_LIMIT, decimal, hex, now};
use crate::package_content::{ARCHIVE_LIMIT, CONTENT_LIMIT, Id, Root};
use crate::package_manifest::name::Version;
use crate::package_store::PackageStore;
use crate::registry_fetch::Client;
use crate::registry_verify::{Release, ReleaseClaim};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, Write};
use std::slice;
use zstd_safe::{CCtx, get_error_name};

const FRAME_LIMIT: usize = 65 << 20;
const REPLY_LIMIT: usize = 4096;

pub(super) struct Frames {
    file: File,
    offsets: BTreeMap<Root, (u64, usize)>,
    encoder: CCtx<'static>,
    compressed: Vec<u8>,
}

impl Frames {
    pub(super) fn new() -> Result<Self, Failure> {
        Ok(Self {
            file: tempfile::tempfile()?,
            offsets: BTreeMap::new(),
            encoder: crate::package_content::frame_encoder()?,
            compressed: Vec::new(),
        })
    }

    pub(super) fn stage(&mut self, root: Root, bytes: &[u8]) -> Result<(), Failure> {
        if self.offsets.contains_key(&root) {
            return Ok(());
        }
        if bytes.len() > CONTENT_LIMIT {
            return Err(format!("{root}: package expands past 64 MiB").into());
        }
        self.compressed.clear();
        self.compressed
            .reserve(zstd_safe::compress_bound(bytes.len()));
        self.encoder
            .compress2(&mut self.compressed, bytes)
            .map_err(get_error_name)?;
        let len = self.compressed.len();
        if len > FRAME_LIMIT {
            return Err(format!("{root}: compressed frame exceeds 65 MiB").into());
        }
        let at = self.file.stream_position()?;
        self.file.write_all(&(len as u32).to_be_bytes())?;
        self.file.write_all(&self.compressed)?;
        self.offsets.insert(root, (at, len));
        Ok(())
    }

    pub(super) fn finish_staging(&mut self) {
        self.compressed = Vec::new();
    }

    pub(super) fn check_total(&self, roots: &[Root]) -> Result<(), Failure> {
        let size: u64 = roots.iter().map(|root| self.offsets[root].1 as u64).sum();
        if size > ARCHIVE_LIMIT as u64 {
            return Err("compressed submission exceeds 512 MiB".into());
        }
        Ok(())
    }
}

struct Body<'a> {
    metadata: Cursor<Vec<u8>>,
    roots: slice::Iter<'a, Root>,
    frames: &'a mut Frames,
    remaining: usize,
}

impl Read for Body<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let n = self.metadata.read(output)?;
        if n > 0 || output.is_empty() {
            return Ok(n);
        }
        if self.remaining == 0 {
            let Some(root) = self.roots.next() else {
                return Ok(0);
            };
            let (offset, len) = self.frames.offsets[root];
            self.frames.file.seek(io::SeekFrom::Start(offset))?;
            self.remaining = len + 4;
        }
        let n = self.remaining.min(output.len());
        self.frames.file.read_exact(&mut output[..n])?;
        self.remaining -= n;
        Ok(n)
    }
}

pub(super) struct Receipt {
    checkpoint_origin: String,
    pub(super) index: u64,
    pub(super) head_size: u64,
}

pub(super) fn submit(
    store: &PackageStore,
    client: &Client<'_>,
    root: Root,
    metadata: &Value,
    roots: &[Root],
    frames: &mut Frames,
) -> Result<Receipt, Failure> {
    let mut bytes = vec![0; 4];
    serde_json::to_writer(&mut bytes, metadata)?;
    let length = bytes.len() - 4;
    if length > METADATA_LIMIT {
        return Err("submission metadata exceeds 1 MiB".into());
    }
    bytes[..4].copy_from_slice(&(length as u32).to_be_bytes());
    let size = bytes.len() as u64
        + roots
            .iter()
            .map(|root| frames.offsets[root].1 as u64 + 4)
            .sum::<u64>();
    let mut body = Body {
        metadata: Cursor::new(bytes),
        roots: roots.iter(),
        frames,
        remaining: 0,
    };
    let agent = store
        .transport
        .agent()
        .ok_or("registry publication needs network access")?;
    let origin = client.route().origin();
    let response = agent
        .post(format!("{origin}/v1/submit"))
        .header("content-type", "application/octet-stream")
        .header("content-length", size.to_string())
        .config()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .send(ureq::SendBody::from_reader(&mut body))
        .map_err(|_| format!("{origin}: submit transport failed; the commit may be uncertain"))?;
    let status = response.status().as_u16();
    let mut reply = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(REPLY_LIMIT as u64 + 1)
        .read_to_end(&mut reply)
        .map_err(|_| format!("{origin}: could not read submit reply (HTTP {status})"))?;
    if reply.len() > REPLY_LIMIT {
        return Err(format!("{origin}: oversized submit reply (HTTP {status})").into());
    }
    if status != 200 {
        let reason = match status {
            403 => "unauthorized",
            409 => "conflict",
            413 => "too-large",
            422 => "invalid-input",
            503 => "unavailable",
            _ => return Err(format!("{origin}: unexpected submit HTTP {status}").into()),
        };
        if reply != format!("[{reason:?}]").as_bytes() {
            return Err(format!("{origin}: invalid submit refusal (HTTP {status})").into());
        }
        return Err(format!("{origin}: {reason} (HTTP {status})").into());
    }
    let bad = || format!("{origin}: invalid registry publish receipt");
    let row: [String; 4] = serde_json::from_slice(&reply).map_err(|_| bad())?;
    if serde_json::to_vec(&row)? != reply || row[3] != hex(&root.0) {
        return Err(bad().into());
    }
    let [checkpoint_origin, index, head_size, _] = row;
    let index = decimal(&index).ok_or_else(bad)?;
    let head_size = decimal(&head_size)
        .filter(|size| *size > index)
        .ok_or_else(bad)?;
    Ok(Receipt {
        checkpoint_origin,
        index,
        head_size,
    })
}

impl Receipt {
    pub(super) fn verify(
        &self,
        client: &Client<'_>,
        id: &Id,
        version: &Version,
        root: Root,
    ) -> Result<Release, Failure> {
        let origin = client.route().origin().to_string();
        let authority = origin.strip_prefix("https://").expect("HTTPS route");
        if self.checkpoint_origin != format!("{authority}/log") {
            return Err(format!("{origin}: receipt names the wrong checkpoint origin").into());
        }
        let head = now()
            .and_then(|time| client.release_checkpoint(time))
            .map_err(|error| -> Failure { error })?;
        if head.size() < self.head_size {
            return Err(
                format!("{origin}: registry has not exposed the receipt's covering head").into(),
            );
        }
        let release = client
            .release(
                &head,
                self.index,
                ReleaseClaim {
                    identity: &id.to_string(),
                    version: &version.to_string(),
                    root,
                    facts: None,
                },
            )
            .map_err(|error| -> Failure { error })?;
        Ok(release)
    }
}
