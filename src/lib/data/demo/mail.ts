/** Demo mailbox for the MAIL tab: three engine agents tied to demo sessions,
 * one mail in each delivery state, and a send path that walks a new mail
 * through `queued → delivering → delivered`. */
import type { C9pAgentRow, C9pMsgAddress, C9pMsgFrom, C9pMsgReadResult, C9pMsgStateEvent, C9pMsgSummary } from '../../c9p-1';

const MACHINE = 'm_demo';
type DemoAgent = { id: string; name: string; sessionKey: string };
const AGENTS: DemoAgent[] = [
    { id: 'agt_demo_pm', name: 'pm-web', sessionKey: 'claudeCode:demo-1' },
    { id: 'agt_demo_api', name: 'worker-api', sessionKey: 'claudeCode:demo-3' },
    { id: 'agt_demo_rev', name: 'reviewer', sessionKey: 'claudeCode:demo-5' }
];
const [PM, API, REV] = AGENTS;
const to = (a: DemoAgent): C9pMsgAddress => ({ id: a.id, name: a.name, addr: `${MACHINE}/${a.id}` });
const agentFrom = (a: DemoAgent): C9pMsgFrom => ({ kind: 'agent', addr: `${MACHINE}/${a.id}`, name: a.name });
const HUMAN: C9pMsgFrom = { kind: 'human', addr: null, name: null };
const ENGINE: C9pMsgFrom = { kind: 'engine', addr: null, name: null };

type Row = C9pMsgSummary & { body: string };
let rows: Row[] = [];
let seq = 0;
const listeners = new Set<(e: C9pMsgStateEvent) => void>();

function row(id: string, minutesAgo: number, from: C9pMsgFrom, target: DemoAgent, body: string, more: Partial<C9pMsgSummary> = {}): Row {
    const at = Date.now() - minutesAgo * 60_000;
    return {
        msgId: id, from, to: to(target), kind: 'request', state: 'delivered', stateAt: at + 4000, createdAt: at,
        detail: 'prompt_hook', via: from.kind === 'agent' ? 'socket' : 'paste', replyTo: null, threadId: id,
        noticeId: null, hop: from.kind === 'agent' ? 1 : 0, preview: body.slice(0, 200), body, ...more
    };
}
function seed(): Row[] {
    return [
        row('msg_demo_01', 42, HUMAN, PM, 'Plan the OAuth2 refactor. Split it into an API part and a review part, and send each to a worker.'),
        row('msg_demo_02', 38, agentFrom(PM), API, 'Implement refresh-token rotation in api-server/src/auth. Keep the old tokens valid for 5 minutes. Reply with the test output.'),
        row('msg_demo_03', 21, agentFrom(API), PM, 'Done. Rotation is in place; 48 tests pass. One open point: the 5-minute grace window needs a config flag.', { kind: 'result', replyTo: 'msg_demo_02', threadId: 'msg_demo_02' }),
        row('msg_demo_04', 15, agentFrom(PM), REV, 'Review the refresh-token rotation diff on branch feature/oauth-rotation.', { state: 'uncertain', detail: 'paste not seen in input box', via: 'paste' }),
        row('msg_demo_05', 14, ENGINE, PM, 'Your mail to reviewer is uncertain: the paste was not seen in its input box. It was not sent again.', { kind: 'system', noticeId: 'msg_demo_04', via: 'paste', hop: 0 }),
        row('msg_demo_06', 9, agentFrom(PM), API, 'Add the grace-window config flag and its test.', { state: 'delivering', detail: 'held: waiting for the permission prompt' }),
        row('msg_demo_07', 6, HUMAN, API, 'Also log each rotation at debug level.', { state: 'failed', detail: 'agent exited before delivery' }),
        row('msg_demo_08', 2, HUMAN, REV, 'When you are free, check the CSRF middleware too.', { state: 'queued', detail: 'reviewer is working', via: null })
    ];
}
rows = seed();

function summary(r: Row): C9pMsgSummary {
    const { body: _body, ...s } = r;
    return { ...s };
}
function emit(r: Row) {
    const e: C9pMsgStateEvent = {
        msgId: r.msgId, from: r.from.kind === 'agent' ? `agent:${agentIdOf(r)}` : 'local-unverified', to: r.to.id,
        state: r.state, at: r.stateAt, attempts: 1, detail: r.detail, via: r.via
    };
    if (r.state === 'queued') Object.assign(e, { sender: r.from, recipient: r.to, threadId: r.threadId, replyTo: r.replyTo, kind: r.kind, preview: r.preview });
    for (const l of listeners) l(e);
}
function agentIdOf(r: Row) { return r.from.addr?.split('/').pop() ?? ''; }
function setState(r: Row, state: string, detail: string | null, via: string | null = r.via) {
    r.state = state; r.detail = detail; r.via = via; r.stateAt = Date.now();
    emit(r);
}
function resolve(agent: string): DemoAgent {
    const a = AGENTS.find(x => x.id === agent || x.name === agent || `${MACHINE}/${x.id}` === agent);
    if (!a) throw Object.assign(new Error(`unknown agent ${agent}`), { code: 'unknown_agent', msg: `unknown agent ${agent}` });
    return a;
}

export function demoMailSubscribe(listener: (e: C9pMsgStateEvent) => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
}
export function demoMailRequest(method: string, p: any): unknown {
    switch (method) {
        case 'msg.list': {
            const sorted = [...rows].sort((a, b) => b.createdAt - a.createdAt);
            const start = p?.before ? sorted.findIndex(r => r.msgId === p.before) + 1 : 0;
            const limit = p?.limit ?? 50;
            const page = sorted.slice(start, start + limit);
            return { items: page.map(summary), next: start + limit < sorted.length ? page[page.length - 1].msgId : null };
        }
        case 'msg.read': {
            const r = rows.find(x => x.msgId === p.msgId);
            if (!r) throw Object.assign(new Error('unknown message'), { code: 'bad_request', msg: 'unknown message' });
            const read: C9pMsgReadResult = { ...summary(r), body: r.body, attempts: 1, provenance: '', status: [{ at: r.stateAt, state: r.state, detail: r.detail }] };
            return read;
        }
        case 'msg.cancel': {
            const r = rows.find(x => x.msgId === p.msgId);
            if (!r || r.state !== 'queued') throw Object.assign(new Error('only queued mail can be cancelled'), { code: 'not_cancellable', msg: 'only queued mail can be cancelled' });
            setState(r, 'cancelled', null);
            return { ok: true };
        }
        case 'mail.pause': case 'mail.resume': resolve(p.agent); return { ok: true };
        case 'msg.send': {
            const a = resolve(p.to);
            const r = row(`msg_demo_new_${++seq}`, 0, HUMAN, a, p.body, { state: 'queued', detail: null, via: null });
            r.stateAt = r.createdAt;
            rows.push(r);
            emit(r);
            setTimeout(() => setState(r, 'delivering', null, 'paste'), 1200);
            setTimeout(() => setState(r, 'delivered', 'prompt_hook', 'paste'), 2600);
            return { msgId: r.msgId, cursor: `demo:${seq}`, replayed: false, state: 'queued', to: to(a) };
        }
        case 'agent.list': {
            const agents: Partial<C9pAgentRow>[] = AGENTS.map(a => ({ id: a.id, name: a.name, sessionKey: a.sessionKey, control: 'pty', role: 'worker', provider: 'claude', source: 'engine', state: 'ready' }));
            return { agents };
        }
    }
    return undefined;
}
