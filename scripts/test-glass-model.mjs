import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// Load the glass model (and the demo fixture) as ES modules: each file is
// transpiled and its runtime imports point at the already-loaded modules.
const urls = {};
async function load(spec, path) {
    let out = ts.transpileModule(readFileSync(new URL(path, import.meta.url), 'utf8'), {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
    }).outputText;
    for (const [s, url] of Object.entries(urls)) out = out.replaceAll(`from '${s}'`, `from '${url}'`);
    assert(!/from '(\.|\$lib)/.test(out), `${path} has an unresolved import`);
    urls[spec] = 'data:text/javascript;base64,' + Buffer.from(out).toString('base64');
    return import(urls[spec]);
}
const M = '../src/shells/glass/model/';
const { STALE_MS, trackChanges, isStale } = await load('./stale', M + 'stale.ts');
const { repoRoot, workspaceId } = await load('./workspaces', M + 'workspaces.ts');
await load('./rooms', M + 'rooms.ts');
const { sortNeeds } = await load('./needs', M + 'needs.ts');
const { deriveGlassModel } = await load('./model', M + 'index.ts');
const fixture = await load('$lib/data/demo/glass', '../src/lib/data/demo/glass.ts');
const gapsDemo = await load('./gaps-demo', M + 'gaps-demo.ts');
const GAPS = { workspaces: gapsDemo.DEMO_WORKSPACES, goals: gapsDemo.DEMO_GOALS, needs: gapsDemo.DEMO_NEEDS };

const NOW = 1_800_000_000_000;
const iso = minAgo => new Date(NOW - minAgo * 60_000).toISOString();
const view = (key, path, more = {}) => ({
    sessionKey: key, id: key, projectPath: path, sessionName: key, customTitle: null, status: 'WaitingForInput',
    modified: iso(1), latestMessage: '', pendingToolName: null, waitingFor: null, ...more
});
const agent = (id, path, more = {}) => ({
    id, cwd: path, name: id, role: 'worker', parentId: null, sessionKey: null, status: 'working', state: 'working',
    goneAt: null, lastSeen: NOW - 60_000, createdAt: NOW - 3_600_000, mailPaused: false, generation: 1, ...more
});
const derive = (input) => deriveGlassModel({ sessions: [], agents: [], now: NOW, ...input });

test('two worktrees of one repo are one workspace; another repo is another', () => {
    assert.equal(repoRoot('/code/app/.worktrees/feat/src/'), '/code/app');
    assert.equal(repoRoot('/code/app/.claude/worktrees/x'), '/code/app');
    assert.match(workspaceId('/code/app'), /^ws_[0-9a-f]{8}$/);
    assert.equal(workspaceId('/code/app'), workspaceId('/code/app'));
    const m = derive({
        sessions: [view('claudeCode:a', '/code/app'), view('claudeCode:b', '/code/app/.claude/worktrees/x'), view('codex:c', '/code/other')],
        agents: [agent('agt_w', '/code/app/.worktrees/feat')]
    });
    assert.deepEqual(m.workspaces.map(w => [w.name, w.rows.length]), [['app', 3], ['other', 1]]);
    assert.equal(m.workspaces[0].id, workspaceId('/code/app'));
    assert.equal(m.workspaces[0].verifier, null);
});

test('an observed session without parentId is its own talk row, joined to its agent row once', () => {
    const m = derive({
        sessions: [view('claudeCode:t', '/code/app'), view('claudeCode:u', '/code/app')],
        agents: [agent('agt_t', '/code/app', { role: 'observed', sessionKey: 'claudeCode:t', mailPaused: true })]
    });
    const rows = m.workspaces[0].rows;
    assert.deepEqual(rows.map(r => [r.kind, r.members.length, r.lead, r.cap]), [['talk', 1, null, null], ['talk', 1, null, null]]);
    const [t, u] = rows.map(r => r.members[0]);
    assert.deepEqual([t.role, t.agentId, t.sessionKey, t.mailPaused], ['talk', 'agt_t', 'claudeCode:t', true]);
    assert.deepEqual([u.role, u.agentId, u.key], ['talk', null, 'claudeCode:u']);
});

test('a lead tree is one row; free seats count live children; a worker without its lead heads a row', () => {
    const m = derive({
        agents: [
            agent('agt_lead', '/code/app', { role: 'orch' }),
            agent('agt_w1', '/code/app', { parentId: 'agt_lead' }),
            agent('agt_w2', '/code/app', { parentId: 'agt_lead', state: 'retired', goneAt: NOW - 1000 }),
            agent('agt_orphan', '/code/app', { parentId: 'agt_missing' })
        ]
    });
    const [lead, orphan] = m.workspaces[0].rows;
    assert.deepEqual([lead.kind, lead.lead.key, lead.members.map(x => x.role), lead.freeSeats, lead.title], ['lead', 'agt_lead', ['lead', 'worker', 'worker'], 7, 'agt_lead']);
    assert.equal(lead.members[2].status, 'done');
    assert.deepEqual([orphan.kind, orphan.lead.key, orphan.members.length], ['lead', 'agt_orphan', 1]);
});

test('needs: blocked count first only when known, else longest wait first', () => {
    const n = (id, since, blocking = null) => ({ id, since, blocking });
    assert.deepEqual(sortNeeds([n('a', 30), n('b', 10), n('c', 20)]).map(x => x.id), ['b', 'c', 'a']);
    assert.deepEqual(sortNeeds([n('a', 30, 2), n('b', 10), n('c', 20, 2), n('d', 5, 1)]).map(x => x.id), ['c', 'a', 'd', 'b']);
});

test('needs: a pending tool call is 允許 with what a later answer needs', () => {
    const m = derive({
        sessions: [
            view('claudeCode:p', '/code/app', { status: 'NeedsAttention', modified: iso(2), pendingToolName: 'Bash', pendingToolInput: { command: 'npm test' }, waitingFor: { kind: 'approval', tool: 'Bash' } }),
            view('claudeCode:q', '/code/app', { status: 'NeedsAttention', modified: iso(9), waitingFor: { kind: 'question' }, pendingToolName: 'AskUserQuestion', pendingToolInput: { question: '改色？' } }),
            view('claudeCode:idle', '/code/app')
        ],
        agents: [agent('agt_p', '/code/app', { sessionKey: 'claudeCode:p', status: 'needs_attention', generation: 3 }), agent('agt_h', '/code', { role: 'human' })],
        attention: [{ event: { id: 'agt_p', generation: 3, reason: 'permission', toolUseId: 'toolu_1' }, at: NOW - 120_000 }],
        mail: [
            { msgId: 'm1', kind: 'request', to: { id: 'agt_h', addr: 'x/agt_h', name: null }, from: { kind: 'agent', addr: 'x/agt_p', name: null }, state: 'delivered', preview: 'ok?', createdAt: NOW, replyTo: null },
            { msgId: 'm2', kind: 'request', to: { id: 'agt_h', addr: 'x/agt_h', name: null }, from: { kind: 'agent', addr: 'x/agt_p', name: null }, state: 'delivered', preview: 'done', createdAt: NOW, replyTo: null },
            { msgId: 'm3', kind: 'reply', to: { id: 'agt_p' }, from: { kind: 'human' }, state: 'delivered', replyTo: 'm2', createdAt: NOW }
        ]
    });
    assert.deepEqual(m.needs.map(x => [x.verb, x.title, x.source]), [['決定', '改色？', 'session'], ['允許', 'npm test', 'session']]);
    assert.deepEqual(m.needs[1].permission, { sessionKey: 'claudeCode:p', agentId: 'agt_p', toolName: 'Bash', toolInput: { command: 'npm test' }, toolUseId: 'toolu_1', generation: 3 });
    assert.equal(m.needs[1].tag, '等你允許');
    // The mail to the human is from agt_p, which already has an item: one item per member.
    assert.equal(m.needs.filter(x => x.agentId === 'agt_p').length, 1);
    const mailOnly = derive({ agents: [agent('agt_p', '/code/app', { status: 'working' }), agent('agt_h', '/code', { role: 'human' })], mail: [
        { msgId: 'm1', kind: 'request', to: { id: 'agt_h' }, from: { addr: 'x/agt_p' }, state: 'queued', preview: 'ok?', createdAt: NOW - 5, replyTo: null }] });
    assert.deepEqual(mailOnly.needs.map(x => [x.verb, x.msgId, x.source]), [['決定', 'm1', 'mail']]);
});

test('stale: a working member with no change for 20 minutes; a waiting one never', () => {
    assert.equal(isStale(true, NOW - STALE_MS + 1, NOW), false);
    assert.equal(isStale(true, NOW - STALE_MS, NOW), true);
    assert.equal(isStale(false, NOW - 2 * STALE_MS, NOW), false);
    const first = trackChanges(new Map(), [{ key: 'a', sig: 'working', seenAt: NOW - STALE_MS }], NOW);
    assert.equal(first.get('a').at, NOW - STALE_MS);
    const same = trackChanges(first, [{ key: 'a', sig: 'working', seenAt: NOW }], NOW + 5);
    assert.equal(same.get('a').at, NOW - STALE_MS);
    assert.equal(trackChanges(same, [{ key: 'a', sig: 'needs', seenAt: NOW }], NOW + 9).get('a').at, NOW + 9);
    const sessions = [view('claudeCode:w', '/code/app', { status: 'Working', modified: iso(21) }), view('claudeCode:i', '/code/app', { modified: iso(40) })];
    const m = derive({ sessions });
    assert.deepEqual(m.workspaces[0].rows.map(r => r.members[0].stale), [true, false]);
    // A status change resets the clock even when `modified` is old.
    const later = derive({ sessions: [{ ...sessions[0], status: 'NeedsAttention' }], changes: m.changes, now: NOW + 1000 });
    assert.equal(later.workspaces[0].rows[0].members[0].lastChange, NOW + 1000);
});

test('live mode shows no demo data; demo mode adds the gaps file', () => {
    const input = { sessions: fixture.glassDemoSessionViews(), agents: fixture.glassDemoAgents(), now: Date.now() };
    const live = deriveGlassModel({ ...input, gaps: null });
    const demoIds = new Set(GAPS.workspaces.map(w => w.id));
    assert(live.workspaces.every(w => /^ws_/.test(w.id) && !demoIds.has(w.id) && w.verifier === null));
    assert.deepEqual(live.workspaces.map(w => w.name).sort(), ['c9watch', 'code', 'flockbook', 'jumpmark']);
    const rows = live.workspaces.flatMap(w => w.rows);
    assert(rows.every(r => r.goalId === null && !GAPS.goals.some(g => g.name === r.title)));
    assert(live.needs.every(n => n.source !== 'demo' && !['審', '給'].includes(n.verb)));
    // Only c9p facts: talk-main's approval and w-agents' question.
    assert.deepEqual(live.needs.map(n => n.verb).sort(), ['允許', '決定'].sort());

    const demo = deriveGlassModel({ ...input, gaps: GAPS });
    assert.deepEqual(demo.workspaces.map(w => w.id), ['c9watch', 'flockbook', 'jumpmark', 'cross']);
    assert.deepEqual(demo.needs.map(n => n.agentId).sort(), GAPS.needs.map(n => n.agentId).sort());
    assert.deepEqual(demo.needs.map(n => n.verb).sort(), GAPS.needs.map(n => n.verb).sort());
    const allow = demo.needs.find(n => n.verb === '允許');
    assert.equal(allow.permission.toolName, 'Bash');
    const c9 = demo.workspaces[0].rows;
    assert.deepEqual(c9.map(r => r.title), ['狀態用詞統一', '引擎權限重構', 'spike']);
    assert.deepEqual(c9.map(r => r.freeSeats), [1, 1, null]);
    // One row per goal, every row in the goal's workspace.
    for (const g of GAPS.goals) assert.equal(demo.workspaces.find(w => w.id === g.workspace).rows.filter(r => r.goalId === g.id).length, 1);
});
