pub mod custom_names;
pub mod detector;
pub mod parser;
pub mod permissions;
pub mod status;

pub use custom_names::{CustomNames, CustomTitles};
pub use detector::{DetectedSession, SessionDetector};
pub use parser::{
    extract_messages, parse_all_entries, parse_last_n_entries, parse_sessions_index,
    MessageContent, MessageType, SessionEntry, SessionIndexEntry, SessionsIndex,
};
pub use permissions::PermissionChecker;
pub use status::{
    determine_status, determine_status_with_context, get_pending_tool_name, SessionStatus,
};

pub mod history;
pub use history::{deep_search, get_history, DeepSearchHit, HistoryEntry};

pub mod cost;
pub use cost::{get_cost_data, CostData};

pub mod subagents;
pub use subagents::{read_active_subagents, TrackedSubagent};
