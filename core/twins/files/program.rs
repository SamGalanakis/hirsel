use crate::support::*;
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct OwnedFile {
    file: Option<File>,
    name: &'static str,
    trace: Trace,
    fail: bool,
    causes: Rc<RefCell<Vec<String>>>,
}
impl OwnedFile {
    fn new(
        file: File,
        name: &'static str,
        trace: &Trace,
        variant: &str,
        causes: &Rc<RefCell<Vec<String>>>,
    ) -> Self {
        trace.event(format!("acquire={name}"));
        Self {
            file: Some(file),
            name,
            trace: trace.clone(),
            fail: variant == format!("error-release-{name}") || variant == "error-cleanup-all",
            causes: causes.clone(),
        }
    }
}
impl Drop for OwnedFile {
    fn drop(&mut self) {
        if self.name == "lock" {
            self.file.as_ref().unwrap().unlock().unwrap();
        }
        drop(self.file.take());
        self.trace.event(format!("release={}", self.name));
        if self.fail {
            let e = format!("Io@release-{}", self.name);
            self.trace.event(format!("cleanup-error={e}"));
            self.causes.borrow_mut().push(e);
        }
    }
}
fn open(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}
pub fn run() {
    let (input, variant) = input();
    let admission = Admission::new(&variant);
    let trace = Trace::default();
    let causes = Rc::new(RefCell::new(Vec::<String>::new()));
    let dir =
        Directory(std::env::temp_dir().join(format!("hirsel-core-files-{}", std::process::id())));
    std::fs::create_dir(&dir.0).unwrap();
    let source = dir.0.join("source");
    let destination = dir.0.join("destination");
    let lock_path = dir.0.join("lock");
    std::fs::write(&source, &input).unwrap();
    if variant == "error-second" {
        std::fs::write(&destination, b"existing").unwrap();
    }
    let holder = if variant == "error-lock" {
        let f = open(&lock_path);
        f.lock().unwrap();
        Some(f)
    } else {
        None
    };
    let result = (|| {
        let path = if variant == "error-first" {
            dir.0.join("missing")
        } else {
            source.clone()
        };
        let mut first = OwnedFile::new(
            File::open(path).map_err(|_| "Io@open-source")?,
            "source",
            &trace,
            &variant,
            &causes,
        );
        let mut second = OwnedFile::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|_| "Io@create-destination")?,
            "destination",
            &trace,
            &variant,
            &causes,
        );
        let file = open(&lock_path);
        file.try_lock().map_err(|_| "Lock@acquire")?;
        let _guard = OwnedFile::new(file, "lock", &trace, &variant, &causes);
        admission.at("copy.buffer")?;
        let mut buffer = vec![0u8; 4096];
        let mut copied = 0;
        loop {
            let n = first
                .file
                .as_mut()
                .unwrap()
                .read(&mut buffer)
                .map_err(|_| "Io@read")?;
            if n == 0 {
                break;
            }
            let count = if variant == "error-work" || variant == "error-cleanup-all" {
                n.min(17)
            } else {
                n
            };
            second
                .file
                .as_mut()
                .unwrap()
                .write_all(&buffer[..count])
                .map_err(|_| "Io@write")?;
            copied += count;
            if variant == "error-work" || variant == "error-cleanup-all" {
                return Err("Io@copy".into());
            }
        }
        trace.event(format!("copied={copied}"));
        Ok(())
    })();
    drop(holder);
    let probe = open(&lock_path);
    let free = probe.try_lock().is_ok();
    if free {
        probe.unlock().unwrap();
    }
    drop(probe);
    trace.print();
    let bytes = std::fs::read(&destination).unwrap_or_default();
    println!(
        "destination-bytes={} sum={} lock-free={free}",
        bytes.len(),
        sum(&bytes)
    );
    drop(dir);
    let mut failures = causes.borrow_mut();
    let result = if let Err(primary) = result {
        Err(primary)
    } else if !failures.is_empty() {
        Err(failures.remove(0))
    } else {
        Ok(())
    };
    for cause in failures.iter() {
        println!("suppressed={cause}");
    }
    finish(result);
}
