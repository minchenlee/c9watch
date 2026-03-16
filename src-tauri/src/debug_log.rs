use chrono::Utc;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

const MAX_ENTRIES: usize = 500;

/// When true, suppress all eprintln! output (used in CLI mode so agents
/// don't get noise on stderr).
static QUIET_MODE: AtomicBool = AtomicBool::new(false);

/// Enable quiet mode — suppresses stderr output from the debug log system.
pub fn set_quiet(quiet: bool) {
    QUIET_MODE.store(quiet, Ordering::Relaxed);
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

static LOG_BUFFER: LazyLock<Mutex<VecDeque<LogEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)));

pub fn debug_log(level: LogLevel, message: &str) {
    let entry = LogEntry {
        timestamp: Utc::now().to_rfc3339(),
        level: level.clone(),
        message: message.to_string(),
    };

    if !QUIET_MODE.load(Ordering::Relaxed) {
        eprintln!("[c9watch][{}] {}", entry.level.as_str(), message);
    }

    let mut buffer = match LOG_BUFFER.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    if buffer.len() == MAX_ENTRIES {
        buffer.pop_front();
    }
    buffer.push_back(entry);
}

pub fn get_logs() -> Vec<LogEntry> {
    let buffer = match LOG_BUFFER.lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    buffer.iter().cloned().collect()
}

pub fn log_info(message: &str) {
    debug_log(LogLevel::Info, message);
}

pub fn log_warn(message: &str) {
    debug_log(LogLevel::Warn, message);
}

pub fn log_error(message: &str) {
    debug_log(LogLevel::Error, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clear the global buffer before each test to avoid cross-test pollution.
    fn clear_buffer() {
        let mut buffer = LOG_BUFFER.lock().unwrap();
        buffer.clear();
    }

    #[test]
    fn test_log_and_retrieve() {
        // Use unique messages to avoid interference from parallel tests
        // sharing the global buffer.
        log_info("test_lar_hello");
        log_error("test_lar_broke");

        let logs = get_logs();
        let hello = logs.iter().find(|l| l.message == "test_lar_hello");
        let broke = logs.iter().find(|l| l.message == "test_lar_broke");

        assert!(hello.is_some(), "expected to find 'test_lar_hello' in logs");
        assert_eq!(hello.unwrap().level, LogLevel::Info);

        assert!(broke.is_some(), "expected to find 'test_lar_broke' in logs");
        assert_eq!(broke.unwrap().level, LogLevel::Error);
    }

    #[test]
    fn test_ring_buffer_capacity() {
        clear_buffer();

        let prefix = "cap_test_";
        for i in 0..(MAX_ENTRIES + 50) {
            log_info(&format!("{}{}", prefix, i));
        }

        let logs = get_logs();
        // Buffer should never exceed MAX_ENTRIES, even with parallel test writes
        assert!(logs.len() <= MAX_ENTRIES);

        // Our prefixed messages should be present and the earliest ones evicted
        let our_msgs: Vec<_> = logs
            .iter()
            .filter(|l| l.message.starts_with(prefix))
            .collect();
        assert!(
            !our_msgs.is_empty(),
            "expected to find cap_test_ messages in logs"
        );
        // The last message we wrote should be present
        assert!(
            our_msgs.iter().any(|l| l.message == format!("{}549", prefix)),
            "expected to find the last written message"
        );
    }
}
