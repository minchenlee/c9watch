/** CORE DOES NOT HAVE THIS DATA. Demo values for what the glass design needs
 * and c9p 1.x cannot answer yet: workspaces with names, goals, seat caps,
 * the needs verbs, verifier results and rules (build plan §4, rows W G S V N R).
 * When core grows these methods, replace this one file.
 * Agents are named by `glassAgentId` from the c9p fixture, so the two files
 * describe the same office (`design-r7/office.html` `DATA`, R7-SPEC §A.6). */
import { GLASS_DEMO_ROOTS, glassAgentId, type GlassDemoWorkspace } from '$lib/data/demo/glass';

export type DemoWorkspace = { id: GlassDemoWorkspace; name: string; root: string; verifier: string[] };
export type DemoGoal = { id: string; workspace: GlassDemoWorkspace; name: string; cap: number; leadId: string };
export type NeedVerb = '審' | '決定' | '處理' | '給' | '允許';
export type DemoNeed = { agentId: string; verb: NeedVerb; tag: string; title: string };
export type DemoRule = { scope: string; kind: '界線' | '指示' | '授權' | '金鑰'; text: string; lastUsed: string; count: number };

export const DEMO_WORKSPACES: DemoWorkspace[] = [
    { id: 'c9watch', name: 'C9WATCH', root: GLASS_DEMO_ROOTS.c9watch, verifier: ['今天通過 3 項：MONITOR 畫面 07:50、authz 介面 04:31、policy 載入 04:12', '今天退回 1 項：HISTORY 畫面 00:40（還有 2 個舊詞沒換）'] },
    { id: 'flockbook', name: 'FLOCKBOOK', root: GLASS_DEMO_ROOTS.flockbook, verifier: ['今天通過 1 項：欄位對應 06:58', '今天退回 0 項'] },
    { id: 'jumpmark', name: 'JUMPMARK', root: GLASS_DEMO_ROOTS.jumpmark, verifier: ['今天還沒有東西交件', '登入測試要等你給 key 才能跑'] },
    { id: 'cross', name: '跨專案', root: GLASS_DEMO_ROOTS.cross, verifier: ['今天通過 2 項：索引重建 07:16、結果頁 07:16', '這個 goal 已經整個完成'] }
];

export const DEMO_GOALS: DemoGoal[] = [
    { id: 'goal-terms', workspace: 'c9watch', name: '狀態用詞統一', cap: 4, leadId: glassAgentId('lead-terms') },
    { id: 'goal-authz', workspace: 'c9watch', name: '引擎權限重構', cap: 6, leadId: glassAgentId('lead-authz') },
    { id: 'goal-csv', workspace: 'flockbook', name: '匯出 CSV', cap: 3, leadId: glassAgentId('lead-csv') },
    { id: 'goal-login', workspace: 'jumpmark', name: '雲端同步登入', cap: 3, leadId: glassAgentId('lead-login') },
    { id: 'goal-search', workspace: 'cross', name: '搜尋改版', cap: 2, leadId: glassAgentId('lead-search') }
];

/** The 5 items that need you. Only 允許 maps to a c9p fact today
 * (`waitingFor.kind = approval`); the other verbs exist only here. */
export const DEMO_NEEDS: DemoNeed[] = [
    { agentId: glassAgentId('w-monitor'), verb: '審', tag: '等你審', title: 'MONITOR 改用新詞表' },
    { agentId: glassAgentId('w-agents'), verb: '決定', tag: '等你決定', title: '要不要改狀態色？' },
    { agentId: glassAgentId('w-export'), verb: '處理', tag: '卡住', title: '匯出 CSV 的測試環境' },
    { agentId: glassAgentId('w-login'), verb: '給', tag: '等你給 key', title: 'staging 的 API key' },
    { agentId: glassAgentId('talk-main'), verb: '允許', tag: '等你允許', title: 'npm run build && npm run test:e2e' }
];

/** R7-SPEC §A.6: one rules list. None of them may contradict the 5 needs. */
export const DEMO_RULES: DemoRule[] = [
    { scope: 'JUMPMARK › 對談 · main', kind: '授權', text: '`npm run lint`（一字不差）直接允許', lastUsed: '07:40', count: 6 },
    { scope: 'FLOCKBOOK', kind: '金鑰', text: '`SENTRY_DSN` agent 可以直接從鑰匙圈拿', lastUsed: '11/12', count: 3 },
    { scope: 'FLOCKBOOK', kind: '指示', text: '匯出相關的測試用 staging 資料', lastUsed: '06:41', count: 4 },
    { scope: '狀態用詞統一', kind: '界線', text: '要先問 改狀態色、刪功能；不能做 改 engine 的狀態定義', lastUsed: '07:57（擋下 w-agents 改狀態色）', count: 1 }
];

/** What the handling panel shows for each demo item (R7-SPEC §A.3–A.4,
 * `design-r7/needs-proto.html` ITEMS). Core has none of it: no reason text,
 * options, evidence or before / after shots (build plan §4 Gap N). */
export type DemoFact = { text: string; source: string; confidence: '確定' | '估計' | '不確定' };
export type DemoOption = { label: string; by?: string; effect: string };
/** One row of a before / after shot. `tone`: `need` = the "needs you" orange,
 * `dim` = dimmed, otherwise plain gray. */
