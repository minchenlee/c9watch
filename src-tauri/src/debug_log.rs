use chrono::Utc;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

const MAX_ENTRIES: usize = 500;

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

    eprintln!("[c9watch][{}] {}", entry.level.as_str(), message);

    let mut buffer = LOG_BUFFER.lock().unwrap();
    if buffer.len() == MAX_ENTRIES {
        buffer.pop_front();
    }
    buffer.push_back(entry);
}

pub fn get_logs() -> Vec<LogEntry> {
    let buffer = LOG_BUFFER.lock().unwrap();
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
        clear_buffer();

        log_info("hello world");
        log_error("something broke");

        let logs = get_logs();
        assert_eq!(logs.len(), 2);

        assert_eq!(logs[0].level, LogLevel::Info);
        assert_eq!(logs[0].message, "hello world");

        assert_eq!(logs[1].level, LogLevel::Error);
        assert_eq!(logs[1].message, "something broke");
    }

    #[test]
    fn test_ring_buffer_capacity() {
        clear_buffer();

        for i in 0..(MAX_ENTRIES + 50) {
            log_info(&format!("msg {}", i));
        }

        let logs = get_logs();
        assert_eq!(logs.len(), MAX_ENTRIES);

        // The oldest entry should be message #50 (the first 50 were evicted).
        assert_eq!(logs[0].message, "msg 50");
    }
}
