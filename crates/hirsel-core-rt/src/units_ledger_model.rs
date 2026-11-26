//! The ledger law's model-only operations. They compile in test builds, where
//! `units_ledger_law.rs` and the connection checker drive them; the language
//! units and the connection benchmark do not use them.
use super::{
    Account, ArrivalRef, Body, Book, Cap, Charge, Charges, Credit, Fin, Port, Res, Shared, UnitCtx,
    Value, ValueExtra, repay,
};
use std::{sync::Arc, sync::atomic::Ordering};

/// Bytes a spawn reserves for the task itself, released at the end take.
const TASK_FIXED: u64 = 256;
/// A primary or replacement cause cell.
const CELL: u64 = 32;
/// A board's root.
const BOARD_ROOT: u64 = 32;

impl Shared {
    pub(crate) fn share(&self) -> Shared {
        // SAFETY: a live share keeps the node alive.
        unsafe { self.0.as_ref() }
            .count
            .fetch_add(1, Ordering::Relaxed);
        Shared(self.0)
    }
    pub(crate) fn same(&self, o: &Shared) -> bool {
        self.0 == o.0
    }
}

pub(crate) struct BoardCore {
    root: Charge,
    home: Arc<Port>,
    st: crate::units::sync::Mutex<BoardSt>,
    label: String,
}
struct BoardSt {
    cur: Option<Shared>,
    poster: bool,
    handles: u64,
}
// SAFETY: the state is behind the lock; the root charge is read only at release.
unsafe impl Send for BoardCore {}
unsafe impl Sync for BoardCore {}

impl UnitCtx {
    // -- local operations -------------------------------------------------------------

    /// The bytes charged to the scope that admits now.
    pub(crate) fn current_used(&self) -> u64 {
        self.funding.as_ref().map_or(0, |f| f.payer.get().usage().0)
    }

    pub(crate) fn probe(&self) -> u64 {
        self.funding().available()
    }

    pub(crate) fn alloc_run_lifetime(&mut self, size: u64) -> Result<(), Cap> {
        let r = self.admit(size)?;
        self.run_lifetime.push(Charge { size, payer: r });
        Ok(())
    }

    pub(crate) fn make_resource(
        &mut self,
        fields: u64,
        grant: u64,
        fin: Option<Fin>,
    ) -> Result<Value, Cap> {
        let r = self.admit(fields + CELL + grant)?;
        let mut charges = Charges::Empty;
        if fields > 0 {
            charges.push(Charge {
                size: fields,
                payer: r,
            });
        }
        let cell = charges.len();
        charges.push(Charge {
            size: CELL,
            payer: r,
        });
        let g = (grant > 0).then(|| {
            charges.push(Charge {
                size: grant,
                payer: r,
            });
            charges.len() - 1
        });
        let mut v = Value::plain(charges);
        v.extra_mut().res = Some(Res {
            cell,
            grant: g,
            grant_size: grant,
            fin,
        });
        Ok(v)
    }

    /// One value holding all of these (no charge; a resource's finalizer is lost).
    pub(crate) fn combine(&mut self, vals: Vec<Value>) -> Value {
        let mut charges = Charges::Empty;
        let mut shared = Vec::new();
        for v in vals {
            let parts = v.data.into_parts();
            charges.extend(parts.charges.iter().copied());
            shared.extend(parts.shared);
        }
        let mut out = Value::plain(charges);
        if !shared.is_empty() {
            out.extra_mut().shared = shared;
        }
        out
    }

    pub(crate) fn enter_bound(&mut self, size: u64) -> Result<(), Cap> {
        let f = self
            .funding
            .as_mut()
            .expect("a language unit has no bounds here");
        let avail = f.available();
        if size > avail {
            return Err(Cap { req: size, avail });
        }
        f.enter(size).expect("a bound within the available bytes");
        let k = self.next_bound;
        self.next_bound += 1;
        let b = Book::Bound(f.payer.get());
        self.books.push((format!("{}/b{k}", self.label), b));
        let _ = k;
        Ok(())
    }

    /// Leave the innermost bound: its unused bytes go back to the scope
    /// around it, which pays for its charges from now on.
    pub(crate) fn exit_bound(&mut self) {
        let i = self
            .books
            .iter()
            .rposition(|(_, b)| matches!(b, Book::Bound(_)));
        self.books.remove(i.expect("cannot exit the root scope"));
        self.funding.as_mut().unwrap().exit();
    }

    /// Catch a failure inside an isolate: a replacement cell is admitted;
    /// in a helper, the active primary cell is caught and the replacement
    /// becomes the primary cell.
    pub(crate) fn isolate_fail(&mut self) -> Result<Value, Cap> {
        let r = self.admit(CELL)?;
        let new = Charge {
            size: CELL,
            payer: r,
        };
        let caught = match self.cell.as_mut() {
            Some(c) => std::mem::replace(c, new),
            None => new,
        };
        Ok(Value::plain(Charges::One(caught)))
    }

    // -- helpers --------------------------------------------------------------------

