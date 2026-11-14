// Matched to files/main.hirsel (matched delegates to it). Independent review pending.
// Stable safe Rust Read requires initialized bytes; its 4096-byte zero initialization
// is reported separately from Hirsel's uninitialized read_into destination.
#[path = "../matched_support.rs"]
mod support;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use support::*;
enum Event {
    Line(String),
    Cleanup(String),
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Held(File);
impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn event(tx: &SyncSender<Event>, prefix: &str, name: impl AsRef<[u8]>) {
    let mut line = String::copy(prefix);
    line.push_str(name);
    tx.try_send(Event::Line(line)).unwrap();
}
struct Owned {
    handle: Option<File>,
    lock: Option<File>,
    name: String,
    injected: bool,
    tx: SyncSender<Event>,
}
impl Owned {
    fn new(
        handle: Option<File>,
        lock: Option<File>,
        name: &str,
        variant: &str,
        tx: &SyncSender<Event>,
    ) -> Self {
        let mut failure = String::copy("error-release-");
        failure.push_str(name);
        let mut line = String::copy("acquire=");
        line.push_str(name);
        let owner = Self {
            handle,
            lock,
            name: String::copy(name),
            injected: failure == variant || variant == "error-cleanup-all",
            tx: tx.clone(),
        };
        tx.try_send(Event::Line(line)).unwrap();
        owner
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let mut failed = false;
        if let Some(f) = self.lock.take() {
            if f.unlock().is_err() {
                failed = true;
            }
            drop(f);
        }
        drop(self.handle.take());
        event(&self.tx, "release=", &self.name);
        if failed || self.injected {
            event(&self.tx, "cleanup-error=Io@release-", &self.name);
            let mut label = String::copy("Io@release-");
            label.push_str(&self.name);
            let _ = self.tx.try_send(Event::Cleanup(label));
        }
    }
}
fn open(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().append(true).create(true).open(path)
}
fn io<T>(r: std::io::Result<T>, label: &'static str) -> Result<T> {
    r.map_err(|_| Error::from(String::copy(label)))
}
fn copy_files(dir: &Path, variant: &str, tx: &SyncSender<Event>) -> Result<()> {
    let source_path = if variant == "error-first" {
        "missing"
    } else {
        "source.txt"
    };
    let file = io(File::open(dir.join(source_path)), "Io@open-source")?;
    let mut source = Owned::new(Some(file), None, "source", variant, tx);
    let file = io(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("destination.txt")),
        "Io@create-destination",
    )?;
    let mut destination = Owned::new(Some(file), None, "destination", variant, tx);
    let locked = io(open(&dir.join("lock.txt")), "Lock@acquire")?;
    locked.try_lock().map_err(|_| "Lock@acquire")?;
    let _lock = Owned::new(None, Some(locked), "lock", variant, tx);
    admission(variant, "copy.buffer", 4096)?;
    let mut buffer = vec![0u8; 4096];
    let mut copied = 0u64;
    loop {
        let mut src = source.handle.take().unwrap();
        let read = src.read(&mut buffer);
        source.handle = Some(src);
        let n = io(read, "Io@read")?;
        if n == 0 {
            break;
        }
        let inject = variant == "error-work" || variant == "error-cleanup-all";
        let count = if inject && n > 17 { 17 } else { n };
        let mut dst = destination.handle.take().unwrap();
        let written = dst.write_all(&buffer[..count]);
        destination.handle = Some(dst);
        written.map_err(|_| "Io@write")?;
        copied += count as u64;
        if inject {
            return Err("Io@copy".into());
        }
    }
    event(tx, "copied=", &name("", copied, ""));
    Ok(())
}
fn run(input: View<'_>, variant: &str) -> Result<()> {
    let (tx, rx) = mpsc::sync_channel(16);
    let mut folder = String::with_capacity(32);
    folder.push_str(".m1-files-");
    num(&mut folder, std::process::id());
    io(
        std::fs::create_dir({
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(folder.as_bytes())
        }),
        "Io@temp",
    )?;
    let directory = Directory({
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(folder.as_bytes()))
    });
    let dir = &directory.0;
    io(
        std::fs::write(dir.join("source.txt"), input.as_bytes()),
        "Io@setup",
    )?;
    if variant == "error-second" {
        let mut existing = Vec::with_capacity(8);
        existing.extend_from_slice(b"existing");
        io(
            std::fs::write(dir.join("destination.txt"), &existing),
            "Io@setup",
        )?;
    }
    let mut holder = None;
    if variant == "error-lock" {
        let f = io(open(&dir.join("lock.txt")), "Io@setup")?;
        f.try_lock().map_err(|_| "Lock@setup")?;
        holder = Some(Held(f));
    }
    let outcome = copy_files(dir, variant, &tx);
    drop(holder.take());
    let probe = io(open(&dir.join("lock.txt")), "Io@probe")?;
    let free = probe.try_lock().is_ok();
    if free {
        let _ = probe.unlock();
    }
    drop(probe);
    let data = match read_bytes(dir.join("destination.txt")) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => return Err("Io@read-destination".into()),
    };
    drop(tx);
    let mut cleanups = Vec::new();
    for event in rx {
        match event {
            Event::Line(s) => {
                put(&s)?;
                put("\n")?;
            }
            Event::Cleanup(s) => cleanups.push(s),
        }
    }
    let mut rendered = name("destination-bytes=", data.len(), " sum=");
    num(&mut rendered, sum(&data));
    rendered.push_str(if free {
        " lock-free=true\n"
    } else {
        " lock-free=false\n"
    });
    put(&rendered)?;
    let mut primary = outcome;
    if primary.is_ok() && !cleanups.is_empty() {
        primary = Err(cleanups.remove(0).into());
    }
    for cause in cleanups {
        put("suppressed=")?;
        put(&cause)?;
        put("\n")?;
    }
    if let Err(e) = &primary {
        print_error(e)?
    }
    primary
}
fn main() {
    let (input, variant) = match input() {
        Ok(x) => x,
        Err(_) => std::process::exit(2),
    };
    exit_status(run(input.view(), &variant));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn io_label_ownership() {
        let e = io::<()>(Err(std::io::ErrorKind::NotFound.into()), "Io@test").unwrap_err();
        assert_eq!(e.label.capacity(), 7);
        assert_eq!(e.exit, 2);
        let direct = Error::from("Io@write");
        assert_eq!(direct.label.capacity(), 0)
    }
}
