// src-tauri/src/session/state.rs
use super::owners::{global_provider_source_owners, ProviderSourceOwners};
use super::source::{DetectedSession, DetectionDiagnostics, SessionDetectorError, SessionSource};
use super::{create_session_source, mode_from_env, BackendMode};
use crate::session::detector::LegacySessionSource;
use crate::session::detector_cli::CliSessionSource;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DOWNGRADE_THRESHOLD: u32 = 5;

/// How often Auto mode, while on the legacy backend, probes whether
/// `claude agents --json` works again. Each probe spawns `claude` twice, so it
/// runs on this interval rather than every poll (#138).
const LEGACY_RECHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

type Clock = Box<dyn Fn() -> Instant + Send>;
type CliProbe = Box<dyn Fn() -> bool + Send>;
type SourceFactory = Box<dyn Fn(bool) -> Box<dyn SessionSource> + Send>;

fn backend_source(supports_cli: bool) -> Box<dyn SessionSource> {
    if supports_cli {
        Box::new(CliSessionSource::new())
    } else {
        Box::new(LegacySessionSource::new().expect("legacy ctor"))
    }
}

pub struct DetectorState {
    source: Box<dyn SessionSource>,
    consecutive_failures: u32,
    telemetry_counter: Arc<AtomicU32>,
    mode: BackendMode,
    provider_sources: ProviderSourceOwners,
    /// When the backend was last chosen: start, downgrade, or recheck.
    last_backend_check: Instant,
    now: Clock,
    probe_cli: CliProbe,
    make_source: SourceFactory,
}

impl DetectorState {
    pub fn new() -> Self {
        Self {
            source: create_session_source(),
            consecutive_failures: 0,
            telemetry_counter: Arc::new(AtomicU32::new(0)),
            mode: mode_from_env(),
            provider_sources: global_provider_source_owners(),
            last_backend_check: Instant::now(),
            now: Box::new(Instant::now),
            probe_cli: Box::new(super::probe_claude_supports_agents_json),
            make_source: Box::new(backend_source),
        }
    }

    pub fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        if self.legacy_recheck_due() {
            self.recheck_and_maybe_swap();
        }
        let claude_result = self.source.detect();
        let codex_result = self.provider_sources.detect_codex();
        let cursor_result = self.provider_sources.detect_cursor();
        let pi_result = self.provider_sources.detect_pi();
        let opencode_sessions = self.provider_sources.detect_opencode();
        match claude_result {
            Ok((mut sessions, diagnostics)) => {
                self.consecutive_failures = 0;
                if let Some((mut extra, _)) = codex_result {
                    sessions.append(&mut extra);
                }
                if let Some((mut extra, _)) = cursor_result {
                    sessions.append(&mut extra);
                }
                if let Some((mut extra, _)) = pi_result {
                    sessions.append(&mut extra);
                }
                sessions.extend(opencode_sessions);
                Ok((sessions, diagnostics))
            }
            Err(e) => {
                self.consecutive_failures += 1;
                if self.should_downgrade() {
                    self.downgrade_to_legacy();
                }
                let mut fallback = opencode_sessions;
                let mut diagnostics = DetectionDiagnostics::default();
                if let Some((sessions, extra)) = codex_result {
                    fallback.extend(sessions);
                    diagnostics = extra;
                }
                if let Some((sessions, extra)) = cursor_result {
                    fallback.extend(sessions);
                    if diagnostics.claude_processes_found == 0 {
                        diagnostics = extra;
                    }
                }
                if let Some((sessions, extra)) = pi_result {
                    fallback.extend(sessions);
                    if diagnostics.claude_processes_found == 0 {
                        diagnostics = extra;
                    }
                }
                if !fallback.is_empty() {
                    return Ok((fallback, diagnostics));
                }
                Err(e)
            }
        }
    }

    /// Auto mode on the legacy backend retries the CLI backend every
    /// `LEGACY_RECHECK_INTERVAL`, so a transient CLI failure doesn't leave the
    /// legacy heuristics in charge until the window next gains focus.
    fn legacy_recheck_due(&self) -> bool {
        self.mode == BackendMode::Auto
            && self.source.backend_name() == "legacy"
            && (self.now)().saturating_duration_since(self.last_backend_check)
                >= LEGACY_RECHECK_INTERVAL
    }

    fn should_downgrade(&self) -> bool {
        self.mode == BackendMode::Auto
            && self.source.backend_name() == "cli"
            && self.consecutive_failures >= DOWNGRADE_THRESHOLD
    }

    fn downgrade_to_legacy(&mut self) {
        self.source = (self.make_source)(false);
        self.consecutive_failures = 0;
        self.last_backend_check = (self.now)();
        self.telemetry_counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn recheck_and_maybe_swap(&mut self) {
        if self.mode != BackendMode::Auto {
            return;
        }
        self.last_backend_check = (self.now)();
        let supports_cli = (self.probe_cli)();
        let want = if supports_cli { "cli" } else { "legacy" };
        if self.source.backend_name() != want {
            self.source = (self.make_source)(supports_cli);
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
            provider_sources: ProviderSourceOwners::from_test_sources(None, None, None),
            last_backend_check: Instant::now(),
            now: Box::new(Instant::now),
            probe_cli: Box::new(|| false),
            make_source: Box::new(|cli| {
                Box::new(TestSource {
                    name: if cli { "cli" } else { "legacy" },
                })
            }),
        }
    }

    /// Test-only: replace the clock, the CLI probe, and the backend factory.
    pub fn with_test_backends(mut self, now: Clock, probe_cli: CliProbe) -> Self {
        self.last_backend_check = now();
        self.now = now;
        self.probe_cli = probe_cli;
        self
    }

    /// Test-only helper to replace the underlying source mid-test (e.g. to
    /// schedule a fresh batch of failures via a new FakeSource instance).
    pub fn replace_source(&mut self, src: Box<dyn SessionSource>) {
        self.source = src;
        self.consecutive_failures = 0;
    }
}

