use super::*;

pub(super) fn render_final_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(other).unwrap_or_default()
        ),
    }
}

pub(super) async fn owner_turn_input(turn: &OwnerTurn) -> anyhow::Result<TurnInput> {
    let mut items = vec![InputItem::text(owner_turn_text(turn))];

    // Image bytes travel inside the item itself now: a turn no longer carries a
    // side table of blobs keyed by id, so the id-and-lookup pair collapses into
    // one inline attachment source.
    for attachment in &turn.attachments {
        let Ok(media_type) = lash::attachments::MediaType::parse(&attachment.blob.mime) else {
            continue;
        };
        if !media_type.is_image() {
            continue;
        }
        let bytes = tokio::fs::read(&attachment.path)
            .await
            .with_context(|| format!("read image attachment {}", attachment.path.display()))?;
        items.push(InputItem::attachment(
            lash::direct::AttachmentSource::inline(media_type, bytes),
        ));
    }

    Ok(TurnInput::items(items))
}

pub(super) fn owner_turn_source_key(client_id: &str) -> String {
    format!("host:{client_id}")
}

pub(super) fn owner_turn_text(turn: &OwnerTurn) -> String {
    let mut text = match turn.anchor {
        Some(anchor) => format!("Owner replied to Ping anchor {anchor}.\n\n{}", turn.body),
        None => turn.body.clone(),
    };
    for attachment in &turn.attachments {
        text.push('\n');
        text.push_str(&format!(
            "[attachment stored at {}: {} ({}, {} bytes)]",
            attachment.path.display(),
            attachment.blob.name,
            attachment.blob.mime,
            attachment.blob.size
        ));
    }
    append_mentioned_ping_context(&mut text, &turn.mentioned_pings);
    if let Some(context) = &turn.task_action {
        let payload = json!({
            "task": {
                "id": context.event.id,
                "name": context.event.name,
                "kind": context.event.kind,
                "source": context.event.source,
                "anchor": context.event.anchor,
                "status": context.event.status,
                "ui": context.event.ui,
            },
            "action": context.action,
            "data": context.data,
        });
        text.push_str("\n\n[authoritative Task action context]\n");
        text.push_str(&serde_json::to_string_pretty(&payload).unwrap_or_default());
        text.push_str("\nRecompose this exact open Task with events.recompose. Preserve its id and Anchor; do not create a nested task or session.");
    }
    text
}

pub(crate) fn append_mentioned_ping_context(text: &mut String, pings: &[Ping]) {
    for ping in pings {
        text.push('\n');
        text.push_str(&format!(
            "[mentioned ping @{} (ping_id {}, {}, requires_response={}, anchor {}): {}]",
            ping.name,
            ping.id,
            match ping.status {
                hirsel_proto::PingStatus::Open => "open",
                hirsel_proto::PingStatus::Done => "done",
            },
            ping.requires_response,
            ping.anchor,
            ping.description
        ));
    }
}

pub(super) fn slow_turn_duration(body: &str) -> anyhow::Result<Option<Duration>> {
    let Some(rest) = body.trim_start().strip_prefix("slow:") else {
        return Ok(None);
    };
    let seconds_text = rest
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("slow turn hook requires seconds after `slow:`"))?;
    let seconds: f64 = seconds_text
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid slow turn seconds `{seconds_text}`: {error}"))?;
    if !(0.0..=600.0).contains(&seconds) {
        anyhow::bail!("slow turn seconds must be between 0 and 600");
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

pub(super) async fn sleep_until_done_or_cancelled(
    duration: Duration,
    cancel: &lash::CancellationToken,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => true,
        () = cancel.cancelled() => false,
    }
}
