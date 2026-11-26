mod admission;
use admission::{Admission, Calls, HeldRefusal, Rates};

use crate::codec::{self, Bad, Done, Entry, Frame, Group, Outcome, Refusal, Row, StreamId, Want};
use crate::{BITS, CHUNKS, IMAGE_BYTES, MAX_FRAME, Result, flip};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

const WAL_MAGIC: &[u8] = b"WIREW8\0\x01";
const RECORD: usize = 53;
const OUTBOX: usize = 4;
const MAX_SUBS: usize = 128;
type Principal = [u8; 16];
type Stream = (Principal, StreamId);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub per_ip_sessions: usize,
    pub principal_rate: u32,
    pub principal_burst: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            per_ip_sessions: 32,
            principal_rate: 20,
            principal_burst: 40,
        }
    }
}
impl Limits {
    pub fn benchmark() -> Self {
        Self {
            per_ip_sessions: 20000,
            principal_rate: 1_000_000_000,
            principal_burst: 40,
        }
    }
}

pub struct Launch {
    pub target: String,
    pub n_place: u64,
}
struct Bucket {
    tokens: f64,
    at: std::time::Instant,
}
impl Bucket {
    fn take(&mut self, rate: u32, burst: u32) -> bool {
        let now = std::time::Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * f64::from(rate))
            .min(f64::from(burst));
        self.at = now;
        if self.tokens < 1.0 {
            false
        } else {
            self.tokens -= 1.0;
            true
        }
    }
}

