use super::notes::{Checkpoint, Log, conflict};
use super::tiles::{self, Frontier, WIDTH};
use super::{
    Error, Hash, Release, Root, Value, Version, decimal, entry_json, json_digest, valid_auth,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusReplay {
    head: Option<Checkpoint>,
    frontier: Frontier,
    states: BTreeMap<Root, Status>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Disposition {
    #[default]
    Active,
    Retired {
        reason: String,
        message: String,
    },
    TakenDown {
        reason: String,
        message: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Status {
    last: Option<Hash>,
    pub(crate) disposition: Disposition,
    /// Current canonical OSV documents, preserving all advisory fields.
    pub(crate) advisories: BTreeMap<String, Arc<[u8]>>,
}

impl StatusReplay {
    pub(crate) fn checkpoint(&self) -> Option<&Checkpoint> {
        self.head.as_ref()
    }

    pub(crate) fn state(&self, root: Root) -> Option<&Status> {
        self.states.get(&root)
    }

    pub(crate) fn snapshot(&self) -> Result<Vec<u8>, serde_json::Error> {
        let levels = self.frontier.levels();
        let states: Vec<_> = self
            .states
            .iter()
            .map(|(root, state)| (&root.0, state))
            .collect();
        serde_json::to_vec(&(levels.as_slice(), states))
    }

    pub(crate) fn restore(head: Checkpoint, snapshot: &[u8]) -> Result<Self, Error> {
        type Snapshot = (Vec<Option<Hash>>, Vec<(Hash, Status)>);
        let (levels, records): Snapshot =
            serde_json::from_slice(snapshot).map_err(|_| Error::Entry)?;
        let frontier = Frontier::restore(head.size(), levels).ok_or(Error::MerkleRoot)?;
        if head.log() != Log::Status || frontier.root() != head.root() {
            return Err(Error::MerkleRoot);
        }
        let mut states = BTreeMap::new();
        for (root, status) in records {
            if status.last.is_none() || states.insert(Root(root), status).is_some() {
                return Err(Error::StatusTransition);
            }
        }
        Ok(Self {
            head: Some(head),
            frontier,
            states,
        })
    }

    /// Replays every entry through the accepted status checkpoint. The evidence
    /// callback supplies a verified release from this registry and its OSV document.
    /// Retains the prior frontier, statuses and note on every refusal.
    pub(crate) fn advance<R: Borrow<Release>>(
        &mut self,
        head: &Checkpoint,
        now: u64,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, Error>,
        mut advisory: impl FnMut(Root, Hash) -> Result<(R, Vec<u8>), Error>,
    ) -> Result<(), Error> {
        if head.log() != Log::Status {
            return Err(Error::Origin);
        }
        if let Some(previous) = &self.head {
            if !head.same_trust(previous) {
                return Err(Error::Trust);
            }
            if head.size() < previous.size()
                || (head.size() == previous.size() && head.root() != previous.root())
            {
                return Err(conflict(previous, head));
            }
        }
        head.check_fresh(now)?;
        let mut frontier = self.frontier.clone();
        let mut changes: BTreeMap<Root, Status> = BTreeMap::new();
        let mut pending = Vec::new();
        while frontier.size() < head.size() {
            let index = frontier.size() / WIDTH;
            let skip = (frontier.size() % WIDTH) as usize;
            let bundle = tiles::read_bundle(head.size(), index, &mut read)?;
            for entry in tiles::bundle_entries(&bundle, head.size(), index)?
                .into_iter()
                .skip(skip)
            {
                let hash = tiles::leaf(entry);
                let event = entry_json(entry)?;
                let event = Event::parse(&event).ok_or(Error::Entry)?;
                let status = changes
                    .entry(event.root)
                    .or_insert_with(|| self.states.get(&event.root).cloned().unwrap_or_default());
                if event.previous != status.last
                    || matches!(status.disposition, Disposition::TakenDown { .. })
                {
                    return Err(Error::StatusTransition);
                }
                match event.action {
                    Action::Retire(reason, message)
                        if status.disposition == Disposition::Active =>
                    {
                        status.disposition = Disposition::Retired {
                            reason: reason.into(),
                            message: message.into(),
                        };
                    }
                    Action::Reinstate
                        if matches!(status.disposition, Disposition::Retired { .. }) =>
                    {
                        status.disposition = Disposition::Active;
                    }
                    Action::Takedown(reason, message) => {
                        status.disposition = Disposition::TakenDown {
                            reason: reason.into(),
                            message: message.into(),
                        };
                    }
                    Action::Advise(id, digest) => pending.push((event.root, id.to_owned(), digest)),
                    _ => return Err(Error::StatusTransition),
                }
                status.last = Some(hash);
                frontier.append(hash)?;
            }
        }
        if frontier.root() != head.root() {
            return Err(Error::MerkleRoot);
        }
        for (root, id, digest) in pending {
            let (release, bytes) = advisory(root, digest)?;
            let release = release.borrow();
            if release.root() != root
                || release.authority != head.authority()
                || Hash::from(Sha256::digest(&bytes)) != digest
            {
                return Err(Error::Advisory);
            }
            let withdrawn = verify_osv(&bytes, &id, release).ok_or(Error::Advisory)?;
            let advisories = &mut changes
                .get_mut(&root)
                .expect("status for advisory root")
                .advisories;
            if withdrawn {
                advisories.remove(&id);
            } else {
                advisories.insert(id, bytes.into());
            }
        }
        self.states.extend(changes);
        self.frontier = frontier;
        self.head = Some(head.clone());
        Ok(())
    }
}

struct Event<'a> {
    root: Root,
    previous: Option<Hash>,
    action: Action<'a>,
}

enum Action<'a> {
    Retire(&'a str, &'a str),
    Reinstate,
    Advise(&'a str, Hash),
    Takedown(&'a str, &'a str),
}

impl<'a> Event<'a> {
    fn parse(value: &'a Value) -> Option<Self> {
        let [tag, root, previous, payload @ .., time, auth] = value.as_array()?.as_slice() else {
            return None;
        };
        let root = Root(json_digest(root)?);
        let previous = if previous.is_null() {
            None
        } else {
            Some(json_digest(previous)?)
        };
        decimal(time.as_str()?)?;
        if !valid_auth(auth) {
            return None;
        }
        let action = match (tag.as_str()?, payload) {
            ("retire", [reason, message]) => {
                let reason = reason.as_str()?;
                if !matches!(
                    reason,
                    "security" | "deprecated" | "invalid" | "renamed" | "other"
                ) {
                    return None;
                }
                Action::Retire(reason, message.as_str()?)
            }
            ("reinstate", [message]) if message.is_string() => Action::Reinstate,
            ("advise", [id, digest]) if id.as_str().is_some_and(|id| !id.is_empty()) => {
                Action::Advise(id.as_str()?, json_digest(digest)?)
            }
            ("takedown", [reason, message]) if auth[0][0] == "ed25519" => {
                Action::Takedown(reason.as_str()?, message.as_str()?)
            }
            _ => return None,
        };
        Some(Self {
            root,
            previous,
            action,
        })
    }
}

fn verify_osv(bytes: &[u8], id: &str, release: &Release) -> Option<bool> {
    if bytes.len() > 1 << 20 {
        return None;
    }
    let doc: Value = serde_json::from_slice(bytes).ok()?;
    let mut raw = bytes;
    canonical_json(&mut raw)?;
    if !raw.is_empty()
        || doc.get("id")?.as_str()? != id
        || doc.get("modified")?.as_str()?.is_empty()
    {
        return None;
    }
    let mut affected = false;
    let version_text = release.version().to_string();
    for item in doc.get("affected")?.as_array()? {
        let Some(package) = item.get("package") else {
            continue;
        };
        if package.get("ecosystem")?.as_str()? != "Hirsel"
            || package.get("name")?.as_str()? != release.id().name
        {
            continue;
        }
        if let Some(versions) = item.get("versions") {
            for version in versions.as_array()? {
                affected |= version.as_str()? == version_text;
            }
        }
        if let Some(ranges) = item.get("ranges") {
            for range in ranges.as_array()? {
                if range.get("type")?.as_str()? == "SEMVER" {
                    affected |=
                        covered_by_range(range.get("events")?.as_array()?, release.version())?;
                }
            }
        }
    }
    if !affected {
        return None;
    }
    match doc.get("withdrawn") {
        None => Some(false),
        Some(withdrawn) => (!withdrawn.as_str()?.is_empty()).then_some(true),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RangeEvent {
    Fixed,
    LastAffected,
    Introduced,
}

fn covered_by_range(events: &[Value], version: &Version) -> Option<bool> {
    let mut boundaries = Vec::new();
    let mut limited = false;
    let mut before_limit = false;
    let mut has_introduction = false;
    let mut has_fixed = false;
    let mut has_last_affected = false;
    for event in events {
        let event = event.as_object()?;
        if event.len() != 1 {
            return None;
        }
        let (tag, value) = event.iter().next()?;
        let text = value.as_str()?;
        let kind = match tag.as_str() {
            "introduced" => {
                has_introduction = true;
                RangeEvent::Introduced
            }
            "fixed" => {
                has_fixed = true;
                RangeEvent::Fixed
            }
            "last_affected" => {
                has_last_affected = true;
                RangeEvent::LastAffected
            }
            "limit" => {
                limited = true;
                before_limit |= text == "*" || version < &osv_version(text)?;
                continue;
            }
            _ => return None,
        };
        let boundary = if kind == RangeEvent::Introduced && text == "0" {
            None
        } else {
            Some(osv_version(text)?)
        };
        boundaries.push((boundary, kind));
    }
    if !has_introduction || (has_fixed && has_last_affected) {
        return None;
    }
    if limited && !before_limit {
        return Some(false);
    }
    boundaries.sort_unstable();
    let mut affected = false;
    for (boundary, kind) in boundaries {
        match kind {
            RangeEvent::Introduced if boundary.as_ref().is_none_or(|from| version >= from) => {
                affected = true;
            }
            RangeEvent::Fixed if boundary.as_ref().is_some_and(|until| version >= until) => {
                affected = false;
            }
            RangeEvent::LastAffected if boundary.as_ref().is_some_and(|last| version > last) => {
                affected = false;
            }
            _ => {}
        }
    }
    Some(affected)
}

fn osv_version(text: &str) -> Option<Version> {
    let Some((version, build)) = text.split_once('+') else {
        return Version::parse(text);
    };
    if build
        .split('.')
        .any(|id| id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
        return None;
    }
    Version::parse(version)
}

/// Check JCS spelling and strict UTF-16 key order on already parsed OSV JSON.
/// Reading numbers from the source preserves correct rounding independent of
/// serde_json's float parser configuration.
fn canonical_json(input: &mut &[u8]) -> Option<()> {
    match input.first()? {
        b'{' => {
            *input = &input[1..];
            if let Some(rest) = input.strip_prefix(b"}") {
                *input = rest;
                return Some(());
            }
            let mut previous: Option<String> = None;
            loop {
                let key = canonical_string(input)?;
                if previous
                    .as_ref()
                    .is_some_and(|p| !p.encode_utf16().cmp(key.encode_utf16()).is_lt())
                {
                    return None;
                }
                previous = Some(key);
                *input = input.strip_prefix(b":")?;
                canonical_json(input)?;
                if let Some(rest) = input.strip_prefix(b"}") {
                    *input = rest;
                    return Some(());
                }
                *input = input.strip_prefix(b",")?;
            }
        }
        b'[' => {
            *input = &input[1..];
            if let Some(rest) = input.strip_prefix(b"]") {
                *input = rest;
                return Some(());
            }
            loop {
                canonical_json(input)?;
                if let Some(rest) = input.strip_prefix(b"]") {
                    *input = rest;
                    return Some(());
                }
                *input = input.strip_prefix(b",")?;
            }
        }
        b'"' => {
            canonical_string(input)?;
        }
        b'n' => *input = input.strip_prefix(b"null")?,
        b't' => *input = input.strip_prefix(b"true")?,
        b'f' => *input = input.strip_prefix(b"false")?,
        b'-' | b'0'..=b'9' => {
            let end = input
                .iter()
                .position(|b| matches!(b, b',' | b']' | b'}'))
                .unwrap_or(input.len());
            let text = std::str::from_utf8(&input[..end]).ok()?;
            if text != jcs_number(text.parse().ok()?)? {
                return None;
            }
            *input = &input[end..];
        }
        _ => return None,
    }
    Some(())
}

fn canonical_string(input: &mut &[u8]) -> Option<String> {
    let mut stream = serde_json::Deserializer::from_slice(input).into_iter::<String>();
    let value = stream.next()?.ok()?;
    let (encoded, rest) = input.split_at(stream.byte_offset());
    if serde_json::to_vec(&value).ok()? != encoded {
        return None;
    }
    *input = rest;
    Some(value)
}

fn jcs_number(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some("0".into());
    }
    let shortest = serde_json::to_string(&value.abs()).ok()?;
    let (mantissa, exponent) = match shortest.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (shortest.as_str(), 0),
    };
    let decimal = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits = mantissa.replace('.', "");
    let significant = digits.trim_start_matches('0');
    let point = decimal + exponent - (digits.len() - significant.len()) as i32;
    let significant = significant.trim_end_matches('0');
    let mut out = if value < 0.0 {
        String::from("-")
    } else {
        String::new()
    };
    if point > 0 && point <= 21 {
        let point = point as usize;
        if point < significant.len() {
            out.push_str(&significant[..point]);
            out.push('.');
            out.push_str(&significant[point..]);
        } else {
            out.push_str(significant);
            out.extend(std::iter::repeat_n('0', point - significant.len()));
        }
    } else if point > -6 && point <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-point) as usize));
        out.push_str(significant);
    } else {
        let (leading, rest) = significant.split_at(1);
        out.push_str(leading);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push_str(&format!("e{:+}", point - 1));
    }
    Some(out)
}
