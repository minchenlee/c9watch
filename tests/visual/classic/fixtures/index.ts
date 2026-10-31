/**
 * Recorded IPC fixtures for the Classic visual harness.
 *
 * Every value here is a fixed response to a real Tauri command. All times
 * are absolute and relative to FIXED_NOW, which the tests also use as the
 * page clock. Do not use Date.now() or Math.random() in this file.
 */
import type {
	AwayActivity,
	Conversation,
	CostData,
	DailyCost,
	DetectionDiagnostics,
	HistoryEntry,
	Message,
	ProjectMemory,
	Session,
	SessionCostRecord,
	Task
} from '../../../../src/lib/types';
import type { SubagentInfo } from '../../../../src/lib/stores/subagents';
import type { SubscriptionUsage } from '../../../../src/lib/subscription-usage';

/** The page clock in every test: Tuesday 2026-03-10 15:30:00 UTC. */
export const FIXED_NOW = Date.UTC(2026, 2, 10, 15, 30, 0);

const MIN = 60_000;
const iso = (minutesAgo: number) => new Date(FIXED_NOW - minutesAgo * MIN).toISOString();
const ms = (minutesAgo: number) => FIXED_NOW - minutesAgo * MIN;

// ── Sessions: one or more in every status ──────────────────────────────

const base = {
	customTitle: null,
	summary: null,
	sourceHealth: 'fresh',
	provider: 'claudeCode',
	surface: 'claudeCode',
	agentKind: 'root',
	kind: 'interactive',
	entrypoint: 'cli',
	openTarget: 'terminal',
	canOpen: true,
	canStop: true,
	canRename: true
} as const;

export const sessions: Session[] = [
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000001',
		pid: 41001,
		sessionName: 'web-app',
		projectPath: '/Users/visual/projects/web-app',
		gitBranch: 'feat/oauth',
		firstPrompt: 'Add OAuth2 login with Google and GitHub providers',
		aiTitle: 'OAuth2 login with Google and GitHub',
		messageCount: 42,
		modified: iso(2),
		startedAtMs: ms(48),
		status: 'NeedsAttention' as Session['status'],
		latestMessage: 'I need to run `npm install passport-google-oauth20` — allow?',
		lastAssistantMessage: 'Google and GitHub providers are wired. Next I install the passport strategy.',
		pendingToolName: 'Bash',
		waitingFor: { kind: 'approval', tool: 'Bash' }
	},
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000002',
		pid: 41002,
		sessionName: 'web-app',
		projectPath: '/Users/visual/projects/web-app',
		gitBranch: 'fix/table-perf',
		firstPrompt: 'Profile and fix the slow render in the dashboard table',
		customTitle: 'Dashboard table perf',
		messageCount: 118,
		modified: iso(1),
		startedAtMs: ms(95),
		status: 'Working' as Session['status'],
		latestMessage: 'Running the profiler on VirtualizedTable to find the bottleneck...',
		pendingToolName: null
	},
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000003',
		pid: 41003,
		sessionName: 'api-server',
		projectPath: '/Users/visual/projects/api-server',
		gitBranch: 'main',
		firstPrompt: 'Which cache backend should the rate limiter use?',
		aiTitle: 'Rate limiter cache backend',
		messageCount: 17,
		modified: iso(4),
		startedAtMs: ms(30),
		status: 'NeedsAttention' as Session['status'],
		latestMessage: 'Should the rate limiter use Redis or the in-memory store?',
		lastAssistantMessage: 'Both backends work. Redis survives restarts; memory is faster.',
		pendingToolName: 'AskUserQuestion',
		waitingFor: { kind: 'question' }
	},
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000004',
		pid: 41004,
		sessionName: 'api-server',
		projectPath: '/Users/visual/projects/api-server',
		gitBranch: 'feat/webhooks',
		firstPrompt: 'Write tests for the subscription renewal webhook',
		summary: 'Webhook renewal tests with idempotency checks',
		messageCount: 64,
		modified: iso(9),
		startedAtMs: ms(120),
		status: 'WaitingForInput' as Session['status'],
		latestMessage: 'All 23 webhook tests pass. Coverage is at 94%.',
		lastAssistantMessage: 'All 23 webhook tests pass. Coverage is at 94%.',
		pendingToolName: null,
		waitingFor: { kind: 'input' }
	},
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000005',
		pid: 41005,
		sessionName: 'cli-tools',
		projectPath: '/Users/visual/projects/cli-tools',
		gitBranch: null,
		firstPrompt: 'Migrate every command handler to the Result<T, E> pattern',
		messageCount: 203,
		modified: iso(26),
		startedAtMs: ms(240),
		status: 'WaitingForInput' as Session['status'],
		latestMessage: 'All 14 command handlers use the Result pattern now. Tests pass.',
		lastAssistantMessage: 'All 14 command handlers use the Result pattern now. Tests pass.',
		pendingToolName: null,
		waitingFor: { kind: 'input' }
	},
	{
		...base,
		id: '0b6c1f0e-1111-4a2b-9c3d-000000000006',
		pid: 41006,
		sessionName: 'cli-tools',
		projectPath: '/Users/visual/projects/cli-tools',
		gitBranch: 'feat/init-wizard',
		firstPrompt: 'Build an interactive init wizard',
		messageCount: 1,
		modified: iso(0),
		startedAtMs: ms(0),
		status: 'Connecting' as Session['status'],
		latestMessage: '',
		pendingToolName: null
	},
	{
		...base,
		id: 'codex-thread-0007',
		pid: 41007,
		sessionName: 'cli-tools',
		projectPath: '/Users/visual/projects/cli-tools',
		gitBranch: 'feat/init-wizard',
		firstPrompt: 'Review the init wizard prompts for clarity',
		codexTitle: 'Init wizard copy review',
		messageCount: 12,
		modified: iso(3),
		startedAtMs: ms(20),
		status: 'Working' as Session['status'],
		latestMessage: 'Reading src/commands/init.ts',
		pendingToolName: null,
		provider: 'codex',
		surface: 'cli',
		entrypoint: null,
		canRename: false
	}
];

