/** Compatibility adapter: keep Classic's remote capabilities and response shapes. */
import { wsClient } from '../ws';
import type { C9pMethod } from '../c9p-1';
import type { Transport } from './transport';
import { C9pError } from './transport';
async function request(method: C9pMethod, p: any): Promise<any> {
    switch (method) {
        case 'session.views': return { sessions: await wsClient.request('getSessions'), cursor: null };
        case 'session.away': return { activity: await wsClient.request('getAwayActivity', { requests: p.requests }) };
        case 'conversation.get': {
            const sessionId = p.sessionKey.replace(/^(claudeCode|codex|cursor|pi|opencode):/, '');
            const c = await wsClient.request('getConversation', { sessionId, provider: p.provider, includeTools: p.includeTools });
            return { ...c, nextOffset: null };
        }
        case 'session.stop': return wsClient.request('session.stop', { addr: p.sessionKey, pid: p.pid });
        case 'session.rename': return wsClient.request('renameSession', { sessionId: p.sessionId, newName: p.newName, provider: p.provider });
        case 'usage.subscriptions': return { usage: await wsClient.request('getSubscriptionUsage') };
        case 'history.list': return { entries: [], nextOffset: null };
        case 'history.search': return { hits: [] };
        case 'memory.list': return { projects: [], nextOffset: null };
        case 'diag.detection': case 'cost.get': return null;
        case 'subagent.list': return { subagents: {} };
        case 'session.tasks': return { tasks: [] };
        default: throw new C9pError('not_allowed', `${method} is unavailable over legacy WebSocket`);
    }
}
export const legacyTransport: Transport = {
    request,
    async subscribe(_params, onFrame) {
        const sessions = (data: any) => onFrame({ type: 'event', v: 1, topic: 'legacy.sessions', data, cursor: '', ts: Date.now() });
        wsClient.on('sessionsUpdated', sessions);
        return () => wsClient.off('sessionsUpdated', sessions);
    }
};
export async function openLegacySession(addr: string, pid: number) {
    await wsClient.request('openSession', { addr, pid });
}
