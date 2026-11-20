//! Crossing-price microbenchmarks outside hirsel.
//!
//! prim_*: the units crossing primitive alone (bounded Mutex ring of owned
//! bytes, eventfd wake only when the receiver armed a wait), between two
//! threads pinned to two CPUs. The receiver either busy-polls the ring or
//! sleeps in epoll_wait on its eventfd.
//! tokio_*: the same shapes on tokio::sync::mpsc of Vec<u8>.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use units_bench::{kernel, now_ns, pin};

struct Ring {
    bytes: VecDeque<u8>,
    lens: VecDeque<usize>,
    rx_waiting: bool,
    tx_waiting: bool,
}
struct Dir {
    ring: Mutex<Ring>,
    rx_fd: i32,
    tx_fd: i32,
    epfd_rx: i32,
    epfd_tx: i32,
    cap: usize,
}
fn efd() -> i32 {
    unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) }
}
fn ep(fd: i32) -> i32 {
    unsafe {
        let e = libc::epoll_create1(libc::EPOLL_CLOEXEC);
        let mut ev = libc::epoll_event { events: (libc::EPOLLIN | libc::EPOLLET) as u32, u64: 0 };
        libc::epoll_ctl(e, libc::EPOLL_CTL_ADD, fd, &mut ev);
        e
    }
}
fn kick(fd: i32) {
    let one = 1u64;
    unsafe { libc::write(fd, (&one as *const u64).cast(), 8) };
}
fn sleep_on(epfd: i32) {
    let mut ev = libc::epoll_event { events: 0, u64: 0 };
    unsafe { libc::epoll_wait(epfd, &mut ev, 1, -1) };
}
impl Dir {
    fn new(cap: usize) -> Arc<Dir> {
        let (rx_fd, tx_fd) = (efd(), efd());
        Arc::new(Dir {
            ring: Mutex::new(Ring { bytes: VecDeque::with_capacity(cap), lens: VecDeque::new(), rx_waiting: false, tx_waiting: false }),
            rx_fd,
            tx_fd,
            epfd_rx: ep(rx_fd),
            epfd_tx: ep(tx_fd),
            cap,
        })
    }
    fn send(&self, msg: &[u8], spin: bool) {
        loop {
            let mut r = self.ring.lock().unwrap();
            if r.bytes.len() + msg.len() <= self.cap {
                r.bytes.extend(msg);
                r.lens.push_back(msg.len());
                let wake = std::mem::take(&mut r.rx_waiting);
                drop(r);
                if wake {
                    kick(self.rx_fd);
                }
                return;
            }
            if spin {
                drop(r);
                std::hint::spin_loop();
                continue;
            }
            r.tx_waiting = true;
            drop(r);
            sleep_on(self.epfd_tx);
        }
    }
    /// Receive up to `max` whole messages into out; returns how many.
    fn recv(&self, out: &mut Vec<u8>, max: usize, spin: bool) -> usize {
        loop {
            let mut r = self.ring.lock().unwrap();
            if !r.lens.is_empty() {
                let mut n = 0;
                while n < max {
                    let Some(len) = r.lens.pop_front() else { break };
                    out.extend(r.bytes.drain(..len));
                    n += 1;
                }
                let wake = std::mem::take(&mut r.tx_waiting);
                drop(r);
                if wake {
                    kick(self.tx_fd);
                }
                return n;
            }
            if spin {
                drop(r);
                std::hint::spin_loop();
                continue;
            }
            r.rx_waiting = true;
            drop(r);
            sleep_on(self.epfd_rx);
        }
    }
}

