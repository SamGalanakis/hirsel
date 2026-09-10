//! Durable per-turn timeline events.
use super::Storage;
use hirsel_proto::{ThreadTurnTimeline, TurnEvent, TurnEventKind};
use rusqlite::{Connection, OptionalExtension, params};

fn ensure_turn_owner(c: &Connection, thread_id: u64, turn_id: u64) -> anyhow::Result<()> {
    let owner = c
        .query_row(
            "SELECT thread_id FROM thread_turns WHERE id=?1",
            [turn_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()?;
    anyhow::ensure!(owner == Some(thread_id), "turn belongs to another thread");
    Ok(())
}

fn decode_event(seq: u64, payload: String) -> anyhow::Result<TurnEvent> {
    Ok(TurnEvent {
        seq,
        event: serde_json::from_str(&payload)?,
    })
}

fn append_next(
    c: &Connection,
    thread_id: u64,
    turn_id: u64,
    event: TurnEventKind,
) -> anyhow::Result<TurnEvent> {
    let tx = c.unchecked_transaction()?;
    ensure_turn_owner(&tx, thread_id, turn_id)?;
    let seq = tx.query_row(
        "SELECT COALESCE(MAX(seq)+1,0) FROM thread_turn_events WHERE turn_id=?1",
        [turn_id],
        |row| row.get::<_, u64>(0),
    )?;
    tx.execute(
        "INSERT INTO thread_turn_events(turn_id,seq,event) VALUES(?1,?2,?3)",
        params![turn_id, seq, serde_json::to_string(&event)?],
    )?;
    tx.commit()?;
    Ok(TurnEvent { seq, event })
}

impl Storage {
    /// Append at the store-assigned sequence so every producer shares one
    /// chronological order for a turn.
    pub(crate) async fn append_next_turn_event(
        &self,
        thread_id: u64,
        turn_id: u64,
        event: TurnEventKind,
    ) -> anyhow::Result<TurnEvent> {
        let c = self.conn.lock().await;
        append_next(&c, thread_id, turn_id, event)
    }

    /// Append while the caller retains an execution guard on this connection,
    /// keeping cancellation/reset fenced across the durable commit.
    pub(crate) fn append_next_turn_event_guarded(
        &self,
        guard: &tokio::sync::MutexGuard<'_, Connection>,
        thread_id: u64,
        turn_id: u64,
        event: TurnEventKind,
    ) -> anyhow::Result<TurnEvent> {
        append_next(guard, thread_id, turn_id, event)
    }
}

pub(super) fn for_turns(
    c: &Connection,
    turn_ids: &[u64],
) -> anyhow::Result<Vec<ThreadTurnTimeline>> {
    let mut statement =
        c.prepare("SELECT seq,event FROM thread_turn_events WHERE turn_id=?1 ORDER BY seq")?;
    let mut timelines = Vec::with_capacity(turn_ids.len());
    for &turn_id in turn_ids {
        let rows = statement
            .query_map([turn_id], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let events = rows
            .into_iter()
            .map(|(seq, payload)| decode_event(seq, payload))
            .collect::<anyhow::Result<Vec<_>>>()?;
        timelines.push(ThreadTurnTimeline { turn_id, events });
    }
    Ok(timelines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_proto::{ThreadAttention, TurnEventPayload};
    use serde_json::json;

    #[tokio::test]
    async fn concurrent_producers_share_one_sequence_space() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let thread = storage
            .create_thread(
                "timeline",
                "Timeline",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let turn = storage.start_thread_turn(thread.id, None).await.unwrap();
        let first_store = storage.clone();
        let second_store = storage.clone();
        let (first, second) = tokio::join!(
            first_store.append_next_turn_event(
                thread.id,
                turn.id,
                TurnEventKind::Reasoning {
                    text: "checking".into(),
                },
            ),
            second_store.append_next_turn_event(
                thread.id,
                turn.id,
                TurnEventKind::ToolStart {
                    id: "call-1".into(),
                    name: "shell".into(),
                    summary: Some("true".into()),
                    input: Some(TurnEventPayload {
                        text: r#"{"cmd":"true"}"#.into(),
                        truncated: false,
                    }),
                },
            ),
        );
        let mut sequences = vec![first.unwrap().seq, second.unwrap().seq];
        sequences.sort_unstable();
        assert_eq!(sequences, vec![0, 1]);
        assert!(
            storage
                .append_next_turn_event(
                    thread.id + 1,
                    turn.id,
                    TurnEventKind::Prose {
                        text: "foreign".into(),
                    },
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn open_thread_replays_only_the_timelines_in_each_message_page() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let thread = storage
            .create_thread(
                "pages",
                "Pages",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let history = storage.history_id().await.unwrap();
        let mut expected = Vec::new();
        let mut message_ids = Vec::new();
        for (client_id, text) in [("first", "older"), ("second", "newer")] {
            let message = storage
                .append_thread_owner_message(
                    &history,
                    thread.id,
                    client_id,
                    text,
                    None,
                    &[],
                    &[],
                    &[],
                )
                .await
                .unwrap()
                .0;
            message_ids.push(message.id);
            let turn = storage
                .queue_thread_turn(thread.id, Some(message.id))
                .await
                .unwrap();
            storage.run_thread_turn(turn.id).await.unwrap();
            let event = storage
                .append_next_turn_event(
                    thread.id,
                    turn.id,
                    TurnEventKind::Prose {
                        text: text.to_string(),
                    },
                )
                .await
                .unwrap();
            expected.push((turn.id, event));
            storage
                .finish_thread_turn(turn.id, hirsel_proto::ThreadTurnState::Completed, None)
                .await
                .unwrap();
        }

        let newest = storage.thread_detail(thread.id, None, 1).await.unwrap();
        assert_eq!(newest.messages[0].id, message_ids[1]);
        assert_eq!(
            newest.turn_timelines,
            vec![ThreadTurnTimeline {
                turn_id: expected[1].0,
                events: vec![expected[1].1.clone()],
            }]
        );
        let older = storage
            .thread_detail(thread.id, Some(message_ids[1]), 1)
            .await
            .unwrap();
        assert_eq!(older.messages[0].id, message_ids[0]);
        assert_eq!(
            older.turn_timelines,
            vec![ThreadTurnTimeline {
                turn_id: expected[0].0,
                events: vec![expected[0].1.clone()],
            }]
        );

        drop(storage);
        let reopened = Storage::open(dir.path()).await.unwrap();
        assert_eq!(
            reopened
                .thread_detail(thread.id, None, 1)
                .await
                .unwrap()
                .turn_timelines,
            newest.turn_timelines
        );
    }

    #[tokio::test]
    async fn newest_page_keeps_visible_message_timeline_beside_later_background_turns() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let thread = storage
            .create_thread(
                "mixed-page",
                "Mixed page",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let older_background = storage.start_thread_turn(thread.id, None).await.unwrap();
        storage
            .append_next_turn_event(
                thread.id,
                older_background.id,
                TurnEventKind::Reasoning {
                    text: "older background outside the supplemental bound".into(),
                },
            )
            .await
            .unwrap();
        storage
            .finish_thread_turn(
                older_background.id,
                hirsel_proto::ThreadTurnState::Interrupted,
                None,
            )
            .await
            .unwrap();
        let history = storage.history_id().await.unwrap();
        let owner = storage
            .append_thread_owner_message(
                &history,
                thread.id,
                "owner",
                "Keep this work visible",
                None,
                &[],
                &[],
                &[],
            )
            .await
            .unwrap()
            .0;
        let visible_turn = storage
            .queue_thread_turn(thread.id, Some(owner.id))
            .await
            .unwrap();
        storage.run_thread_turn(visible_turn.id).await.unwrap();
        let visible_event = storage
            .append_next_turn_event(
                thread.id,
                visible_turn.id,
                TurnEventKind::Reasoning {
                    text: "durable visible work".into(),
                },
            )
            .await
            .unwrap();
        let reply = storage
            .materialize_thread_reply(visible_turn.id, "Visible reply", Some(owner.id), vec![])
            .await
            .unwrap();
        storage
            .finish_thread_turn(
                visible_turn.id,
                hirsel_proto::ThreadTurnState::Completed,
                Some(reply.id),
            )
            .await
            .unwrap();

        let mut background_turn_ids = Vec::new();
        for text in ["first background", "second background"] {
            let turn = storage.start_thread_turn(thread.id, None).await.unwrap();
            storage
                .append_next_turn_event(
                    thread.id,
                    turn.id,
                    TurnEventKind::Reasoning { text: text.into() },
                )
                .await
                .unwrap();
            background_turn_ids.push(turn.id);
            storage
                .finish_thread_turn(turn.id, hirsel_proto::ThreadTurnState::Interrupted, None)
                .await
                .unwrap();
        }

        drop(storage);
        let reopened = Storage::open(dir.path()).await.unwrap();
        let detail = reopened.thread_detail(thread.id, None, 2).await.unwrap();

        assert_eq!(
            detail
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![owner.id, reply.id]
        );
        assert_eq!(
            detail
                .turn_timelines
                .iter()
                .map(|timeline| timeline.turn_id)
                .collect::<Vec<_>>(),
            [vec![visible_turn.id], background_turn_ids].concat()
        );
        assert_eq!(detail.turn_timelines[0].events, vec![visible_event]);
        assert_eq!(detail.turn_timelines.len(), 1 + 2);
        assert!(
            detail
                .turn_timelines
                .iter()
                .all(|timeline| timeline.turn_id != older_background.id)
        );
    }

    #[tokio::test]
    async fn interrupted_turn_keeps_partial_timeline_and_reverse_completion_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let thread = storage
            .create_thread(
                "interrupted",
                "Interrupted",
                "",
                &json!({}),
                ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let turn = storage.start_thread_turn(thread.id, None).await.unwrap();
        for event in [
            TurnEventKind::ToolStart {
                id: "A".into(),
                name: "shell".into(),
                summary: None,
                input: None,
            },
            TurnEventKind::ToolStart {
                id: "B".into(),
                name: "shell".into(),
                summary: None,
                input: None,
            },
            TurnEventKind::ToolDone {
                id: "B".into(),
                name: "shell".into(),
                ok: false,
                summary: Some("B failed".into()),
                result: None,
            },
            TurnEventKind::ToolDone {
                id: "A".into(),
                name: "shell".into(),
                ok: true,
                summary: Some("A done".into()),
                result: None,
            },
        ] {
            storage
                .append_next_turn_event(thread.id, turn.id, event)
                .await
                .unwrap();
        }
        let interrupted = storage.interrupt_unfinished_thread_turns().await.unwrap();
        assert_eq!(interrupted.len(), 1);
        let detail = storage.thread_detail(thread.id, None, 100).await.unwrap();
        assert_eq!(
            detail.turns[0].state,
            hirsel_proto::ThreadTurnState::Interrupted
        );
        let events = &detail.turn_timelines[0].events;
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(matches!(&events[2].event, TurnEventKind::ToolDone { id, .. } if id == "B"));
        assert!(matches!(&events[3].event, TurnEventKind::ToolDone { id, .. } if id == "A"));
    }
}
