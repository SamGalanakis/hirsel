//! One stable row per named process, independent of subscriptions and runs.
use std::collections::BTreeMap;

use hirsel_proto::{ProcessInfo, ProcessState};
use lash_core::{
    ProcessStatus, TriggerDeliveryReservation, TriggerSubscriptionRecord,
    facade_support::ObservedWorkItem,
};

use super::{
    TIMER_SOURCE_TYPE,
    process_bridge::{bound_text, terminal_result, timestamp, trigger_display},
    timers::TimerSchedule,
};

#[derive(Default)]
struct Group<'a> {
    runs: Vec<&'a ObservedWorkItem>,
    subscriptions: Vec<&'a TriggerSubscriptionRecord>,
    deliveries: Vec<&'a TriggerDeliveryReservation>,
}

pub(super) fn recurring(record: &TriggerSubscriptionRecord) -> bool {
    record.source_type != TIMER_SOURCE_TYPE
        || TimerSchedule::from_registration(record)
            .is_ok_and(|schedule| schedule.every_secs.is_some())
}

fn subscription_name(record: &TriggerSubscriptionRecord) -> &str {
    record
        .target_label
        .as_deref()
        .or(record.target_identity.label.as_deref())
        .or(record.name.as_deref())
        .unwrap_or(&record.source_type)
}

fn active(item: &ObservedWorkItem) -> bool {
    matches!(
        item.process.lifecycle,
        ProcessStatus::Running | ProcessStatus::Waiting
    )
}

pub(super) fn process_rows(
    thread_id: u64,
    runs: &[ObservedWorkItem],
    subscriptions: &[TriggerSubscriptionRecord],
    deliveries: &[TriggerDeliveryReservation],
) -> Vec<ProcessInfo> {
    let mut groups: BTreeMap<&str, Group<'_>> = BTreeMap::new();
    for run in runs {
        groups.entry(&run.label).or_default().runs.push(run);
    }
    for subscription in subscriptions.iter().filter(|s| !s.tombstoned) {
        groups
            .entry(subscription_name(subscription))
            .or_default()
            .subscriptions
            .push(subscription);
    }
    for delivery in deliveries {
        // Use the run's name where available: it is the executed process label.
        let name = runs
            .iter()
            .find(|run| run.process.process_id == delivery.process_id)
            .map(|run| run.label.as_str())
            .unwrap_or_else(|| subscription_name(&delivery.subscription));
        if let Some(group) = groups.get_mut(name) {
            group.deliveries.push(delivery);
        }
    }
    groups
        .into_iter()
        .map(|(name, group)| row(thread_id, name, group))
        .collect()
}

fn row(thread_id: u64, name: &str, group: Group<'_>) -> ProcessInfo {
    let newest = |a: &&ObservedWorkItem, b: &&ObservedWorkItem| {
        (a.process.created_at_ms, &a.process.process_id)
            .cmp(&(b.process.created_at_ms, &b.process.process_id))
    };
    let running = group
        .runs
        .iter()
        .copied()
        .filter(|run| active(run))
        .max_by(newest);
    let latest = group.runs.iter().copied().max_by(newest);
    let future = group
        .subscriptions
        .iter()
        .copied()
        .filter(|record| {
            record.enabled
                && !record.tombstoned
                && (recurring(record)
                    || !group.deliveries.iter().any(|delivery| {
                        delivery.subscription.subscription_id == record.subscription_id
                            && delivery.subscription.revision == record.revision
                            && group
                                .runs
                                .iter()
                                .any(|run| run.process.process_id == delivery.process_id)
                    }))
        })
        .max_by_key(|record| (record.updated_at_ms, record.subscription_id.as_str()));
    // Action targets must belong to a live, recurring subscription. One-shots
    // may be waiting but are retired automatically once their delivery exists.
    let actionable = group
        .subscriptions
        .iter()
        .copied()
        .filter(|record| record.enabled && !record.tombstoned && recurring(record))
        .max_by_key(|record| (record.updated_at_ms, record.subscription_id.as_str()));
    let latest_delivery = group
        .deliveries
        .iter()
        .copied()
        .max_by_key(|d| (d.occurrence.occurred_at_ms, d.process_id.as_str()));
    let selected = running.or(latest);
    let provenance = selected.and_then(|run| {
        group
            .deliveries
            .iter()
            .find(|d| d.process_id == run.process.process_id)
            .map(|d| &d.subscription)
    });
    let subscription = future.or(provenance).or_else(|| {
        group
            .subscriptions
            .iter()
            .copied()
            .max_by_key(|s| (s.updated_at_ms, s.subscription_id.as_str()))
    });
    let terminal = group
        .runs
        .iter()
        .copied()
        .filter(|run| !active(run))
        .max_by_key(|run| (run.process.updated_at_ms, run.process.process_id.as_str()));
    let state = if running.is_some() {
        ProcessState::Running
    } else if future.is_some() {
        ProcessState::Waiting
    } else {
        match latest.map(|run| &run.process.lifecycle) {
            Some(ProcessStatus::Completed) => ProcessState::Done,
            Some(ProcessStatus::Failed) => ProcessState::Failed,
            Some(ProcessStatus::Abandoned) => ProcessState::Abandoned,
            Some(ProcessStatus::CallerDeparted) => ProcessState::CallerDeparted,
            _ => ProcessState::Cancelled,
        }
    };
    let started = group
        .runs
        .iter()
        .map(|r| r.process.created_at_ms)
        .chain(group.subscriptions.iter().map(|s| s.created_at_ms))
        .chain(
            group
                .deliveries
                .iter()
                .map(|d| d.subscription.created_at_ms),
        )
        .min()
        .unwrap_or(0);
    let updated = group
        .runs
        .iter()
        .map(|r| r.process.updated_at_ms)
        .chain(group.subscriptions.iter().map(|s| s.updated_at_ms))
        .max()
        .unwrap_or(started);
    ProcessInfo {
        thread_id,
        id: format!("process-name:{thread_id}:{name}"),
        active_process_id: running.map(|run| run.process.process_id.clone()),
        trigger_recurring: actionable.is_some(),
        name: name.into(),
        trigger: subscription.map(trigger_display),
        trigger_subscription_key: actionable.map(|s| s.subscription_key.clone()),
        trigger_revision: actionable.map(|s| s.revision),
        trigger_enabled: subscription.map(|_| future.is_some()),
        cancellable: running.is_some(),
        state,
        started_ts: timestamp(started),
        last_event_ts: timestamp(updated),
        last_fired_ts: latest_delivery.map(|d| timestamp(d.occurrence.occurred_at_ms)),
        last_outcome: terminal
            .and_then(|run| {
                run.events.iter().rev().find(|event| {
                    matches!(
                        event.event_type.as_str(),
                        "process.completed"
                            | "process.failed"
                            | "process.cancelled"
                            | "process.abandoned"
                    )
                })
            })
            .map(|event| {
                bound_text(&crate::storage::process_deliveries::result_body(
                    &terminal_result(&event.payload),
                ))
            }),
    }
}

#[cfg(test)]
pub(super) mod tests;
