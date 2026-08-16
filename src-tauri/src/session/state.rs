// src-tauri/src/session/state.rs
use super::codex::CodexSessionSource;
use super::source::{DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource};
use super::{create_session_source, mode_from_env, BackendMode};
use crate::session::detector::LegacySessionSource;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const DOWNGRADE_THRESHOLD: u32 = 5;

pub struct DetectorState {
    source: Box<dyn SessionSource>,
    consecutive_failures: u32,
    telemetry_counter: Arc<AtomicU32>,
    mode: BackendMode,
    codex_source: Option<CodexSessionSource>,
}

impl DetectorState {
    pub fn new() -> Self {
        Self {
            source: create_session_source(),
            consecutive_failures: 0,
            telemetry_counter: Arc::new(AtomicU32::new(0)),
            mode: mode_from_env(),
            codex_source: CodexSessionSource::new().ok(),
        }
    }

    pub fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        let claude_result = self.source.detect();
        let codex_result = self.codex_source.as_mut().map(SessionSource::detect);
        match claude_result {
            Ok((mut sessions, diagnostics)) => {
                self.consecutive_failures = 0;
                if let Some(Ok((mut codex_sessions, _))) = codex_result {
                    sessions.append(&mut codex_sessions);
                }
                Ok((sessions, diagnostics))
            }
            Err(e) => {
                self.consecutive_failures += 1;
                if self.should_downgrade() {
                    self.downgrade_to_legacy();
                }
                if let Some(Ok((codex_sessions, diagnostics))) = codex_result {
                    if !codex_sessions.is_empty() {
                        return Ok((codex_sessions, diagnostics));
                    }
                }
                Err(e)
            }
        }
    }

    fn should_downgrade(&self) -> bool {
        self.mode == BackendMode::Auto
            && self.source.backend_name() == "cli"
            && self.consecutive_failures >= DOWNGRADE_THRESHOLD
    }

    fn downgrade_to_legacy(&mut self) {
        self.source = Box::new(LegacySessionSource::new().expect("legacy ctor"));
        self.consecutive_failures = 0;
        self.telemetry_counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn recheck_and_maybe_swap(&mut self) {
        if self.mode != BackendMode::Auto {
            return;
        }
        let supports_cli = super::probe_claude_supports_agents_json();
        let want = if supports_cli { "cli" } else { "legacy" };
        if self.source.backend_name() != want {
            self.source = create_session_source();
            self.consecutive_failures = 0;
            self.telemetry_counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.source.backend_name()
    }

    pub fn telemetry_counter(&self) -> Arc<AtomicU32> {
        self.telemetry_counter.clone()
    }
}

#[cfg(test)]
impl DetectorState {
    /// Test-only constructor that injects an explicit source + mode.
    pub fn for_test(source: Box<dyn SessionSource>, mode: BackendMode) -> Self {
        Self {
            source,
            consecutive_failures: 0,
            telemetry_counter: Arc::new(AtomicU32::new(0)),
            mode,
            codex_source: None,
        }
    }

    /// Test-only helper to replace the underlying source mid-test (e.g. to
    /// schedule a fresh batch of failures via a new FakeSource instance).
    pub fn replace_source(&mut self, src: Box<dyn SessionSource>) {
        self.source = src;
        self.consecutive_failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeSource {
        name: &'static str,
        fail_next: Cell<u32>,
    }

    impl FakeSource {
        fn new(name: &'static str, fails: u32) -> Self {
            Self {
                name,
                fail_next: Cell::new(fails),
            }
        }
    }

    impl SessionSource for FakeSource {
        fn detect(
            &mut self,
        ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
            let n = self.fail_next.get();
            if n > 0 {
                self.fail_next.set(n - 1);
                return Err(SessionDetectorError::CliFailed("fake".into()));
            }
            Ok((Vec::new(), DetectionDiagnostics::default()))
        }
        fn backend_name(&self) -> &'static str {
            self.name
        }
    }

    #[test]
    fn detect_success_resets_counter() {
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 0)), BackendMode::Auto);
        s.consecutive_failures = 3;
        let _ = s.detect().unwrap();
        assert_eq!(s.consecutive_failures, 0);
    }

    #[test]
    fn detect_failure_increments_counter() {
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 1)), BackendMode::Auto);
        let _ = s.detect();
        assert_eq!(s.consecutive_failures, 1);
    }

    #[test]
    fn five_failures_in_auto_cli_mode_swap_to_legacy() {
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 5)), BackendMode::Auto);
        for _ in 0..5 {
            let _ = s.detect();
        }
        assert_eq!(s.backend_name(), "legacy");
        assert_eq!(s.consecutive_failures, 0);
    }

    #[test]
    fn force_cli_mode_never_downgrades() {
        let mut s =
            DetectorState::for_test(Box::new(FakeSource::new("cli", 20)), BackendMode::ForceCli);
        for _ in 0..20 {
            let _ = s.detect();
        }
        assert_eq!(s.backend_name(), "cli");
    }

    #[test]
    fn legacy_backend_failures_do_not_swap() {
        let mut s =
            DetectorState::for_test(Box::new(FakeSource::new("legacy", 10)), BackendMode::Auto);
        for _ in 0..10 {
            let _ = s.detect();
        }
        assert_eq!(s.backend_name(), "legacy");
    }

    #[test]
    fn counter_resets_on_success_between_failures() {
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 3)), BackendMode::Auto);
        for _ in 0..3 {
            let _ = s.detect();
        }
        let _ = s.detect();
        assert_eq!(s.consecutive_failures, 0);
        s.replace_source(Box::new(FakeSource::new("cli", 3)));
        for _ in 0..3 {
            let _ = s.detect();
        }
        assert_eq!(s.backend_name(), "cli");
    }

    #[test]
    fn recheck_no_swap_in_force_modes() {
        let mut s = DetectorState::for_test(
            Box::new(FakeSource::new("cli", 0)),
            BackendMode::ForceLegacy,
        );
        s.recheck_and_maybe_swap();
        assert_eq!(s.backend_name(), "cli");
    }
}