fn arg(i: usize, d: u64) -> u64 {
    std::env::args().nth(i).and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn cpus() -> Vec<usize> {
    std::env::var("XCPUS").unwrap_or("0,8".into()).split(',').map(|c| c.parse().unwrap()).collect()
}
fn report(name: &str, n: u64, ns: u64, check: u64) {
    eprintln!("{name} n={n} ns={ns} check={check}");
}

/// Request/reply: client on cpus[0], echo on cpus[1]. depth outstanding.
fn prim_reqrep(n: u64, size: usize, depth: u64, spin: bool) {
    let cpu = cpus();
    let (req, rep) = (Dir::new(256 * 1024), Dir::new(256 * 1024));
    let (rq, rp) = (req.clone(), rep.clone());
    let c1 = cpu[1];
    let echo = std::thread::spawn(move || {
        pin(c1);
        let mut buf = Vec::with_capacity(size * 64);
        let mut done = 0;
        while done < n {
            buf.clear();
            let got = rq.recv(&mut buf, 64, spin) as u64;
            for m in buf.chunks_mut(size) {
                m[0] = m[0].wrapping_add(1);
                rp.send(m, spin);
            }
            done += got;
        }
    });
    pin(cpu[0]);
    let mut msg = vec![7u8; size];
    let mut buf = Vec::with_capacity(size * 64);
    let mut check = 0u64;
    let started = now_ns();
    let (mut sent, mut got) = (0u64, 0u64);
    while got < n {
        while sent < n && sent - got < depth {
            msg[0] = sent as u8;
            req.send(&msg, spin);
            sent += 1;
        }
        buf.clear();
        let k = rep.recv(&mut buf, 64, spin) as u64;
        for m in buf.chunks(size) {
            assert_eq!(m[0], (got as u8).wrapping_add(1));
            check = check.wrapping_add(m[0] as u64);
            got += 1;
        }
        let _ = k;
    }
    let ns = now_ns() - started;
    echo.join().unwrap();
    report(&format!("prim_reqrep_{}_d{depth}", if spin { "poll" } else { "sleep" }), n, ns, check);
}

/// Fan-in: producers on cpus[1..] each send n to one consumer on cpus[0].
fn prim_fanin(n: u64, size: usize, producers: usize, grain: u64, spin: bool) {
    let cpu = cpus();
    let dirs: Vec<Arc<Dir>> = (0..producers).map(|_| Dir::new(256 * 1024)).collect();
    // One shared wake fd for the consumer, as a unit's epoll sees all its ports.
    let mut handles = Vec::new();
    let started = now_ns();
    for (p, d) in dirs.iter().enumerate() {
        let d = d.clone();
        let c = cpu[1 + p % (cpu.len() - 1)];
        handles.push(std::thread::spawn(move || {
            pin(c);
            let mut msg = vec![7u8; size];
            for i in 0..n {
                msg[0] = i as u8;
                d.send(&msg, spin);
            }
        }));
    }
    pin(cpu[0]);
    let mut buf = Vec::with_capacity(size * 64);
    let mut check = 0u64;
    let mut left: Vec<u64> = vec![n; producers];
    let mut total = 0;
    while total < n * producers as u64 {
        let mut progress = false;
        for (p, d) in dirs.iter().enumerate() {
            if left[p] == 0 {
                continue;
            }
            buf.clear();
            // Non-blocking poll of each port; sleep only when all are empty.
            let k = {
                let mut r = d.ring.lock().unwrap();
                let mut k = 0;
                while k < 64 {
                    let Some(len) = r.lens.pop_front() else { break };
                    buf.extend(r.bytes.drain(..len));
                    k += 1;
                }
                if k > 0 && std::mem::take(&mut r.tx_waiting) {
                    kick(d.tx_fd);
                }
                k
            };
            for m in buf.chunks(size) {
                check = check.wrapping_add(kernel(m[0] as u64, grain) & 0xff);
            }
            left[p] -= k as u64;
            total += k as u64;
            progress |= k > 0;
        }
        if !progress && !spin {
            // Arm every port, then sleep on the first one with no data (all share
            // nothing here, so poll with a short sleep via epoll on port 0's fd).
            let mut armed = true;
            for d in &dirs {
                let mut r = d.ring.lock().unwrap();
                if !r.lens.is_empty() {
                    armed = false;
                    break;
                }
                r.rx_waiting = true;
            }
            if armed {
                let mut ev = libc::epoll_event { events: 0, u64: 0 };
                unsafe { libc::epoll_wait(shared_ep(&dirs), &mut ev, 1, -1) };
            }
        }
    }
    let ns = now_ns() - started;
    for h in handles {
        h.join().unwrap();
    }
    report(&format!("prim_fanin_{}_g{grain}", if spin { "poll" } else { "sleep" }), n * producers as u64, ns, check);
}

fn shared_ep(dirs: &[Arc<Dir>]) -> i32 {
    use std::sync::OnceLock;
    static EP: OnceLock<i32> = OnceLock::new();
    *EP.get_or_init(|| unsafe {
        let e = libc::epoll_create1(libc::EPOLL_CLOEXEC);
        for d in dirs {
            let mut ev = libc::epoll_event { events: (libc::EPOLLIN | libc::EPOLLET) as u32, u64: 0 };
            libc::epoll_ctl(e, libc::EPOLL_CTL_ADD, d.rx_fd, &mut ev);
        }
        e
    })
}

/// Tokio mpsc request/reply on a runtime with `workers` threads.
fn tokio_reqrep(n: u64, size: usize, depth: u64, workers: usize) {
    let rt = rt(workers);
    rt.block_on(async move {
        let cap = depth.max(1) as usize;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(cap);
        let (otx, mut orx) = tokio::sync::mpsc::channel::<Vec<u8>>(cap);
        let echo = tokio::spawn(async move {
            while let Some(mut m) = rx.recv().await {
                m[0] = m[0].wrapping_add(1);
                if otx.send(m).await.is_err() {
                    break;
                }
            }
        });
        let started = now_ns();
        let feeder = tokio::spawn(async move {
            let msg = vec![7u8; size];
            for i in 0..n {
                let mut m = msg.clone();
                m[0] = i as u8;
                tx.send(m).await.unwrap();
            }
        });
        let mut check = 0u64;
        for i in 0..n {
            let m = orx.recv().await.unwrap();
            assert_eq!(m[0], (i as u8).wrapping_add(1));
            check = check.wrapping_add(m[0] as u64);
        }
        let ns = now_ns() - started;
        feeder.await.unwrap();
        echo.await.unwrap();
        report(&format!("tokio_reqrep_w{workers}_d{depth}"), n, ns, check);
    });
}

fn tokio_fanin(n: u64, size: usize, producers: usize, grain: u64, workers: usize) {
    let rt = rt(workers);
    rt.block_on(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let started = now_ns();
        let mut hs = Vec::new();
        for _ in 0..producers {
            let tx = tx.clone();
            hs.push(tokio::spawn(async move {
                let msg = vec![7u8; size];
                for i in 0..n {
                    let mut m = msg.clone();
                    m[0] = i as u8;
                    tx.send(m).await.unwrap();
                }
            }));
        }
        drop(tx);
        let consumer = tokio::spawn(async move {
            let mut check = 0u64;
            let mut c = 0u64;
            while let Some(m) = rx.recv().await {
                check = check.wrapping_add(kernel(m[0] as u64, grain) & 0xff);
                c += 1;
            }
            (c, check)
        });
        for h in hs {
            h.await.unwrap();
        }
        let (c, check) = consumer.await.unwrap();
        report(&format!("tokio_fanin_w{workers}_g{grain}"), c, now_ns() - started, check);
    });
}

