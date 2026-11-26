//! Cutover coalescing: a cutover is kept by (OLD, attempt) from its first forward until the
//! host's `CutoverAck`, so an identical re-send is forwarded once and answered with OLD's
//! saved reply.
use super::session::{Attempt, Child, Event, HostCommand, HostReply};
use std::collections::BTreeMap;

/// A cutover forwarded to OLD, kept from first forward until the host's `CutoverAck`.
struct Forwarded {
    command: HostCommand,
    reply: Option<HostReply>,
}

#[derive(Default)]
pub(super) struct Cutovers {
    held: BTreeMap<(Child, Attempt), Forwarded>,
}

/// Where a host command to a child goes.
pub(super) enum Route {
    /// Forward it to the child; a cutover is kept under its key once forwarded.
    Forward(Option<(Child, Attempt)>),
    /// A held cutover answers it, with this event if any.
    Held(Option<Event>),
}

impl Cutovers {
    pub(super) fn route(&self, child: &Child, command: &HostCommand) -> Route {
        let key = match command {
            HostCommand::Cutover(a, ..) => <[u8; 16]>::try_from(a.as_slice()).ok().map(|a| {
                let hi = u64::from_le_bytes(a[..8].try_into().unwrap());
                let lo = u64::from_le_bytes(a[8..].try_into().unwrap());
                (child.clone(), Attempt { hi, lo })
            }),
            _ => None,
        };
        let Some(held) = key.as_ref().and_then(|key| self.held.get(key)) else {
            return Route::Forward(key);
        };
        let (child, attempt) = key.unwrap();
        if held.command != *command {
            return Route::Held(Some(Event::Placed(
                child,
                HostReply::Refused("a different cutover holds this attempt".into()),
            )));
        }
        Route::Held(
            held.reply
                .clone()
                .map(|reply| Event::CutoverResult(child, attempt, reply)),
        )
    }

    pub(super) fn forwarded(&mut self, key: (Child, Attempt), command: &HostCommand) {
        self.held.insert(
            key,
            Forwarded {
                command: command.clone(),
                reply: None,
            },
        );
    }

    /// OLD's next answer after a forwarded cutover is that cutover's; a held-write
    /// acknowledgement is a notice. The event is the cutover's result when `reply` is one.
    pub(super) fn answer(&mut self, child: &Child, reply: &HostReply) -> Option<Event> {
        if matches!(
            reply,
            HostReply::Opened(_) | HostReply::WriteAck(_) | HostReply::StoreMismatch(..)
        ) {
            return None;
        }
        let ((c, a), forwarded) = self
            .held
            .iter_mut()
            .find(|((c, _), f)| c == child && f.reply.is_none())?;
        forwarded.reply = Some(reply.clone());
        Some(Event::CutoverResult(c.clone(), *a, reply.clone()))
    }

    /// OLD ended with a forwarded cutover unanswered: the launcher cannot tell whether it
    /// installed, so its saved reply is Uncertain.
    pub(super) fn exited(&mut self, child: &Child) -> Vec<Event> {
        let mut events = Vec::new();
        for ((c, a), forwarded) in &mut self.held {
            if c == child && forwarded.reply.is_none() {
                forwarded.reply = Some(HostReply::Uncertain);
                events.push(Event::CutoverResult(c.clone(), *a, HostReply::Uncertain));
            }
        }
        events
    }

    pub(super) fn ack(&mut self, child: Child, attempt: Attempt) {
        self.held.remove(&(child, attempt));
    }

    pub(super) fn held(&self) -> Vec<(Child, Attempt)> {
        self.held.keys().cloned().collect()
    }
}
