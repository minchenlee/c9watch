# Settings 可沿用的既有 design tokens／recipes

這份擷取承接 design-system-inventory。沒有修改 app CSS／Svelte；下面的新名稱只是文件中的 alias，尚未注入全域 tokens。來源是 notification worktree 中與 `db460ab` 相同的既有頁面，排除新 Settings 自己的樣式，避免循環論證。

## 已找到依據的項目

| Settings 角色 | 可直接參照的來源與值 | 套用界線 |
|---|---|---|
| 頁面名稱 SETTINGS | Memory／History／Cost：Pixel 22px、600、uppercase、tracking .1em、line-height 1 | 沿用既有外層頁首 |
| 子區段識別 | Memory file-header：Pixel 13px、600、uppercase、tracking .1em、padding 8px 0 | 不是拿 18px 頁名計數當所有 h2 尺寸；顏色原為 contextual accent |
| 左側 navigation | Memory：width 200px、min 160px；Mono 12px；padding 8px 12px | 比 Settings 自訂 180px 更有既有依據；不是所有表單 label 的規格 |
| 文字閱讀區 | Memory：Sans 15px、line-height 1.65 | 用於長文；不是將 toolbar 一起放大 |
| 內容標題 | Monitor card：Sans 15px、600 | 可參照 session／preview title，而非一律 Pixel |
| 短操作按鈕 | Cost scale-trigger：Pixel 11px、tracking .05em、uppercase、padding 4px 8px、1px border-default | 可沿用 action 外框與排字；現有 Copy 為 Mono 10px，是不同角色 |
| 選項群組 | History option-btn：Pixel 10px、padding 4px 8px；group 外框 1px | 可作 Brief／Detailed 的既有 segmented-choice 依據；沒有必要再加大型 radio 卡片 |
| Boolean 選項 | History case-sensitive／whole-word 按鈕：35.5×35.5px、Pixel 11px、aria-pressed | **找到的是 pressed-button 模式，不是滑動 switch**；尺寸因搜尋列而定，不直接當所有 toggle 標準 |
| 輸入欄 | History search-input：Mono 13px、padding 8px 12px、bg-elevated、border-default | 數字欄可沿用視覺角色；width 仍依資料長度決定 |
| Focus | app.css：1px border-focus outline、offset 0 | 新 Settings 的 2px／offset 4 沒有此基準支持 |
| 輔助色 | 全域 text-secondary #888 | 已存在且在黑底對比 5.92:1；#aaa 是本次新增，不能稱作從舊頁擷取 |

**不存在一個可直接擷取的「所有 button 高度」。** 舊頁多數用字型、line-height 與 padding 決定高度；35.5px 是 History 搜尋按鈕特例。目前 Settings 的 32px 仍是新方案，不能包裝成已存在的 size token。

## 互動狀態的來源

| 狀態／用途 | 既有規則 | 是否可共用 |
|---|---|---|
| 一般 action default | 透明底、secondary、1px default 邊框 | Cost scale-trigger，有直接範例 |
| 一般 action hover | primary 文字、muted 邊框 | Cost scale-trigger:hover |
| 中性選項 selected | white 10% 背景、primary 文字 | History option-btn.active；適合中性設定候選 |
| 有語意的 selected | accent-amber 文字 | Cost option-btn.active；不應直接讓所有 Settings 選取都變橘 |
| 導覽 hover | white 3% 背景、primary 文字 | Memory project-item:hover |
| 導覽 selected | white 8% 背景、primary 文字、2px contextual accent 側線 | Memory project-item.selected；accent 需要語意來源，不能默默指定 provider 色 |
| Focus | 1px 白框、offset 0 | 全域預設；History input 另以白色邊框處理 focus |
| Disabled／pending／error | 沒有從這幾頁找到一致矩陣 | 維持缺口；不抽拼湊的值 |

背景 white 3%／8%／10% 是實際 rgba 值，**不等同** bg-card／bg-card-hover。JSON 保留原值，不做未經確認的合併。

## 控制項語意：可以取代之前的「找不到任何參考」

History 已有 `aria-pressed` 的 Aa／W boolean 按鈕，及 sort 選項群組；Cost 也有 mode／sort 群組。它們可以提供 c9watch 的按鈕式選取語言。

但這些都在點擊後直接變更目前查詢／顯示狀態，沒有 Save 階段。不能因此推論通知偏好應該即時写入磁碟。**資料保存時機不是 design token**；需要與設定語意一起決定。

目前最有依據的後續方向：Brief／Detailed 參照 History segmented choice；事件類型可比較 History pressed-button 模式；不再沿用沒有舊頁依據的方形滑軌。這是來源映射建議，尚未改 UI。

## 不納入基準的例外

- ProviderFilter 的 7–9px 字與 2–4px radius 是極緊湊 header 專用，不用來定義一般表單。
- PromptInput 有 disabled 樣式，但仍是淺色底與藍色 action；不能當 Noir 頁面共用 disabled 規範。也未在此盤點確認它目前的執行入口。
- Checkbox 外框、內方塊、switch thumb 尺寸在 NotificationSettings 外未找到同型基準，不能說是擷取完成。
- 複製貼上的 recipe 應保留互動語意、字體角色與使用場景，不能只抄某個 px 值。

