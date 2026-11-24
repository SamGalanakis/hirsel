use crate::{
    call::{decode, encode, from_hex, hex},
    sdk::{Describe, Schema, entry_path},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{SocketAddr, TcpStream},
    sync::{Arc, Mutex, mpsc::TryRecvError},
    time::Duration,
};

use super::{Error, STOP, transport::Socket};

pub(super) struct Release {
    pub(super) generation: u64,
    pub(super) describe: Arc<Describe>,
}

pub(super) type Current = Arc<Mutex<Option<Arc<Release>>>>;

#[derive(Default)]
struct Session {
    hello: Option<Value>,
    subs: BTreeMap<u32, Value>,
    pending: BTreeSet<(u32, u64)>,
    welcomes: usize,
    version: u64,
}

fn number(value: &Value) -> Result<u32, Error> {
    Ok(u32::try_from(
        value.as_u64().ok_or("missing frame number")?,
    )?)
}

fn send(socket: &Socket, codec: &Schema, frame: &Value) -> Result<(), Error> {
    socket.send(&encode(codec, frame)?)
}

impl Session {
    fn from_client(
        &mut self,
        codec: &Schema,
        raw: &[u8],
        upstream: Option<&Socket>,
        client: &Socket,
    ) -> Result<(), Error> {
        let frame = decode(codec, raw)?;
        let values = &frame["values"];
        match frame["tag"].as_str() {
            Some("Hello") if self.hello.is_none() => self.hello = Some(frame),
            Some("Subscribe") if self.hello.is_some() => {
                self.subs.insert(number(&values[0])?, frame);
            }
            Some("Unsubscribe") if self.hello.is_some() => {
                self.subs.remove(&number(&values[0])?);
            }
            Some("Call") if self.hello.is_some() => {
                let stream = number(&values[0])?;
                let p = values[1].as_u64().ok_or("missing call position")?;
                if upstream.is_none() {
                    return self.outcomes(
                        codec,
                        client,
                        [json!({
                            "stream": stream, "p": p,
                            "outcome": {"tag":"Refused", "value":{"tag":"Overloaded"}},
                        })],
                    );
                }
                self.pending.insert((stream, p));
            }
            _ => return Err("unexpected frame from a development client".into()),
        }
        if let Some(upstream) = upstream {
            upstream.send(raw)?;
        }
        Ok(())
    }

    fn from_server(&mut self, codec: &Schema, raw: &[u8], client: &Socket) -> Result<(), Error> {
        let frame = decode(codec, raw)?;
        let values = &frame["values"];
        match frame["tag"].as_str() {
            Some("Batch" | "Reset") => {
                self.version = values[0].as_u64().ok_or("missing batch version")?;
                let index = if frame["tag"] == "Batch" { 3 } else { 1 };
                for done in values[index].as_array().into_iter().flatten() {
                    if let (Ok(stream), Some(p)) = (number(&done["stream"]), done["p"].as_u64()) {
                        self.pending.remove(&(stream, p));
                    }
                }
            }
            Some("Ended") => {
                self.subs.remove(&number(&values[0])?);
            }
            Some("Welcome" | "Placed") => {
                self.welcomes += 1;
            }
            _ => {}
        }
        client.send(raw)
    }

    fn outcomes<const N: usize>(
        &self,
        codec: &Schema,
        client: &Socket,
        done: [Value; N],
    ) -> Result<(), Error> {
        send(
            client,
            codec,
            &json!({"tag":"Batch", "values":[self.version, [], [], done.as_slice()]}),
        )
    }

    fn interrupted(&mut self, codec: &Schema, client: &Socket) -> Result<(), Error> {
        for (stream, p) in std::mem::take(&mut self.pending) {
            self.outcomes(
                codec,
                client,
                [json!({"stream":stream,"p":p,"outcome":{"tag":"InDoubt"}})],
            )?;
        }
        Ok(())
    }

    fn rejoin(
        &mut self,
        codec: &Schema,
        upstream: &Socket,
        client: &Socket,
        release: &Release,
    ) -> Result<(), Error> {
        let Some(mut hello) = self.hello.clone() else {
            return Ok(());
        };
        let wants = hello["values"][1]
            .as_array_mut()
            .ok_or("missing Hello wants")?;
        for want in wants.iter_mut() {
            let old = want["identity"]
                .as_str()
                .and_then(from_hex)
                .ok_or("missing entry identity")?;
            let args_fp = want["args_fp"]
                .as_u64()
                .ok_or("missing argument fingerprint")?;
            let result_fp = want["result_fp"]
                .as_u64()
                .ok_or("missing result fingerprint")?;
            if release.describe.exported.iter().any(|entry| {
                entry.identity == old && entry.args_fp == args_fp && entry.result_fp == result_fp
            }) {
                continue;
            }
            let Ok(path) = entry_path(&old) else { continue };
            let mut candidates = release.describe.exported.iter().filter(|entry| {
                entry.kind == 2
                    && entry.args_fp == args_fp
                    && entry_path(&entry.identity).ok() == Some(path)
            });
            if let Some(entry) = candidates.next()
                && candidates.next().is_none()
            {
                *want = json!({"identity":hex(&entry.identity),"args_fp":entry.args_fp,"result_fp":entry.result_fp});
            }
        }
        let replies = 1 + wants.len();
        send(upstream, codec, &hello)?;
        for i in 0..replies {
            let raw = upstream.incoming.recv_timeout(Duration::from_secs(5))?;
            let frame = decode(codec, &raw)?;
            let expected = if i == 0 { "Welcome" } else { "Placed" };
            if frame["tag"] != expected {
                return Err(
                    format!("expected {expected} when restoring a development session").into(),
                );
            }
            if i >= self.welcomes {
                self.from_server(codec, &raw, client)?;
            }
        }
        for frame in self.subs.values() {
            let mut fresh = frame.clone();
            // The server must send a complete group even if the old group was resumable.
            fresh["values"][3] = json!({"tag":"None"});
            send(upstream, codec, &fresh)?;
        }
        Ok(())
    }
}

pub(super) fn serve(
    stream: TcpStream,
    backend: SocketAddr,
    current: Current,
    codec: Arc<Schema>,
) -> Result<(), Error> {
    let Some((client, path)) = Socket::accept(stream, backend)? else {
        return Ok(());
    };
    let mut session = Session::default();
    let mut observed = None;
    let mut upstream: Option<Socket> = None;
    while !STOP.load(std::sync::atomic::Ordering::Relaxed) {
        let release = current.lock().unwrap().clone();
        let selected = release.as_ref().map(|release| release.generation);
        if observed != selected {
            drop(upstream.take());
            session.interrupted(&codec, &client)?;
            observed = selected;
            if let Some(release) = release {
                let installed = Socket::open(backend, &path)?;
                session.rejoin(&codec, &installed, &client, &release)?;
                upstream = Some(installed);
            }
        }
        if let Some(connected) = &upstream {
            match connected.incoming.try_recv() {
                Ok(raw) => session.from_server(&codec, &raw, &client)?,
                Err(TryRecvError::Disconnected) => {
                    drop(upstream.take());
                    session.interrupted(&codec, &client)?;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        match client.incoming.recv_timeout(Duration::from_millis(10)) {
            Ok(raw) => session.from_client(&codec, &raw, upstream.as_ref(), &client)?,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    Ok(())
}
