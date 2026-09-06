# c9watch design system 盤點

狀態：現況盤點與待決定規格，**不是已核准的新 design system**。

## 範圍與證據

- 工作目錄：`/Users/liminchen/Documents/GitHub/c9watch-notification-settings`；checkpoint `0531978`，分支 `codex/notification-settings`。這份文件在 checkpoint 之後新增。
- 基準：`db460ab` 的既有 Monitor／History／Cost／Memory、ProviderBadge、ProviderFilter 與 app.css；本次檢查的六個上述 component 檔案與基準相同。app.css 的差異只有這次 Settings 新增的 `--text-description`。
- 原始 checkout 目前是另一條含未提交改動的 `codex/agent-messaging-research`。初步讀取後改以隔離 worktree 為主要證據，不把另一條分支的新實作視為規範。
- 螢幕依據：使用者提供的 Memory、Settings 圖片，以及前一輪實際 QA 的標準／放大 Settings 視窗。本輪未重跑各頁 live QA，不宣稱完成全 app 視覺或無障礙稽核。
- 所列尺寸是 CSS px，不能直接拿 Retina 圖片的實體像素比較。
- [現況元件對照](./design-system-inventory.html) 是靜態摘錄樣本；它沒有接入 app，也不是完整元件實作。

## 1. 目前真正穩定的視覺語言

**黑底、低彩度表面、細線分隔、緊湊的操作層，加上按角色分工的三種字體。** 不是「所有東西都小、都方、都 Mono」。

| 特徵 | 現況證據 | 判定 |
|---|---|---|
| 真黑 canvas、多層近黑背景 | [全域 tokens](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/app.css:65) | 明確 token 規則 |
| 1px 分隔線與輪廓 | [Memory 頁首](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:202)、[Memory 左側清單](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:285)、[Monitor card](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionCard.svelte:354) | 跨頁重複模式 |
| 頁名 Pixel uppercase 22px；計數 18px | [Memory 頁首](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:202)、[History 頁首](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:755)、Cost 頁首 | 最一致的字體層級 |
| Mono 用於導航、路徑、金額、metadata | [Memory 左側清單](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:285)、[Provider badge](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/ProviderBadge.svelte:23)、Cost | 穩定用途，但尺寸未統一 |
| Sans 用於 session 標題與閱讀內容 | Monitor card 15px、Memory Markdown 15px | 正文不應一律改成 Mono |
| 色彩表示 provider／狀態 | [Provider badge](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/ProviderBadge.svelte:23)、app.css status tokens | 語意導向，不是裝飾底色 |
| 密集操作、較寬鬆閱讀區 | Memory 左側 12px 與右側 15px Markdown | 密度應分層，不應整頁等比放大縮小 |
| 銳角方向 | 全部 radius tokens 為 0；使用者明確要求 Settings 無圓形 | 設計方向成立，但現有實作有例外 |

## 2. Foundation tokens

### 色彩

| 用途 | Token | 值 | 注意 |
|---|---|---|---|
| Canvas | bg-base | #000000 | 主背景 |
| 微升高表面 | bg-elevated | #0a0a0a | 操作／浮層底色 |
| Card | bg-card | #111111 | 有內容邊界的表面 |
| Hover | bg-card-hover | #1a1a1a | 不應自動套在整列所有空間 |
| 主文字 | text-primary | #ffffff | 主要資訊、選取與 focus |
| 次文字 | text-secondary | #888888 | 次要資訊 |
| 弱文字 | text-muted | #666666 | 很多現有說明也使用它，對比不足 |
| 說明文字（新） | text-description | #aaaaaa | 僅本次 Settings 引入，尚非全 app 已接受標準 |
| 一般／弱邊框 | border-default / border-muted | #333333 / #222222 | 分隔與表面輪廓 |
| Focus | border-focus | #ffffff | 與選取狀態需要區分 |

Provider：Claude 橘 `#ff6600`、Codex 藍 `#0070f3`、Cursor 紫 `#7928ca`、Pi 綠 `#00ff88`。同一批色值也用於 permission／working／input 狀態，**同色不代表相同語意**；必須搭配文字／圖示。

靜態 sRGB 對比計算（沒有計入 opacity、混色或圖片；不是全 app WCAG 判定）：

