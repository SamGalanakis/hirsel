//! The ledger law, as a differential against the charge model
//! (`hirsel-briefs/mc-conn/model`, Set B): every scenario the model ran —
//! its own witnesses, the attack files' cross-unit witnesses, and its
//! exhaustive and seeded random schedules — is replayed here against the
//! runtime. The result of every call and the balance of every live record
//! after every call must equal the model's. The order in which arrivals
//! from different sources reach each unit is the model's (`reach` lines),
//! through the runtime's test hook; takes, intake and forwarding are the
//! runtime's own.
//!
//! The trace is regenerated with `export.py` in the model directory.
use super::conn::{Client, Inbox, Received, Sent};
use super::model::BoardCore;
use super::{Fin, NAMING, Released, Shared, TakeInfo, Took, UnitCtx, Value};
use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::NonNull,
    sync::{Arc, atomic::Ordering},
};

const TRACE: &str = include_str!("units_ledger_law.trace");

enum Slot {
    Live(Box<UnitCtx>),
    /// Ended, its end not yet taken: the context travels in its end arrival.
    Ended(NonNull<UnitCtx>),
    Gone,
}

struct ConnH {
    inbox: Inbox,
    clients: Vec<Client>,
}

#[derive(Default)]
struct Run {
    units: HashMap<u64, Slot>,
    vals: HashMap<u64, Value>,
    seen: HashSet<u64>,
    conns: HashMap<u64, ConnH>,
    nodes: HashMap<u64, Vec<Shared>>,
    boards: HashMap<u64, Arc<BoardCore>>,
    waits: HashMap<u64, (u64, u32, u64)>,
    helpers: HashMap<u64, (u64, usize)>,
    by_label: HashMap<String, u64>,
}

fn id(t: &str) -> u64 {
    t[1..].parse().unwrap_or_else(|_| panic!("bad id {t}"))
}

fn ctx(units: &mut HashMap<u64, Slot>, u: u64) -> &mut UnitCtx {
    match units.get_mut(&u) {
        Some(Slot::Live(b)) => b,
        // SAFETY: the end arrival holding it has not been taken (the harness
        // marks the slot gone at that take), and the harness is single-threaded.
        Some(Slot::Ended(p)) => unsafe { p.as_mut() },
        _ => panic!("no unit u{u}"),
    }
}

fn fin(s: &str) -> Option<Fin> {
    if s == "-" {
        return None;
    }
    let mut f = Fin::default();
    for kv in s.split(',') {
        let (k, v) = kv.split_once('=').unwrap();
        let n: u64 = v.parse().unwrap();
        match k {
            "fails" => f.fails = n != 0,
            "fail_msg_size" => f.fail_msg = n,
            "spawn_shared_size" => f.spawn_shared = n,
            "share_to_unit" => f.share_out = true,
            "keep_grant_survivor" => f.keep_survivor = n,
            "run_lifetime_size" => f.run_lifetime = n,
            _ => panic!("unknown finalizer key {k}"),
        }
    }
    Some(f)
}

fn cap(c: super::Cap) -> String {
    format!("cap {} {}", c.req, c.avail)
}

impl Run {
    /// Name a value the runtime returned: a fresh one takes the model's id
    /// (if the model's id is fresh too), a known one must carry it.
    fn bind(&mut self, mut v: Value, want: Option<&str>) -> String {
        if v.tag == 0 {
            if let Some(n) = want.filter(|w| w.starts_with('v')).map(id) {
                if !self.seen.contains(&n) {
                    v.tag = n;
                }
            }
        }
        if v.tag == 0 {
            return "v<fresh>".into();
        }
        self.seen.insert(v.tag);
        let s = format!("v{}", v.tag);
        self.vals.insert(v.tag, v);
        s
    }

    fn val(&mut self, t: &str) -> Value {
        self.vals
            .remove(&id(t))
            .unwrap_or_else(|| panic!("no value {t}"))
    }

    fn put(&mut self, v: Value) {
        assert!(v.tag != 0);
        self.vals.insert(v.tag, v);
    }

    fn opt_val(&mut self, t: &str) -> Option<Value> {
        (t != "-").then(|| self.val(t))
    }

