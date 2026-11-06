/** The one demo fixture for the glass shell, in c9p shapes only
 * (`SessionView`, `AgentRow`, `Annotation`). Dev, preview and tests all use it.
 * Content follows `design-r7/office.html` `DATA` and R7-SPEC §A.6: 4
 * workspaces, one lead → worker tree per goal, one talk session per repo, and
 * the 5 items that need you. Data c9p does not have (goals, needs verbs, seat
 * caps, rules) lives in `src/shells/glass/model/gaps-demo.ts`.
 * Times are relative to `Date.now()`, so a fixed test clock gives fixed rows. */
import type { C9pAgentRow, C9pAnnotation, C9pAnnotationValue, C9pSessionView, C9pWaitingFor } from '../../c9p-1';

/** Workspace roots. `cross` is the parent folder: its goal spans repos. */
export const GLASS_DEMO_ROOTS = {
    c9watch: '/Users/demo/code/c9watch',
    flockbook: '/Users/demo/code/flockbook',
    jumpmark: '/Users/demo/code/jumpmark',
    cross: '/Users/demo/code'
} as const;
export type GlassDemoWorkspace = keyof typeof GLASS_DEMO_ROOTS;

/** `need`: needs you; `quiet`: no answer for a while; `done`: retired. */
type Seat = {
    slug: string; name: string; ws: GlassDemoWorkspace; role: 'orch' | 'worker' | 'observed';
    parent?: string; provider: 'claudeCode' | 'codex'; seat: 'need' | 'quiet' | 'work' | 'idle' | 'done';
    act: string; ago: number; branch?: string;
    wait?: C9pWaitingFor; tool?: { name: string; input: unknown };
};
const SEATS: Seat[] = [
    // C9WATCH · goal 狀態用詞統一
    { slug: 'lead-terms', name: 'lead-terms', ws: 'c9watch', role: 'orch', provider: 'claudeCode', seat: 'idle', act: '分派 3 個 worker，等 w-monitor 交件', ago: 3 },
    { slug: 'w-monitor', name: 'w-monitor', ws: 'c9watch', role: 'worker', parent: 'lead-terms', provider: 'codex', seat: 'need', act: '交付：改前改後截圖 2 張 · 測試 12/12', ago: 4, branch: 'feature/status-terms-monitor' },
    { slug: 'w-agents', name: 'w-agents', ws: 'c9watch', role: 'worker', parent: 'lead-terms', provider: 'claudeCode', seat: 'need', act: '要不要改狀態色？停在 src/agents/StatusChip.svelte', ago: 6, wait: { kind: 'question' }, tool: { name: 'AskUserQuestion', input: { question: '要不要改狀態色？' } } },
    { slug: 'w-history', name: 'w-history', ws: 'c9watch', role: 'worker', parent: 'lead-terms', provider: 'claudeCode', seat: 'quiet', act: '讀 src/history/list.svelte', ago: 14 },
    // C9WATCH · goal 引擎權限重構
    { slug: 'lead-authz', name: 'lead-authz', ws: 'c9watch', role: 'orch', provider: 'claudeCode', seat: 'work', act: '協調 5 個 worker', ago: 1 },
    { slug: 'w-authz', name: 'w-authz', ws: 'c9watch', role: 'worker', parent: 'lead-authz', provider: 'claudeCode', seat: 'work', act: '改 engine/authz.rs', ago: 1 },
    { slug: 'w-policy', name: 'w-policy', ws: 'c9watch', role: 'worker', parent: 'lead-authz', provider: 'claudeCode', seat: 'work', act: '寫 policy 測試', ago: 1 },
    { slug: 'w-audit', name: 'w-audit', ws: 'c9watch', role: 'worker', parent: 'lead-authz', provider: 'codex', seat: 'work', act: '讀 audit log 格式', ago: 2 },
    { slug: 'w-ipc', name: 'w-ipc', ws: 'c9watch', role: 'worker', parent: 'lead-authz', provider: 'claudeCode', seat: 'idle', act: '等 w-authz 交件', ago: 9 },
    { slug: 'w-tests', name: 'w-tests', ws: 'c9watch', role: 'worker', parent: 'lead-authz', provider: 'codex', seat: 'work', act: '跑 cargo test', ago: 1 },
    { slug: 'talk-spike', name: 'spike', ws: 'c9watch', role: 'observed', provider: 'claudeCode', seat: 'work', act: '讀 engine/authz.rs', ago: 1 },
    // FLOCKBOOK · goal 匯出 CSV
    { slug: 'lead-csv', name: 'lead-csv', ws: 'flockbook', role: 'orch', provider: 'claudeCode', seat: 'idle', act: '等 w-export 的測試環境', ago: 5 },
    { slug: 'w-columns', name: 'w-columns', ws: 'flockbook', role: 'worker', parent: 'lead-csv', provider: 'claudeCode', seat: 'done', act: '欄位對應完成 · verifier 通過', ago: 62 },
    { slug: 'w-export', name: 'w-export', ws: 'flockbook', role: 'worker', parent: 'lead-csv', provider: 'claudeCode', seat: 'need', act: '測試環境起不來，重試 2 次', ago: 7 },
    { slug: 'talk-feed-cache', name: 'feed-cache', ws: 'flockbook', role: 'observed', provider: 'claudeCode', seat: 'idle', act: '你和 claude 的對談', ago: 40 },
    // JUMPMARK · goal 雲端同步登入
    { slug: 'lead-login', name: 'lead-login', ws: 'jumpmark', role: 'orch', provider: 'claudeCode', seat: 'idle', act: '等 w-login 拿到 key', ago: 8 },
    { slug: 'w-login', name: 'w-login', ws: 'jumpmark', role: 'worker', parent: 'lead-login', provider: 'claudeCode', seat: 'need', act: '要 staging 的 API key 才能跑登入測試', ago: 8 },
    { slug: 'w-sync', name: 'w-sync', ws: 'jumpmark', role: 'worker', parent: 'lead-login', provider: 'codex', seat: 'idle', act: '等 w-login', ago: 8 },
    { slug: 'talk-main', name: 'main', ws: 'jumpmark', role: 'observed', provider: 'claudeCode', seat: 'need', act: '想執行 npm run build && npm run test:e2e', ago: 2, wait: { kind: 'approval', tool: 'Bash' }, tool: { name: 'Bash', input: { command: 'npm run build && npm run test:e2e' } } },
    // 跨專案 · goal 搜尋改版 (all done)
    { slug: 'lead-search', name: 'lead-search', ws: 'cross', role: 'orch', provider: 'claudeCode', seat: 'idle', act: '搜尋改版完成', ago: 50 },
    { slug: 'w-index', name: 'w-index', ws: 'cross', role: 'worker', parent: 'lead-search', provider: 'claudeCode', seat: 'done', act: '搜尋從 1.2 秒降到 180 毫秒', ago: 55 },
    { slug: 'w-ui', name: 'w-ui', ws: 'cross', role: 'worker', parent: 'lead-search', provider: 'codex', seat: 'done', act: '搜尋結果頁改版', ago: 55 }
];

