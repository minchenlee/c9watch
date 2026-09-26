> 實作更新：以下保留研究當時的判斷；目前已完成 Rust bridge、私人 endpoint 探測、Settings 單次啟動入口與生命週期清理。操作方式和最新驗證範圍見 [Codex messaging](codex-messaging-2026-09-05.md)。Desktop 原生端到端驗收仍未完成，功能標示 Preview。

# Codex Desktop bridge：可行性與驗證邊界

## 結論

有具體可驗證的入口，但目前沒有完成 Desktop 原對話端到端驗證。建議把它列為實驗功能，先做「保留 Desktop 原啟動參數的 transport wrapper」spike，再決定是否提供給使用者。不要直接移用 app 私有 MCP pipe。

本輪僅讀取官方文件與已安裝 app 程式碼，另隔離執行 endpoint 選擇函式；未修改 app、使用者設定、登入資料，未重啟 Desktop 或傳送訊息。

以上描述初步研究。後續已完成下述隔離原型測試；仍未接入或重啟真實 Desktop UI。

## 隔離原型實測（後續）

已新增 `scripts/experimental/codex-desktop-bridge.py` 與 `test-desktop-bridge.py`。原型用 Python 3.11+ / websockets，只改 transport，仍執行 Desktop 原附的 Codex 0.153.0。它是測試用 wrapper，**尚不可直接設為 CODEX_CLI_PATH**：目前只接受明確的 app-server 呼叫，還沒有一般 CLI 子命令 passthrough、正式 endpoint discovery 或 Desktop 啟動整合。

測試使用獨立 `/tmp/c9b-*` 工作目錄、新的 ephemeral session 與 gpt-5.4-mini。owner 是我們的 stdio 測試 client，sender 是第二個 Unix WebSocket 測試 client；owner 不是實際 Desktop 視窗，也沒有冒用其 client identity。

| 項目 | 結果與證據 |
|---|---|
| 原參數與設定 | 保留前後 `-c` 參數；config/read 回傳指定模型 |
| framing | 分段 UTF-8、CRLF、U+2028、反引號與 `$()` 保留；EOF 半包拒絕 |
| 雙連線 request ID | 兩個 client 同時使用相同 request ID，各收到自己的回應 |
| 原 session 訊息 | 第二個 client 連續發送兩次，stdio owner 收到同一 session 的兩次正確回覆 |
| 工作中追加 | 兩次 ACK 的 turn ID 相同；owner 收到包含追加 marker 的回覆 |
| 真實審批路由 | 第二個 client 發起 require_escalated 的無害 printf 測試，`item/commandExecution/requestApproval` 出現在 stdio owner；觀察窗口內 sender 沒收到審批 |
| 審批回覆 | owner 回覆 decline，模型回覆 declined，回合以 completed 結束。沒有批准該命令 |
| endpoint 保護 | runtime 目錄 0700、Unix socket 0600 |
| 正常退出 | stdin EOF 後，owned server PID 消失，runtime/socket 清除 |

執行記錄：無模型基本測試初次因 harness 預設 64 KiB readline 上限失敗，調整至 8 MiB 明確上限後通過。`--live` 通過原 session/steering；`--approval` 通過實際審批路由、拒絕及完成。

```sh
# 不呼叫模型：協定、設定、雙連線與清理
python3 scripts/experimental/test-desktop-bridge.py

# 呼叫短模型回合：同 session 的訊息與 steering
python3 scripts/experimental/test-desktop-bridge.py --live

# 呼叫模型：請求無害命令的審批，再由原 owner 拒絕
python3 scripts/experimental/test-desktop-bridge.py --approval
```

原型使用 8 MiB record/message 上限與最多 4 個 WS receive frames，不是長時間記憶體壓力驗證。尚未驗證真實 Desktop 的 attestation、app tools、Native UI 審批、休眠重連、SIGKILL/崩潰復原、一般 CLI 子命令與版本更新。實際 Desktop 的 owner 註冊與審批能力可能不同，不能把本測試視為 Desktop 已可用。

## 新的直接證據

安裝版本：ChatGPT/Codex Desktop `26.901.22334`，build `7746`；內附 Codex CLI `0.153.0`。前輪測試的獨立 CLI 是 `0.152.1`，不可混用版本相容性結論。

讀取 `Contents/Resources/app.asar` 的兩個程式檔：

- `.vite/build/main-7G1VcsUF.js`
- `.vite/build/src-BXVxNf6C.js`，SHA-256 `4c68eec5cc8a1bd1a8bbe70fc7ff325999df5e47b2ed795198af85779be9fe7f`

### 指定 WebSocket endpoint 的內建路徑

`gH(hostConfig)` 讀取 `CODEX_APP_SERVER_WS_URL`，其次是 `hostConfig.websocket_url`。`CODEX_APP_SERVER_FORCE_CLI=1` 會停用這條路徑。

主程序取得 endpoint 後，會建立 WebSocket transport；localhost/127.0.0.1/::1 走直接連線。這條分支不呼叫標準 local stdio 分支的 `getConfigOverrides: () => tk(...)`。

隔離執行實際 `gH` 函式的結果：

| 輸入 | 結果 |
|---|---|
| 沒有環境變數、沒有 host URL | null，走預設 transport |
| `CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:4500` | 選中此 URL |
| 同上，加 `CODEX_APP_SERVER_FORCE_CLI=1` | null |
| 只有 `hostConfig.websocket_url` | 選中 host URL |