    fn snapshot(&self) -> String {
        let mut out = Vec::new();
        for s in self.units.values() {
            match s {
                Slot::Live(b) => b.snapshot(&mut out),
                // SAFETY: as in `ctx`.
                Slot::Ended(p) => unsafe { p.as_ref() }.snapshot(&mut out),
                Slot::Gone => {}
            }
        }
        out.sort();
        format!("snap {}", out.join(" "))
    }

    /// Bind the nodes a finalizer made during the call to the model's ids.
    fn made(&mut self, u: u64, want: &str) -> String {
        let made = std::mem::take(&mut ctx(&mut self.units, u).fin_made);
        if made.is_empty() {
            return String::new();
        }
        let ids: Vec<&str> = want
            .split(' ')
            .find_map(|t| t.strip_prefix("new_shared="))
            .map(|l| l.split(',').collect())
            .unwrap_or_default();
        let mut names = Vec::new();
        for (i, m) in made.into_iter().enumerate() {
            let Some(t) = ids.get(i) else {
                names.push("s?".to_string());
                continue;
            };
            let e = self.nodes.entry(id(t)).or_default();
            e.extend(m);
            names.push(t.to_string());
        }
        format!(" new_shared={}", names.join(","))
    }

    fn op(&mut self, line: &str, want: &str) -> String {
        let (op, rest) = line.split_once(' ').unwrap_or((line, ""));
        let a: Vec<&str> = rest.split(' ').filter(|s| !s.is_empty()).collect();
        let w: Vec<&str> = want.split(' ').collect();
        let u = a.first().filter(|t| t.starts_with('u')).map(|t| id(t));
        let num = |i: usize| -> u64 { a[i].parse().unwrap() };
        let out = match op {
            "probe" => ctx(&mut self.units, u.unwrap()).probe().to_string(),
            "alloc_block" => match ctx(&mut self.units, u.unwrap()).alloc_block(num(1)) {
                Ok(v) => format!("ok {}", self.bind(v, w.get(1).copied())),
                Err(c) => cap(c),
            },
            "alloc_run_lifetime" => {
                match ctx(&mut self.units, u.unwrap()).alloc_run_lifetime(num(1)) {
                    Ok(()) => "ok".into(),
                    Err(c) => cap(c),
                }
            }
            "make_resource" => {
                let f = fin(a[3]);
                match ctx(&mut self.units, u.unwrap()).make_resource(num(1), num(2), f) {
                    Ok(v) => format!("ok {}", self.bind(v, w.get(1).copied())),
                    Err(c) => cap(c),
                }
            }
            "combine_values" => {
                let vs: Vec<Value> = a[1].split(',').map(|t| self.val(t)).collect();
                let v = ctx(&mut self.units, u.unwrap()).combine(vs);
                self.bind(v, w.first().copied())
            }
            "drop_value" => {
                let v = self.val(a[1]);
                match ctx(&mut self.units, u.unwrap()).drop_value(v) {
                    Some(c) => self.bind(c, w.first().copied()),
                    None => "-".into(),
                }
            }
            "enter_bound" => match ctx(&mut self.units, u.unwrap()).enter_bound(num(1)) {
                Ok(()) => "ok".into(),
                Err(c) => cap(c),
            },
            "exit_bound" => {
                ctx(&mut self.units, u.unwrap()).exit_bound();
                String::new()
            }
            "isolate_fail" => match ctx(&mut self.units, u.unwrap()).isolate_fail() {
                Ok(v) => format!("ok {}", self.bind(v, w.get(1).copied())),
                Err(c) => cap(c),
            },
            "spawn_helper" => {
                let arg = self.opt_val(a[3]);
                let owner = u.unwrap();
                match ctx(&mut self.units, owner).spawn_helper(num(1), num(2), arg) {
                    Ok((k, mut h, arg)) => {
                        if let Some(v) = arg {
                            self.put(v);
                        }
                        let hid = w.get(1).map(|t| id(t)).unwrap_or(u64::MAX);
                        h.hook();
                        self.by_label.insert(h.label.clone(), hid);
                        self.units.insert(hid, Slot::Live(h));
                        self.helpers.insert(hid, (owner, k));
                        format!("ok u{hid}")
                    }
                    Err((c, arg)) => {
                        if let Some(v) = arg {
                            self.put(v);
                        }
                        cap(c)
                    }
                }
            }
            "end_helper" => {
                let h = u.unwrap();
                let ret = self.opt_val(a[1]);
                let Some(Slot::Live(b)) = self.units.remove(&h) else {
                    panic!("end of a unit that is not live")
                };
                let p = b.end_helper(ret, a[3] == "1");
                self.units.insert(h, Slot::Ended(p));
                String::new()
            }
            "consume_helper_handle" => {
                let (_, k) = self.helpers[&id(a[1])];
                match ctx(&mut self.units, u.unwrap()).consume_helper(k, a[2] == "1") {
                    Some(v) => self.bind(v, w.first().copied()),
                    None => "-".into(),
                }
            }
            "make_shared" => {
                let p = self.opt_val(a[2]);
                match ctx(&mut self.units, u.unwrap()).make_shared(num(1), p) {
                    Ok(s) => {
                        let sid = id(w[1]);
                        self.nodes.insert(sid, vec![s]);
                        format!("ok s{sid}")
                    }
                    Err((c, p)) => {
                        if let Some(v) = p {
                            self.put(v);
                        }
                        cap(c)
                    }
                }
            }
            "share_node" => {
                let e = self.nodes.get_mut(&id(a[1])).unwrap();
                let s = e.last().expect("no share left").share();
                e.push(s);
                String::new()
            }
            "drop_shared" => {
                let s = self
                    .nodes
                    .get_mut(&id(a[1]))
                    .unwrap()
                    .pop()
                    .expect("no share left");
                ctx(&mut self.units, u.unwrap()).drop_shared(s);
                String::new()
            }
            "make_board" => {
                let s = self
                    .nodes
                    .get_mut(&id(a[1]))
                    .unwrap()
                    .pop()
                    .expect("no share left");
                match ctx(&mut self.units, u.unwrap()).make_board(s) {
                    Ok(b) => {
                        let bid = id(w[1]);
                        self.boards.insert(bid, b);
                        format!("ok b{bid}")
                    }
                    Err((c, s)) => {
                        self.nodes.get_mut(&id(a[1])).unwrap().push(s);
                        cap(c)
                    }
                }
            }
            "board_add_handle" => {
                UnitCtx::board_add_handle(&self.boards[&id(a[0])]);
                String::new()
            }
            "board_read" => {
                let b = self.boards[&id(a[1])].clone();
                let s = ctx(&mut self.units, u.unwrap()).board_read(&b);
                let sid = id(w[0]);
                let e = self.nodes.entry(sid).or_default();
                let ok = e.first().is_none_or(|x| x.same(&s));
                e.push(s);
                if ok {
                    format!("s{sid}")
                } else {
                    "s<other>".into()
                }
            }
            "board_post" => {
                let b = self.boards[&id(a[1])].clone();
                let s = self
                    .nodes
                    .get_mut(&id(a[2]))
                    .unwrap()
                    .pop()
                    .expect("no share left");
                ctx(&mut self.units, u.unwrap()).board_post(&b, s);
                String::new()
            }
            "board_drop_poster" => {
                let b = self.boards[&id(a[1])].clone();
                ctx(&mut self.units, u.unwrap()).board_drop_poster(&b);
                String::new()
            }
            "board_drop_handle" => {
                let b = self.boards[&id(a[1])].clone();
                ctx(&mut self.units, u.unwrap()).board_drop_handle(&b);
                String::new()
            }
            "conn_new" => match ctx(&mut self.units, u.unwrap()).conn_new(num(1), num(2), num(3)) {
                Ok((inbox, client)) => {
                    let cid = id(w[1]);
                    self.conns.insert(
                        cid,
                        ConnH {
                            inbox,
                            clients: vec![client],
                        },
                    );
                    format!("ok c{cid}")
                }
                Err(c) => cap(c),
            },
            "conn_add_client" => {
                let c = self.conns.get_mut(&id(a[0])).unwrap();
                let cl = c.clients.last().expect("no client left").clone_client();
                c.clients.push(cl);
                String::new()
            }
            "conn_send" => {
                let v = self.val(a[2]);
                let cid = id(a[1]);
                let sender = u.unwrap();
                let c = &self.conns[&cid];
                let cl = c.clients.last().expect("no client left");
                match ctx(&mut self.units, sender).send(cl, v, a[3] == "1") {
                    Sent::Bounds => "Bounds".into(),
                    Sent::Closed(v) => {
                        self.put(v);
                        "Closed".into()
                    }
                    Sent::Accepted => "Accepted".into(),
                    Sent::Full(v) => {
                        self.put(v);
                        "Full".into()
                    }
                    Sent::Waiting(sid) => {
                        let wid = w.get(1).map(|t| id(t)).unwrap_or(u64::MAX);
                        self.waits.insert(wid, (sender, sid, cid));
                        format!("Waiting w{wid}")
                    }
                }
            }
            "conn_withdraw" => {
                let (sender, sid, cid) = self.waits[&id(a[0])];
                let c = self.conns[&cid].inbox.0.clone();
                let won = ctx(&mut self.units, sender).withdraw(&Client(c), sid);
                if won { "won".into() } else { "lost".into() }
            }
            "conn_receive" => {
                let c = self.conns[&id(a[1])].inbox.0.clone();
                match ctx(&mut self.units, u.unwrap()).receive(&Inbox(c)) {
                    Received::Ok(v) => format!("Ok {}", self.bind(v, w.get(1).copied())),
                    Received::Empty => "Empty".into(),
                    Received::Closed => "Closed".into(),
                    Received::Capacity(c) => format!("Capacity {} {}", c.req, c.avail),
                }
            }
            "conn_seal" => {
                let c = self.conns[&id(a[1])].inbox.0.clone();
                ctx(&mut self.units, u.unwrap()).seal(&Inbox(c));
                String::new()
            }
            "conn_clean_items" => {
                let c = self.conns[&id(a[1])].inbox.0.clone();
                let uid = u.unwrap();
                let (causes, took) = ctx(&mut self.units, uid).clean(&Inbox(c));
                let wl: Vec<&str> = w[0].split(',').collect();
                let causes = if causes.is_empty() {
                    "-".into()
                } else {
                    let mut names = Vec::new();
                    for (i, v) in causes.into_iter().enumerate() {
                        names.push(self.bind(v, wl.get(i).copied()));
                    }
                    names.join(",")
                };
                format!("{causes} {}", self.release_took(uid, took))
            }
            "conn_drop_client" => {
                let cl = self
                    .conns
                    .get_mut(&id(a[1]))
                    .unwrap()
                    .clients
                    .pop()
                    .expect("no client left");
                let uid = u.unwrap();
                let took = ctx(&mut self.units, uid).drop_client(cl);
                self.release_took(uid, took)
            }
            "take_arrivals" => {
                let n = w
                    .iter()
                    .filter(|t| !t.is_empty() && !t.starts_with("new_shared="))
                    .count();
                let uid = u.unwrap();
                let mut got = Vec::new();
                for _ in 0..n {
                    let Some((t, arr)) = ctx(&mut self.units, uid).take() else {
                        got.push("<none>".to_string());
                        break;
                    };
                    got.push(self.took(uid, t, arr));
                }
                got.join(" ")
            }
            "end_root" => {
                ctx(&mut self.units, 0).end_root();
                String::new()
            }
            _ => panic!("unknown op {op}"),
        };
        let made = match u {
            Some(u) if matches!(self.units.get(&u), Some(Slot::Live(_) | Slot::Ended(_))) => {
                self.made(u, want)
            }
            _ => String::new(),
        };
        out + &made
    }
}