export type DemoShotRow = { name: string; state: string; tone?: 'need' | 'dim' };
export type DemoShot = { title: string; rows: DemoShotRow[]; caption: string };
export type DemoNeedDetail = {
    agentId: string;
    why: string;
    code?: string;
    facts?: DemoFact[];
    shots?: DemoShot[];
    options?: DemoOption[];
    /** Index of the 「自己寫」 option, which opens a text box. */
    freeIdx?: number;
    freeLabel?: string;
    /** Index of the 「退回」 option, which opens the reason box. */
    rejectIdx?: number;
    rejectLabel?: string;
    input?: { label: string; placeholder: string };
    primary?: string;
    no?: string;
    note?: string;
    reversible: boolean;
};

const plain = (name: string, state: string): DemoShotRow => ({ name, state });
export const DEMO_NEED_DETAILS: DemoNeedDetail[] = [
    {
        agentId: glassAgentId('w-monitor'), reversible: true,
        why: 'MONITOR 畫面的 6 個狀態都換成新詞表的說法。verifier 已經跑過測試和截圖比對：測試 12/12。',
        shots: [
            { title: 'MONITOR · 改前', rows: [plain('w-login', 'APPROVAL'), plain('w-ui', 'READY')], caption: '改前' },
            { title: 'MONITOR · 改後', rows: [plain('w-login', '等你允許'), plain('w-ui', '等你審')], caption: '改後 · verifier 通過 · 3 個檔案 +42 −18' }
        ],
        options: [{ label: '驗收', effect: '改動進 goal。verifier 已通過。' }, { label: '退回', effect: 'w-monitor 照你寫的原因重做。' }],
        rejectIdx: 1, rejectLabel: '退回原因（w-monitor 會照這個重做）', primary: '驗收'
    },
    {
        agentId: glassAgentId('w-agents'), reversible: true,
        why: '新詞表有 8 個狀態，色票只有 4 種顏色，「等你審」和「工作中」會變成同一個顏色。改狀態色在這個 goal 的「要先問」清單裡。',
        shots: [
            { title: 'AGENTS · 現在', rows: [plain('w-login', '等你允許'), plain('w-ui', '等你審'), plain('w-sync', '工作中')], caption: '現在：3 種狀態同一個顏色' },
            { title: 'AGENTS · 選 1 之後', rows: [{ name: 'w-login', state: '等你允許', tone: 'need' }, { name: 'w-ui', state: '等你審', tone: 'need' }, { name: 'w-sync', state: '工作中', tone: 'dim' }], caption: '選 1：等你的變橘色，工作中變暗' }
        ],
        options: [
            { label: '只有「需要你」上色，其他用灰階', by: 'w-agents 的建議', effect: '3 個畫面都要改，截圖要重拍。' },
            { label: '不改顏色', effect: '只換文字。「等你審」和「工作中」暫時同色。' },
            { label: '延到下一個 goal', effect: '這個 goal 只做文字。另開一個 goal 處理色票。' },
            { label: '自己寫', effect: '你寫的話直接送給 w-agents。' }
        ],
        freeIdx: 3, freeLabel: '你要回 w-agents 的話', note: '回答給 w-agents，記進 goal 的決定紀錄。'
    },
    {
        agentId: glassAgentId('w-export'), reversible: true,
        why: '匯出測試要先啟動測試資料庫。我重試了 2 次，都在同一步失敗：本機的 5432 port 被佔用了。',
        code: '$ pnpm test:export\n▸ starting postgres (docker) …\n✗ Error: bind: address already in use (0.0.0.0:5432)\n  佔用 5432 的是：Postgres.app（你本機開著的）',
        options: [
            { label: '測試改用 5433 port', by: 'w-export 的建議', effect: 'w-export 自己改測試設定，不碰正式設定。' },
            { label: '我先關掉 Postgres.app，再試一次', effect: '你關掉之後再送出。' },
            { label: '先跳過這一項', effect: '匯出測試先停，goal 的其他部分照常進行。' }
        ]
    },
    {
        agentId: glassAgentId('w-login'), reversible: false,
        why: '登入測試要打 staging 的 API。repo 裡沒有這個 key，我不能自己產生。',
        input: { label: 'STAGING_API_KEY', placeholder: '貼上 key（⌘V）' },
        primary: '存入 key（不能復原）', no: '我沒有這把 key',
        note: '存進 macOS 鑰匙圈，只給 w-login 使用。不會寫進對談、記憶或 log。key 交出就收不回，所以存入後會先給你看收據。選「我沒有」：lead 先跳過登入測試。'
    },
    {
        agentId: glassAgentId('talk-main'), reversible: false,
        why: '我想先 build 再跑 e2e 測試，確認登入流程沒壞。',
        code: '$ npm run build && npm run test:e2e\n  在 ~/code/jumpmark（main 分支）',
        facts: [
            { text: '會寫入 dist/、test-results/', source: '來源：package.json 的 build 和 test:e2e 指令', confidence: '確定' },
            { text: '會開瀏覽器跑測試，約 4 分鐘', source: '來源：11/10 上次執行的紀錄', confidence: '估計' },
            { text: '會不會連外網：不確定', source: 'package.json 看不出來。11/10 那次只連了 localhost（來源：執行紀錄）', confidence: '不確定' }
        ]
    }
];

/** Demo switch for the disconnect banner (R7-SPEC §A.8): with the glass
 * fixture on and this key set to '1', the 需要你 page acts as if the agents
 * went away 2 minutes ago. The demo transport never fails on its own. */
export const DEMO_OFFLINE_KEY = 'c9watch.demoGlassOffline';
export const DEMO_OFFLINE_AGO_MS = 2 * 60_000;
