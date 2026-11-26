//! The upload of an attested recipe's objects to the host, relayed through `Data` frames.
use super::{CHUNK, Upload};
use crate::code_admission::{AdmissionError, ObjectKey, ObjectKind, send_release};
use crate::launcher::session::Attempt;
use crate::operator::{self, Ask};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The local end of an upload: `send_release` writes the admission bytes into one end of a
/// socket pair, a pump relays them to the host as `Data`, and the host's `Need` parts are
/// written back into the pair once the last one arrives.
pub(super) struct Pump {
    peer: UnixStream,
    stopped: Arc<AtomicBool>,
    sender: std::thread::JoinHandle<Result<(), AdmissionError>>,
    relay: std::thread::JoinHandle<()>,
    needed: Vec<ObjectKey>,
}

impl Pump {
    pub(super) fn start(
        upload: &Upload,
        d: &[u8],
        a: Attempt,
        out: Arc<Mutex<operator::Connection>>,
    ) -> io::Result<Self> {
        let (peer, mut local) = UnixStream::pair()?;
        let store = upload.store.clone();
        let root = upload.root;
        let mut limits = upload.limits;
        limits.timeout = Some(Duration::from_secs(600));
        let sender = std::thread::spawn(move || send_release(&mut local, &store, root, &limits));
        let stopped = Arc::new(AtomicBool::new(false));
        let (mut input, stop, d) = (peer.try_clone()?, stopped.clone(), d.to_vec());
        let relay = std::thread::spawn(move || {
            let mut chunk = vec![0; CHUNK];
            loop {
                let n = match input.read(&mut chunk) {
                    Ok(n) => n,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let ask = if n == 0 {
                    Ask::End(d.clone(), a)
                } else {
                    Ask::Data(d.clone(), a, chunk[..n].to_vec())
                };
                if out.lock().unwrap().send(&ask).is_err() || n == 0 {
                    return;
                }
            }
        });
        Ok(Self {
            peer,
            stopped,
            sender,
            relay,
            needed: Vec::new(),
        })
    }

    /// One `Need` part; after the last, the whole set goes to the sender as its Need.
    pub(super) fn need(&mut self, keys: Vec<ObjectKey>, last: bool) -> io::Result<()> {
        self.needed.extend(keys);
        if !last {
            return Ok(());
        }
        let mut frame = (self.needed.len() as u64).to_le_bytes().to_vec();
        for key in self.needed.drain(..) {
            frame.push(match key.kind {
                ObjectKind::Group => 1,
                ObjectKind::Body => 2,
                ObjectKind::Annex => 3,
                ObjectKind::ForeignArchive => 4,
                ObjectKind::Value => 5,
                ObjectKind::Recipe => return Err(io::ErrorKind::InvalidData.into()),
            });
            frame.extend_from_slice(&key.hash.0);
        }
        self.peer.write_all(&frame)
    }

    pub(super) fn stop(self) -> Result<(), AdmissionError> {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.peer.shutdown(std::net::Shutdown::Both);
        let sent = self.sender.join().unwrap_or(Ok(()));
        let _ = self.relay.join();
        sent
    }
}
