# Codex 原 session 訊息傳送

分支：`codex/codex-send-messaging`，基底 `91f995f`。

## 已實作

Codex 的 Monitor session detail 底部新增文字輸入框。先確認指定 UUID 已載入本機 CLI daemon 或由 c9watch 啟動的 Desktop bridge，再允許傳送。閒置時開始一個 turn；工作中追加 instruction（steer），不是等待整個工作結束的 follow-up queue。

用 Unix WebSocket 探測私人 Desktop bridge socket 與 `~/.codex/app-server-control/app-server-control.sock`，Host 為 localhost。同一 UUID 出現在多個 server 時拒絕傳送。依序使用 `initialize`、`initialized`、`thread/loaded/list`、`turn/start`。每次 send 都重新確認 loaded membership。不啟動 daemon、不呼叫 resume、不覆寫 cwd/model/approval/sandbox，也不碰 Codex transcript。

介面用 c9watch 既有 Noir tokens、方塊狀態標記。支援多行及 ⌘/Ctrl Enter（排除 IME composition）、每 session 草稿、防重複送出。ACK 只顯示 Accepted，不宣稱完成。傳送結果不明會保留草稿，必須確認原對話後才能再送。工具執行審批仍在原 Codex 介面進行。

資源有界：32 KiB 訊息、20 個草稿、最多 4 個同時傳送、1 MiB WebSocket frame/message、每 endpoint 探測 3 秒，最多 8 個 Desktop endpoints、投遞 ACK 10 秒。只在開啟 composer、手動 recheck 或 send 連線，不隨 Monitor poll 建立連線，不累積 transcript。

## 明確限制

- **Codex Desktop 支援已實作為 Preview。** Settings 的 Launch with support 以單次啟動環境指定 c9watch Rust bridge，保留原附 Codex binary、參數、環境、cwd 與 app-tools 設定。原本已開啟的 Desktop 必須先結束工作並退出；不能熱接入既有 stdio server。實際 Desktop UI、來源驗證與 app-tools 的端到端驗收仍待操作確認。
- 未找到已確認支援第三方 app 的 Desktop 傳送入口；沒有將 app 私有 MCP pipe 或其他 session 的權限憑證拿來當產品 transport。
- 支援私人本機 Desktop bridge 與 default home 的本機 Unix daemon，尚未支援自訂 CODEX_HOME、遠端主機、Web companion、歷史頁 composer 或非圖片附件。
- 不支援投遞完成後由 c9watch 獨立追蹤回合結果；目前依既有對話輪詢看回覆，Accepted 文案提示回原介面檢查審批及結果。
- 尚未完成 native UI 操作與長時間 RSS 驗收，不把編譯/transport 測試當成這兩項驗收。

## 驗證證據

- `npm run check`：0 errors、0 warnings。
- `npm run build`：通過。
- `cargo test --lib codex_`：47 passed、2 ignored，涵蓋橋接參數、endpoint 權限與 stale marker、重複 server 拒絕及 Codex archive 回歸。
- `node --conditions=browser --test scripts/test-codex-composer.mjs scripts/test-conversation-selection.mjs`：5 passed。涵蓋雙擊、切换 session 後遲到的 ACK、unknown delivery、IPC 失敗與既有輪詢回歸。
- 另外明確執行 ignored `live_codex_same_session`：通過。使用全新 ephemeral、read-only、gpt-5.4-mini 測試 session，連續兩次原 session 正確回覆；另一次工作中追加指令的 ACK 保持相同 turn ID，最終回覆包含追加指定的 marker。
- 首次預設模型實測被服務端拒絕：CLI 0.152.1 的 gpt-6-astra 需要更新版本。改用 daemon model/list 列出的 gpt-5.4-mini 僅限新測試 session，未更改使用者設定。
- Rust build 有既有 dead_code warnings；不宣稱全專案無 warnings。

## 本機操作驗證

1. 開啟此分支打包的 c9watch app。
2. 若本機 daemon 未啟動，在終端機執行 `codex app-server daemon start`。再用 `codex --remote unix:// -m gpt-5.4-mini` 開一個專門的測試 session（此模型用於避開此機舊 CLI 的 Astra 限制）。
3. 先從 CLI 傳一則無工具的測試問題，確認帳號、模型與原介面正常。保持 CLI 開啟。
4. 在 c9watch 找到同一個 session，展開 detail；Message Codex 應可輸入。輸入「請只回覆 C9WATCH_OK，不要使用工具」，按 Send。
5. 確認原 CLI session 收到訊息與回覆；c9watch 不應另增一個 session。再於工作中追加一則指令，確認原工作收到。
6. 多行輸入、中文 IME、快速雙擊、切換其他 session，再回來檢查草稿與狀態不串台。未透過支援模式啟動的 Desktop session 應看到不可連線原因，且不能送出。