export const longSessionId = sessions[1].id;

/** Away counts for the waiting sessions, keyed by provider-scoped key. */
export const awayActivity: Record<string, AwayActivity> = {
	[`claudeCode:${sessions[0].id}`]: { sinceMs: ms(48), toolCalls: 18, filesEdited: 4, commandsRun: 6, subagentsCompleted: 2, partial: false },
	[`claudeCode:${sessions[2].id}`]: { sinceMs: ms(30), toolCalls: 3, filesEdited: 0, commandsRun: 1, subagentsCompleted: 0, partial: false },
	[`claudeCode:${sessions[3].id}`]: { sinceMs: ms(120), toolCalls: 41, filesEdited: 9, commandsRun: 12, subagentsCompleted: 1, partial: true },
	[`claudeCode:${sessions[4].id}`]: { sinceMs: ms(240), toolCalls: 0, filesEdited: 0, commandsRun: 0, subagentsCompleted: 0, partial: false }
};

export const diagnostics: DetectionDiagnostics = {
	claudeProcessesFound: 6,
	processesWithCwd: 6,
	fdaLikelyNeeded: false,
	hiddenClaudeSessions: []
};

// ── Conversations ──────────────────────────────────────────────────────

const TOPICS = [
	['Read', 'src/components/VirtualizedTable.tsx'],
	['Grep', 'useMemo\\( in src/components'],
	['Bash', 'npm run profile -- --component VirtualizedTable'],
	['Edit', 'src/components/Row.tsx'],
	['Bash', 'npm test -- table'],
	['Read', 'src/hooks/useRows.ts']
] as const;