| 文字 | 黑底 | #111 card | #1a1a1a hover |
|---|---:|---:|---:|
| #fff | 21.00 | 18.88 | 17.40 |
| #aaa | 9.04 | 8.13 | 7.49 |
| #888 | 5.92 | 5.33 | 4.91 |
| #666 | 3.66 | 3.29 | 3.03 |

一般小字以 4.5:1 為檢查門檻時，`text-muted` 不適合必要說明。視覺上「低調」與「難讀」要分開決定。

### 字體與尺寸

| 角色 | 現況 | 證據／例外 |
|---|---|---|
| 頁面主標題 | Pixel 22px、600、uppercase、tracking .1em | Memory／History／Cost |
| 頁名旁計數 | Pixel 18px | 同上 |
| 頂部 tab | Pixel 10px、uppercase，active 白色下底線 | [App 頂部 tabs](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/routes/(app)/+page.svelte:893) |
| 子導覽 | Mono 12px | Memory project-item |
| Metadata／paths | Mono 約 10–13px | 各頁不完全一致 |
| Card 標題 | Sans 15px、600 | Monitor |
| 長文 | Sans 15px；Markdown heading 20／17／15px | Memory |
| Toolbar action | Pixel 10–11px 或 Mono 10–12px | Cost／Memory／ProviderFilter 並存 |
| Provider badge | Mono 8px，compact 7px | 太小的風險，不能直接當新表單的字級 |

### 間距、幾何、動態

- Spacing scale：4／8／12／16／24／32／48px。常見列 padding 8×12、card padding 16、內容 pane padding 16。沒有正式 density variant。
- Radius：sm 至 2xl 全為 0。局部仍硬編碼 1／2／3／4／6px，表示規範與實作尚未收斂。
- Shadow tokens 多為 none 或 1px outline；Monitor hover 另外使用 `0 4px 12px` shadow。
- Transition tokens：100／150／250ms linear，名為 spring 的 token 實際為 200ms linear。實際元件另有 150ms ease、200ms cubic-bezier、350ms flyIn。尚未形成统一 motion 規則。
- 全域 focus 是 1px white outline、offset 0；Settings 是 2px、offset 4。兩者不是同一規格。
- 全域隱藏 scrollbar；這是目前實作，不代表已驗證可發現性良好。

## 3. Layout 模式

| Surface | 現況 | 可保留的結構 |
|---|---|---|
| App shell | 頂部小 tab、主內容頁名＋橫線 | 統一的頁面骨架 |
| Monitor | Session card／清單與狀態區 | 可掃描、多個平行物件 |
| History | 搜尋／篩選＋結果列表 | 查找與閱讀的層級 |
| Cost | 篩選、摘要、圖表／資料 | 操作層較密，資料層依內容調整 |
| Memory | 200px 左 pane、min 160px；右側內容；1px divider | 最清楚的 navigation/content 分離範例 |
| Settings checkpoint | 180px 導覽、右側 max 960px；1180px 以下預覽移下方；680px 以下導覽橫排 | 只是待評估方案，不能反過來定義 DS |

Memory item 不是一個大型表單 option：它的全列 hover 有導航用途。因此不能把「左側導航全寬 hover」照抄到短 checkbox label。

## 4. 元件現況與缺口

| 元件 | 已有做法 | 不一致／尚未定義 |
|---|---|---|
| Button | 小字、薄框／無框、hover 變亮；[Cost scale 控制項](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/CostTracker.svelte:1083) | Pixel 或 Mono、字重、大小寫、padding、disabled 規則各自寫；沒有共用 Button 元件 |
| Tabs／導航 | 頂 tab 下底線；Memory 選取背景＋側線；ProviderFilter 小背景塊 | 選取標示沒有明確分成不同導航層 |
| Checkbox | 主要在本次 NotificationSettings 新增 | 沒找到可作為既有標準的共用 checkbox；方塊填色是新方案 |
| Toggle | NotificationSettings 方形軌道／滑塊 | 將原生 switch 改方形不是既有 c9watch 元件規範；是否真的需要獨立 toggle 尚未決定 |
| Radio／detail choice | NotificationSettings 有框選項＋內標記 | 與 checkbox 外型相近但語意不同；是否採 segmented choice 待決定 |
| Provider badge | 共用 [Provider badge](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/ProviderBadge.svelte:23) | 2px 圓角、7–8px 字與全域銳角／可讀性方向衝突 |
| Provider filter | 共用 [Provider filter](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/ProviderFilter.svelte:141) | 2–4px radius，7–9px 字，另有自身 focus 行為 |
| Card | [Monitor card](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionCard.svelte:354) | 全域 minimal-shadow 與 card hover shadow 不一致 |
| Readable text | Sans 正文＋Mono 技術資訊 | muted 小字濫用、字級下限沒有規則 |

