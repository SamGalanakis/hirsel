use std::collections::HashSet;

use chrono::Utc;
use hirsel_proto::{
    ChatAuthor, Event, EventKind, EventSource, EventSourceKind, HostToClient, Ping, QuickReply,
};

use super::{JudgmentOptionInput, ToolSuite, info_ui};
use crate::text::option_key;

#[derive(Debug, Clone)]
pub(super) struct JudgmentOption {
    pub(super) key: String,
    pub(super) label: String,
    pub(super) detail: String,
    pub(super) recommended: bool,
}

impl ToolSuite {
    /// Authoritative producer seam for an adaptive Task instrument. The
    /// storage boundary validates the constrained UI and preserves the Task's
    /// identity/Anchor; clients cannot call this directly.
    pub async fn events_recompose(
        &self,
        event_id: u64,
        description: Option<String>,
        ui: serde_json::Value,
    ) -> anyhow::Result<Event> {
        let event = self
            .storage
            .recompose_event(event_id, description, ui)
            .await?
            .ok_or_else(|| anyhow::anyhow!("event not found: {event_id}"))?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
    }

    pub async fn pings_send(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Ping> {
        self.pings_send_with_view(
            name,
            description,
            content_md,
            anchor,
            requires_response,
            quick_replies,
            None,
            None,
        )
        .await
    }

    pub async fn events_judgment(
        &self,
        question: impl Into<String>,
        context: impl Into<String>,
        anchor: u64,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let question = question.into();
        let name = judgment_event_name(&question);
        self.events_judgment_named(name, question, context, anchor, options, view, unblocks)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn events_judgment_named(
        &self,
        name: impl Into<String>,
        question: impl Into<String>,
        context: impl Into<String>,
        anchor: u64,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let question = question.into();
        let context = context.into();
        validate_judgment_context(&question, &context)?;
        let options = normalize_judgment_options(options)?;
        let quick_replies = options
            .iter()
            .map(|option| QuickReply {
                value: option.key.clone(),
                label: option.label.clone(),
            })
            .collect();
        let ui = blessed_judgment_ui_from_options(&question, &context, &options, view, unblocks);
        self.create_agent_event(
            EventKind::Judgment,
            name,
            question,
            ui,
            anchor,
            true,
            quick_replies,
        )
        .await
    }

    pub async fn events_notify(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: Option<String>,
        anchor: u64,
    ) -> anyhow::Result<Event> {
        let description = description.into();
        let content = content_md.unwrap_or_else(|| description.clone());
        self.create_agent_event(
            EventKind::Info,
            name,
            description,
            info_ui(&content),
            anchor,
            false,
            Vec::new(),
        )
        .await
    }

    pub async fn events_summary(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: Option<String>,
        ui: Option<serde_json::Value>,
        anchor: u64,
    ) -> anyhow::Result<Event> {
        let ui = match (content_md, ui) {
            (Some(content), None) => summary_ui(&content),
            (None, Some(ui)) => {
                crate::templates::validate(&ui)?;
                ui
            }
            _ => anyhow::bail!("provide exactly one of `content_md` or `ui`"),
        };
        self.create_agent_event(
            EventKind::Summary,
            name,
            description,
            ui,
            anchor,
            false,
            Vec::new(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_agent_event(
        &self,
        kind: EventKind,
        name: impl Into<String>,
        description: impl Into<String>,
        ui: serde_json::Value,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Event> {
        let event = self
            .storage
            .create_event(
                kind,
                EventSource {
                    kind: EventSourceKind::Agent,
                    r#ref: None,
                },
                name,
                description,
                ui,
                anchor,
                requires_response,
                quick_replies,
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        self.pushes.enqueue_event(&event).await;
        Ok(event)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pings_send_with_view(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        let name = name.into();
        let description = description.into();
        let content = content_md.into();
        if requires_response {
            let options = quick_replies
                .into_iter()
                .map(|reply| JudgmentOptionInput {
                    key: None,
                    label: reply.label,
                    detail: reply.value,
                    recommended: false,
                })
                .collect();
            self.events_judgment_named(name, description, content, anchor, options, view, unblocks)
                .await
        } else {
            self.events_notify(name, description, Some(content), anchor)
                .await
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pings_send_with_options(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content_md: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        options: Vec<JudgmentOptionInput>,
        view: Option<serde_json::Value>,
        unblocks: Option<u64>,
    ) -> anyhow::Result<Event> {
        if !requires_response && !options.is_empty() {
            anyhow::bail!("info events cannot carry judgment options");
        }
        if requires_response {
            self.events_judgment_named(
                name,
                description,
                content_md,
                anchor,
                options,
                view,
                unblocks,
            )
            .await
        } else {
            self.events_notify(name, description, Some(content_md.into()), anchor)
                .await
        }
    }

    pub async fn pings_resolve(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let ping = self.storage.resolve_ping(ping_id).await?;
        if let Some(ping) = &ping {
            self.broadcast(HostToClient::EventUpsert {
                event: ping.clone(),
            });
        }
        Ok(ping)
    }

    pub async fn events_archive(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let event = self.storage.archive_event(event_id).await?;
        if let Some(event) = &event {
            self.broadcast(HostToClient::EventUpsert {
                event: event.clone(),
            });
        }
        Ok(event)
    }

    pub async fn events_clear(&self) -> anyhow::Result<usize> {
        let events = self.storage.archive_finished_events().await?;
        for event in &events {
            debug_assert!(!crate::storage::Storage::is_live(event, Utc::now()));
            self.broadcast(HostToClient::EventUpsert {
                event: event.clone(),
            });
        }
        Ok(events.len())
    }

    pub(crate) async fn return_expired_snoozes(&self) -> anyhow::Result<Vec<Event>> {
        let events = self.storage.clear_expired_snoozes(Utc::now()).await?;
        for event in &events {
            self.broadcast(HostToClient::EventUpsert {
                event: event.clone(),
            });
            self.pushes.reenqueue_event(event).await;
        }
        Ok(events)
    }

    pub async fn emit_scheduled_digest(
        &self,
        job_id: impl Into<String>,
        text: impl Into<String>,
        status: impl Into<String>,
    ) -> anyhow::Result<Event> {
        let job_id = job_id.into();
        let text = text.into();
        let status = status.into();
        let anchor = self
            .storage
            .append_chat(
                ChatAuthor::Agent,
                format!("Scheduled lash job `{job_id}` emitted a digest."),
                None,
            )
            .await?
            .id;
        let event = self
            .storage
            .create_event(
                EventKind::Summary,
                EventSource {
                    kind: EventSourceKind::Scheduled,
                    r#ref: Some(job_id),
                },
                "morning-digest",
                "Scheduled fleet digest",
                digest_ui(&text, &status),
                anchor,
                false,
                Vec::new(),
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
    }
}

pub(super) fn blessed_judgment_ui_from_options(
    heading: &str,
    context: &str,
    options: &[JudgmentOption],
    view: Option<serde_json::Value>,
    unblocks: Option<u64>,
) -> serde_json::Value {
    let options = options
        .iter()
        .map(|option| {
            serde_json::json!({
                "key": option.key,
                "label": option.label,
                "detail": option.detail,
                "recommended": option.recommended,
            })
        })
        .collect::<Vec<_>>();
    let mut children = Vec::new();
    // Eyebrow: the boundary accent stripe stays, but the fixed "fleet stopped"
    // copy becomes the human unblocks fact — what deciding actually frees. When a
    // decision unblocks no one, no eyebrow is emitted at all (the heading leads).
    if let Some(unblocks) = unblocks.filter(|&u| u > 0) {
        let agents = if unblocks == 1 { "agent" } else { "agents" };
        children.push(serde_json::json!({
            "type": "eyebrow",
            "tone": "accent",
            "boundary": true,
            "text": format!("Deciding unblocks {unblocks} {agents}")
        }));
    }
    children.push(serde_json::json!({ "type": "heading", "text": heading }));
    if !context.trim().is_empty() {
        children.push(serde_json::json!({ "type": "text", "text": context }));
    }
    children.push(serde_json::json!({ "type": "optionList", "options": options }));
    if let Some(view) = view {
        children.push(serde_json::json!({ "type": "viewSlot", "view": view }));
    }
    serde_json::json!({ "type": "card", "children": children })
}

pub(super) fn normalize_judgment_options(
    options: Vec<JudgmentOptionInput>,
) -> anyhow::Result<Vec<JudgmentOption>> {
    validate_judgment_option_count(options.len())?;
    let recommended_count = options.iter().filter(|option| option.recommended).count();
    if recommended_count > 1 {
        anyhow::bail!("judgment events require exactly one recommended option");
    }

    let mut keys = HashSet::new();
    let mut normalized = Vec::with_capacity(options.len());
    for (index, option) in options.into_iter().enumerate() {
        let key = option.key.unwrap_or_else(|| option_key(index));
        if key.len() != 1 || !key.as_bytes()[0].is_ascii_uppercase() {
            anyhow::bail!("judgment option keys must be one uppercase ASCII letter");
        }
        if !keys.insert(key.clone()) {
            anyhow::bail!("judgment option keys must be unique");
        }
        if option.label.trim().is_empty() || option.detail.trim().is_empty() {
            anyhow::bail!("judgment option labels and details must not be empty");
        }
        normalized.push(JudgmentOption {
            key,
            label: option.label,
            detail: option.detail,
            recommended: option.recommended || (recommended_count == 0 && index == 0),
        });
    }
    Ok(normalized)
}

fn validate_judgment_option_count(count: usize) -> anyhow::Result<()> {
    if !(2..=4).contains(&count) {
        anyhow::bail!("judgment events require 2–4 options");
    }
    Ok(())
}

pub(super) fn validate_judgment_context(heading: &str, context: &str) -> anyhow::Result<()> {
    let heading = normalize_judgment_text(heading);
    let context = normalize_judgment_text(context);
    let heading_tokens = heading.split_whitespace().collect::<HashSet<_>>();
    let context_tokens = context.split_whitespace().collect::<HashSet<_>>();
    let shared_tokens = context_tokens.intersection(&heading_tokens).count();
    let context_is_heading_paraphrase =
        !context_tokens.is_empty() && shared_tokens * 5 >= context_tokens.len() * 4;
    if !context.is_empty()
        && (heading == context
            || heading.starts_with(&context)
            || context.starts_with(&heading)
            || context_is_heading_paraphrase)
    {
        anyhow::bail!(
            "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
        );
    }
    Ok(())
}

fn normalize_judgment_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character.is_whitespace() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn summary_ui(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [{ "type": "text", "text": text }]
    })
}

fn digest_ui(text: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "card",
        "children": [
            { "type": "text", "text": text },
            { "type": "status", "state": "success", "label": status },
            {
                "type": "keyValue",
                "items": [{ "label": "producer", "value": "scheduled lash job" }]
            }
        ]
    })
}

pub(super) fn judgment_event_name(question: &str) -> String {
    const MAX_LEN: usize = 32;
    // A lowercase, hyphen-joined slug of the question's words, truncated on a
    // WORD boundary so the name never ends mid-word: a trailing word that would
    // overflow the cap is dropped whole rather than sliced (the live bug sliced
    // it, producing `…-digestincl`). A single word longer than the cap is
    // hard-clipped so the slug still stays bounded.
    let mut name = String::new();
    for word in question
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
    {
        let projected = if name.is_empty() {
            word.chars().count()
        } else {
            name.chars().count() + 1 + word.chars().count()
        };
        if projected > MAX_LEN {
            if name.is_empty() {
                name = word.chars().take(MAX_LEN).collect();
            }
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&word);
    }
    if name.is_empty() {
        "judgment".to_string()
    } else {
        name
    }
}
