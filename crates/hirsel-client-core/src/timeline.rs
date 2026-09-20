use hirsel_proto::TurnEventKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTextKind {
    Prose,
    Reasoning,
}

/// Reduce ordered text events for a native client: chunks from one block are
/// concatenated, while adjacent provider blocks retain a paragraph boundary.
/// Legacy events have no block identity and keep their historical contiguous
/// behavior.
pub fn timeline_text(events: &[TurnEventKind], wanted: TimelineTextKind) -> String {
    let mut blocks: Vec<(Option<&str>, String)> = Vec::new();
    let mut adjacent = false;
    for event in events {
        let current = match (wanted, event) {
            (TimelineTextKind::Prose, TurnEventKind::Prose { text, block_id })
            | (TimelineTextKind::Reasoning, TurnEventKind::Reasoning { text, block_id }) => {
                Some((text.as_str(), block_id.as_deref()))
            }
            _ => None,
        };
        let Some((text, block_id)) = current else {
            adjacent = false;
            continue;
        };
        if adjacent && blocks.last().is_some_and(|(last, _)| *last == block_id) {
            blocks.last_mut().expect("block exists").1.push_str(text);
        } else {
            blocks.push((block_id, text.to_owned()));
        }
        adjacent = true;
    }
    blocks
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_blocks_and_joins_chunks() {
        let events = [
            TurnEventKind::Reasoning {
                text: "**First ".into(),
                block_id: Some("first".into()),
            },
            TurnEventKind::Reasoning {
                text: "thought.**".into(),
                block_id: Some("first".into()),
            },
            TurnEventKind::Reasoning {
                text: "**Second thought.**".into(),
                block_id: Some("second".into()),
            },
        ];
        assert_eq!(
            timeline_text(&events, TimelineTextKind::Reasoning),
            "**First thought.**\n\n**Second thought.**"
        );
    }

    #[test]
    fn legacy_chunks_still_join_and_mixed_kinds_break_runs() {
        let events = [
            TurnEventKind::Prose {
                text: "old ".into(),
                block_id: None,
            },
            TurnEventKind::Prose {
                text: "frame".into(),
                block_id: None,
            },
            TurnEventKind::Reasoning {
                text: "aside".into(),
                block_id: None,
            },
            TurnEventKind::Prose {
                text: "next".into(),
                block_id: None,
            },
        ];
        assert_eq!(
            timeline_text(&events, TimelineTextKind::Prose),
            "old frame\n\nnext"
        );
    }

    #[test]
    fn tool_outcome_interrupts_the_same_reasoning_block_id() {
        let events = [
            TurnEventKind::ToolStart {
                id: "tool-1".into(),
                name: "read".into(),
                summary: None,
                input: None,
            },
            TurnEventKind::Reasoning {
                text: "first".into(),
                block_id: Some("reasoning-1".into()),
            },
            TurnEventKind::ToolDone {
                id: "tool-1".into(),
                name: "read".into(),
                ok: true,
                summary: None,
                result: None,
            },
            TurnEventKind::Reasoning {
                text: "second".into(),
                block_id: Some("reasoning-1".into()),
            },
        ];

        assert_eq!(
            timeline_text(&events, TimelineTextKind::Reasoning),
            "first\n\nsecond"
        );
    }
}