整合測試會呼叫模型，只有需要重驗 transport 時才執行：

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib \
  codex_messaging::tests::live_codex_same_session -- --ignored --nocapture
```

## Desktop 操作與復原

1. 本輪 release app 已加入 Settings → CODEX DESKTOP MESSAGING → LAUNCH WITH SUPPORT；session composer 的 DESKTOP SUPPORT 也可進入。
2. 等 Codex 工作結束，完整退出 Codex / ChatGPT，再從 c9watch 點上述按鈕。若 app 還在執行，按鈕會回報原因，不會強制中斷工作。
3. 在 Codex 開啟要操作的對話，回 c9watch 展開同一 session，點 RECHECK，再傳「請只回覆 C9WATCH_OK，不要使用工具」。核對同一 UUID 收到訊息與回覆。
4. 審批應留在原 Codex；需要另驗 app-tools、取消、sleep/wake 與正常重啟。這些不能由隔離 server 的測試取代。
5. 要停用，退出 Codex，再照平常方式開啟即可。沒有修改 Codex 設定、app bundle、認證或 transcript。每次需要支援時從 c9watch 啟動。

Bridge 為隨 app 打包的 Rust 程式；Python 只用於獨立測試。每個 server 使用 0700 私人目錄和 0600 socket；8 MiB 訊息上限、背壓、EOF/訊號清理及獨立 reaper 避免 bridge 遭 SIGKILL 後留下 agent。傳輸不保存對話副本，也沒有固定輪詢連線。

Release binary 的隔離驗證命令：

```sh
python3 scripts/experimental/test-desktop-bridge.py --rust-binary src-tauri/target/release/c9watch --kill-bridge
python3 scripts/experimental/test-desktop-bridge.py --rust-binary src-tauri/target/release/c9watch --live --approval
```

測試前段 fragmented UTF-8/CRLF 檢查針對原 Python prototype；後段使用指定 Rust release binary 與 Desktop 原附 0.153.0 server。`--live` / `--approval` 使用新建 ephemeral、read-only、gpt-5.4-mini session，會呼叫模型；不更改使用者既有對話設定。

本輪 release 驗證結果：兩則訊息與 active steer 均留在相同 session／turn；原 stdio owner 收到 command approval 並明確拒絕後正常完成。stdin EOF 與 SIGKILL 均清理自己的 server 和 endpoint。這些是隔離 transport 的證據，尚非 Codex Desktop 視窗內的審批驗收。App bundle 已以 `--no-sign` 本機打包，不代表簽章或公版發行驗收。

## 圖片附件與 preview 更新

Composer 支援輸入框貼上圖片、加號選取檔案、縮圖移除與純圖片傳送。PNG / JPEG / WebP，每則最多四張、解碼後合計 4 MiB；所有 session 的圖片草稿 data URL 合計上限 16 MiB。接受 ACK 才清空附件，未知投遞保留附件且禁止自動重送。後端再次檢查 MIME、檔頭、base64 與大小，以原生 `image` input 的 data URL 傳送，不建立暫存圖檔。訊息連線 frame/message 上限更新為 8 MiB。

開啟的 Monitor preview 在每次讀取完成後隔兩秒刷新，關閉後停止；在底部跟隨新的訊息，閱讀歷史時保留位置。Desktop runtime context 不再當成使用者訊息；附件自動生成的 Files mentioned 前言移除，保留 My request。Archive cache version 7 使舊快取重新套用分類。

本輪 `cargo test --lib codex` 82 passed / 2 ignored；前端型別檢查無錯誤或警告，9 項前端回歸測試通過。原生貼上與檔案選擇互動仍需使用者操作確認。

## 2026-09-07 Review fixes

- 草稿上限仍為 20 個 session；只回收空白且非 pending／unknown 的草稿。滿額時停用新 session 的輸入及附件，提示清空其他草稿，不再默默刪除未送出的文字或圖片。
- 後端驗證、並行限制及連線探測階段失敗回傳 `not_sent` receipt；前端保留草稿並允許手動重試。送出後無法取得 ACK 或 IPC 本身失敗仍保留 unknown 防重送流程。
- 圖片路徑先檢查普通檔案，Unix 使用 `O_NONBLOCK` 開啟後再檢查 descriptor，避免 FIFO 或開啟前替換造成對話載入無限等待。
- 新增草稿滿額及圖片保留、明確未送出後重試、後端輸入驗證 receipt，以及 FIFO 無 writer 的限時回歸測試。

## 2026-09-07 Waiting 與問題回答

沿用 Monitor 的 WaitingForInput 狀態。Native frontend 用每兩秒一次、有界的私人 control socket snapshot，將目前 bridge 的 pending requests 疊加到 Codex session；不修改 transcript，也不對模型 server 增加固定連線輪詢。卡片顯示等待原因/數量，session detail 的輸入框上方列出待處理卡片。

- 支援 wire `item/tool/requestUserInput` 的單選、協定允許的 Other、純文字及多題集中提交。不預選、不點選即送、不用 turn/start 代替回答。
- 原 owner 與 c9watch 共用 bridge 的單一送出序列；先提交的一方取得該 request，另一方的重複回答被拒絕或攔下。c9watch 回覆使用原 JSON-RPC ID；等 `serverRequest/resolved` 或 turn 完成/中止才清除卡片。
- request token 是 bridge 當次產生的 UUID。endpoint、thread、turn、token 隔離；舊連線不復活舊卡，不從歷史推測 pending。
- 命令/檔案/權限審批及 MCP elicitation 只顯示原因，回原 Codex 操作。私密問題也留在 Codex，c9watch 不接收秘密答案。
- 斷線保留停用的舊卡，可手動 dismiss；恢復同一 endpoint 時以新 snapshot 取代。IPC 結果不明保留答案並禁止重送。
- 資源限制：每 bridge 最多 64 個 pending、每請求 8 KiB、512 個 thread 狀態、8 個同時控制客戶端、8 個排隊操作、每次讀取 3 秒、每則答案 32 KiB；最多 8 個 bridge 探測與 16 個含離線卡片的 frontend endpoint。為保留反重送 tombstone，每個 bridge 最多接受 1024 次 c9watch 回答，其後回原 Codex，直到正常重啟 bridge。

### 啟用邊界

必須以**新版 c9watch** 的 Launch with support 啟動 Codex，讓該次 Desktop 使用新版 bridge；既有已啟動的 bridge 不會熱升級。沒有 interactions.sock 的舊 bridge、一般 CLI daemon 和未以 support 啟動的 Desktop，不會有可回答卡片。即時狀態只有等待旗標而缺少 request 時，顯示回原 Codex 查看，不編造問題。

### 本輪驗證

- Svelte check 0 errors / 0 warnings；前端互動、composer、selection 回歸 16 項通過。
- 完整 Rust lib 測試 427 passed / 4 ignored。Registry 回歸 5 項通過：多題原 ID、兩方先回答競爭、錯誤答案、原端已處理、turn 取消、審批/秘密欄位只讀、容量限制與等待旗標。
- `python3 scripts/experimental/test-codex-interactions.py src-tauri/target/debug/c9watch`：實際 Rust binary + 無模型假 server，驗證 owner/control sockets、多題 payload、兩種先後順序、原介面收到 resolved、取消、EOF 清理。
- `python3 scripts/experimental/test-desktop-bridge.py --rust-binary src-tauri/target/debug/c9watch --kill-bridge`：已安裝 Codex 0.153.4 的無模型相容性測試與 SIGKILL 清理通過。sandbox 內首次因 OS Operation not permitted 失敗；獲准在 sandbox 外執行隔離測試後通過。未執行 --live/--approval，沒有模型呼叫或使用者審批操作。
- Debug app 已打包。Native UI 驗證首次被 macOS 鎖定畫面擋住，仍待解鎖；不把上述測試當成 Desktop 真實提問端到端驗收。

原生 fixture（不連線 Codex、不寫 transcript），可用獨立 QA bundle 驗證顯示與提交：

```sh
python3 scripts/experimental/codex-interaction-ui-fixture.py <visible-codex-thread-uuid>
```

所有卡片明確標示 QA fixture；退出 fixture 即關閉該次私人 sockets。它只能證明 native UI/IPC，不能取代真實 Desktop request 路由驗收。


## 2026-09-07 審批、MCP 與回合控制

本節取代前述「命令/檔案/權限審批及 MCP 只讀」限制。仍需新版 bridge；既有 bridge 不會熱更新。沿用 Waiting 與同一份待處理清單。

- 命令：顯示 command、cwd、原因及 network context，按 server 提供的決定支援批准一次、拒絕、取消。不提供 session-wide 或 policy amendment 授權。
- 檔案：顯示每個路徑、操作及完整 diff；須明確勾選已檢閱才可批准。缺少完整 diff、未知操作或 grantRoot 的持續範圍授權交回 Codex。提案變更會更新 token，舊 review 不能沿用。
- 權限：網路與檔案範圍逐項選取，預設不授權；後端只回傳原 request 的已選子集合，保留 deny 限制，scope 固定 turn。拒絕回傳空權限。未知權限 schema 交回 Codex。
- MCP：一般平面表單支援文字、數字、整數、boolean、單選/多選與必要欄位、長度、數值限制。後端重驗型別與範圍。复杂或未知 schema 不接受回答，交回 Codex。URL 模式獨立呈現，僅使用者點擊才開啟 http/https 連結，明確確認外部步驟完成後才提交。
- 回合：顯示即時 plan、步驟狀態、解釋與最新 turn diff。摘要有界並標示截斷，不當成檔案審批。停止只送到觀察到的目前 thread/turn；ACK 只表示停止已請求，完成事件才改成停止/完成。
- 原介面與 c9watch 共用單次回答序列，沿用原 JSON-RPC ID。未知投遞禁止自動重送；斷線卡停用。每個 pending request 上限調為 32 KiB，control snapshot 上限 8 MiB；保留最多 32 個 turn progress、64 個 item，plan 最多 64 步、diff preview 最多 8192 字元。

驗證：Svelte check 0 errors / 0 warnings；前端回歸 19 passed；Rust lib 434 passed / 4 ignored。實際 Rust bridge + 假 server 的無模型整合測試涵蓋全部決定 payload、原 ID、重複決定抑制、精確 turn 停止、plan/diff 與 EOF 清理。Debug app 本機打包；Mac 鎖定使 native UI QA 仍待完成，不宣稱真實 Desktop 審批端到端驗收。fixture 已加入各類審批/表單及回合控制，僅操作臨時資料。

### Native QA follow-up (2026-09-07 evening)

在獨立 `com.minchenlee.c9watch.interactions-qa` app 與臨時 fixture 上完成選項/文字回答、命令 approve once，並核對 fixture 的原生 IPC payload。AX 驗證檔案審批未檢閱時 disabled、勾選後 enabled；權限可只選 read 而 network 保持未選；MCP 必填驗證接受 0/false，選項可清除。URL 完成按鈕預設 disabled，plan 可見。沒有執行真實命令、授予權限或提交外部表單。

後續提交卡住。macOS sample 顯示多個 Tokio worker 同時執行 `get_subagents → all_subagents_by_session` 的同步 transcript 掃描，導致 interaction IPC 與 timeout 得不到執行。修正為 spawn_blocking，frontend refresh 加入單一 in-flight guard，避免 sessions 更新/interval 重疊排隊。新增慢掃描/失敗後恢復回歸；前端共 20 passed，Svelte check 0 errors/warnings，debug app 重建成功。

重啟修正版時 Mac 再次鎖定，因此檔案/權限/MCP 提交、URL 操作、停止回合及修正後流暢度的 native 驗收仍未完成。fixture 已停止並清除 sockets，臨時證據位於 `/tmp/c9watch-codex-501/322abc2a-9f5a-4b27-b705-f218e6126a78`；stack sample 位於 `/tmp/c9watch-interactions-qa-sample.txt`。

### Native QA retest completed (2026-09-07 23:09)

解鎖後以修正版獨立 QA bundle 重測通過：問題回答、命令 decline、檔案 review 後 accept、只選 read:0 權限、MCP 表單含 count=0/ok=false/mode=local 均經原生 UI 提交並清除卡片，沒有再次停在 submitting。外部步驟按鈕在 Zen 開啟 example.com，確認目的地後關閉測試分頁；開啟本身未提交，勾選完成後才接受。停止 fixture 回合後 AX 顯示 Stopped、移除停止按鈕及 Waiting，計畫與展開 diff 保留。截圖檢查原生版面正常。

本次 fixture 證據 `/tmp/c9watch-codex-501/fff3fdb8-6a91-43c8-9035-cf7bc1933798/decisions.jsonl` 核對 reviewed=true、selected=[read:0]、表單原始型別及 completed=true。fixture 已停止、sockets 清除。本項完成隔離 native UI/IPC 驗證；仍不等同真實 Codex Desktop approval/model 或外部 MCP 服務端到端驗收。沒有操作真實命令、權限或模型回合。

### Recheck feedback (2026-09-08)

Native reproduction: RECHECK completed but returned the same generic unavailable text, appearing inert. Read-only thread/loaded/list confirmed the target acceptance task was not loaded while the Desktop bridge itself was healthy. Resolution now distinguishes a reachable server with an unloaded task from no reachable matching server; unsuccessful checks show their completion time. Recheck remains read-only and never resumes a thread. Messaging tests: 7 passed / 1 ignored; Svelte check: 0 errors/warnings. No real task was resumed or sent a message during this diagnosis.

### Edited paginated task history (2026-09-08)

Real acceptance task kept its thread UUID while edit/resubmit created `rollout-...-<thread>_<rollout>.jsonl`. The new session_meta contains history_base.end_ordinal_exclusive; the original file remains on disk. Conversation discovery previously matched only `-<thread>.jsonl`, leaving the preview on the abandoned segment. Filename discovery/indexing now recognizes both formats. For revision histories, the reader applies later ordinal cutoffs to earlier segments before projecting messages, preserving logical order and excluding rolled-back tails. It does not merge withdrawn content by timestamp. Incomplete final JSONL records remain deferred.

Regression covers original + edit and a second edit cutting further back. Full Rust lib: 435 passed / 5 ignored. Read-only real-task diagnostic loaded 14 messages through the latest 2026-09-08T01:12:08 response (previous UI showed 8 messages from the original file). Optional rerun: `C9WATCH_QA_THREAD=<uuid> cargo test --manifest-path src-tauri/Cargo.toml --lib local_edited_conversation_diagnostic -- --ignored --nocapture`. No task input, answer or approval was submitted by this diagnostic.

Native QA of the rebuilt independent bundle confirmed Respond to greeting shows the edited 00:46/00:47 continuation and the latest 08:11 user / 08:12 assistant messages; the superseded 00:39/00:40 tail is absent. The QA app was closed afterward. The separately scoped first-load performance change is not included in this messaging build.

### Chained edited-history correction (2026-09-08)

A second Desktop edit can reference the previous rollout UUID in `history_base.thread_id`, rather than the root task UUID. Resolve that parent only among segments whose metadata matches the requested task, follow the selected rollout's ancestry, apply ordinal cutoffs, and exclude abandoned sibling edits. Missing parents and cycles fail explicitly. The conversation UI now shows scoped load errors and clears them on successful retry instead of remaining on Loading.

Validation: chained-edit and abandoned-tail regression; real task `01a07a11-f12e-7571-98f6-33d3f4fe6ce0` loaded 112 messages in a read-only diagnostic (0.17s); Rust 435 passed, 5 ignored; frontend 21 passed; Svelte check clean. The separate cold-start archive-lock performance fix remains on `codex/first-session-load`.
Native follow-up: rebuilt and relaunched the messaging app bundle. The target task reached 114 messages with 28 navigation entries; selecting its earlier question-report entry visibly displayed the user message and attached screenshot. Cold first-open delay remains separate from the corrected history-chain rejection.

### Composer turn controls (2026-09-08)

Moved Running/Waiting status into the composer heading as a labeled icon. Empty active drafts show Stop; text or attachments reveal Send alongside a smaller Stop. Completed turns no longer add a separate row. Plan/diff content and pending approvals remain above the composer. Stop receipts remain keyed to endpoint/task/turn across reopening, disable duplicate delivery, and preserve unknown outcomes without automatic retry.

Validation: Svelte check clean; 23 frontend tests passed, including duplicate stop, exact turn identity, late receipts and unavailable controls. Rebuilt/relaunched the native bundle; verified a real waiting approval remains visible, and running empty/draft composer layouts show the expected stop-only / send-plus-stop controls. Preview draft was cleared without sending or interrupting the actual turn.

### Approval terminology (2026-09-08)

Approval actions now consistently use APPROVE / REJECT / CANCEL, with ONCE and SELECTED · THIS TURN retaining explicit scope. MCP requests with no schema fields are labeled MCP APPROVAL / APPROVE; forms with fields use MCP FORM / SUBMIT ANSWERS, while URL workflows retain explicit completion confirmation. Protocol actions and validation remain unchanged. Svelte check passed without warnings.

### Computer Use approval blocking (2026-09-08)

A connected endpoint retaining `statuses[thread] = notLoaded` was incorrectly counted as a second owner, disabling a live request on another endpoint. Shared thread membership now excludes unloaded entries unless an actual pending request remains; the card, composer and answer/decision guards use the same rule. Genuine duplicate live owners remain blocked. If a request changes between render and click, the approval card now explains that nothing was sent and refreshes instead of silently returning.

Validation: 25 frontend tests pass, including unloaded-owner, genuine duplicate-owner, exact endpoint and stale-click cases; Svelte check clean. No live failing Computer Use request remained during diagnosis, so the reported incident itself is not yet reproduced end-to-end. MCP schema handling and permission scope were not broadened.
