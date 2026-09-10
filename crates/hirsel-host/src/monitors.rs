use std::time::Duration;

use serde::Serialize;

use crate::process_run::run_bash_command;
use crate::storage::{MonitorCondition, MonitorRecord};

const MONITOR_TIMEOUT_SECS: u64 = 60;
const MONITOR_OUTPUT_CAP: usize = 16 * 1024;
const MONITOR_WAKE_TAIL: usize = 4 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorProbeOutput {
    pub status: Option<i32>,
    pub output: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct MonitorTick {
    pub probe: MonitorProbeOutput,
    pub summary: String,
    pub wake: bool,
    pub wake_text: Option<String>,
}

pub async fn run_monitor_tick(record: &MonitorRecord) -> MonitorTick {
    let probe = run_probe(&record.cmd).await;
    let summary = monitor_summary(&probe);
    let wake = monitor_should_wake(record, &probe);
    let wake_text = wake.then(|| {
        format!(
            "Monitor `{}` fired.\n\n{}",
            record.label,
            output_tail(&probe.output, MONITOR_WAKE_TAIL)
        )
    });
    MonitorTick {
        probe,
        summary,
        wake,
        wake_text,
    }
}

async fn run_probe(cmd: &str) -> MonitorProbeOutput {
    match run_bash_command(
        cmd.to_string(),
        None,
        Duration::from_secs(MONITOR_TIMEOUT_SECS),
    )
    .await
    {
        Ok(output) => {
            let mut text = String::new();
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            MonitorProbeOutput {
                status: output.status,
                output: output_tail(&text, MONITOR_OUTPUT_CAP),
                timed_out: output.timed_out,
            }
        }
        Err(error) => MonitorProbeOutput {
            status: None,
            output: output_tail(&error.to_string(), MONITOR_OUTPUT_CAP),
            timed_out: false,
        },
    }
}

fn monitor_should_wake(record: &MonitorRecord, probe: &MonitorProbeOutput) -> bool {
    match &record.condition {
        MonitorCondition::Changed => record
            .last_output
            .as_ref()
            .is_some_and(|previous| previous != &probe.output),
        MonitorCondition::ExitZero => probe.status == Some(0),
        MonitorCondition::ExitNonzero => probe.timed_out || probe.status != Some(0),
        MonitorCondition::Regex(regex) => regex.is_match(&probe.output),
    }
}

fn monitor_summary(probe: &MonitorProbeOutput) -> String {
    let status = if probe.timed_out {
        "timed out".to_string()
    } else {
        probe
            .status
            .map(|status| format!("exit {status}"))
            .unwrap_or_else(|| "no exit status".to_string())
    };
    let tail = output_tail(&probe.output, 180)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if tail.is_empty() {
        status
    } else {
        format!("{status}: {tail}")
    }
}

pub fn output_tail(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[truncated]\n{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn record(condition: MonitorCondition, last_output: Option<&str>) -> MonitorRecord {
        let now = Utc::now();
        MonitorRecord {
            thread_id: 1,
            id: "monitor-1".to_string(),
            cmd: "printf ready".to_string(),
            every_secs: 30,
            condition,
            label: "monitor".to_string(),
            created_ts: now,
            last_event_ts: now,
            last_run_ts: None,
            last_output: last_output.map(str::to_string),
            summary: None,
            cancelled_ts: None,
        }
    }

    fn probe(status: Option<i32>, output: &str, timed_out: bool) -> MonitorProbeOutput {
        MonitorProbeOutput {
            status,
            output: output.to_string(),
            timed_out,
        }
    }

    #[test]
    fn valid_monitor_conditions_preserve_wake_behavior() {
        assert!(!monitor_should_wake(
            &record(MonitorCondition::Changed, None),
            &probe(Some(0), "ready", false)
        ));
        assert!(!monitor_should_wake(
            &record(MonitorCondition::Changed, Some("ready")),
            &probe(Some(0), "ready", false)
        ));
        assert!(monitor_should_wake(
            &record(MonitorCondition::Changed, Some("waiting")),
            &probe(Some(0), "ready", false)
        ));

        assert!(monitor_should_wake(
            &record(MonitorCondition::ExitZero, None),
            &probe(Some(0), "", false)
        ));
        assert!(!monitor_should_wake(
            &record(MonitorCondition::ExitZero, None),
            &probe(Some(1), "", false)
        ));
        assert!(monitor_should_wake(
            &record(MonitorCondition::ExitNonzero, None),
            &probe(Some(1), "", false)
        ));
        assert!(monitor_should_wake(
            &record(MonitorCondition::ExitNonzero, None),
            &probe(None, "", true)
        ));
        assert!(!monitor_should_wake(
            &record(MonitorCondition::ExitNonzero, None),
            &probe(Some(0), "", false)
        ));

        let condition =
            MonitorCondition::parse("regex", Some(r"build (ready|complete)".to_string())).unwrap();
        assert!(monitor_should_wake(
            &record(condition.clone(), None),
            &probe(Some(0), "build ready", false)
        ));
        assert!(!monitor_should_wake(
            &record(condition, None),
            &probe(Some(0), "still building", false)
        ));
    }
}
