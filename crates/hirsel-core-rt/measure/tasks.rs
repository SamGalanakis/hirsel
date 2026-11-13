//! Runtime ABI/Rust twins. Built by measure-tasks.py with the same pinned rustc
//! flags on every dependency; this is not a source-language compiler benchmark.
use hirsel_core_rt::{Context, causes, channel::*, provider::*, tasks::*};
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    ptr,
};
const MB: usize = 1_000_000;
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn scalar<T>(logical: u64) -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical,
        drop: inert,
    }
}
fn layout<X, R>(logical: u64) -> TaskLayout {
    TaskLayout {
        input: scalar::<X>(0),
        result: scalar::<R>(logical),
    }
}
unsafe extern "C" fn empty(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    0
}
#[derive(Clone, Copy)]
struct Worker {
    rx: *mut Channel,
    tx: *mut Channel,
    n: u64,
    role: u8,
}
unsafe extern "C" fn worker(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: input transfers its endpoints; each slot has a single custodian.
    unsafe {
        let w = input.cast::<Worker>().read();
        let mut sum = 0u64;
        for i in 0..w.n {
            let mut x = i;
            if w.role != 0 {
                let got = hirsel_core_receive(context, w.rx, (&mut x as *mut u64).cast(), 1);
                assert!(got.transferred && got.status == 0);
            }
            if w.role != 2 {
                let sent = hirsel_core_send(context, w.tx, (&mut x as *mut u64).cast(), 2);
                assert!(sent.transferred && sent.status == 0);
            } else {
                sum += x;
            }
        }
        if !w.rx.is_null() {
            assert_eq!(hirsel_core_receiver_drop(context, w.rx), 0);
        }
        if !w.tx.is_null() {
            hirsel_core_sender_drop(w.tx);
        }
        if w.role == 2 {
            out.cast::<u64>().write(sum);
        }
    }
    0
}
#[derive(Clone, Copy)]
struct Operation {
    kind: u8,
    n: u64,
    provider: *const Provider,
    dir: u64,
    input: *mut u8,
    output: *mut u8,
}
unsafe fn host(
    context: *mut Context,
    p: Operation,
    op: u8,
    mode: u8,
    handle: u64,
    path: &[u8],
    data: *mut u8,
    len: usize,
    capacity: usize,
) -> (Outcome, usize) {
    let mut request = Request {
        operation: op,
        mode,
        handle,
        path: path.as_ptr(),
        path_len: path.len(),
        buffer: Buffer {
            data,
            len,
            capacity,
        },
        site: 1,
    };
    // SAFETY: root pins all caller buffers, provider and exact request extents.
    let out = unsafe { hirsel_core_host(context, p.provider, &mut request) };
    assert_eq!((out.status, out.error), (0, IoError::OK));
    (out, request.buffer.len)
}
unsafe extern "C" fn root(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: operation is plain prepared input; every task/queue/file is consumed.
    unsafe {
        let p = input.cast::<Operation>().read();
        let result = match p.kind {
            0 => {
                let mut unit = 0u8;
                for _ in 0..p.n {
                    let h = hirsel_core_spawn(context, 0, &mut unit, empty, layout::<(), ()>(0));
                    assert!(!h.task.is_null());
                    let j = hirsel_core_join(context, h.task, &mut unit, 1);
                    assert_eq!((j.kind, j.status), (0, 0));
                }
                p.n
            }
            1 => {
                let a = hirsel_core_channel_new(context, 1, scalar::<u64>(8), 1);
                let b = hirsel_core_channel_new(context, 1, scalar::<u64>(8), 1);
                let mut w = Worker {
                    rx: a.receiver,
                    tx: b.sender,
                    n: p.n,
                    role: 1,
                };
                let h = hirsel_core_spawn(
                    context,
                    0,
                    (&mut w as *mut Worker).cast(),
                    worker,
                    layout::<Worker, ()>(0),
                );
                assert!(!h.task.is_null());
                let mut sum = 0u64;
                for mut x in 0..p.n {
                    let sent = hirsel_core_send(context, a.sender, (&mut x as *mut u64).cast(), 1);
                    assert!(sent.transferred && sent.status == 0);
                    let mut y = 0u64;
                    let got =
                        hirsel_core_receive(context, b.receiver, (&mut y as *mut u64).cast(), 1);
                    assert!(got.transferred && got.status == 0);
                    assert_eq!(x, y);
                    sum += y;
                }
                hirsel_core_sender_drop(a.sender);
                hirsel_core_receiver_drop(context, b.receiver);
                let mut unit = 0u8;
                assert_eq!(hirsel_core_join(context, h.task, &mut unit, 1).kind, 0);
                sum
            }
            2 => {
                let a = hirsel_core_channel_new(context, 1, scalar::<u64>(8), 1);
                let b = hirsel_core_channel_new(context, 1, scalar::<u64>(8), 1);
                let mut p0 = Worker {
                    rx: ptr::null_mut(),
                    tx: a.sender,
                    n: p.n,
                    role: 0,
                };
                let mut p1 = Worker {
                    rx: a.receiver,
                    tx: b.sender,
                    n: p.n,
                    role: 1,
                };
                let mut p2 = Worker {
                    rx: b.receiver,
                    tx: ptr::null_mut(),
                    n: p.n,
                    role: 2,
                };
                let h0 = hirsel_core_spawn(
                    context,
                    0,
                    (&mut p0 as *mut Worker).cast(),
                    worker,
                    layout::<Worker, ()>(0),
                );
                let h1 = hirsel_core_spawn(
                    context,
                    0,
                    (&mut p1 as *mut Worker).cast(),
                    worker,
                    layout::<Worker, ()>(0),
                );
                let h2 = hirsel_core_spawn(
                    context,
                    0,
                    (&mut p2 as *mut Worker).cast(),
                    worker,
                    layout::<Worker, u64>(8),
                );
                assert!(!h0.task.is_null() && !h1.task.is_null() && !h2.task.is_null());
                let mut unit = 0u8;
                let mut sum = 0u64;
                assert_eq!(hirsel_core_join(context, h0.task, &mut unit, 1).kind, 0);
                assert_eq!(hirsel_core_join(context, h1.task, &mut unit, 1).kind, 0);
                assert_eq!(
                    hirsel_core_join(context, h2.task, (&mut sum as *mut u64).cast(), 1).kind,
                    0
                );
                sum
            }
            3 | 4 => {
                for _ in 0..p.n {
                    let reading = p.kind == 3;
                    let path = if reading {
                        b"input".as_slice()
                    } else {
                        b"output".as_slice()
                    };
                    let file = host(
                        context,
                        p,
                        OPEN,
                        u8::from(!reading),
                        p.dir,
                        path,
                        ptr::null_mut(),
                        0,
                        0,
                    )
                    .0
                    .value;
                    if reading {
                        let mut at = 0;
                        while at < MB {
                            let (r, len) = host(context, p, READ, 0, file, b"", p.output, at, MB);
                            assert!(r.value > 0);
                            at = len;
                        }
                    } else {
                        let (r, _) = host(context, p, WRITE, 0, file, b"", p.input, MB, MB);
                        assert_eq!(r.written, MB as u64);
                    }
                    host(
                        context,
                        p,
                        hirsel_core_rt::provider::CLOSE,
                        0,
                        file,
                        b"",
                        ptr::null_mut(),
                        0,
                        0,
                    );
                }
                p.n * MB as u64
            }
            _ => unreachable!(),
        };
        out.cast::<u64>().write(result);
    }
    0
}
fn core(p: Operation) -> u64 {
    let mut p = p;
    let mut context = Context::new(1 << 24);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut out = 0u64;
    // SAFETY: prepared operation and result outlive every child and provider call.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                (&mut p as *mut Operation).cast(),
                (&mut out as *mut u64).cast(),
                size_of::<Operation>(),
                8,
                SchedulerProvider::default()
            ),
            0
        );
    }
    assert_eq!(context.available(), before);
    out
}
fn mpsc(kind: u8, n: u64) -> u64 {
    use std::sync::mpsc::sync_channel;
    let (a, ar) = sync_channel::<u64>(1);
    let (b, br) = sync_channel::<u64>(1);
    if kind == 1 {
        let child = std::thread::spawn(move || {
            for _ in 0..n {
                b.send(ar.recv().unwrap()).unwrap();
            }
        });
        let mut sum = 0;
        for i in 0..n {
            a.send(i).unwrap();
            let j = br.recv().unwrap();
            assert_eq!(i, j);
            sum += j;
        }
        child.join().unwrap();
        sum
    } else {
        let producer = std::thread::spawn(move || {
            for i in 0..n {
                a.send(i).unwrap();
            }
        });
        let relay = std::thread::spawn(move || {
            for _ in 0..n {
                b.send(ar.recv().unwrap()).unwrap();
            }
        });
        let consumer = std::thread::spawn(move || {
            let mut sum = 0;
            for _ in 0..n {
                sum += br.recv().unwrap();
            }
            sum
        });
        producer.join().unwrap();
        relay.join().unwrap();
        consumer.join().unwrap()
    }
}
fn crossbeam(kind: u8, n: u64) -> u64 {
    let (a, ar) = crossbeam_channel::bounded::<u64>(1);
    let (b, br) = crossbeam_channel::bounded::<u64>(1);
    if kind == 1 {
        let child = std::thread::spawn(move || {
            for _ in 0..n {
                b.send(ar.recv().unwrap()).unwrap();
            }
        });
        let mut sum = 0;
        for i in 0..n {
            a.send(i).unwrap();
            let j = br.recv().unwrap();
            assert_eq!(i, j);
            sum += j;
        }
        child.join().unwrap();
        sum
    } else {
        let producer = std::thread::spawn(move || {
            for i in 0..n {
                a.send(i).unwrap();
            }
        });
        let relay = std::thread::spawn(move || {
            for _ in 0..n {
                b.send(ar.recv().unwrap()).unwrap();
            }
        });
        let consumer = std::thread::spawn(move || {
            let mut sum = 0;
            for _ in 0..n {
                sum += br.recv().unwrap();
            }
            sum
        });
        producer.join().unwrap();
        relay.join().unwrap();
        consumer.join().unwrap()
    }
}
async fn tokio_channels(kind: u8, n: u64) -> u64 {
    use tokio::sync::mpsc::channel;
    let (a, mut ar) = channel::<u64>(1);
    let (b, mut br) = channel::<u64>(1);
    if kind == 1 {
        let child = tokio::spawn(async move {
            for _ in 0..n {
                b.send(ar.recv().await.unwrap()).await.unwrap();
            }
        });
        let mut sum = 0;
        for i in 0..n {
            a.send(i).await.unwrap();
            let j = br.recv().await.unwrap();
            assert_eq!(i, j);
            sum += j;
        }
        child.await.unwrap();
        sum
    } else {
        let producer = tokio::spawn(async move {
            for i in 0..n {
                a.send(i).await.unwrap();
            }
        });
        let relay = tokio::spawn(async move {
            for _ in 0..n {
                b.send(ar.recv().await.unwrap()).await.unwrap();
            }
        });
        let consumer = tokio::spawn(async move {
            let mut sum = 0;
            for _ in 0..n {
                sum += br.recv().await.unwrap();
            }
            sum
        });
        producer.await.unwrap();
        relay.await.unwrap();
        consumer.await.unwrap()
    }
}
fn io(model: &str, kind: u8, n: u64) -> u64 {
    let path = std::env::temp_dir().join(format!("hirsel-task-cost-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let mut input = vec![0x5a; MB];
    let mut output = vec![0u8; MB];
    std::fs::write(path.join("input"), &input).unwrap();
    let result = match model {
        "core" => {
            let sink = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .unwrap();
            let mut os =
                hirsel_core_rt::os::OsProvider::new(sink.try_clone().unwrap().into(), sink.into());
            let dir = os.grant(&path).unwrap();
            let provider = os.provider();
            core(Operation {
                kind,
                n,
                provider: &provider,
                dir,
                input: input.as_mut_ptr(),
                output: output.as_mut_ptr(),
            })
        }
        "std" => {
            for _ in 0..n {
                if kind == 3 {
                    std::fs::File::open(path.join("input"))
                        .unwrap()
                        .read_exact(&mut output)
                        .unwrap();
                } else {
                    std::fs::File::create(path.join("output"))
                        .unwrap()
                        .write_all(&input)
                        .unwrap();
                }
            }
            n * MB as u64
        }
        "confined" => {
            let directory = std::fs::File::open(&path).unwrap();
            for _ in 0..n {
                let mut how: libc::open_how = unsafe { std::mem::zeroed() };
                how.flags = (libc::O_CLOEXEC
                    | if kind == 3 {
                        libc::O_RDONLY
                    } else {
                        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC
                    }) as u64;
                how.mode = if kind == 3 { 0 } else { 0o600 };
                how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;
                let name = if kind == 3 { c"input" } else { c"output" };
                // SAFETY: Rust twin uses the same confined lookup, owned fd and capacity.
                unsafe {
                    use std::os::fd::FromRawFd;
                    let fd = libc::syscall(
                        libc::SYS_openat2,
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        &how,
                        size_of::<libc::open_how>(),
                    ) as i32;
                    assert!(fd >= 0);
                    let mut file = std::fs::File::from_raw_fd(fd);
                    if kind == 3 {
                        file.read_exact(&mut output).unwrap();
                    } else {
                        file.write_all(&input).unwrap();
                    }
                }
            }
            n * MB as u64
        }
        "tokio" => {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(async {
                for _ in 0..n {
                    if kind == 3 {
                        tokio::fs::File::open(path.join("input"))
                            .await
                            .unwrap()
                            .read_exact(&mut output)
                            .await
                            .unwrap();
                    } else {
                        let mut file = tokio::fs::File::create(path.join("output")).await.unwrap();
                        file.write_all(&input).await.unwrap();
                        file.flush().await.unwrap();
                    }
                }
            });
            n * MB as u64
        }
        _ => panic!("unknown IO model"),
    };
    if kind == 3 {
        assert_eq!(output, input);
    } else {
        assert_eq!(std::fs::read(path.join("output")).unwrap(), input);
    }
    std::fs::remove_dir_all(path).unwrap();
    result
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let model = &args[1];
    let kind = match args[2].as_str() {
        "spawn" => 0,
        "roundtrip" => 1,
        "pipeline" => 2,
        "read" => 3,
        "write" => 4,
        _ => panic!("unknown workload"),
    };
    let n = args[3].parse::<u64>().unwrap();
    assert!(n > 0);
    let result = if kind >= 3 {
        io(model, kind, n)
    } else {
        match model.as_str() {
            "core" => core(Operation {
                kind,
                n,
                provider: ptr::null(),
                dir: 0,
                input: ptr::null_mut(),
                output: ptr::null_mut(),
            }),
            "threads" => {
                for _ in 0..n {
                    std::thread::spawn(|| std::hint::black_box(()))
                        .join()
                        .unwrap();
                }
                n
            }
            "mpsc" => mpsc(kind, n),
            "crossbeam" => crossbeam(kind, n),
            "tokio" => {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    if kind == 0 {
                        for _ in 0..n {
                            tokio::spawn(async { std::hint::black_box(()) })
                                .await
                                .unwrap();
                        }
                        n
                    } else {
                        tokio_channels(kind, n).await
                    }
                })
            }
            _ => panic!("unknown model"),
        }
    };
    assert_eq!(
        result,
        if kind == 0 {
            n
        } else if kind < 3 {
            n * (n - 1) / 2
        } else {
            n * MB as u64
        }
    );
    println!("{result}");
}