    /// Spawn an owned model helper with limit `n` and an inline result slot
    /// of `r` bytes. The spawn charges `256 + r + 32 + n` to the current
    /// scope: the task record and the result slot stay with the owner; `n`
    /// and the 32-byte cell are reservations prepaid from that charge,
    /// ended at the end take. The argument's blocks move into the root.
    #[allow(clippy::type_complexity)]
    pub(crate) fn spawn_helper(
        &mut self,
        n: u64,
        r: u64,
        arg: Option<Value>,
    ) -> Result<(usize, Box<UnitCtx>, Option<Value>), (Cap, Option<Value>)> {
        let t = TASK_FIXED + r + CELL + n;
        let avail = self.funding().available();
        if t > avail {
            return Err((Cap { req: t, avail }, arg));
        }
        let ab = arg.as_ref().map_or(0, Value::bytes);
        if ab > n {
            return Err((Cap { req: ab, avail: n }, arg));
        }
        let Ok(payer) = self.funding().unit_charge(t) else {
            unreachable!()
        };
        let charge = |size| Charge {
            size,
            payer: Account::limit(payer),
        };
        let (k, mut h) = self.new_helper(Some([charge(TASK_FIXED), charge(r)]));
        let root = payer.prepaid(n);
        let cell_res = payer.prepaid(CELL);
        let Ok(cell) = cell_res.unit_charge(CELL) else {
            unreachable!()
        };
        h.cell = Some(Charge {
            size: CELL,
            payer: Account::limit(cell),
        });
        let label = h.label.clone();
        h.books.push((label.clone(), Book::Scope(root.payer.get())));
        h.books
            .push((format!("{label}/cell"), Book::Scope(cell_res.payer.get())));
        let arg = arg.map(|mut v| {
            for c in v.charges_mut().iter_mut() {
                repay(c.payer, c.size);
                let Ok(p) = root.unit_charge(c.size) else {
                    unreachable!()
                };
                c.payer = Account::limit(p);
            }
            v
        });
        h.helper.as_mut().unwrap().cell_res = Some(cell_res);
        h.funding = Some(root);
        Ok((k, h, arg))
    }

    // -- shared nodes and boards ------------------------------------------------------

    pub(crate) fn make_shared(
        &mut self,
        size: u64,
        payload: Option<Value>,
    ) -> Result<Shared, (Cap, Option<Value>)> {
        let r = match self.admit(size) {
            Ok(r) => r,
            Err(c) => return Err((c, payload)),
        };
        let parts = payload.map_or_else(ValueExtra::default, |v| v.data.into_parts());
        Ok(self.new_node(Charge { size, payer: r }, parts.charges, parts.shared))
    }

    pub(crate) fn make_board(&mut self, init: Shared) -> Result<Arc<BoardCore>, (Cap, Shared)> {
        let r = match self.admit(BOARD_ROOT) {
            Ok(r) => r,
            Err(c) => return Err((c, init)),
        };
        let b = self.next_board;
        self.next_board += 1;
        let _ = b;
        Ok(Arc::new(BoardCore {
            root: Charge {
                size: BOARD_ROOT,
                payer: r,
            },
            home: self.port.clone(),
            st: crate::units::sync::Mutex::new(BoardSt {
                cur: Some(init),
                poster: true,
                handles: 1,
            }),
            label: format!("{}.B{b}", self.label),
        }))
    }

    pub(crate) fn board_add_handle(b: &BoardCore) {
        b.st.lock().unwrap().handles += 1;
    }

    pub(crate) fn board_read(&self, b: &BoardCore) -> Shared {
        b.st.lock()
            .unwrap()
            .cur
            .as_ref()
            .expect("a released board")
            .share()
    }

    pub(crate) fn board_post(&mut self, b: &BoardCore, s: Shared) {
        let old = {
            let mut st = b.st.lock().unwrap();
            assert!(st.poster);
            st.cur.replace(s)
        };
        if let Some(o) = old {
            self.drop_shared(o);
        }
    }

    pub(crate) fn board_drop_poster(&mut self, b: &BoardCore) {
        let rel = {
            let mut st = b.st.lock().unwrap();
            assert!(st.poster);
            st.poster = false;
            (st.handles == 0).then(|| st.cur.take()).flatten()
        };
        self.board_release(b, rel);
    }

    pub(crate) fn board_drop_handle(&mut self, b: &BoardCore) {
        let rel = {
            let mut st = b.st.lock().unwrap();
            st.handles -= 1;
            (st.handles == 0 && !st.poster)
                .then(|| st.cur.take())
                .flatten()
        };
        self.board_release(b, rel);
    }

    fn board_release(&mut self, b: &BoardCore, cur: Option<Shared>) {
        let Some(s) = cur else { return };
        self.drop_shared(s);
        let label = b.label.clone();
        let a = named!(
            ArrivalRef::boxed(Body::Credits(vec![Credit {
                amount: b.root.size,
                to: b.root.payer,
            }])),
            format!("board:{label}"),
            format!("credits:{label}")
        );
        b.home.publish(a);
    }
}
