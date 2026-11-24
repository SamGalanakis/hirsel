//! The descriptors a link hands the linker: one per object, through which
//! its bytes were checked or written, and the process limit they count
//! against.
#[cfg(test)]
use crate::native::{AFTER_CHECK, BEFORE_OPEN};
use crate::native::{NativeStop, ObjectProduct};
use hirsel_core_ir::Identity;
use std::io;
#[cfg(test)]
use std::path::PathBuf;

/// Raise the soft descriptor limit to the hard limit once, as Go does since
/// 1.19: a build holds one descriptor per object from the object's check or
/// write to the link, so every place that makes such a descriptor
/// (`native_object`) raises it before the first. The limit a link may use.
pub(crate) fn descriptor_limit() -> u64 {
    static LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit and setrlimit read and write the struct passed.
        unsafe {
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                return 1024;
            }
            if limit.rlim_cur < limit.rlim_max {
                let raised = libc::rlimit {
                    rlim_cur: limit.rlim_max,
                    rlim_max: limit.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                    limit = raised;
                }
            }
        }
        limit.rlim_cur
    })
}

/// A checked object's descriptor for the link: the one an object this
/// request read or emitted holds, lent, or one opened here.
pub(crate) enum Opened<'a> {
    Lent(&'a std::fs::File),
    Owned(std::fs::File),
}
impl std::ops::Deref for Opened<'_> {
    type Target = std::fs::File;
    fn deref(&self) -> &std::fs::File {
        match self {
            Self::Lent(file) => file,
            Self::Owned(file) => file,
        }
    }
}

/// One descriptor per object, through which its bytes were checked or
/// written: an object this request read or emitted lends its own, and a
/// stored object named by a record is opened once and checked through that
/// descriptor. The linker names each by `/proc/<pid>/fd/N` of this process,
/// which resolves to that inode, so no store path is reopened after a check.
/// Every stored object is checked; each that fails is removed, and the
/// instances of all of them are returned.
pub(crate) fn open_objects<'a>(
    objects: &[&'a ObjectProduct],
    counters: &mut crate::reach::ReachCounters,
) -> Result<Result<Vec<Opened<'a>>, Vec<Identity>>, NativeStop> {
    use std::io::Read;
    let limit = descriptor_limit();
    if objects.len() as u64 + 64 > limit {
        return Err(NativeStop::Link(format!(
            "{} objects exceed the descriptor limit of {limit}",
            objects.len()
        )));
    }
    #[cfg(test)]
    BEFORE_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    let mut files = Vec::with_capacity(objects.len());
    let mut refused = Vec::new();
    let mut bytes = Vec::new();
    for &object in objects {
        if let Some(held) = &object.held {
            files.push(Opened::Lent(held));
            continue;
        }
        let mut file = match std::fs::File::open(&object.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                refused.push(object.instance.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        crate::meter::count(crate::meter::Count::ObjectsOpened, 1);
        bytes.clear();
        file.read_to_end(&mut bytes)?;
        counters.objects_verified += 1;
        if crate::store::checksum(&bytes) != object.check {
            drop(file);
            match std::fs::remove_file(&object.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            refused.push(object.instance.clone());
            continue;
        }
        files.push(Opened::Owned(file));
    }
    if !refused.is_empty() {
        return Ok(Err(refused));
    }
    #[cfg(test)]
    AFTER_CHECK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    Ok(Ok(files))
}
