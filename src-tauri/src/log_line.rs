//! One structured log line on stderr: `elog!` (D10).
//!
//! The format is described in `engine::logging`, which also rotates
//! `engine.log`. This module is always compiled, so the GUI, the CLI and
//! the engine all log the same way.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

/// One log line, without the trailing newline.
pub fn format_line(
    ts: chrono::DateTime<chrono::Utc>,
    level: Level,
    target: &str,
    msg: fmt::Arguments,
) -> String {
    let msg = msg.to_string();
    let mut escaped = String::with_capacity(msg.len());
    for c in msg.chars() {
        match c {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            c => escaped.push(c),
        }
    }
    format!(
        "{} {} {}: {}",
        ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        level.as_str(),
        target,
        escaped
    )
}

/// Write one record to stderr. Use `elog!`.
pub fn write(level: Level, target: &str, msg: fmt::Arguments) {
    eprintln!("{}", format_line(chrono::Utc::now(), level, target, msg));
}

/// `elog!(Info, "run", "Listening on {:?}", path)`: one structured line on
/// stderr (see the module doc).
#[macro_export]
macro_rules! elog {
    ($level:ident, $target:expr, $($arg:tt)*) => {
        $crate::log_line::write(
            $crate::log_line::Level::$level,
            $target,
            format_args!($($arg)*),
        )
    };
}
