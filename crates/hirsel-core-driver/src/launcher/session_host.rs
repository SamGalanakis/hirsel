use super::{HostCommand, HostReply, InstallRefusal, PlaceReady, Reader, Wire, Writer, malformed};
use std::io;

impl Wire for HostCommand {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Export(a, retry) => {
                w.u32(0);
                w.bytes(a);
                w.bool(*retry);
            }
            Self::Import(d, release, digest, fates) => {
                w.u32(1);
                w.bytes(d);
                w.bytes(release);
                w.bytes(digest);
                w.bytes(fates);
            }
            Self::Cutover(a, digest, pause, retry) => {
                w.u32(2);
                w.bytes(a);
                w.bytes(digest);
                w.u64(*pause);
                w.bool(*retry);
            }
            Self::Abort(a, previous) => {
                w.u32(3);
                w.bytes(a);
                w.option(previous, |w, previous| w.bytes(previous));
            }
            Self::StoreAccepted => w.u32(4),
            Self::Backup(a) => {
                w.u32(5);
                w.bytes(a);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Export(r.bytes()?, r.bool()?),
            1 => Self::Import(r.bytes()?, r.bytes()?, r.bytes()?, r.bytes()?),
            2 => Self::Cutover(r.bytes()?, r.bytes()?, r.u64()?, r.bool()?),
            3 => Self::Abort(r.bytes()?, r.option(Reader::bytes)?),
            4 => Self::StoreAccepted,
            5 => Self::Backup(r.bytes()?),
            _ => return Err(malformed("host command tag")),
        })
    }
}

impl Wire for HostReply {
    fn put(&self, w: &mut Writer) {
        match self {
            Self::Opened(ready) => {
                w.u32(0);
                w.u64(ready.version);
                w.u64(ready.store_hi);
                w.u64(ready.store_lo);
                w.bytes(&ready.release);
                w.u64(ready.retain_from);
                w.u64(ready.ready_ns);
                w.bytes(&ready.read);
            }
            Self::Current(version, release) => {
                w.u32(1);
                w.u64(*version);
                w.bytes(release);
            }
            Self::Refused(why) => (w.u32(2), w.bytes(why.as_bytes())).1,
            Self::Exported(version, name) => {
                w.u32(3);
                w.u64(*version);
                w.bytes(name.as_bytes());
            }
            Self::Imported(hash, digest, changes) => {
                w.u32(4);
                w.bytes(hash);
                w.bytes(digest);
                w.len(changes.len());
                changes.iter().for_each(|change| w.bytes(change));
            }
            Self::Installed(version, waiting) => {
                w.u32(5);
                w.u64(*version);
                w.bool(*waiting);
            }
            Self::Drift => w.u32(6),
            Self::Aborted => w.u32(7),
            Self::Uncertain => w.u32(8),
            Self::WriteAck(at) => (w.u32(9), w.u64(*at)).1,
            Self::StoreMismatch(ehi, elo, ahi, alo) => {
                w.u32(10);
                w.u64(*ehi);
                w.u64(*elo);
                w.u64(*ahi);
                w.u64(*alo);
            }
            Self::InstallRefused(why) => {
                w.u32(11);
                why.put(w);
            }
            Self::BackedUp(store_hi, store_lo, head, name) => {
                w.u32(12);
                w.u64(*store_hi);
                w.u64(*store_lo);
                w.u64(*head);
                w.bytes(name.as_bytes());
            }
            Self::Deposed(epoch, holder) => {
                w.u32(13);
                w.u64(*epoch);
                w.bytes(holder);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(match r.u32()? {
            0 => Self::Opened(PlaceReady {
                version: r.u64()?,
                store_hi: r.u64()?,
                store_lo: r.u64()?,
                release: r.bytes()?,
                retain_from: r.u64()?,
                ready_ns: r.u64()?,
                read: r.bytes()?,
            }),
            1 => Self::Current(r.u64()?, r.bytes()?),
            2 => Self::Refused(r.string()?),
            3 => Self::Exported(r.u64()?, r.string()?),
            4 => Self::Imported(r.bytes()?, r.bytes()?, {
                let n = r.count(4)?;
                (0..n).map(|_| r.bytes()).collect::<io::Result<_>>()?
            }),
            5 => Self::Installed(r.u64()?, r.bool()?),
            6 => Self::Drift,
            7 => Self::Aborted,
            8 => Self::Uncertain,
            9 => Self::WriteAck(r.u64()?),
            10 => Self::StoreMismatch(r.u64()?, r.u64()?, r.u64()?, r.u64()?),
            11 => Self::InstallRefused(InstallRefusal::get(r)?),
            12 => Self::BackedUp(r.u64()?, r.u64()?, r.u64()?, r.string()?),
            13 => Self::Deposed(r.u64()?, r.bytes()?),
            _ => return Err(malformed("host reply tag")),
        })
    }
}
