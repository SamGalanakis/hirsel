//! Optional diagnostic instruction attribution. Absent from ordinary compiler units.
//! Phase, construct, body and instance are independent views; never sum across views.
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Row {
    pub view: String,
    pub name: String,
    pub occurrences: u64,
    pub inclusive: u64,
    pub exclusive: u64,
}
#[derive(Debug, Default, serde::Serialize)]
pub struct Profile {
    pub rows: Vec<Row>,
    pub counter_refusal: Option<String>,
    pub excluded_instrumentation: u64,
}
struct Frame {
    row: usize,
    start: u64,
    children: u64,
}
struct State {
    read: Box<dyn Fn() -> Result<u64, String>>,
    rows: Vec<Row>,
    keys: BTreeMap<(&'static str, String), usize>,
    stack: Vec<Frame>,
    refusal: Option<String>,
    last: u64,
    work: u64,
    excluded: u64,
}
impl State {
    fn pause(&mut self) -> Result<u64, String> {
        let raw = (self.read)()?;
        self.work += raw - self.last;
        Ok(raw)
    }
    fn resume(&mut self, before: u64) -> Result<(), String> {
        self.last = (self.read)()?;
        self.excluded += self.last - before;
        Ok(())
    }
}
thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}
/// Install the serial driver's counter. Event bookkeeping is excluded from all views.
/// Call/return and counter-wrapper overhead remains; use the ordinary driver for savings.
pub fn start(read: impl Fn() -> Result<u64, String> + 'static) -> Result<(), String> {
    let last = read()?;
    STATE.with_borrow_mut(|s| {
        assert!(s.is_none(), "an instruction profile is already running");
        *s = Some(State {
            read: Box::new(read),
            rows: vec![],
            keys: BTreeMap::new(),
            stack: vec![],
            refusal: None,
            last,
            work: 0,
            excluded: 0,
        });
    });
    Ok(())
}
pub fn finish() -> Profile {
    STATE.with_borrow_mut(|s| {
        let Some(s) = s.take() else {
            return Profile::default();
        };
        assert!(s.stack.is_empty(), "instruction profile has an open scope");
        Profile {
            rows: s.rows,
            counter_refusal: s.refusal,
            excluded_instrumentation: s.excluded,
        }
    })
}
#[must_use]
pub struct Scope(bool);
impl Scope {
    pub fn enter(view: &'static str, name: impl FnOnce() -> String) -> Self {
        STATE.with_borrow_mut(|state| {
            let Some(s) = state else { return Self(false) };
            let raw = match s.pause() {
                Ok(raw) => raw,
                Err(e) => {
                    s.refusal = Some(e);
                    return Self(false);
                }
            };
            let name = name();
            let next = s.rows.len();
            let row = *s.keys.entry((view, name.clone())).or_insert_with(|| {
                s.rows.push(Row {
                    view: view.into(),
                    name,
                    ..Default::default()
                });
                next
            });
            s.stack.push(Frame {
                row,
                start: s.work,
                children: 0,
            });
            if let Err(e) = s.resume(raw) {
                s.refusal = Some(e);
            }
            Self(true)
        })
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if !self.0 {
            return;
        }
        STATE.with_borrow_mut(|state| {
            let Some(s) = state else { return };
            let f = s.stack.pop().expect("profile scope nesting");
            let raw = match s.pause() {
                Ok(raw) => raw,
                Err(e) => {
                    s.refusal = Some(e);
                    return;
                }
            };
            let inclusive = s.work - f.start;
            let row = &mut s.rows[f.row];
            row.occurrences += 1;
            row.inclusive += inclusive;
            row.exclusive += inclusive - f.children;
            let view = &s.rows[f.row].view;
            if let Some(parent) = s
                .stack
                .iter_mut()
                .rev()
                .find(|p| s.rows[p.row].view == *view)
            {
                parent.children += inclusive;
            }
            if let Err(e) = s.resume(raw) {
                s.refusal = Some(e);
            }
        });
    }
}
