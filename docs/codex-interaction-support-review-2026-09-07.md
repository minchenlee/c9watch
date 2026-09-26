# Codex 互動能力取樣與 c9watch 支援建議

日期：2026-09-07。範圍：`codex/codex-send-messaging` worktree。這輪只研究與新增此文件，沒有對使用者 session 發送訊息、批准請求或修改產品功能。

## 結論

下一個產品切片建議是 **待處理互動清單 + 真實等待狀態 + 選項/文字回答**，再接命令與檔案審批。不能把目前 composer 的 turn/start 當成 request_user_input 的回答或 approval decision。

## 本機取樣

從 `~/.codex/sessions/2026/09/01..07` 各日依檔名排序，最多等距取 12 個 rollout，共 68 個檔案，首次掃描約 101.5 MB，無 JSON 解碼錯誤。這是探索性分層取樣，不是使用率統計：包含子代理、automations、測試與本研究 task；活躍檔案會增長。

首次結構掃描：

| 類型 | 記錄數 | 檔案數 | 能證明什麼 |
|---|---:|---:|---|
| request_user_input_async function_call | 6 | 2 | 真實有選項及純文字問題 |
| task_started / task_complete | 437 / 433 | 55 / 55 | 回合起迄有來源；數差不能直接解讀為待處理數 |
| turn_aborted | 3 | 3 | 應區分取消與成功完成 |
| context_compacted | 6 | 5 | 存在上下文壓縮事件 |
| item_completed | 3474 | 未統計 | Desktop 還有結構化工具結果，不能只看 function_call |

後續 item_completed 檢視，排除目前研究 task：1222 個 completed 與 136 個 failed 的 CommandExecution、35 個 completed FileChange。失敗不等於拒絕審批；不要從 exit code 猜使用者決定。

六個問題中，五個含字串選項、一個只有 title；其中還有「是否允許推送/PR/合併」這種高影響選擇。因此預選選項不能自動提交，也不能把所有提問都當低風險偏好。

可定位案例（只存指標，不複製私人完整對話）：

- `~/.codex/sessions/2026/09/05/rollout-2026-09-05T09-40-47-01a06f70-a045-76b1-b4ef-962f65b95462.jsonl:93`：兩個工作範圍選項。
- 同檔 `:245`：推送/PR/合併選項。
- 同檔 `:2200`：CLI/Desktop/兩者三選一。
- `~/.codex/sessions/2026/09/06/rollout-2026-09-06T00-55-28-01a072b6-0c84-7a30-9103-316206731da5.jsonl:2229`：無選項、等待使用者解鎖。

沒有在本批 rollout 的 event type 中看到可直接重建完整 pending approval 的 request/resolved 配對。程式碼字串包含 require_escalated 只能作候選搜尋，可能是測試或文件內容，沒有把它計為真實審批次數。沒有取樣到不代表功能不存在。

## 已安裝協定與目前缺口

本輪執行 Desktop 附帶 binary `--version`：**codex-cli 0.153.4**。用其 `app-server generate-json-schema --experimental` 匯出到 `/tmp/c9watch-codex-schema-review`。不啟動模型或新的 session。

