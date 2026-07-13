# Codex monitoring 手動測試指南

這份文件用來驗收 PR [#112](https://github.com/minchenlee/c9watch/pull/112) 的 Codex monitoring 功能。

## 目前可使用的 App 與 binary

目前已經產生可直接啟動的 macOS arm64 debug App：

```text
/private/tmp/c9watch-codex-monitoring-todo/src-tauri/target/debug/bundle/macos/c9watch.app
```

啟動前，先關閉其他正在執行的 c9watch，避免 macOS 同時執行兩個相同 bundle ID 的版本。接著執行：

```bash
open /private/tmp/c9watch-codex-monitoring-todo/src-tauri/target/debug/bundle/macos/c9watch.app
```

也可以先打開輸出資料夾，再從 Finder 雙擊 `c9watch.app`：

```bash
open /private/tmp/c9watch-codex-monitoring-todo/src-tauri/target/debug/bundle/macos
```

App 資訊：

- 版本：`c9watch 0.8.1`
- 架構：macOS arm64
- 功能版本基準：`c895caf`
- App executable SHA-256：`48aca6e4bd59132720374b9a0ee91ad02225dc5d17a70332b72b34f6b27285e7`
- 這是本機 ad-hoc signed debug App，尚未 notarize，也不是正式 release `.dmg`

CLI 與原始 GUI executable 仍位於：

```text
/private/tmp/c9watch-codex-monitoring-todo/src-tauri/target/debug/c9watch
```

正常情況下，不帶參數執行這個 executable 也會開啟 GUI：

```bash
cd /private/tmp/c9watch-codex-monitoring-todo
./src-tauri/target/debug/c9watch
```

如果它只印出 `Monitor and manage Claude Code sessions` 和 CLI Usage，代表 binary 被 `--no-default-features --features cli` 的測試 build 覆蓋。請使用本文件最後的指令重新 build GUI App。

確認 binary 版本：

```bash
./src-tauri/target/debug/c9watch --version
```

查看 c9watch 偵測到的 session 資料：

```bash
./src-tauri/target/debug/c9watch list --pretty
```

## 測試前準備

1. 確認 Codex CLI 可以使用：

   ```bash
   codex --version
   ```

2. 確認本機已有 Codex session 資料：

   ```bash
   ls ~/.codex/sessions
   ```

3. 每次測試使用容易搜尋、而且不重複的文字，例如：

   ```text
   C9WATCH-MANUAL-20260713-01
   ```

4. 如果要測試 provider filter，請同時保留至少一個 Claude Code session 和一個 Codex session。

## 最小驗收流程

如果時間有限，至少完成以下五項：

- [ ] Codex App session 可以出現在 Monitor，並顯示 `CODEX` badge。
- [ ] Codex CLI session 可以出現在 Monitor，而且 CLI JSON 顯示 `surface: cli`。
- [ ] `All | Claude Code | Codex` filter 會真的改變 Monitor、History、Cost 的內容。
- [ ] Resume 同一個 Codex session 後只出現一張卡片，而且完整對話仍可查看。
- [ ] Cost 的 Codex token 有被計入，沒有價格時顯示 `UNPRICED`，不是 `$0`。

## 完整測試步驟

### 1. 啟動與基本檢查

1. 啟動上面的 debug App。
2. 打開主視窗和 menu bar popover。
3. 確認程式沒有立即關閉，也沒有空白畫面。
4. 執行：

   ```bash
   ./src-tauri/target/debug/c9watch status
   ./src-tauri/target/debug/c9watch list --pretty
   ```

預期結果：

- 指令正常回傳 JSON。
- 既有 Claude Code session 仍然可以正常顯示。
- 不應因為加入 Codex monitoring 而破壞原本的 session。

### 2. Codex App session

1. 在 Codex App 建立一個新 session。
2. 輸入包含唯一測試文字的 prompt，例如：

   ```text
   請只回覆 C9WATCH-APP-20260713-01
   ```

3. 回到 c9watch Monitor。

預期結果：

- Session 出現在 Monitor。
- 卡片顯示 `CODEX` badge。
- Project path 和 prompt 內容正確。
- Codex session 不應顯示不支援的 Stop、Rename 或 Open session 操作。
- CLI JSON 的 `provider` 是 `codex`，`surface` 是 `app`。

### 3. Codex CLI session

1. 在另一個 Terminal、測試專案目錄中啟動：

   ```bash
   codex
   ```

2. 輸入唯一測試文字：

   ```text
   請只回覆 C9WATCH-CLI-20260713-01
   ```

3. 執行：

   ```bash
   ./src-tauri/target/debug/c9watch list --pretty
   ```

預期結果：

- Monitor 只出現一張對應的 Codex session 卡片。
- 卡片顯示 `CODEX` badge。
- CLI JSON 顯示：
  - `provider: codex`
  - `surface: cli`
  - `agentKind: root`
  - `canOpen: false`
  - `canStop: false`
  - `canRename: false`

### 4. Provider filter

1. 同時準備至少一個 Claude Code session 和一個 Codex session。
2. 在主視窗依序選擇：
   - `All`
   - `Claude Code`
   - `Codex`
3. 打開 menu bar popover，確認 filter 同步。
4. 依序查看 Monitor、History、Cost。
5. 關閉並重新啟動 c9watch。

預期結果：

- `All` 顯示兩種 provider。
- `Claude Code` 只顯示 Claude Code 資料。
- `Codex` 只顯示 Codex 資料。
- Filter 會同步到 popover。
- 切換 tab 後 filter 仍然生效，不只是 badge 改變。
- 重啟後仍保留最後選擇的 filter。

### 5. Codex subagent grouping

1. 在 Codex session 中要求它建立一個 subagent，例如：

   ```text
   請啟動一個 subagent，讓它檢查目前資料夾有哪些 Markdown 文件，完成後回報數量。
   ```

2. 在 subagent 執行期間查看 Monitor 和 popover。

預期結果：

- 正常 subagent 顯示在 parent session 的 Subagents 區域。
- Nested subagent 不會消失；它會被整理到可見的 root session 下。
- 如果 parent 暫時不存在，仍在執行的 orphan subagent 不會完全消失。
- Codex 內部的 guardian、review 或其他 internal helper 不會變成獨立卡片。

### 6. Resume 同一個 Codex session

這項測試是 PR review comment 的主要回歸測試。

1. 在 Codex CLI 建立 session，輸入：

   ```text
   請記住 C9WATCH-RESUME-FIRST-20260713-01
   ```

2. 結束 Codex CLI。
3. 立即 resume 最近的 session：

   ```bash
   codex resume --last "請回覆 C9WATCH-RESUME-SECOND-20260713-01"
   ```

   如果 `--last` 可能選到別的 session，可以使用 c9watch CLI 找到 session ID，再執行：

   ```bash
   codex resume <SESSION_ID> "請回覆 C9WATCH-RESUME-SECOND-20260713-01"
   ```

4. 查看 Monitor。
5. 點開 session 的 conversation。

預期結果：

- 同一個 session ID 只出現一張卡片。
- 卡片使用最新 rollout 的狀態，不會被舊的 idle 狀態蓋掉。
- Conversation 同時包含 `FIRST` 和 `SECOND` 兩段內容。
- 重複的 rollout message 不會重複顯示。

### 7. History 與搜尋

1. 完成一個 Codex App 或 CLI session。
2. 前往 History。
3. 選擇 `Codex` filter。
4. 搜尋前面使用的唯一測試文字。
5. 打開搜尋結果的完整 conversation。

預期結果：

- History 會出現 Codex session。
- Metadata search 和 deep search 都可以找到 Codex 內容。
- Claude Code 資料不會在 `Codex` filter 下出現。
- Resumed session 的所有 conversation fragments 都可以看到。
- Internal helper 不會出現在 History。

### 8. Cost 與 token

1. 前往 Cost。
2. 選擇 `Codex` filter。
3. 切換 USD 和 TOKENS 圖表。
4. 再切回 `All`，查看混合 provider 的資料。

預期結果：

- Codex token 會被計入 token 總數和圖表。
- Codex-only 日期不會從 token 圖表消失。
- 沒有 Codex 定價時顯示 `UNPRICED`，不能顯示成 `$0`。
- 混合資料會分開表示已知 USD cost 和 unpriced Codex tokens。
- Provider filter 會重新計算資料，不只是隱藏畫面上的 label。

## 測試結果記錄

可以複製下面的格式記錄：

```text
日期：
macOS 版本：
Codex 版本：
c9watch binary SHA-256：

[ ] 1. 啟動與基本檢查
[ ] 2. Codex App session
[ ] 3. Codex CLI session
[ ] 4. Provider filter
[ ] 5. Codex subagent grouping
[ ] 6. Resume 同一個 session
[ ] 7. History 與搜尋
[ ] 8. Cost 與 token

問題與備註：
```

## 發現問題時要保留什麼

請保留：

- 問題畫面的 screenshot。
- 測試使用的唯一文字。
- Codex session ID。
- `codex --version` 和 `c9watch --version` 的輸出。
- c9watch 啟動 Terminal 中的 log。
- 以下指令的輸出：

  ```bash
  ./src-tauri/target/debug/c9watch list --pretty
  find ~/.codex/sessions -name "*<SESSION_ID>.jsonl" -print
  ```

Rollout file 可能包含私人 prompt、路徑或 tool output。不要直接把完整檔案公開貼到 GitHub；先移除敏感內容。

## 重新 build App

如果目前的 `/private/tmp` worktree 已被刪除，可以在 feature branch 重新 build：

```bash
git switch feature/codex-monitoring
npm install
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_STRIP=none CARGO_INCREMENTAL=0 \
  npm run tauri build -- --debug --bundles app \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

Debug App 會在：

```text
src-tauri/target/debug/bundle/macos/c9watch.app
```

GUI 與 CLI 共用的 executable 會在：

```text
src-tauri/target/debug/c9watch
```

如果需要正式的 `.app` 和 `.dmg`：

```bash
npm run tauri build
```

輸出位置：

```text
src-tauri/target/release/bundle/macos/c9watch.app
src-tauri/target/release/bundle/dmg/
```

目前這個 worktree 已有 debug `.app`，但尚未產生 release `.app` 或 `.dmg`。手動測試請使用上面的 debug App。
