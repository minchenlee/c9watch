# macOS notification settings

Implemented on `codex/notification-settings`, based on `origin/main` db460ab. Final QA app: `/Users/liminchen/Applications/c9watch Notification QA.app`.

## Behavior

- Task title plus event, provider, and project name.
- Detailed mode adds bounded assistant text from the current turn, actual question text when available, or an allowlisted command/path description for permission requests. Thinking, tool results, arbitrary tool JSON, and previous-turn replies are excluded. Missing text falls back to event context.
- WaitingForInput is labeled “Reply ready”; it does not assert completion of the entire task.
- Settings: master switch, reply/question/permission filters, brief/detailed content, Glass sound, 0–600 second cooldown, and a test button. The detail options and checkbox states have explicit contrast in the existing dark design.
- Defaults: enabled, all events enabled, detailed, sound off, 30 seconds. QA preferences were restored to these defaults after testing.
- Cooldown uses provider-qualified session identity plus event type. Existing Working-to-waiting/attention detection is preserved.
- Preferences are saved atomically to `notifications.json` in Tauri's app configuration directory. Settings changes require Save; tests use saved detail/sound and bypass master/event/cooldown filters.
- macOS delivery uses one dedicated native thread and a bounded 64-item queue. It does not wait behind async transcript/subagent scans. The process notification identifier is initialized once. Queue saturation/disconnection is returned to the caller; native delivery failures are logged.
- WebSocket notifications, web toasts, and non-macOS delivery retain their prior behavior.
- Test success means accepted into the native delivery queue. macOS permissions and Focus still control presentation.

## Verified acceptance (2026-09-06)

- `npm run check`: zero errors and warnings. `npm run build`: passed.
- GUI Rust library tests: 413 passed, 3 ignored.
- CLI-only `cargo test --no-default-features --features cli`: 387 library tests and 3 integration tests passed; existing ignored tests remain ignored.
- `git diff --check`: passed.
- Native Settings: all six controls changed and saved, then retained across a full app restart. Final bundle loaded those saved preferences. Checked and unchecked controls and both detail options were inspected in the actual app.
- Final bundle: consecutive native test requests returned promptly. Notification Center showed brief text without the preview and detailed text with the preview. System logs confirm Glass played for the brief test and `hasSound: false` for the detailed test.
- Native dispatcher smoke: synthetic Session events entered the real production dispatcher and macOS delivery path. Exactly three unique notifications were delivered. Master-off, event-off and duplicate attempts produced no additional notifications. Question and permission banner text was directly observed in Notification Center.
- Native smoke covers notification dispatch with deterministic session inputs; it is not a claim that every external provider's live session/status detection was revalidated. That detection logic is unchanged.
- The exact QA app passed `codesign --verify --deep --strict`; binary SHA-256 and source hashes are in `notification-validation.json`.

## Evidence and reproduction

- `notification-native-smoke.log`: scoped macOS delivery records for the successful smoke run.
- `notification-native-settings.log`: final QA app's native delivery and sound records.
- `notification-validation.json`: source/artifact fingerprints, counts, and saved preferences.
- `src-tauri/examples/notification_smoke.rs`: opt-in smoke runner, gated behind the GUI feature. It requires `--native-smoke` and expects exactly three native deliveries. Build/package it as a separate macOS app with identifier `com.minchenlee.c9watch.notification-smoke`, register/install that app, then invoke its executable with that flag. Verify the current run's `usernoted` records, not just the runner's queue-acceptance message.

## Build boundary

The initial build ran out of disk space. The successful local QA builds temporarily used rlib-only output to avoid an unnecessary static archive; Cargo.toml was restored afterward. QA used a separate identifier, updater artifacts disabled, and an ad-hoc signature. This is a local validation app, not a notarized public release. The original running c9watch app, its settings, and the original worktree were not replaced; no commit or push was made.

## Settings readability revision (2026-09-06)

- Constrained the content to 1040px with adjacent label/control placement, 15px main labels and 14px explanations. Descriptions use the shared high-contrast `--text-description` token.
- Grouped event choices and detail options; native semantic checkbox/radio inputs retain keyboard focus indication. Master and sound settings use visible switches.
- Desktop uses a settings column and a separate preview/test panel. At 1000px and below, the panel flows below the controls. Native screenshots were inspected at 900x600 and in the zoomed desktop window, including the lower actions after scrolling into view.
- Added explicit saved/unsaved feedback, inline cooldown validation, and the disabled-master explanation. Version/update labels and buttons use larger sans-serif text.
- Rechecked Brief/Detailed preview changes, Save/Test interlock, successful saves and responsive test command completion. Restored detailed mode. npm check/build and ad-hoc bundle signature verification passed. Earlier Rust/native dispatch evidence remains applicable to the unchanged backend; it was not rerun for this UI revision.

## Design system alignment

Settings reuses the Vercel Noir surfaces, borders, spacing scale, and font families in `src/app.css`. Added `--text-description` for readable long help text without changing existing muted text throughout the app. Action labels use Geist Mono; the Settings/Version/Updates section identity retains Geist Pixel. All Settings surfaces and controls use the sharp-corner radius token, including switch tracks/thumbs, detail selection markers, and the notification preview. Event option hover areas fit their label content instead of spanning the column. Native radio/switch semantics and keyboard behavior are preserved.

Event checkboxes now use an inset 8px square on black instead of a typographic checkmark on solid white. Native checkbox semantics and the separate switch styles are preserved.

## Compact settings navigation

Settings now has a 180px left navigation with Notifications and About & updates, following the existing split-panel app layout. The navigation becomes horizontal below 680px. Notification preferences stay mounted while changing sections, retaining unsaved edits. Non-native environments show About & updates only.

Buttons are 32px tall, checkbox/radio markers are 16px, and square switches are 32×18px with 12px thumbs. Event rows retain 32px click targets; detail choices use compact content blocks instead of filling the row. Descriptive text retains the high-contrast token.

## Applying source-derived recipes

Replaced bespoke checkbox/radio/switch graphics with History-style pressed buttons and a Brief/Detailed segmented group. Save remains explicit; explanatory copy states when changes apply. Actions and focus follow Cost/global styles, navigation follows Memory (200px, 8×12px padding, 3%/8% white hover/selection), and subsection headings use Pixel 13px. Neutral white selection replaces Memory's context-specific accent; necessary text uses secondary rather than insufficient-contrast muted text. These are deliberate adaptations, not exact copies of every property. Disabled styling remains a local fallback because the inventory found no consistent shared recipe.

Frontend checks and native bundle build passed. After unlocking the Mac, the source-verified bundle replaced the isolated QA app. Native screenshots at 900x600 and zoomed desktop were inspected; event pressed state, draft retention across navigation, Save/Test interlock, and detail previews were verified. Original preferences were restored and saved. See sourceRecipesRevision in notification-validation.json.
