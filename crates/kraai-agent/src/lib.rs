#![forbid(unsafe_code)]

mod compaction;
mod context_state;
mod manager;
mod profiles;
mod request_usage;
pub use request_usage::{AuxiliaryRequestUsage, AuxiliaryUsageRecorder};
mod skills;

pub use compaction::{CompactionOutcome, ContextCompaction};
pub use skills::discover_skill_read_roots;

pub use manager::{
    AgentManager, CancelledStreamResult, PendingStreamRequest, ScriptTurnContext,
    SessionContextUsage, SessionSnapshotData, SessionSnapshotReader,
};