/** A long conversation: 30 rounds, every message type, code and lists. */
function longConversation(sessionId: string): Conversation {
	const messages: Message[] = [];
	let t = 95;
	const at = () => iso((t = Math.max(1, t - 1)));
	messages.push({ timestamp: at(), messageType: 'User', content: 'Profile and fix the slow render in the dashboard table. It takes about 340ms per keystroke in the filter box.' });
	for (let round = 0; round < 30; round++) {
		const [tool, target] = TOPICS[round % TOPICS.length];
		messages.push({
			timestamp: at(),
			messageType: 'Thinking',
			content: `Round ${round + 1}: the render cost comes from rows that re-render on every filter change. I should check ${target} next.`
		});
		messages.push({
			timestamp: at(),
			messageType: 'Assistant',
			content:
				round % 5 === 0
					? `Step ${round + 1}. I found three causes:\n\n1. \`Row\` gets a new \`style\` object on every render.\n2. \`useRows\` sorts the full list again on each keystroke.\n3. The header measures every column with \`getBoundingClientRect\`.\n\nI fix them one at a time and measure after each fix.`
					: `Step ${round + 1}. Next I check \`${target}\`.`
		});
		messages.push({ timestamp: at(), messageType: 'ToolUse', content: `${tool} ${target}` });
		messages.push({
			timestamp: at(),
			messageType: 'ToolResult',
			content:
				round % 3 === 0
					? '```ts\nexport const Row = memo(function Row({ row, style }: RowProps) {\n  return <div style={style}>{row.cells.map(renderCell)}</div>;\n});\n```'
					: `ok: ${tool.toLowerCase()} finished in ${120 + round * 7}ms`
		});
		if (round % 7 === 6) {
			messages.push({ timestamp: at(), messageType: 'User', content: `Looks good so far. Keep going and keep the public API of ${target} unchanged.` });
		}
	}
	messages.push({ timestamp: at(), messageType: 'System', content: 'Context compacted.' });
	messages.push({
		timestamp: iso(1),
		messageType: 'Assistant',
		content: 'Render time per keystroke dropped from **340ms to 18ms**. The main fix is the memoized `Row` with a stable `style` prop.'
	});
	return { sessionId, provider: 'claudeCode', messages };
}

function shortConversation(session: Session): Conversation {
	return {
		sessionId: session.id,
		provider: session.provider ?? 'claudeCode',
		messages: [
			{ timestamp: iso(40), messageType: 'User', content: session.firstPrompt },
			{ timestamp: iso(39), messageType: 'Assistant', content: 'I start by reading the existing code.' },
			{ timestamp: iso(38), messageType: 'ToolUse', content: 'Read src/index.ts' },
			{ timestamp: iso(38), messageType: 'ToolResult', content: '```ts\nexport { app } from "./app";\n```' },
			{ timestamp: iso(5), messageType: 'Assistant', content: session.lastAssistantMessage ?? session.latestMessage }
		]
	};
}

export const conversations: Record<string, Conversation> = Object.fromEntries(
	sessions.map((s) => [s.id, s.id === longSessionId ? longConversation(s.id) : shortConversation(s)])
);

export const subagents: Record<string, SubagentInfo[]> = {
	[`claudeCode:${longSessionId}`]: [
		{ id: 'agent-a1', agentType: 'Explore', description: 'Find every caller of useRows', startedAt: iso(40), completedAt: iso(37), parentSessionId: longSessionId, status: 'completed' },
		{ id: 'agent-a2', agentType: 'general-purpose', description: 'Benchmark the table with 10k rows', startedAt: iso(3), completedAt: null, parentSessionId: longSessionId, status: 'running' }
	]
};

export const tasks: Record<string, Task[]> = {
	[longSessionId]: [
		{ id: '1', subject: 'Profile the filter keystroke', activeForm: 'Profiling the filter keystroke', status: 'completed' },
		{ id: '2', subject: 'Memoize Row', activeForm: 'Memoizing Row', status: 'completed' },
		{ id: '3', subject: 'Stop re-sorting in useRows', activeForm: 'Stopping re-sorts in useRows', status: 'in_progress' },
		{ id: '4', subject: 'Cache header column widths', activeForm: 'Caching header column widths', status: 'pending' }
	]
};

// ── History ────────────────────────────────────────────────────────────

const historyProjects = ['web-app', 'api-server', 'cli-tools', 'docs-site', 'infra'];
const historyPrompts = [
	'Add OAuth2 login with Google and GitHub providers',
	'Fix the flaky webhook retry test',
	'Write a migration for the invoices table',
	'Explain why the build is slow on CI',
	'Refactor the logger to structured JSON',
	'Add dark mode to the settings page',
	'Upgrade to Node 22 and fix the type errors',
	'Draft the release notes for v2.4'
];