impl Run {
    /// Act on one take as the language runtime would; its identity.
    fn took(&mut self, uid: u64, t: Took, arr: TakeInfo) -> String {
        let ident = arr.as_ref().map_or("<unnamed>".into(), |n| n.ident.clone());
        match t {
            Took::HelperEnd => {
                let label = ident.strip_prefix("helper_end:").unwrap();
                let hid = self.by_label[label];
                self.units.insert(hid, Slot::Gone);
            }
            Took::Closed(sid) => {
                let v = ctx(&mut self.units, uid).closed_value(sid).unwrap();
                self.put(v);
            }
            _ => {}
        }
        ident
    }

    /// Rule 6: the prefix an owner's own release took, `took=0` when none.
    fn release_took(&mut self, uid: u64, took: Vec<Released>) -> String {
        let mut out = format!("took={}", took.len());
        for (t, _, arr) in took {
            out.push(' ');
            out += &self.took(uid, t, arr);
        }
        out
    }
}

struct Scenario<'a> {
    name: &'a str,
    first_line: usize,
    lines: Vec<&'a str>,
}

fn scenarios() -> Vec<Scenario<'static>> {
    let mut out = Vec::new();
    let mut cur: Option<Scenario> = None;
    for (i, l) in TRACE.lines().enumerate() {
        if let Some(n) = l.strip_prefix("scenario ") {
            cur = Some(Scenario {
                name: n,
                first_line: i + 1,
                lines: Vec::new(),
            });
        } else if l == "end" {
            out.push(cur.take().unwrap());
        } else {
            cur.as_mut().unwrap().lines.push(l);
        }
    }
    out
}

