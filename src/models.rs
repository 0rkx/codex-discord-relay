use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Running,
    NeedsUser,
    Done,
    Failed,
    Idle,
}

impl TaskState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::NeedsUser => "Needs you",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Idle => "Ready",
        }
    }

    #[must_use]
    pub const fn color(self) -> u32 {
        match self {
            Self::Running => 0x5865F2,
            Self::NeedsUser => 0xFEE75C,
            Self::Done => 0x57F287,
            Self::Failed => 0xED4245,
            Self::Idle => 0x95A5A6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMirror {
    pub thread_id: String,
    pub channel_id: Option<u64>,
    pub title: String,
    pub cwd: Option<String>,
    pub state: TaskState,
    pub turn_id: Option<String>,
    /// Per-task model override applied to newly started turns.
    pub model: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}
