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
