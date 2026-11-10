//! Recorders (MODE-6): the runtime's task-local event queues
//! (`hirsel-rt/src/recorders.rs`) with each queued event a harness root, so H
//! observes the moves, the finish order and the releases of events a dropped
//! recorder still holds.
//!
//! An event is the `Some` image of the compiled bodies' `Option[E]` slot, as a
//! channel message is. The open names the element: `Option[E]`'s layout,
//! through its descriptor (this executor's descriptor index is the layout id),
//! and its `None` image. An append moves a `Some` of that layout into the queue
//! and leaves `None`; a take moves the oldest into a `None` slot; the drop's
//! `CloseHandle` drops what is left, oldest first. Recorder operations are local:
//! no scheduling point, no timer choice.
use super::*;
use std::collections::VecDeque;

/// The tag of a recorder's handle word, below the mutex and channel tags.
pub(super) const RECORDER: u64 = 1 << 60;
/// Harness root keys of the queued events.
const EVENT_ROOT: u64 = 1 << 59;

#[derive(Clone)]
struct Recorder {
    layout: LayoutId,
    none: Extent,
    queue: VecDeque<u64>,
}

#[derive(Clone, Default)]
pub(super) struct Recorders {
    rows: Vec<Option<Recorder>>,
    next: u64,
}

impl Recorders {
    fn row(&mut self, word: u64) -> Checked<&mut Recorder> {
        if word & RECORDER == 0 {
            return fail(V::Malformed, "a recorder handle of another kind");
        }
        self.rows
            .get_mut((word & !RECORDER) as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| err(V::Malformed, "a recorder that is closed or of another run"))
    }
}

/// The recorder intrinsic `id` names, if it is one.
pub(super) fn operation(id: &hirsel_core_contract::OperationId) -> Option<&'static str> {
    let path = &id.definition.0;
    if !path.package.name_is("core") {
        return None;
    }
    let name = path
        .modules
        .iter()
        .chain(std::iter::once(&path.name))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(".");
    [
        "$intrinsic.RecorderOpen",
        "$intrinsic.RecorderAppend",
        "$intrinsic.RecorderCount",
        "$intrinsic.RecorderTake",
    ]
    .into_iter()
    .find(|n| *n == name)
}

