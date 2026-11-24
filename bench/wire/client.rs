use crate::codec::{self, Done, Entry, Frame, Outcome, StreamId, Want};
use crate::{BITS, IMAGE_BYTES, Result, flip};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub struct Client {
    pub socket: Socket,
    pub credential: Vec<u8>,
    pub image: Vec<u8>,
    pub version: u64,
    pub toggle_slot: u32,
    pub board_slot: u32,
    /// The one stream this principal's calls use. A reconnect that resends a call keeps it.
    pub stream: StreamId,
    /// `complete`s that replaced a live image: the publisher's repairs (RUNNER-SPEC §3 "Repair").
    pub repairs: usize,
    resubscribes: usize,
    subscribed: bool,
    sub: u32,
    /// The current subscription's first `complete` has not arrived.
    pending: bool,
}
impl Client {
    pub async fn connect(url: &str, wants: &[Want], credential: Option<Vec<u8>>) -> Result<Self> {
        let mut request = url.into_client_request()?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "hirsel.4".parse()?);
        let (mut socket, _) = tokio_tungstenite::connect_async_with_config(
            request,
            Some(crate::websocket_config()),
            true,
        )
        .await?;
        if let MaybeTlsStream::Plain(tcp) = socket.get_ref() {
            tcp.set_nodelay(true)?;
        }
        let credential = credential.unwrap_or_else(|| rand::random::<[u8; 16]>().to_vec());
        socket
            .send(Message::Binary(
                codec::encode(&Frame::Hello(Some(credential.clone()), wants.to_vec())).into(),
            ))
            .await?;
        // Mint at the server's keyed time, so the stream is never in its future.
        let Frame::Welcome(_, _, _, _, _, _, keyed_at, _) = read_frame(&mut socket).await? else {
            return Err("expected Welcome first".into());
        };
        let stream = StreamId {
            t: keyed_at,
            n: rand::random(),
        };
        let mut slots = Vec::new();
        for _ in wants {
            match read_frame(&mut socket).await? {
                // Call.slot indexes the Hello list, regardless of Ready's registry slot.
                Frame::Placed(Entry::Ready(_)) => slots.push((slots.len()) as u32),
                _ => return Err("checkbox entry not Ready; pass the generated --wants file".into()),
            }
        }
        if slots.len() != 2 {
            return Err("expected [toggle, board] entries".into());
        }
        Ok(Self {
            socket,
            credential,
            image: vec![0; IMAGE_BYTES],
            version: 0,
            toggle_slot: slots[0],
            board_slot: slots[1],
            stream,
            repairs: 0,
            resubscribes: 0,
            subscribed: false,
            sub: 0,
            pending: false,
        })
    }
    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        self.send_timed(frame).await.map(|_| ())
    }
    async fn send_timed(&mut self, frame: Frame) -> Result<Instant> {
        let message = Message::Binary(codec::encode(&frame).into());
        let at = Instant::now();
        self.socket.send(message).await?;
        Ok(at)
    }
    pub async fn subscribe(&mut self) -> Result<()> {
        self.sub = self
            .sub
            .checked_add(1)
            .ok_or("subscription ids exhausted")?;
        self.pending = true;
        self.subscribed = true;
        self.send(Frame::Subscribe(self.sub, self.board_slot, vec![], None))
            .await
    }
    pub async fn snapshot(&mut self) -> Result<usize> {
        self.subscribe().await?;
        loop {
            if let Frame::Batch(_, complete, rows, _) = self.receive().await?
                && complete.contains(&self.sub)
            {
                return Ok(rows
                    .iter()
                    .filter(|group| group.sub == self.sub)
                    .map(|group| group.rows.len())
                    .sum());
            }
        }
    }
    pub async fn receive(&mut self) -> Result<Frame> {
        self.receive_timed().await.map(|(_, f)| f)
    }
    pub async fn receive_timed(&mut self) -> Result<(Instant, Frame)> {
        let (at, frame) = read_frame_timed(&mut self.socket).await?;
        match &frame {
            Frame::Batch(version, complete, rows, _) => {
                if *version < self.version {
                    return Err("session version decreased".into());
                }
                self.version = *version;
                if apply_batch(&mut self.image, &mut self.pending, self.sub, complete, rows)? {
                    self.repairs += 1;
                }
            }
            Frame::Reset(version, _) => {
                if *version < self.version {
                    return Err("Reset version decreased".into());
                }
                self.version = *version;
                self.image.fill(0);
                if self.subscribed {
                    self.subscribe().await?;
                    self.resubscribes += 1;
                }
            }
            Frame::Ended(_, outcome) => return Err(format!("board ended: {outcome:?}").into()),
            _ => return Err("unexpected server frame after Placed".into()),
        }
        Ok((at, frame))
    }
    pub async fn start_call(&mut self, p: u64, bit: u32) -> Result<()> {
        self.start_call_timed(p, bit).await.map(|_| ())
    }
    pub async fn start_call_timed(&mut self, p: u64, bit: u32) -> Result<Instant> {
        self.send_timed(Frame::Call(
            self.stream,
            p,
            self.toggle_slot,
            30_000,
            bit.to_le_bytes().to_vec(),
        ))
        .await
    }
    pub async fn finish_call(&mut self, p: u64) -> Result<bool> {
        loop {
            let frame = self.receive().await?;
            let done = outcomes(frame);
            if done.len() > 1 {
                return Err("more than one outcome for a single in-flight call".into());
            }
            if let Some(done) = done.first() {
                if done.stream != self.stream || done.p != p {
                    return Err("unexpected or duplicate final outcome".into());
                }
                return returned(done);
            }
        }
    }
}

