//! Replay verdict lines and the redacted evidence a divergence appends
//! (docs/recording.md, "Replay" and "Replay of a run with helpers").
use super::*;

/// One stream's verdict line, `replay: VERDICT at N in UNIT`, without its
/// newline (docs/recording.md, "Replay of a run with helpers"). A damaged
/// block is the file's, so it names no unit.
pub(crate) fn format_verdict(fault: Fault, at_corrupt: u64, unit: &str) -> String {
    match fault {
        Fault::Diverged(d) => format!("replay: Diverged({:?}) at {} in {unit}", d.kind, d.at),
        Fault::Truncated(at) => format!("replay: Truncated at {at} in {unit}"),
        Fault::Damaged(block) => format!("replay: Damaged at block {block}"),
        Fault::Corrupt => format!("replay: Corrupt at {at_corrupt} in {unit}"),
        other => format!("replay: {other} in {unit}"),
    }
}

/// Only structural metadata and allowlisted scalar arguments cross the
/// replay diagnostic boundary. Frame payloads may contain credentials.
pub(super) struct FrameEvidence<'a>(pub(super) &'a Frame);

impl std::fmt::Display for FrameEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Frame::Wait { gap, task }
            | Frame::Done { gap, task, .. }
            | Frame::Wake { gap, task }
            | Frame::Cancel { gap, task }
            | Frame::Retired { gap, task, .. } => write!(f, "task={task} gap={gap}"),
            Frame::Op {
                task,
                op,
                object,
                args,
                ..
            }
            | Frame::Issue {
                task,
                op,
                object,
                args,
            } => {
                write!(
                    f,
                    "{}",
                    IssueEvidence {
                        task: *task,
                        op: *op,
                        object: *object,
                        args
                    }
                )
            }
            Frame::Choose { gap, index } => write!(f, "index={index} gap={gap}"),
            Frame::Grant { gap, kind, .. } => write!(f, "kind={kind} gap={gap}"),
            Frame::Repeat { count } => write!(f, "count={count}"),
            Frame::End { gap, ending } => write!(f, "outcome_kind={} gap={gap}", ending.kind),
            Frame::Take { gap, count, .. } => write!(f, "gap={gap} count={count}"),
            Frame::HelperEnd { gap, ending } => {
                write!(f, "outcome_kind={} gap={gap}", ending.kind())
            }
        }
    }
}

impl FrameEvidence<'_> {
    pub(super) fn kind(&self) -> &'static str {
        match self.0 {
            Frame::Wait { .. } => "WAIT",
            Frame::Issue { .. } => "ISSUE",
            Frame::Done { .. } => "DONE",
            Frame::Op { .. } => "OP",
            Frame::Wake { .. } => "WAKE",
            Frame::Cancel { .. } => "CANCEL",
            Frame::Retired { .. } => "RETIRED",
            Frame::Choose { .. } => "CHOOSE",
            Frame::Grant { .. } => "GRANT",
            Frame::Repeat { .. } => "REPEAT",
            Frame::Take { .. } => "TAKE",
            Frame::End { .. } | Frame::HelperEnd { .. } => "END",
        }
    }
}

pub(super) struct IssueEvidence<'a> {
    pub(super) task: u64,
    pub(super) op: u8,
    pub(super) object: u64,
    pub(super) args: &'a Args,
}

impl std::fmt::Display for IssueEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            task,
            op,
            object,
            args,
        } = self;
        write!(f, "task={task} op={op} object={object}")?;
        let field = match *op {
            READ | RANDOM | NEXT_ENTRIES => "capacity",
            SLEEP_UNTIL | SLEEP_UNTIL_WALL => "deadline",
            _ => return f.write_str(" args=redacted"),
        };
        if let Some([spare, _]) = args.recorded_spare() {
            return write!(f, " {field}={spare}");
        }
        match args {
            Args::Literal(bytes) => match bytes.as_slice().try_into() {
                Ok(raw) => write!(f, " {field}={}", u64::from_le_bytes(raw)),
                Err(_) => write!(f, " {field}=invalid({} bytes)", bytes.len()),
            },
            _ => write!(f, " {field}=redacted"),
        }
    }
}
