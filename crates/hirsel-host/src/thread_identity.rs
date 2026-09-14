//! Where the agent is.
//!
//! An agent turn runs inside exactly one Thread, but nothing in the tool
//! surface says which one: an agent that was never told found itself guessing
//! whether the Thread it was answering in was a Space at all. This module
//! renders the one plain-text block that opens every agent turn prompt —
//! Native and CLI alike — in the Owner's own vocabulary (Space/Task, `#id`,
//! quoted title), so "the current Space" in an Owner message has a referent.
//!
//! The values come from the same storage read that `threads.context` returns,
//! so the block and the tool can never disagree.

use hirsel_proto::ThreadKind;

/// One Thread named the way the Owner sees it in the UI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ThreadIdentityRef {
    pub id: u64,
    pub kind: ThreadKind,
    pub title: String,
}

impl ThreadIdentityRef {
    /// `Space #2 "lash"` — the single rendering of a Thread's name, shared by
    /// the identity block and the reach summary.
    pub(crate) fn label(&self) -> String {
        format!("{} #{} {:?}", kind_label(self.kind), self.id, self.title)
    }
}

pub(crate) fn kind_label(kind: ThreadKind) -> &'static str {
    match kind {
        ThreadKind::Space => "Space",
        ThreadKind::Task => "Task",
    }
}

/// Everything the identity block states, read at one storage revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadIdentity {
    pub thread: ThreadIdentityRef,
    pub description: String,
    /// Root first, immediate parent last. Empty at top level.
    pub ancestors: Vec<ThreadIdentityRef>,
    /// The ADR-0022 reach summary, already in Owner vocabulary.
    pub reach: String,
}

impl ThreadIdentity {
    /// The block, ending in a newline. Placed above the tool guidance in the
    /// system prompt and above the accepted brief in a CLI turn prompt.
    pub(crate) fn block(&self) -> String {
        let mut block = String::from("## Where you are\n");
        block.push_str(&format!(
            "You are the agent of {}{}.\n",
            self.thread.label(),
            if self.ancestors.is_empty() {
                " (top level)"
            } else {
                ""
            }
        ));
        let description = self.description.trim();
        block.push_str(&format!(
            "Description: {}\n",
            if description.is_empty() {
                "(none yet)".to_string()
            } else {
                // A description is Owner prose and may wrap; continuation lines
                // are indented so the block stays one field per line.
                description.replace('\n', "\n  ")
            }
        ));
        if !self.ancestors.is_empty() {
            block.push_str(&format!(
                "Ancestors: {}\n",
                self.ancestors
                    .iter()
                    .map(ThreadIdentityRef::label)
                    .collect::<Vec<_>>()
                    .join(" › ")
            ));
        }
        block.push_str(&format!("Reach: {}\n", self.reach));
        // The block spells Threads out in full for the agent's own orientation;
        // its prose must not. The interface draws a `#id` citation as the
        // Thread's avatar and title, so an agent that also writes the title
        // beside the id has the surface say the name twice.
        block.push_str(
            "Refer to Threads by `#id` alone; the interface renders the name. \
             Never write the title next to the id.\n",
        );
        block
    }
}