/// Replay one scenario; the first disagreement, if any.
fn replay(sc: &Scenario) -> Option<String> {
    let mut run = Run::default();
    let mut i = 0;
    let res = catch_unwind(AssertUnwindSafe(|| {
        for (k, l) in sc.lines.iter().enumerate() {
            i = k;
            if let Some(lim) = l.strip_prefix("root ") {
                let mut r = Box::new(UnitCtx::root(lim.parse().unwrap()));
                r.hook();
                run.by_label.insert("U0".into(), 0);
                run.units.insert(0, Slot::Live(r));
            } else if l.starts_with("snap") {
                let got = run.snapshot();
                if got.trim_end() != l.trim_end() {
                    return Some(format!("want {l}\n     got  {got}"));
                }
            } else if let Some(r) = l.strip_prefix("reach ") {
                let mut p = r.splitn(3, ' ');
                let (u, src, ident) = (p.next().unwrap(), p.next().unwrap(), p.next().unwrap());
                let got = ctx(&mut run.units, id(u)).reach(src);
                if got.as_deref() != Some(ident) {
                    return Some(format!("want {ident}\n     got  {got:?}"));
                }
            } else {
                let (call, want) = l.split_once(" =>").unwrap();
                let want = want.trim_start();
                let got = run.op(call, want);
                if got != want {
                    return Some(format!("want {want:?}\n     got  {got:?}"));
                }
            }
        }
        None
    }));
    let at = |m: String| {
        format!(
            "{} (trace line {}): {}\n     {}",
            sc.name,
            sc.first_line + 1 + i,
            sc.lines[i],
            m
        )
    };
    match res {
        Ok(None) => None,
        Ok(Some(m)) => Some(at(m)),
        Err(e) => {
            let m = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Some(at(format!("panicked: {m}")))
        }
    }
}