/// Ping-pong between two current-thread runtimes on two pinned threads: the
/// closest Tokio analogue of two units.
fn tokio_two_rt(n: u64, size: usize) {
    let cpu = cpus();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (otx, mut orx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let c1 = cpu[1];
    let echo = std::thread::spawn(move || {
        pin(c1);
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async move {
            while let Some(mut m) = rx.recv().await {
                m[0] = m[0].wrapping_add(1);
                if otx.send(m).await.is_err() {
                    break;
                }
            }
        })
    });
    pin(cpu[0]);
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async move {
        let msg = vec![7u8; size];
        let mut check = 0u64;
        let started = now_ns();
        for i in 0..n {
            let mut m = msg.clone();
            m[0] = i as u8;
            tx.send(m).await.unwrap();
            let r = orx.recv().await.unwrap();
            check = check.wrapping_add(r[0] as u64);
        }
        report("tokio_tworuntimes_pingpong", n, now_ns() - started, check);
    });
    echo.join().unwrap();
}

fn rt(workers: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(workers).build().unwrap()
}

fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let n = arg(2, 100000);
    let size = arg(3, 64) as usize;
    match mode.as_str() {
        "prim_pingpong_poll" => prim_reqrep(n, size, 1, true),
        "prim_pingpong_sleep" => prim_reqrep(n, size, 1, false),
        "prim_pipe_poll" => prim_reqrep(n, size, arg(4, 64), true),
        "prim_pipe_sleep" => prim_reqrep(n, size, arg(4, 64), false),
        "prim_fanin_poll" => prim_fanin(n, size, arg(4, 3) as usize, arg(5, 0), true),
        "prim_fanin_sleep" => prim_fanin(n, size, arg(4, 3) as usize, arg(5, 0), false),
        "tokio_pingpong" => tokio_reqrep(n, size, 1, arg(4, 2) as usize),
        "tokio_pipe" => tokio_reqrep(n, size, arg(5, 64), arg(4, 2) as usize),
        "tokio_fanin" => tokio_fanin(n, size, arg(4, 3) as usize, arg(5, 0), arg(6, 4) as usize),
        "tokio_tworuntimes" => tokio_two_rt(n, size),
        _ => panic!("mode"),
    }
}