impl<S: Stage> Machine<'_, S> {
    pub(super) fn recorder_operation(
        &mut self,
        f: &mut Frame,
        site: &CallSite,
        name: &str,
        normal: Option<EdgeId>,
    ) -> Checked<Control> {
        let arguments = self
            .pool(site.arguments, &self.unit(f.unit).pools.call_arguments)?
            .to_vec();
        match name {
            "$intrinsic.RecorderOpen" => {
                let descriptor = self.task_word(f, &Self::task_argument(&arguments, 1)?)?;
                let layout = LayoutId(
                    u32::try_from(descriptor)
                        .map_err(|_| err(V::Malformed, "a recorder descriptor beyond u32"))?,
                );
                self.record(layout)?;
                let ArgumentValue::Value(none) = Self::task_argument(&arguments, 0)?.value else {
                    return fail(V::Malformed, "a None image passed by address");
                };
                let none = match self.read_use(f, none)? {
                    Val::Image(image) => image,
                    _ => return fail(V::TypeConfusion, "a None image that is not an aggregate"),
                };
                if none.len() != self.size(layout)? {
                    return fail(
                        V::TypeConfusion,
                        "a None image of another size than its layout",
                    );
                }
                self.recorders.rows.push(Some(Recorder {
                    layout,
                    none,
                    queue: VecDeque::new(),
                }));
                let word = RECORDER | (self.recorders.rows.len() - 1) as u64;
                if self.ledger.handles.contains_key(&word) {
                    return fail(V::Malformed, "a recorder reopened a handle");
                }
                self.ledger
                    .handles
                    .insert(word, (crate::ledger::HandleState::Open, false));
                let result = site
                    .result
                    .ok_or_else(|| err(V::Malformed, "a recorder open without a result"))?;
                self.task_return(f, &arguments, Some((result, Val::Handle(word))), normal)
            }
            "$intrinsic.RecorderAppend" => {
                let word = self.task_word(f, &Self::task_argument(&arguments, 0)?)?;
                let r = self.task_slot(f, &Self::task_argument(&arguments, 1)?)?;
                let row = self.recorders.row(word)?;
                if row.layout != r.layout {
                    return fail(
                        V::TypeConfusion,
                        "an event slot of another layout than its recorder's",
                    );
                }
                let none = row.none.clone();
                let size = self.size(r.layout)?;
                let owned = self.resources_of(r.layout)?;
                let image = self.take_part(r.ptr, size, &owned);
                let (val, _, credited) = self.val_from(image, ValueRepr::Aggregate, r.layout)?;
                self.write_value(
                    r.ptr,
                    r.layout,
                    Moved {
                        val: Val::Image(none),
                        credited: false,
                        borrow: None,
                    },
                )?;
                let id = self.recorders.next;
                self.recorders.next += 1;
                self.recorders.row(word)?.queue.push_back(id);
                self.set_root(
                    EVENT_ROOT | id,
                    Some(Held {
                        val,
                        credits: u32::from(credited),
                        borrow: None,
                    }),
                );
                self.task_return(f, &arguments, None, normal)
            }
            "$intrinsic.RecorderCount" => {
                let word = self.task_word(f, &Self::task_argument(&arguments, 0)?)?;
                let count = self.recorders.row(word)?.queue.len();
                let result = site
                    .result
                    .ok_or_else(|| err(V::Malformed, "a recorder count without a result"))?;
                let val = Val::Bits {
                    scalar: ScalarKind::U64,
                    bits: count as u128,
                };
                self.task_return(f, &arguments, Some((result, val)), normal)
            }
            "$intrinsic.RecorderTake" => {
                let word = self.task_word(f, &Self::task_argument(&arguments, 0)?)?;
                let r = self.task_slot(f, &Self::task_argument(&arguments, 1)?)?;
                let row = self.recorders.row(word)?;
                if row.layout != r.layout {
                    return fail(
                        V::TypeConfusion,
                        "an event slot of another layout than its recorder's",
                    );
                }
                let id = row
                    .queue
                    .pop_front()
                    .ok_or_else(|| err(V::Malformed, "a take from an empty recorder"))?;
                let held = self
                    .roots
                    .get(&(EVENT_ROOT | id))
                    .cloned()
                    .ok_or_else(|| err(V::Malformed, "a queued event without its root"))?;
                self.set_root(EVENT_ROOT | id, None);
                let size = self.size(r.layout)?;
                let owned = self.resources_of(r.layout)?;
                // The slot holds `None`: plain bits with nothing to release.
                self.take_part(r.ptr, size, &owned);
                self.write_value(
                    r.ptr,
                    r.layout,
                    Moved {
                        val: held.val,
                        credited: held.credits > 0,
                        borrow: None,
                    },
                )?;
                self.task_return(f, &arguments, None, normal)
            }
            _ => unreachable!("a recorder operation"),
        }
    }

    /// Close recorder `word` from its drop; the events it still holds are
    /// dropped by `work`, oldest first.
    pub(super) fn close_recorder(&mut self, word: u64, work: &mut Vec<Work>) -> Checked<()> {
        self.recorders.row(word)?;
        let row = self.recorders.rows[(word & !RECORDER) as usize]
            .take()
            .ok_or_else(|| err(V::DoubleClose, "a recorder closed twice"))?;
        // The work list is a stack: the oldest event's drop goes on last.
        for id in row.queue.into_iter().rev() {
            let held = self
                .roots
                .get(&(EVENT_ROOT | id))
                .cloned()
                .ok_or_else(|| err(V::Malformed, "a queued event without its root"))?;
            self.set_root(EVENT_ROOT | id, None);
            let temp = self.temporary(row.layout)?;
            self.write_value(
                temp,
                row.layout,
                Moved {
                    val: held.val,
                    credited: held.credits > 0,
                    borrow: None,
                },
            )?;
            work.push(Work::Death {
                alloc: temp.alloc,
                raw: None,
            });
            work.push(Work::DropAt {
                at: temp,
                layout: row.layout,
                mask: None,
            });
        }
        Ok(())
    }
}