#[test]
fn ledger_law() {
    NAMING.store(true, Ordering::Relaxed);
    let all = scenarios();
    assert!(!all.is_empty());
    let bad: Vec<String> = all.iter().filter_map(replay).collect();
    if !bad.is_empty() {
        let shown: Vec<&String> = bad.iter().take(12).collect();
        panic!(
            "{} of {} scenarios disagree with the model:\n{}",
            bad.len(),
            all.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

impl super::Body {
    /// What an arrival carried, part of its taker's input.
    pub(super) fn content(&self) -> String {
        use super::{Body, Credit, Settle};
        let amounts = |cs: &[Credit]| {
            cs.iter()
                .map(|c| c.amount.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        match self {
            Body::Credits(cs) => amounts(cs),
            Body::LastRelease(retained, _) => retained.to_string(),
            // SAFETY: the end arrival owns the ended helper's context.
            Body::HelperEnd(e) => {
                let h = unsafe { e.0.as_ref() };
                let info = h.helper.as_ref().unwrap();
                format!(
                    "root {} ret {}",
                    h.current_used(),
                    info.ret.as_ref().map_or(0, |v| v.bytes())
                )
            }
            Body::Settlement(_, Settle::Accepted(cs)) => amounts(cs),
            Body::Settlement(_, Settle::Closed(v) | Settle::Withdrawn(v)) => {
                format!("{} bytes", v.bytes())
            }
            Body::Item(_, v) => format!("{} bytes tag {}", v.bytes(), v.tag),
            Body::Close(_) | Body::SealAnswer(_) | Body::Cancel | Body::SharedRelease(_) => {
                String::new()
            }
        }
    }
}