## 逐項來源與原始值

[機器可讀擷取結果](./design-system-source-tokens.json) 保存完整 declarations 與來源 selector／行號。每個 `name` 都是擷取 alias，不是目前程式已存在的 CSS variable。

| Alias | 來源 | 主要值 |
|---|---|---|
| `page.heading` | [MemoryViewer:212](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:212) `.section-title` | `font-family: var(--font-pixel)`; `font-size: 22px`; `color: var(--text-primary)`; `line-height: 1` |
| `page.count` | [MemoryViewer:222](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:222) `.section-count` | `font-family: var(--font-pixel)`; `font-size: 18px`; `line-height: 1`; `color: var(--text-secondary)` |
| `section.heading` | [MemoryViewer:424](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:424) `.file-header` | `width: 100%`; `font-family: var(--font-pixel)`; `font-size: 13px`; `color: var(--accent)`; `padding: var(--space-sm) 0`; `background: none`; `border: none` |
| `navigation.pane` | [MemoryViewer:285](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:285) `.project-list` | `width: 200px` |
| `navigation.item` | [MemoryViewer:293](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:293) `.project-item` | `width: 100%`; `padding: var(--space-sm) var(--space-md)`; `background: none`; `border: none`; `color: var(--text-muted)`; `font-family: var(--font-mono)`; `font-size: 12px` |
| `navigation.hover` | [MemoryViewer:310](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:310) `.project-item:hover` | `background: rgba(255, 255, 255, 0.03)`; `color: var(--text-primary)` |
| `navigation.selected` | [MemoryViewer:315](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:315) `.project-item.selected` | `background: rgba(255, 255, 255, 0.08)`; `color: var(--text-primary)`; `border-left: 2px solid var(--accent)` |
| `reading.body` | [MemoryViewer:469](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:469) `.markdown-body` | `font-family: var(--font-sans)`; `font-size: 15px`; `line-height: 1.65`; `color: var(--text-primary)` |
| `action.compact` | [CostTracker:1083](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/CostTracker.svelte:1083) `.scale-trigger` | `font-family: var(--font-pixel)`; `font-size: 11px`; `padding: 4px var(--space-sm)`; `background: transparent`; `border: 1px solid var(--border-default)`; `color: var(--text-secondary)` |
| `action.compact.hover` | [CostTracker:1095](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/CostTracker.svelte:1095) `.scale-trigger:hover` | `color: var(--text-primary)` |
| `choice.compact` | [SessionHistory:552](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:552) `.option-btn` | `font-family: var(--font-pixel)`; `font-size: 10px`; `padding: 4px var(--space-sm)`; `background: transparent`; `border: none`; `color: var(--text-muted)` |
| `choice.selected.neutral` | [SessionHistory:563](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:563) `.option-btn.active` | `background: rgba(255, 255, 255, 0.1)`; `color: var(--text-primary)` |
| `choice.selected.semantic` | [CostTracker:1510](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/CostTracker.svelte:1510) `.option-btn.active` | `color: var(--accent-amber)` |
| `choice.hover` | [CostTracker:1505](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/CostTracker.svelte:1505) `.option-btn:hover` | `color: var(--text-primary)`; `background: rgba(255, 255, 255, 0.1)` |
| `boolean.button` | [SessionHistory:505](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:505) `.match-btn` | `font-family: var(--font-pixel)`; `font-size: 11px`; `width: 35.5px`; `height: 35.5px`; `padding: 0`; `background: transparent`; `border: none`; `color: var(--text-muted)` |
| `boolean.button.hover` | [SessionHistory:522](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:522) `.match-btn:hover` | `color: var(--text-primary)`; `background: rgba(255, 255, 255, 0.08)` |
| `boolean.button.active` | [SessionHistory:527](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:527) `.match-btn.active` | `background: rgba(255, 255, 255, 0.1)`; `color: var(--text-primary)` |
| `input.text` | [SessionHistory:486](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:486) `.search-input` | `background: var(--bg-elevated)`; `border: 1px solid var(--border-default)`; `color: var(--text-primary)`; `font-family: var(--font-mono)`; `font-size: 13px`; `padding: var(--space-sm) var(--space-md)`; `outline: none` |
| `input.focus` | [SessionHistory:532](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionHistory.svelte:532) `.search-input:focus` |  |
| `content.title` | [SessionCard:390](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/SessionCard.svelte:390) `.card-main-title` | `font-family: var(--font-sans)`; `font-size: 15px`; `color: var(--text-primary)` |
| `action.copy` | [MemoryViewer:398](/Users/liminchen/Documents/GitHub/c9watch-notification-settings/src/lib/components/MemoryViewer.svelte:398) `.cmd-copy` | `font-family: var(--font-mono)`; `font-size: 10px`; `color: var(--accent)` |