const MACHINE = 'm_demo';
export const glassAgentId = (slug: string) => `agt_glass_${slug}`;
const sid = (slug: string) => `glass-${slug}`;
const keyOf = (s: Seat) => `${s.provider}:${sid(s.slug)}`;
const msAgo = (minutes: number) => Date.now() - minutes * 60_000;
const SESSION_STATUS = { need: 'WaitingForInput', quiet: 'Working', work: 'Working', idle: 'WaitingForInput', done: 'WaitingForInput' } as const;
const AGENT_STATUS = { need: 'waiting_for_input', quiet: 'working', work: 'working', idle: 'waiting_for_input', done: null } as const;

const LIVE = SEATS.filter(s => s.seat !== 'done');
const pidOf = (s: Seat) => 41000 + LIVE.indexOf(s);

/** Live sessions only: a retired agent has no session view. */
export function glassDemoSessionViews(): C9pSessionView[] {
    return LIVE.map((s, i): C9pSessionView => ({
        agentKind: 'root', canOpen: true, canRename: s.provider === 'claudeCode', canStop: s.provider === 'claudeCode',
        customTitle: null, firstPrompt: s.act, gitBranch: s.branch ?? 'main', id: sid(s.slug),
        kind: s.role === 'observed' ? 'interactive' : 'background', latestMessage: s.act, messageCount: 12 + i * 3,
        modified: new Date(msAgo(s.ago)).toISOString(), officialName: s.role === 'observed' ? null : s.name,
        pendingToolName: s.tool?.name ?? null, pendingToolInput: s.tool?.input, pid: pidOf(s),
        projectPath: GLASS_DEMO_ROOTS[s.ws], provider: s.provider, sessionKey: keyOf(s), sessionName: s.name,
        startedAtMs: msAgo(s.ago + 30), status: s.wait ? 'NeedsAttention' : SESSION_STATUS[s.seat], summary: null,
        surface: s.provider === 'codex' ? 'cli' : 'claudeCode', waitingFor: s.wait ?? (s.seat === 'need' || s.seat === 'idle' ? { kind: 'input' } : null)
    }));
}

/** One lead → worker tree per goal; talk sessions are observed agents. */
export function glassDemoAgents(): C9pAgentRow[] {
    return SEATS.map((s): C9pAgentRow => {
        const engine = s.role !== 'observed';
        const done = s.seat === 'done';
        return {
            control: engine ? 'pty' : 'none', createdAt: msAgo(s.ago + 30), createdBy: engine ? (s.parent ? glassAgentId(s.parent) : 'local-unverified') : null,
            cwd: GLASS_DEMO_ROOTS[s.ws], generation: 1, goneAt: done ? msAgo(s.ago) : null, id: glassAgentId(s.slug),
            interruptedNotePending: false, lastSeen: done ? null : msAgo(s.ago), machineId: MACHINE, mode: engine ? 'managed' : null,
            name: s.name, parentId: s.parent ? glassAgentId(s.parent) : null, pid: done ? null : pidOf(s),
            provider: s.provider === 'codex' ? 'codex' : 'claude', providerSid: sid(s.slug), resumeCount: 0, role: s.role,
            sessionKey: keyOf(s), source: engine ? 'engine' : 'detected', startedAt: msAgo(s.ago + 30),
            state: engine ? (done ? 'retired' : s.seat === 'work' || s.seat === 'quiet' ? 'working' : 'ready') : null, stateReason: null,
            status: s.wait ? 'needs_attention' : AGENT_STATUS[s.seat]
        };
    });
}

function note(slug: string, key: string, label: string, value: C9pAnnotationValue, ago: number): C9pAnnotation {
    const seat = SEATS.find(s => s.slug === slug)!;
    return { target: { kind: 'session', sessionKey: keyOf(seat) }, key, label, value, writer: 'local-unverified', sensitivity: 'metadata', createdAt: msAgo(ago), updatedAt: msAgo(ago), expiresAt: null };
}
export function glassDemoAnnotations(): C9pAnnotation[] {
    return [
        note('w-monitor', 'ci/tests', '測試', { type: 'string', v: '12/12' }, 4),
        note('w-export', 'ci/retries', '重試', { type: 'number', v: 2 }, 7),
        note('w-tests', 'ci/job', 'cargo test', { type: 'duration', ms: 95_000 }, 1)
    ];
}