/// WIRE §3: every `complete` naming the subscription replaces its image from empty, the
/// first one and each repair alike, so a key absent from it is gone. Other rows apply to the
/// image. Returns whether a `complete` replaced an image the subscription had already completed.
fn apply_batch(
    image: &mut Vec<u8>,
    pending: &mut bool,
    sub: u32,
    complete: &[u32],
    groups: &[codec::Group],
) -> Result<bool> {
    let ours = groups
        .iter()
        .filter(|group| group.sub == sub)
        .flat_map(|group| &group.rows);
    if complete.contains(&sub) {
        let mut whole = vec![0; IMAGE_BYTES];
        for row in ours {
            codec::apply_row(&mut whole, row)?;
        }
        *image = whole;
        let repaired = !*pending;
        *pending = false;
        return Ok(repaired);
    }
    if *pending && groups.iter().any(|group| group.sub == sub) {
        return Err("live subscription row arrived before its complete marker".into());
    }
    for row in ours {
        codec::apply_row(image, row)?;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    // A repair's `complete` without a chunk the image holds clears that chunk: merging it into
    // the image would leave the chunk painted forever (overload-advice-1449).
    #[test]
    fn every_complete_replaces_the_image_from_empty() -> Result<()> {
        let mut image = vec![0; IMAGE_BYTES];
        let mut pending = true;
        let mut bits = vec![0; IMAGE_BYTES];
        bits[0] = 2;
        assert!(
            apply_batch(
                &mut image,
                &mut pending,
                2,
                &[],
                &[codec::Group::board(2, vec![codec::chunk_row(0, &bits)])]
            )
            .is_err()
        );
        assert!(!apply_batch(
            &mut image,
            &mut pending,
            2,
            &[2],
            &[codec::Group::board(2, vec![codec::chunk_row(0, &bits)])]
        )?);
        assert_eq!(image[0], 2);
        assert!(apply_batch(&mut image, &mut pending, 2, &[2], &[])?);
        assert_eq!(image[0], 0);
        Ok(())
    }
}
pub fn outcomes(frame: Frame) -> Vec<Done> {
    match frame {
        Frame::Batch(_, _, _, done) | Frame::Reset(_, done) => done,
        _ => vec![],
    }
}
pub fn returned(done: &Done) -> Result<bool> {
    match &done.outcome {
        Outcome::Returned(b) if b == &[0, 0, 0, 0, 0] => Ok(false),
        Outcome::Returned(b) if b == &[0, 0, 0, 0, 1] => Ok(true),
        other => Err(format!("toggle did not return a bool: {other:?}").into()),
    }
}
async fn read_frame(socket: &mut Socket) -> Result<Frame> {
    read_frame_timed(socket).await.map(|(_, f)| f)
}
async fn read_frame_timed(socket: &mut Socket) -> Result<(Instant, Frame)> {
    loop {
        match socket.next().await.transpose()? {
            Some(Message::Binary(bytes)) => {
                let at = Instant::now();
                return Ok((at, codec::decode(&bytes)?));
            }
            Some(Message::Ping(_)) => socket.flush().await?,
            Some(Message::Pong(_)) => (),
            _ => return Err("connection ended before the expected frame".into()),
        }
    }
}

pub fn seeded(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[derive(Debug)]
pub struct Converged {
    pub clients: usize,
    pub returned: usize,
    pub reconnects: usize,
    pub repairs: usize,
    pub resubscribes: usize,
    pub version: u64,
    pub checksum: u64,
}

/// Identical driver for both servers. The oracle depends only on Returned calls.
pub async fn converge(
    url: &str,
    wants: &[Want],
    n: usize,
    toggles: usize,
    rate: u32,
) -> Result<Converged> {
    if n < 2 || toggles < 2 || rate == 0 {
        return Err("converge requires >=2 clients, >=2 toggles, positive rate".into());
    }
    let mut clients = Vec::new();
    for _ in 0..n {
        let mut c = Client::connect(url, wants, None).await?;
        c.snapshot().await?;
        clients.push(c);
    }
    let start = Instant::now() + Duration::from_millis(20);
    let mut tasks = JoinSet::new();
    for (id, mut client) in clients.into_iter().enumerate() {
        let url = url.to_owned();
        let wants = wants.to_vec();
        tasks.spawn(async move {
            let mut state = id as u64 + 0x1062;
            let reconnect = 1 + seeded(&mut state) as usize % (toggles - 1);
            let mut returned_bits = Vec::new();
            for j in 0..toggles {
                sleep_until(start + Duration::from_secs_f64(j as f64 / f64::from(rate))).await;
                let bit = if j < 6 {
                    [0, 7, 8, 511, 512, 999_999][j]
                } else {
                    (seeded(&mut state) % u64::from(BITS)) as u32
                };
                let p = j as u64;
                client.start_call(p, bit).await?;
                if j == reconnect {
                    // Deliberately lose a possible outcome and resend the same p on the same principal.
                    let credential = client.credential.clone();
                    let repairs = client.repairs;
                    let resubscribes = client.resubscribes;
                    let stream = client.stream;
                    drop(client);
                    client = Client::connect(&url, &wants, Some(credential)).await?;
                    client.stream = stream;
                    client.repairs = repairs;
                    client.resubscribes = resubscribes;
                    client.snapshot().await?;
                    client.start_call(p, bit).await?;
                }
                client.finish_call(p).await?;
                returned_bits.push(bit);
            }
            Ok::<_, crate::Error>((client, returned_bits))
        });
    }
    let mut finished = Vec::new();
    let mut oracle = vec![0; IMAGE_BYTES];
    let mut count = 0;
    while let Some(r) = tasks.join_next().await {
        let (c, bits) = r??;
        count += bits.len();
        for bit in bits {
            flip(&mut oracle, bit);
        }
        finished.push(c);
    }
    let mut fresh = Client::connect(url, wants, None).await?;
    fresh.snapshot().await?;
    if fresh.image != oracle {
        return Err("fresh server table differs from XOR of Returned toggles".into());
    }
    let version = fresh.version;
    let mut repairs = fresh.repairs;
    let mut resubscribes = fresh.resubscribes;
    for mut client in finished {
        // A sub due for repair gets its image only once its owner drains, with no commit
        // needed (RUNNER-SPEC §3 "Repair"), so the image is awaited, not only the version.
        while client.version < version || client.pending || client.image != oracle {
            let frame = tokio::time::timeout(Duration::from_secs(30), client.receive())
                .await
                .map_err(|_| "subscriber image differs from XOR of Returned toggles")??;
            if !outcomes(frame).is_empty() {
                return Err("duplicate final outcome after last call".into());
            }
        }
        repairs += client.repairs;
        resubscribes += client.resubscribes;
        client.socket.close(None).await?;
    }
    fresh.socket.close(None).await?;
    Ok(Converged {
        clients: n,
        returned: count,
        reconnects: n,
        repairs,
        resubscribes,
        version,
        checksum: fresh.image.iter().map(|&byte| u64::from(byte)).sum(),
    })
}
