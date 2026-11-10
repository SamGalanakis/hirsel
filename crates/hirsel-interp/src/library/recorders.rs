//! MODE-6 recorders: invocation-bound append-only buffers of owned inert events.
//! A recorder value is a handle; its events live here until `recorder.finish`
//! returns them or the handle's drop releases them.
use super::Library;
use super::dispatch::take_arg;
use super::tasks::id;
use crate::{Error, OperationResult, Value, invalid};
use hirsel_types::BuiltinType;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(super) struct Recorders {
    next: u64,
    buffers: BTreeMap<u64, Vec<Value>>,
}
impl Recorders {
    /// The logical bytes of every recorded event.
    pub(super) fn retained_bytes(&self) -> u64 {
        self.buffers
            .values()
            .flatten()
            .map(Value::logical_bytes)
            .fold(0, u64::saturating_add)
    }
}
impl Library {
    pub(super) fn recorder_call(
        &mut self,
        name: &str,
        arguments: &mut [Value],
    ) -> Result<OperationResult, Error> {
        match name {
            "recorder.new" => {
                let r = &mut self.recorders;
                r.next += 1;
                r.buffers.insert(r.next, Vec::new());
                Ok(OperationResult::returned(Value::Handle {
                    kind: "Recorder".into(),
                    id: r.next,
                }))
            }
            "recorder.append" => {
                let recorder = id(super::dispatch::arg(arguments, 0)?, "Recorder")?;
                let event = take_arg(arguments, 1)?;
                self.recorders
                    .buffers
                    .get_mut(&recorder)
                    .ok_or_else(|| invalid("an append to a finished recorder"))?
                    .push(event);
                Ok(OperationResult::Complete {
                    value: Value::Unit,
                    arguments: vec![],
                    consumed: vec![1],
                    released: vec![],
                })
            }
            "recorder.finish" => {
                let recorder = id(super::dispatch::arg(arguments, 0)?, "Recorder")?;
                let events = self
                    .recorders
                    .buffers
                    .remove(&recorder)
                    .ok_or_else(|| invalid("a recorder finished twice"))?;
                Ok(OperationResult::Complete {
                    value: Value::Sequence {
                        kind: BuiltinType::List,
                        elements: events.into_iter().map(Some).collect(),
                        arena: None,
                    },
                    arguments: vec![],
                    consumed: vec![0],
                    released: vec![],
                })
            }
            _ => Err(invalid("unknown recorder operation")),
        }
    }
    /// A dropped recorder's events are released with it.
    pub(super) fn drop_recorder(&mut self, id: u64) {
        if let Some(events) = self.recorders.buffers.remove(&id) {
            self.channels.released.extend(events.into_iter().rev());
        }
    }
}