這只證明版本內的路由邏輯，不是完整 Desktop 連線測試。官方文件精確搜尋這個環境變數無結果，不將它稱為公開穩定設定。

### 共用本機 daemon 的另一條路徑

也找到 `CODEX_APP_SERVER_USE_LOCAL_DAEMON=1`，會在符合條件時使用 `app-server-control.sock`。條件包含 local host、非 Windows、沒有額外 config overrides、沒有 CLI path/command override、沒有特定 bundled git 路徑，以及 daemon 版本通過檢查（此版門檻 0.141.0）。

目前主程序的 local 分支會用 `tk()` 產生 `mcp_servers.codex_app` override；即使工具缺失，fallback 也會回傳一筆 disabled override。因此沿這條啟動路徑，不能假設只設 USE_LOCAL_DAEMON 就能生效。這也解釋為什麼找得到旗標，仍不能直接宣稱可用。

### 私有 app-tools pipe

`codex-app-tools/server.mjs` 需要 app 提供 `CODEX_APP_TOOLS_PIPE_PATH`，工具呼叫還需要 executor thread metadata。橋接轉送 namespace、tool、arguments、threadId、turnId、callId。

這是 executor 的工具呼叫通道。`send_message_to_thread` 可由 app 內的 agent 使用，不等於 c9watch 有獨立外部 API。沒有偽造 thread/turn metadata、挪用現有 session 的身分，或透過 pipe 發送測試指令。

## 路線比較

| 路線 | 可行性判斷 | 主要缺口 |
|---|---|---|
| Desktop 與 c9watch 共用 WS server | 有本機程式碼直接支持，可做實驗 | 需要重啟/重新連線；WS 分支繞過 local app-tools 啟動 overrides；原 session 不能熱搬移 |
| 保留原參數的 CLI transport wrapper | 建議下一個工程 spike；尚未實作驗證 | stdio/WS 生命週期、審批歸屬、取消與重連需要完整測試 |
| 官方 Remote / SSH | 官方支援 app 連線與原工作續接 | 未找到第三方 Remote client SDK；SSH 是另一個 host 的工作，不是目前 local Desktop session 的熱接入 |
| App 私有 tools pipe | app 內部有能力 | 執行來源與身分由 app/executor 提供，不能當一般外部 API |
| UI 自動輸入 | 不作此功能的主要方案 | 依賴視窗焦點，容易誤投，難以確認 ACK |

## 建議的 wrapper spike

本機程式碼接受 `CODEX_CLI_PATH` 指向可執行檔。研究方向是在使用者明確選擇的測試啟動流程中，讓 Desktop 呼叫我們的 wrapper；wrapper 仍執行該 Desktop 版本原附的 Codex binary，保留原本所有 config、環境、cwd 及 app-tools 設定，只替換 transport。

```text
Desktop 原 JSONL stdin/stdout
             ↕
       transport wrapper
             ↕ Unix WebSocket（私人 0700 目錄／0600 socket）
      原附版本的 app-server ← c9watch 的第二個連線
```

wrapper 需要把 Desktop 的 stdio JSONL 與它自己的 WebSocket 連線互轉。c9watch 使用另一條連線，先確認目標 UUID 已 loaded。不可混用兩個 client 的 JSON-RPC request ID 空間。Desktop 送出的真實初始化/審批資訊照原樣通過，不由 c9watch 合成。

這是設計提案，尚未證明 app 的來源驗證、進程管理或更新機制接受 wrapper。不能為了讓測試通過而停用審批、來源驗證或替換 app bundle。

## 驗收順序

1. 在獨立測試目錄，用 bundled binary 驗證 wrapper 的 JSONL/WS framing、大小上限、半包、斷線、退出與清理；不呼叫模型。
2. 使用獨立測試 session，確認兩個連線看到同一 UUID、idle send 與 active steer，不讀取其他 session 全文。
3. 在可中斷的 Desktop 測試環境啟動 wrapper：確認 Desktop 實際使用目標 server，並保留 app tools、模型、工作目錄、provider 與 permission profile。
4. 用需要審批的無害測試動作，確認請求仍出現在 Desktop；不得因 c9watch 連線而自動批准、被拒絕或無人處理。
5. 驗證 app restart、取消、sleep/wake、同時操作、CLI/app 版本不一致，及 wrapper crash 後可復原。
6. 通過後才研究既有對話：先等原 turn 結束、確認原程序不再使用該 session，再測同一 UUID 續接。不能承諾目前正在執行的 local session 可無縫熱接入。

下一階段可以先完成 1–2，不需中斷目前這個 Desktop 工作。第 3 步需要另外安排可重啟 Desktop 的驗證時機；本輪未執行。

## 官方資料與其支持範圍

- [App Server](https://learn.chatgpt.com/docs/app-server)：支援 JSON-RPC、stdio、Unix WebSocket、turn/start 與 turn/steer；WebSocket transport 仍標示 experimental/unsupported。沒有保證任意第三方 client 能 attach 到 Desktop 原本的 stdio server。
- [Remote connections](https://learn.chatgpt.com/docs/remote-connections)：官方 app 可連至已配對 host 並傳 follow-up、steer、回覆審批；SSH host 由 app 經 SSH 啟動 server。不等於提供 c9watch 可用的公開 Remote client 協定。
