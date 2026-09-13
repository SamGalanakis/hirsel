use super::*;

// Exercise projection with the actual executed process and durable reservation
// from the timer integration cases, including after the subscription tombstone.
pub(crate) fn assert_folded_process_rows(
    item: &ObservedWorkItem,
    subscription: &TriggerSubscriptionRecord,
    retained: &[TriggerSubscriptionRecord],
    deliveries: &[TriggerDeliveryReservation],
    retired: bool,
) {
    let before = process_rows(7, &[], std::slice::from_ref(subscription), &[]);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].state, ProcessState::Waiting);
    assert!(!before[0].cancellable);
    assert_eq!(before[0].trigger_recurring, !retired);
    let after = process_rows(7, std::slice::from_ref(item), retained, deliveries);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].trigger, before[0].trigger);
    assert_eq!(
        after[0].state,
        if retired {
            ProcessState::Done
        } else {
            ProcessState::Waiting
        }
    );
    assert_eq!(after[0].last_outcome.as_deref(), Some("primitive-ok"));
    assert!(after[0].last_fired_ts.is_some());
    assert!(!after[0].cancellable);
    assert!(after[0].active_process_id.is_none());
    assert_eq!(after[0].trigger_subscription_key.is_some(), !retired);
    // A consumed one-shot cannot produce a phantom waiting row if cleanup was
    // interrupted after the durable reservation and before the tombstone.
    let pending_cleanup = process_rows(
        7,
        std::slice::from_ref(item),
        std::slice::from_ref(subscription),
        deliveries,
    );
    assert_eq!(pending_cleanup, after);

    let mut newer = item.clone();
    newer.process.process_id = "newer-incarnation".into();
    newer.process.created_at_ms += 1000;
    newer.process.updated_at_ms += 1000;
    newer.events.last_mut().unwrap().payload = serde_json::json!("newest result");
    let mut newer_delivery = deliveries[0].clone();
    newer_delivery.process_id = newer.process.process_id.clone();
    newer_delivery.occurrence.occurred_at_ms += 1000;
    let multiple_deliveries = vec![deliveries[0].clone(), newer_delivery];
    let folded = process_rows(
        7,
        &[item.clone(), newer.clone()],
        retained,
        &multiple_deliveries,
    );
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].id, before[0].id);
    assert_eq!(folded[0].last_outcome.as_deref(), Some("newest result"));
    assert!(folded[0].last_fired_ts > after[0].last_fired_ts);

    newer.process.lifecycle = ProcessStatus::Waiting;
    let running = process_rows(
        7,
        &[item.clone(), newer.clone()],
        retained,
        &multiple_deliveries,
    );
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].state, ProcessState::Running);
    assert_eq!(
        running[0].active_process_id.as_deref(),
        Some("newer-incarnation")
    );
    assert!(running[0].cancellable);
    assert_eq!(running[0].last_outcome.as_deref(), Some("primitive-ok"));

    let mut disabled = subscription.clone();
    disabled.enabled = false;
    let stopped = process_rows(7, std::slice::from_ref(item), &[disabled], deliveries);
    assert_eq!(stopped[0].state, ProcessState::Done);
    assert!(!stopped[0].trigger_recurring);
    assert_eq!(stopped[0].trigger_enabled, Some(false));
    assert_ne!(
        process_rows(8, std::slice::from_ref(item), retained, deliveries)[0].id,
        after[0].id
    );

    for (lifecycle, expected) in [
        (ProcessStatus::Failed, ProcessState::Failed),
        (ProcessStatus::Cancelled, ProcessState::Cancelled),
    ] {
        let mut terminal = item.clone();
        terminal.process.lifecycle = lifecycle;
        let rows = process_rows(7, &[terminal], &[], deliveries);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, expected);
        assert!(!rows[0].cancellable);
    }
}