export const history: HistoryEntry[] = [
	...sessions
		.filter((s) => s.provider === 'claudeCode')
		.map((s, i) => ({
			sessionId: s.id,
			display: s.firstPrompt,
			timestamp: ms(50 + i * 13),
			project: s.projectPath,
			projectName: s.sessionName,
			customTitle: s.customTitle,
			aiTitle: s.aiTitle ?? null,
			provider: 'claudeCode' as const,
			surface: 'claudeCode' as const
		})),
	...Array.from({ length: 24 }, (_, i) => {
		const project = historyProjects[i % historyProjects.length];
		return {
			sessionId: `hist-${String(i).padStart(4, '0')}`,
			display: historyPrompts[i % historyPrompts.length],
			timestamp: ms(180 + i * 9 * 60),
			project: `/Users/visual/projects/${project}`,
			projectName: project,
			customTitle: i % 6 === 2 ? `Saved title ${i}` : null,
			aiTitle: null,
			provider: i % 5 === 4 ? ('codex' as const) : ('claudeCode' as const),
			surface: i % 5 === 4 ? ('cli' as const) : ('claudeCode' as const)
		};
	})
];

export const historyConversationId = 'hist-0000';
conversations[historyConversationId] = {
	sessionId: historyConversationId,
	provider: 'claudeCode',
	messages: [
		{ timestamp: iso(180), messageType: 'User', content: historyPrompts[0] },
		{ timestamp: iso(179), messageType: 'Assistant', content: 'I read the auth router first, then add the provider config.' },
		{ timestamp: iso(178), messageType: 'ToolUse', content: 'Read src/auth/index.ts' },
		{ timestamp: iso(178), messageType: 'ToolResult', content: '```ts\nexport const authRouter = Router();\n```' },
		{ timestamp: iso(170), messageType: 'Assistant', content: 'Both providers work. Run `npm test -- auth` to check.' }
	]
};

// ── Cost ───────────────────────────────────────────────────────────────

const models = [
	{ model: 'claude-opus-4-1', displayName: 'Opus', provider: 'claudeCode' as const },
	{ model: 'claude-sonnet-4-5', displayName: 'Sonnet', provider: 'claudeCode' as const },
	{ model: 'gpt-5.4', displayName: 'GPT-5.4', provider: 'codex' as const }
];

function costRecord(i: number, daysAgo: number): SessionCostRecord {
	const project = historyProjects[i % historyProjects.length];
	const m = models[i % models.length];
	const priced = m.provider !== 'codex';
	const input = 20_000 + ((i * 7919) % 90_000);
	const output = 4_000 + ((i * 3571) % 30_000);
	const date = new Date(FIXED_NOW - daysAgo * 24 * 60 * MIN);
	return {
		sessionId: `cost-${String(i).padStart(4, '0')}`,
		project: `/Users/visual/projects/${project}`,
		projectName: project,
		model: m.model,
		cost: priced ? Math.round((input * 3 + output * 15) / 1000) / 100 : 0,
		costAvailable: priced,
		inputTokens: input,
		cachedInputTokens: Math.floor(input / 3),
		outputTokens: output,
		reasoningOutputTokens: m.provider === 'codex' ? Math.floor(output / 4) : 0,
		timestamp: date.toISOString(),
		date: date.toISOString().slice(0, 10),
		totalTokens: input + output,
		sessionName: historyPrompts[i % historyPrompts.length],
		provider: m.provider,
		surface: m.provider === 'codex' ? 'cli' : 'claudeCode',
		agentKind: 'root'
	};
}