## 5. 明確衝突清單

1. **「無圓形／銳角」是目前使用者要求與 token 方向，不是 repo 每一處已做到。** Memory command／provider-note 有 6px，badge 2px，filter 2–4px。先標記，不在盤點中自動全改。
2. **按鈕沒有單一字體。** 上一輪把 Settings 全換 Mono 並不能宣稱完成一致化；需要按 action 層級決定 Pixel／Mono 的用途。
3. **沒有統一 size scale。** 既有 7–11px 細字不是表單應全面繼承的尺度；本次 32px button／16px checkbox 也是候選值，不是已核准標準。
4. **對比不足是既有系統問題。** 新 Settings 增亮說明是合理補救，但不應悄悄把新 token 當成全 app 已定稿方案。
5. **幾何一致不代表操作一致。** checkbox、radio、toggle 全變方塊後更容易混淆；必須同時定義選取數量、即時生效或 Save 才生效。
6. **Settings 目前明確按 Save 才生效，但 toggle 通常給人即時開關的暗示。** 這個互動語意比圓或方更值得先決定。
7. **Focus、hover、selected 沒有共用規格。** Settings 的大 offset focus 可以比縮小後的元件還搶眼；不能只看靜態 default screenshot。
8. **原生 preview 不必等於 app 元件。** 之前保留原生圓角、後來全部改方，是沒有事先定義「示意圖／真實系統預覽」的結果。

## 6. 在下一次改 UI 前，建議先決定的規格

以下為提案，尚未實作或定稿：

| 優先 | 決策 | 建議起點 |
|---|---|---|
| P0 | 控制項語意 | 若繼續使用 Save，偏好 checkbox＋明確選項；若採 toggle，要定義是否即時保存 |
| P0 | 密度分層 | Compact 導航／toolbar 與 Reading 正文分開，不再整頁縮放 |
| P0 | 字體角色 | Pixel 留給頁名／短區段識別；Mono 導航／technical/action；Sans 閱讀／表單 label。先用樣本驗證 action 的 Mono 是否合適 |
| P0 | 狀態矩陣 | 每種 control 同時展示 default、hover、focus、selected、disabled、pending／error |
| P1 | 可讀性 | 次文字用 #888 或 #aaa 的哪一層、最小字級；必要內容不要用 #666 小字 |
| P1 | 幾何例外 | UI 銳角；badge／code／系統預覽是否允許例外，需要具體清單 |
| P1 | 左側導覽 | 以 Memory 200px 為比較基準；Settings 180px 是差異，不先宣布標準 |
| P2 | 實作收斂 | 核准後才抽 Button、SettingRow、ChoiceGroup、SectionNav、FieldHelp 等元件 |

## 7. 對這次 Settings 反覆修改的結論

問題不是只差幾個 px，而是先畫了表單再逐項套外觀：大 label row、卡片式 radio、原生 switch、全寬 hover 來自不同模式；之後只改字體、radius、尺寸，沒有同步處理密度與互動語意。

下一步應先做一張**可核准的 control/state 對照板**，選定 density、type roles 和 Save 語意，再修改 Settings。現有 checkpoint 只作為可回復工作版本，不以它作為設計基準。

本輪交付僅盤點文件與靜態對照板；沒有修改任何 app UI、沒有把建議變成強制規則。

## 後續來源補充

[Settings 來源映射](./design-system-settings-mapping.md) 與 [原始 recipe 擷取 JSON](./design-system-source-tokens.json) 已補上 action、choice、boolean pressed-button、navigation 與 focus 的來源。History 的 Aa／W 是可參照的 boolean 按鈕模式；前文「無 checkbox／switch 標準」仍成立，但不等於沒有可重用的選取語言。這輪未修改 UI。