#[derive(Clone, Debug)]
struct Saved {
    p: u64,
    bit: u32,
    value: bool,
}
struct Wal {
    file: File,
    bitmap: Vec<u8>,
    streams: HashMap<Stream, Saved>,
    version: u64,
    present: HashSet<u32>,
    #[cfg(test)]
    fail_sync: bool,
}
impl Wal {
    fn open(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock()?;
        let size = file.metadata()?.len();
        if size == 0 {
            file.write_all(WAL_MAGIC)?;
            file.sync_all()?;
            File::open(
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .sync_all()?;
        }
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if magic != WAL_MAGIC {
            return Err("invalid reference WAL header".into());
        }
        let mut wal = Self {
            file,
            bitmap: vec![0; IMAGE_BYTES],
            streams: HashMap::new(),
            version: 0,
            present: HashSet::new(),
            #[cfg(test)]
            fail_sync: false,
        };
        let mut record = [0; RECORD];
        loop {
            let mut n = 0;
            while n < RECORD {
                let got = wal.file.read(&mut record[n..])?;
                if got == 0 {
                    break;
                }
                n += got;
            }
            if n != RECORD {
                // A crash may leave only the last record incomplete. Retain every complete prefix.
                wal.file
                    .set_len(WAL_MAGIC.len() as u64 + wal.version * RECORD as u64)?;
                wal.file.seek(SeekFrom::End(0))?;
                wal.file.sync_data()?;
                break;
            }
            let principal = record[..16].try_into()?;
            let stream = StreamId {
                t: u64::from_le_bytes(record[16..24].try_into()?),
                n: u64::from_le_bytes(record[24..32].try_into()?),
            };
            let p = u64::from_le_bytes(record[32..40].try_into()?);
            let bit = u32::from_le_bytes(record[40..44].try_into()?);
            let version = u64::from_le_bytes(record[45..53].try_into()?);
            if bit >= BITS || record[44] > 1 || version != wal.version + 1 {
                return Err("invalid reference WAL record".into());
            }
            let value = flip(&mut wal.bitmap, bit);
            wal.present.insert(bit >> 9);
            if value != (record[44] == 1) {
                return Err("reference WAL parity differs".into());
            }
            wal.streams
                .insert((principal, stream), Saved { p, bit, value });
            wal.version = version;
        }
        Ok(wal)
    }

    fn durable(&self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_sync {
            return Err(std::io::Error::other("injected fdatasync failure"));
        }
        self.file.sync_data()
    }

    fn toggle(
        &mut self,
        principal: Principal,
        stream: StreamId,
        p: u64,
        bit: u32,
    ) -> Result<(Outcome, bool)> {
        if let Some(saved) = self.streams.get(&(principal, stream)) {
            if p < saved.p {
                return Ok((Outcome::Refused(Refusal::HistoryUnavailable), false));
            }
            if p == saved.p {
                // There is one callable entry and its only valid argument encoding is
                // the four-byte bit index. The persisted bit identifies that request.
                if bit != saved.bit {
                    return Ok((Outcome::Refused(Refusal::RequestMismatch), false));
                }
                return Ok((
                    Outcome::Returned(vec![0, 0, 0, 0, u8::from(saved.value)]),
                    false,
                ));
            }
        }
        let expected = self
            .streams
            .get(&(principal, stream))
            .map_or(0, |s| s.p + 1);
        if p != expected {
            return Ok((Outcome::Refused(Refusal::NotAllowed), false));
        }
        let value = flip(&mut self.bitmap, bit);
        self.present.insert(bit >> 9);
        self.version += 1;
        let mut record = Vec::with_capacity(RECORD);
        record.extend(principal);
        record.extend(stream.t.to_le_bytes());
        record.extend(stream.n.to_le_bytes());
        record.extend(p.to_le_bytes());
        record.extend(bit.to_le_bytes());
        record.push(u8::from(value));
        record.extend(self.version.to_le_bytes());
        self.file.write_all(&record)?;
        self.streams
            .insert((principal, stream), Saved { p, bit, value });
        Ok((Outcome::Returned(vec![0, 0, 0, 0, u8::from(value)]), true))
    }
}

#[derive(Clone, Debug)]
enum Output {
    Frame(Frame),
    Close(u16),
    Flush,
}
/// One connection's outbox. Equal contract with Hirsel's publisher (RUNNER-SPEC v2.20): a
/// scheduling pause never ends a subscription, and the client converges to the exact current
/// image. The mechanisms differ. This reference merges one pending frame per peer by
/// `(sub, key)`, and only a merged frame over the row allowance becomes a `Reset`. Hirsel
/// keeps no bounced rows: it marks the subscription due and, once the owner drains, sends one
/// whole `complete` at its base, which the client applies by replacing the image from empty.
struct Peer {
    principal: Principal,
    slots: Vec<Option<usize>>,
    subs: HashSet<u32>,
    tx: mpsc::Sender<Output>,
    pending: Option<Frame>,
    calls: Arc<Calls>,
    overrun: bool,
}
impl Peer {
    fn flush(&mut self) -> bool {
        if let Some(f) = self.pending.take() {
            match self.tx.try_send(Output::Frame(f)) {
                Ok(()) => (),
                Err(mpsc::error::TrySendError::Full(Output::Frame(f))) => self.pending = Some(f),
                Err(_) => return false,
            }
        }
        true
    }
    fn send(&mut self, mut frame: Frame) -> bool {
        self.calls.settle_at_delivery(&mut frame);
        if !self.flush() {
            return false;
        }
        let combined = if let Some(old) = self.pending.take() {
            let Some(merged) = coalesce(old, frame) else {
                return false;
            };
            merged
        } else {
            frame
        };
        let rows_over = match &combined {
            Frame::Batch(_, complete, rows, _) => {
                24 + 4 * complete.len() + rows.iter().map(group_bytes).sum::<usize>()
                    > MAX_FRAME - crate::OUTCOME_RESERVE
            }
            _ => false,
        };
        if rows_over {
            let (version, done) = match combined {
                Frame::Batch(v, _, _, d) | Frame::Reset(v, d) => (v, d),
                _ => return false,
            };
            if self.overrun {
                let _ = self.tx.try_send(Output::Close(1008));
                return false;
            }
            self.overrun = true;
            self.subs.clear();
            self.pending = Some(Frame::Reset(version, done));
        } else {
            self.pending = Some(combined);
        }
        self.flush()
    }
}

fn row_bytes(row: &Row) -> usize {
    2 + 4 + row.key.len() + 4 + row.value.as_ref().map_or(0, |v| 4 + v.len())
}

fn group_bytes(group: &Group) -> usize {
    12 + group.value.len() + group.rows.iter().map(row_bytes).sum::<usize>()
}

fn coalesce(old: Frame, new: Frame) -> Option<Frame> {
    match (old, new) {
        (
            Frame::Batch(_, mut complete, rows, mut done),
            Frame::Batch(version, completed, newer, outcomes),
        ) => {
            let mut kept: BTreeMap<u32, Group> =
                rows.into_iter().map(|group| (group.sub, group)).collect();
            for new_group in newer {
                if completed.contains(&new_group.sub) {
                    kept.insert(new_group.sub, new_group);
                } else if let Some(prior) = kept.get_mut(&new_group.sub) {
                    let mut rows: BTreeMap<(u16, Vec<u8>), Row> = std::mem::take(&mut prior.rows)
                        .into_iter()
                        .map(|row| ((row.slot, row.key.clone()), row))
                        .collect();
                    for row in new_group.rows {
                        rows.insert((row.slot, row.key.clone()), row);
                    }
                    prior.rows = rows.into_values().collect();
                    if !new_group.value.is_empty() {
                        prior.value = new_group.value;
                    }
                } else {
                    kept.insert(new_group.sub, new_group);
                }
            }
            done.extend(outcomes);
            complete.extend(completed);
            complete.sort_unstable();
            complete.dedup();
            Some(Frame::Batch(
                version,
                complete,
                kept.into_values().collect(),
                done,
            ))
        }
        (Frame::Reset(_, mut done), Frame::Batch(version, _, _, outcomes)) => {
            done.extend(outcomes);
            Some(Frame::Reset(version, done))
        }
        (
            Frame::Batch(_, _, _, mut done) | Frame::Reset(_, mut done),
            Frame::Reset(version, outcomes),
        ) => {
            done.extend(outcomes);
            Some(Frame::Reset(version, done))
        }
        // Control output cannot be coalesced. Closing preserves the bounded queue contract.
        _ => None,
    }
}

enum Command {
    Join {
        id: u64,
        principal: Principal,
        slots: Vec<Option<usize>>,
        tx: mpsc::Sender<Output>,
        calls: Arc<Calls>,
    },
    Frame(u64, Frame),
    Leave(u64),
    Barrier(oneshot::Sender<()>),
}
struct Head {
    wal: Wal,
    peers: HashMap<u64, Peer>,
    board_runs: u64,
    groups: u64,
}
impl Head {
    fn execute(&mut self, commands: Vec<Command>) -> Result<()> {
        let mut changed = HashSet::new();
        let mut replies: HashMap<u64, Vec<Done>> = HashMap::new();
        let mut snapshots = Vec::new();
        let mut barriers = Vec::new();
        let mut controls = Vec::new();
        for cmd in commands {
            match cmd {
                Command::Join {
                    id,
                    principal,
                    slots,
                    tx,
                    calls,
                } => {
                    self.peers.insert(
                        id,
                        Peer {
                            principal,
                            slots,
                            tx,
                            subs: HashSet::new(),
                            pending: None,
                            calls,
                            overrun: false,
                        },
                    );
                }
                Command::Leave(id) => {
                    self.peers.remove(&id);
                }
                Command::Barrier(reply) => barriers.push(reply),
                Command::Frame(id, f) => {
                    let Some(peer) = self.peers.get_mut(&id) else {
                        continue;
                    };
                    match f {
                        Frame::Call(stream, p, _, _, arg) => {
                            let (outcome, written) = self.wal.toggle(
                                peer.principal,
                                stream,
                                p,
                                u32::from_le_bytes(arg.try_into().unwrap()),
                            )?;
                            if written {
                                changed
                                    .insert(self.wal.streams[&(peer.principal, stream)].bit >> 9);
                            }
                            replies
                                .entry(id)
                                .or_default()
                                .push(Done { stream, p, outcome });
                        }
                        Frame::Subscribe(sub, slot, arg, resume) => {
                            if resume.is_some() {
                                peer.subs.clear();
                                controls.push((id, Frame::Reset(self.wal.version, vec![])));
                            } else if peer.slots.get(slot as usize) != Some(&Some(1)) {
                                controls.push((
                                    id,
                                    Frame::Ended(sub, Outcome::Refused(Refusal::NoEntry)),
                                ));
                            } else if peer.subs.len() == MAX_SUBS && !peer.subs.contains(&sub) {
                                controls.push((
                                    id,
                                    Frame::Ended(sub, Outcome::Refused(Refusal::NotAllowed)),
                                ));
                            } else if !arg.is_empty() {
                                controls.push((
                                    id,
                                    Frame::Ended(
                                        sub,
                                        Outcome::Refused(Refusal::Malformed(Bad {
                                            kind: 4,
                                            at: 0,
                                        })),
                                    ),
                                ));
                            } else {
                                peer.subs.insert(sub);
                                self.board_runs += 1;
                                snapshots.push((id, sub));
                            }
                        }
                        Frame::Unsubscribe(sub) => {
                            peer.subs.remove(&sub);
                        }
                        _ => {
                            let _ = peer.tx.try_send(Output::Close(1008));
                            self.peers.remove(&id);
                        }
                    }
                }
            }
        }
        // Linux File::sync_data is fdatasync. No row or outcome is enqueued before it succeeds.
        if !changed.is_empty() {
            self.wal.durable()?;
            self.groups += 1;
        }
        let version = self.wal.version;
        for (id, frame) in controls {
            if let Some(peer) = self.peers.get_mut(&id) {
                let f = match frame {
                    Frame::Reset(_, d) => Frame::Reset(version, d),
                    other => other,
                };
                if !peer.send(f) {
                    self.peers.remove(&id);
                }
            }
        }
        self.peers.retain(|id, peer| {
            let completing: Vec<u32> = snapshots
                .iter()
                .filter(|(i, sub)| i == id && peer.subs.contains(sub))
                .map(|(_, sub)| *sub)
                .collect();
            let mut done = replies.remove(id).unwrap_or_default();
            for sub in &completing {
                let rows: Vec<Row> = (0..CHUNKS)
                    .filter(|c| self.wal.present.contains(c))
                    .map(|c| codec::chunk_row(c, &self.wal.bitmap))
                    .collect();
                let requested = rows.iter().map(row_bytes).sum::<usize>();
                let available = MAX_FRAME - crate::OUTCOME_RESERVE - 40;
                if requested > available {
                    peer.subs.remove(sub);
                    if !peer.send(Frame::Ended(
                        *sub,
                        Outcome::Refused(Refusal::Budget(codec::Room {
                            requested: requested as u64,
                            available: available as u64,
                        })),
                    )) {
                        return false;
                    }
                } else if !peer.send(Frame::Batch(
                    version,
                    vec![*sub],
                    vec![Group::board(*sub, rows)],
                    std::mem::take(&mut done),
                )) {
                    return false;
                }
            }
            let mut rows = Vec::new();
            if !changed.is_empty() {
                for sub in &peer.subs {
                    if !completing.contains(sub) {
                        rows.push(Group::board(
                            *sub,
                            changed
                                .iter()
                                .map(|chunk| codec::chunk_row(*chunk, &self.wal.bitmap))
                                .collect(),
                        ));
                    }
                }
            }
            if !rows.is_empty() || !done.is_empty() {
                peer.send(Frame::Batch(version, vec![], rows, done))
            } else {
                peer.flush()
            }
        });
        for reply in barriers {
            let _ = reply.send(());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Stats {
    pub version: u64,
    pub board_runs: u64,
    pub durable_groups: u64,
}

pub async fn serve(
    listener: TcpListener,
    wal: &Path,
    wants: Vec<Want>,
    limits: Limits,
    launch: Launch,
    stop: impl Future<Output = ()>,
) -> Result<Stats> {
    let wal = Wal::open(wal)?;
    let digest = Arc::new(Sha256::digest(std::fs::read(std::env::current_exe()?)?).to_vec());
    let wants = Arc::new(wants);
    let launch = Arc::new(launch);
    let (tx, mut rx) = mpsc::channel(4096);
    let mut head = Head {
        wal,
        peers: HashMap::new(),
        board_runs: 0,
        groups: 0,
    };
    let actor = tokio::spawn(async move {
        loop {
            let mut commands = Vec::new();
            match tokio::time::timeout(Duration::from_millis(1), rx.recv()).await {
                Ok(Some(c)) => {
                    commands.push(c);
                    // One bounded group window, independent of arrival rate.
                    let end = tokio::time::Instant::now() + Duration::from_millis(1);
                    while commands.len() < 1024 {
                        match tokio::time::timeout_at(end, rx.recv()).await {
                            Ok(Some(c)) => commands.push(c),
                            _ => break,
                        }
                    }
                }
                Ok(None) => break,
                Err(_) if head.peers.values().all(|p| p.pending.is_none()) => continue,
                Err(_) => (),
            }
            head = tokio::task::spawn_blocking(move || -> Result<Head> {
                head.execute(commands)?;
                Ok(head)
            })
            .await??;
        }
        Ok::<_, crate::Error>(Stats {
            version: head.wal.version,
            board_runs: head.board_runs,
            durable_groups: head.groups,
        })
    });
    let mut sessions = JoinSet::new();
    let mut next = 0u64;
    let mut ip_slots: HashMap<std::net::IpAddr, Arc<Semaphore>> = HashMap::new();
    let pending = Arc::new(Semaphore::new(1024));
    let rates: Rates = Arc::new(Mutex::new(HashMap::new()));
    tokio::pin!(stop);
    let result: Result<()> = loop {
        tokio::select! {
            _ = &mut stop => break Ok(()),
            accepted = listener.accept() => {
                let (mut socket, addr) = match accepted { Ok(s) => s, Err(e) => break Err(e.into()) };
                let slots = ip_slots.entry(addr.ip()).or_insert_with(|| Arc::new(Semaphore::new(limits.per_ip_sessions))).clone();
                let permits = slots.try_acquire_owned().and_then(|ip| pending.clone().try_acquire_owned().map(|p| (ip, p)));
                let Ok((ip_permit, pending_permit)) = permits else {
                    sessions.spawn(async move {
                        socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                        Ok(())
                    }); continue;
                };
                next += 1; let id = next; let tx = tx.clone(); let digest = digest.clone(); let wants = wants.clone();
                let launch = launch.clone(); let rates = rates.clone();
                sessions.spawn(async move {
                    let _ip = ip_permit;
                    let ingress = Ingress { pending: pending_permit, limits, rates };
                    let r = session(socket, id, tx.clone(), digest, wants, ingress, launch).await;
                    let _ = tx.send(Command::Leave(id)).await; r
                });
            }
            joined = sessions.join_next(), if !sessions.is_empty() => {
                if let Some(Err(e)) = joined { break Err(e.into()); }
            }
        }
    };
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    // Finish every Call already admitted to the durable head before shutdown returns.
    let (reply, done) = oneshot::channel();
    let _ = tx.send(Command::Barrier(reply)).await;
    let _ = done.await;
    drop(tx);
    let stats = actor.await??;
    result?;
    Ok(stats)
}

struct Ingress {
    pending: OwnedSemaphorePermit,
    limits: Limits,
    rates: Rates,
}

#[allow(
    clippy::result_large_err,
    reason = "Tungstenite's Callback API requires an HTTP response by value."
)]
fn handshake(
    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
    mut response: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> std::result::Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    use tokio_tungstenite::tungstenite::http::Response;
    if request.uri().path() != "/hirsel" {
        return Err(Response::builder().status(404).body(None).unwrap());
    }
    let current = request
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(',')
        .any(|token| token.trim() == "hirsel.4");
    if !current {
        return Err(Response::builder().status(400).body(None).unwrap());
    }
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "hirsel.4".parse().unwrap());
    Ok(response)
}

async fn session(
    socket: TcpStream,
    id: u64,
    tx: mpsc::Sender<Command>,
    digest: Arc<Vec<u8>>,
    wants: Arc<Vec<Want>>,
    ingress: Ingress,
    launch: Arc<Launch>,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let mut ws = tokio::time::timeout(
        Duration::from_millis(5000),
        tokio_tungstenite::accept_hdr_async_with_config(
            socket,
            handshake,
            Some(crate::websocket_config()),
        ),
    )
    .await??;
    let first = tokio::time::timeout(Duration::from_millis(10000), async {
        loop {
            match ws.next().await.transpose()? {
                Some(Message::Binary(b)) => break codec::decode(&b),
                Some(Message::Ping(_)) => ws.flush().await?,
                _ => return Err("expected Hello".into()),
            }
        }
    })
    .await??;
    let Frame::Hello(credential, requested) = first else {
        return Err("expected Hello first".into());
    };
    let principal: Principal = match credential {
        Some(bytes) if bytes.len() == 16 => {
            let mut hash = Sha256::new();
            hash.update(b"hirsel.example.credential.v1");
            hash.update(bytes);
            hash.finalize()[..16].try_into()?
        }
        _ => {
            ws.close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "invalid credential".into(),
            }))
            .await?;
            return Ok(());
        }
    };
    drop(ingress.pending);
    ws.send(Message::Binary(
        codec::encode(&Frame::Welcome(
            codec::Caller {
                issuer: 2,
                subject: principal.to_vec(),
            },
            4,
            1024 * 1024,
            digest.to_vec(),
            launch.target.clone(),
            launch.n_place,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            // The reference WAL keeps every stream's last call for its whole life.
            u64::MAX,
        ))
        .into(),
    ))
    .await?;
    let mut slots = Vec::new();
    for (index, want) in requested.into_iter().enumerate() {
        let slot = wants.iter().position(|w| *w == want);
        slots.push(slot);
        let entry = match slot {
            Some(_) => Entry::Ready(index.try_into()?),
            None => Entry::Gone,
        };
        ws.send(Message::Binary(codec::encode(&Frame::Placed(entry)).into()))
            .await?;
    }
    let calls = Arc::new(Calls::new(
        principal,
        slots.clone(),
        ingress.limits,
        ingress.rates,
    ));
    let (out, mut outgoing) = mpsc::channel(OUTBOX);
    tx.send(Command::Join {
        id,
        principal,
        slots,
        tx: out.clone(),
        calls: calls.clone(),
    })
    .await?;
    let (mut writer, mut reader) = ws.split();
    let (refused, mut refusals) = mpsc::channel::<HeldRefusal>(1 + crate::Q);
    let read = async {
        while let Some(message) = reader.next().await {
            let f = match message? {
                Message::Binary(bytes) => match codec::decode(&bytes) {
                    Ok(f)
                        if matches!(
                            f,
                            Frame::Hello(..)
                                | Frame::Call(..)
                                | Frame::Subscribe(..)
                                | Frame::Unsubscribe(..)
                        ) =>
                    {
                        f
                    }
                    _ => {
                        return Ok::<_, crate::Error>(1007);
                    }
                },
                Message::Ping(_) => {
                    out.send(Output::Flush).await?;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(_) => return Ok(1000),
                _ => return Ok(1003),
            };
            if matches!(f, Frame::Hello(..)) {
                return Ok(1008);
            }
            if let Frame::Call(stream, p, slot, deadline, arg) = &f {
                match calls.admit(*stream, *p, *slot, *deadline, arg) {
                    Admission::Forward => (),
                    Admission::Refused(row) => {
                        refused
                            .try_send(row)
                            .unwrap_or_else(|_| unreachable!("refusal permits bound the queue"));
                        continue;
                    }
                    Admission::Close => return Ok(1008),
                }
            }
            tx.send(Command::Frame(id, f)).await?;
        }
        Ok(1000)
    };
    let write = async {
        let mut last_version = 0;
        loop {
            let output = tokio::select! {
                output = outgoing.recv() => match output { Some(output) => output, None => return Ok(1008) },
                row = refusals.recv() => {
                    let Some(HeldRefusal { done, unit }) = row else { return Ok(1008); };
                    drop(unit);
                    writer.send(Message::Binary(codec::encode(&Frame::Batch(last_version, vec![], vec![], vec![done])).into())).await?;
                    continue;
                }
            };
            match output {
                Output::Frame(mut f) => {
                    if let Frame::Batch(v, _, _, _) | Frame::Reset(v, _) = &f {
                        last_version = *v;
                    }
                    calls.hand_to_writer(&mut f);
                    writer
                        .send(Message::Binary(codec::encode(&f).into()))
                        .await?;
                }
                Output::Close(code) => return Ok::<_, crate::Error>(code),
                Output::Flush => writer.flush().await?,
            }
        }
    };
    let code = tokio::select! { r = read => r?, r = write => r? };
    let close = CloseFrame {
        code: CloseCode::from(code),
        reason: "".into(),
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        writer.send(Message::Close(Some(close))),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests;