function buildCost(): CostData {
	const records: SessionCostRecord[] = [];
	for (let day = 0; day < 40; day++) {
		const count = 1 + ((day * 5) % 4);
		for (let k = 0; k < count; k++) records.push(costRecord(records.length, day));
	}
	const round = (n: number) => Math.round(n * 100) / 100;
	const byDay = new Map<string, SessionCostRecord[]>();
	const byProject = new Map<string, SessionCostRecord[]>();
	for (const r of records) {
		byDay.set(r.date, [...(byDay.get(r.date) ?? []), r]);
		byProject.set(r.project, [...(byProject.get(r.project) ?? []), r]);
	}
	const sum = (rs: SessionCostRecord[]) => round(rs.reduce((a, r) => a + r.cost, 0));
	const totalCost = sum(records);
	const dailyCosts: DailyCost[] = [...byDay.entries()]
		.sort(([a], [b]) => (a < b ? -1 : 1))
		.map(([date, rs]) => ({ date, cost: sum(rs), sessions: rs }));
	return {
		totalCost,
		totalTokens: records.reduce((a, r) => a + r.totalTokens, 0),
		unpricedTokens: records.filter((r) => !r.costAvailable).reduce((a, r) => a + r.totalTokens, 0),
		dailyCosts,
		projectCosts: [...byProject.entries()]
			.map(([project, rs]) => ({ project, projectName: rs[0].projectName, totalCost: sum(rs), sessions: rs }))
			.sort((a, b) => b.totalCost - a.totalCost),
		modelCosts: models.map((m) => {
			const rs = records.filter((r) => r.model === m.model);
			const cost = sum(rs);
			return {
				model: m.model,
				displayName: m.displayName,
				provider: m.provider,
				cost,
				percentage: totalCost ? round((cost / totalCost) * 100) : 0,
				totalTokens: rs.reduce((a, r) => a + r.totalTokens, 0),
				costAvailable: m.provider !== 'codex'
			};
		})
	};
}

export const cost: CostData = buildCost();

// ── Memory ─────────────────────────────────────────────────────────────

export const memory: ProjectMemory[] = [
	{
		provider: 'claudeCode',
		projectName: 'web-app',
		projectPath: '/Users/visual/projects/web-app',
		memoryDirPath: '/Users/visual/.claude/projects/-Users-visual-projects-web-app/memory',
		files: [
			{ filename: 'MEMORY.md', content: '# web-app memory\n\n- Use `pnpm`, not `npm`.\n- Tests live next to the code as `*.test.ts`.\n- [Auth notes](auth.md)\n' },
			{ filename: 'auth.md', content: '# Auth\n\nOAuth2 providers are in `src/auth/providers.ts`. Secrets come from `.env.local`.\n' }
		]
	},
	{
		provider: 'claudeCode',
		projectName: 'api-server',
		projectPath: '/Users/visual/projects/api-server',
		memoryDirPath: '/Users/visual/.claude/projects/-Users-visual-projects-api-server/memory',
		files: [{ filename: 'MEMORY.md', content: '# api-server memory\n\n- Rate limiter uses Redis in production.\n' }]
	},
	{
		provider: 'codex',
		projectName: 'Codex',
		projectPath: '/Users/visual/.codex',
		memoryDirPath: '/Users/visual/.codex/memories',
		files: [{ filename: 'notes.md', content: 'Prefer small pull requests.\n' }]
	}
];

// ── Settings and the status bar ────────────────────────────────────────

const nowSec = Math.floor(FIXED_NOW / 1000);
export const subscriptionUsage: SubscriptionUsage[] = [
	{ provider: 'claudeCode', name: 'Claude Code', plan: 'Max', updatedAt: nowSec, message: null, windows: [
		{ label: '5-hour', usedPercent: 41, resetsAt: nowSec + 2 * 3600, windowSeconds: 18000 },
		{ label: 'Weekly', usedPercent: 72, resetsAt: nowSec + 3 * 86400, windowSeconds: 604800 }
	] },
	{ provider: 'codex', name: 'Codex', plan: 'Pro', updatedAt: nowSec, message: null, windows: [
		{ label: '5-hour', usedPercent: 12, resetsAt: nowSec + 4 * 3600, windowSeconds: 18000 },
		{ label: 'Weekly', usedPercent: 30, resetsAt: nowSec + 5 * 86400, windowSeconds: 604800 }
	] }
];

export const notificationPreferences = {
	enabled: true,
	replyReady: true,
	questions: true,
	permissions: true,
	detail: 'brief',
	sound: false,
	cooldownSeconds: 30
};

export const opencodeStatus = { url: 'http://127.0.0.1:4096', connected: false, error: null, sessionCount: 0 };

export const remoteAccess = { lanEnabled: false, allowConversation: false, allowOpen: false, extraAllowedHosts: [] as string[] };

export const serverInfo = { port: 9210, localIp: '127.0.0.1', lanEnabled: false, bind: 'loopback', lanAddresses: [] as string[] };

export const appVersion = '0.11.1';