/// Backend the test factory hands out on a swap: always succeeds, empty.
#[cfg(test)]
struct TestSource {
    name: &'static str,
}

#[cfg(test)]
impl SessionSource for TestSource {
    fn detect(
        &mut self,
    ) -> Result<(Vec<DetectedSession>, DetectionDiagnostics), SessionDetectorError> {
        Ok((Vec::new(), DetectionDiagnostics::default()))
    }
    fn backend_name(&self) -> &'static str {
        self.name
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

    /// A fake clock the test advances by hand, and a probe that counts calls.
    fn fake_backends(
        cli_works: bool,
    ) -> (
        std::sync::Arc<std::sync::Mutex<Instant>>,
        Arc<AtomicU32>,
        Clock,
        CliProbe,
    ) {
        let clock = Arc::new(std::sync::Mutex::new(Instant::now()));
        let probes = Arc::new(AtomicU32::new(0));
        let now = {
            let clock = clock.clone();
            Box::new(move || *clock.lock().unwrap()) as Clock
        };
        let probe = {
            let probes = probes.clone();
            Box::new(move || {
                probes.fetch_add(1, Ordering::Relaxed);
                cli_works
            }) as CliProbe
        };
        (clock, probes, now, probe)
    }

    fn advance(clock: &std::sync::Mutex<Instant>, by: Duration) {
        let mut t = clock.lock().unwrap();
        *t += by;
    }

    fn downgraded_state(
        cli_works: bool,
    ) -> (
        DetectorState,
        Arc<std::sync::Mutex<Instant>>,
        Arc<AtomicU32>,
    ) {
        let (clock, probes, now, probe) = fake_backends(cli_works);
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 5)), BackendMode::Auto)
            .with_test_backends(now, probe);
        for _ in 0..5 {
            let _ = s.detect();
        }
        assert_eq!(s.backend_name(), "legacy");
        (s, clock, probes)
    }

    #[test]
    fn legacy_after_downgrade_rechecks_cli_after_interval_and_swaps_back() {
        let (mut s, clock, probes) = downgraded_state(true);

        advance(&clock, LEGACY_RECHECK_INTERVAL);
        let _ = s.detect();

        assert_eq!(probes.load(Ordering::Relaxed), 1);
        assert_eq!(s.backend_name(), "cli");
    }

    #[test]
    fn legacy_does_not_probe_cli_on_every_poll() {
        let (mut s, clock, probes) = downgraded_state(true);

        for _ in 0..10 {
            advance(&clock, Duration::from_secs(20));
            let _ = s.detect();
        }

        assert_eq!(probes.load(Ordering::Relaxed), 0);
        assert_eq!(s.backend_name(), "legacy");
    }

    #[test]
    fn failed_recheck_stays_legacy_and_waits_another_interval() {
        let (mut s, clock, probes) = downgraded_state(false);

        advance(&clock, LEGACY_RECHECK_INTERVAL);
        let _ = s.detect();
        advance(&clock, LEGACY_RECHECK_INTERVAL / 2);
        let _ = s.detect();

        assert_eq!(probes.load(Ordering::Relaxed), 1);
        assert_eq!(s.backend_name(), "legacy");

        advance(&clock, LEGACY_RECHECK_INTERVAL / 2);
        let _ = s.detect();
        assert_eq!(probes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn cli_backend_never_probes_on_detect() {
        let (clock, probes, now, probe) = fake_backends(true);
        let mut s = DetectorState::for_test(Box::new(FakeSource::new("cli", 0)), BackendMode::Auto)
            .with_test_backends(now, probe);

        for _ in 0..3 {
            advance(&clock, LEGACY_RECHECK_INTERVAL);
            let _ = s.detect();
        }

        assert_eq!(probes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn force_legacy_never_probes_on_detect() {
        let (clock, probes, now, probe) = fake_backends(true);
        let mut s = DetectorState::for_test(
            Box::new(FakeSource::new("legacy", 0)),
            BackendMode::ForceLegacy,
        )
        .with_test_backends(now, probe);

        advance(&clock, LEGACY_RECHECK_INTERVAL * 2);
        let _ = s.detect();

        assert_eq!(probes.load(Ordering::Relaxed), 0);
        assert_eq!(s.backend_name(), "legacy");
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