| 能力 | 本機 schema / 現有實作證據 | c9watch 建議 |
|---|---|---|
| 等審批/等回答 | ThreadStatusChangedNotification 的 activeFlags: waitingOnApproval、waitingOnUserInput | Working、Waiting for approval、Waiting for answer、Disconnected 分開；斷線不是 idle |
| 選項與文字回答 | ToolRequestUserInputParams: isBlocking、itemId、threadId、turnId；question: id/header/question/options/isOther/isSecret | 結構化問題卡，多題、選項說明、自填；秘密欄位遮罩且不保存草稿/記錄 |
| 正式回答 | ToolRequestUserInputResponse: answers[questionId].answers[] | 回覆原 JSON-RPC request，不是 turn/start；async transcript 的 title/string options 與 wire schema 不可直接混用 |
| 命令審批 | CommandExecutionRequestApprovalParams/Response | 命令、cwd、reason、權限範圍，依 availableDecisions 顯示批准一次/拒絕/取消；進階授權另行明示 |
| 檔案修改審批 | FileChangeRequestApprovalParams/Response、FileChange changes | 預覽 diff、檔案與 grantRoot，再決定 |
| 權限請求 | PermissionsRequestApprovalParams/Response | 分開顯示 filesystem/network，以及 turn/session 範圍，不把擴權藏在一般 Send 裡 |
| MCP elicitation | McpServerElicitationRequestParams/Response | 第二階段支援表單、外部 URL、accept/decline/cancel；登入等流程交回原介面 |
| 停止工作 | v2/TurnInterruptParams、TurnInterruptResponse | 以 threadId + turnId 明確停止當前回合，確認停止事件後更新狀態 |
| 計畫與變更 | v2/TurnPlanUpdatedNotification、TurnDiffUpdatedNotification | 較低優先：計畫進度與變更摘要，避免使用者只看到工具輸出 |

目前 codex.rs 的問題解析轉為 Markdown，遺失結構化 question id/狀態；CodexMessage/ConversationMessage 沒有互動 request 欄位。apply_event_message 主要以 task_started/task_complete/turn_aborted 更新 Working/Idle。composer 僅以短連線探測及 turn/start 傳送。

本機 ToolRequestUserInputParams 已有 required isBlocking，autoResolutionMs 標示 deprecated；線上文件仍描述 timeout。實作須依本機版本能力，而非只照線上範例寫死。

## Transport 是第一個技術門檻

現有 codex_bridge.rs 轉送 owner 的 JSONL/WS，但沒有 pending-request registry 或 response arbitration。codex_messaging.rs 每次 RPC 短連線，而且僅匹配自己的 response id。

本 repo 舊的隔離測試記錄（docs/codex-desktop-bridge-research-2026-09-05.md）顯示 command approval 送往原 stdio owner，第二條 sender WS 沒收到。這是前輪版本的測試記錄，**本輪沒有重驗，不能保證 0.153.4 或 Desktop native 路由相同**。

下一步先做無模型 mock + 隔離 owner/sender transport 驗證，確認：

1. 誰收到 command/user-input/permission requests；第二連線能否合法回答；不偽裝 owner 身分。
2. 若需要 bridge 代為轉送，必須保留 Desktop 正常接收與回答，明確定義兩邊回答競爭，只提交一次。
3. registry key 至少涵蓋 endpoint generation、threadId、turnId、requestId；不要把 transcript call_id 當 wire requestId。
4. answered/cleared、turn 結束、取消、斷線後失效；舊卡不可繼續送，歷史只讀。重新連線不憑歷史猜 pending，不自動重送批准。
5. 原介面解決後同步移除 c9watch 的待處理卡；傳送 ACK 與請求 resolved 必須分開。
6. 多 request 同時存在、同 request 在兩個 UI 回覆、睡眠喚醒與 daemon 重啟、scope/secret 欄位回歸。

## 建議順序

- **第一階段**：版本化 capability + pending registry；待處理清單與通知；已確認活躍 request 的選項/文字回覆。無 transport 時保留唯讀問題與回原介面提示。
- **第二階段**：命令與檔案批准一次/拒絕/取消，含清楚的 scope 與 diff。沒有通過原 Desktop 審批競爭驗證前不開正式互動按鈕。
- **第三階段**：權限細分、MCP 表單/URL、停止回合、plan/diff 等。

不用先擴張成完整 IDE；先讓使用者能知道哪個 session 真正在等自己，並完成那個具體互動。

## 官方核對

[Codex App Server](https://learn.chatgpt.com/docs/app-server) 的 Approvals、tool/requestUserInput、Permission requests、MCP server elicitation 與 Events 說明確認：這些是 server-initiated request/response 流程，resolved 通知表示已回答或清除。文件也區分 app tool approval 與一般工具執行。詳細欄位以上述本機生成 schema 為版本依據。
