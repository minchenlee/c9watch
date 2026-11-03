import type { C9pMethod } from '../c9p-1';
import type { Transport } from './transport';
import { getDemoSessions, getDemoHistoryEntries, getDemoCostData, demoConversations, getDemoAwayActivity, getDemoDiagnostics } from '../demo/data';
import { demoSubscriptionUsage } from '../subscription-usage';
import { demoMailRequest, demoMailSubscribe } from './demo/mail';
/** Demo mail is opt-in (`localStorage['c9watch.demoMail'] = '1'`). Without
 * it, demo answers mail like today's Classic bridge, so demo screens do not
 * change until the bridge exposes mail. */
const DEMO_MAIL_KEY = 'c9watch.demoMail';
function demoMailOn(): boolean {
    try { return globalThis.localStorage?.getItem(DEMO_MAIL_KEY) === '1'; } catch { return false; }
}
const mailDenied = () => Object.assign(new Error('Mail is not exposed in demo mode'), { code: 'not_allowed', msg: 'Mail is not exposed in demo mode' });
export const demoTransport: Transport = {
    async request(method: C9pMethod, p: any): Promise<any> {
        switch (method) {
            case 'session.views': return { sessions: getDemoSessions(), cursor: null };
            case 'diag.detection': return getDemoDiagnostics();
            case 'conversation.get': {
                const id = p.sessionKey.replace(/^(claudeCode|codex|cursor|pi|opencode):/, '');
                return { ...(demoConversations[id] ?? { sessionId: id, messages: [] }), nextOffset: null };
            }
            case 'session.away': return { activity: getDemoAwayActivity(p.requests) };
            case 'history.list': return { entries: getDemoHistoryEntries(), nextOffset: null };
            case 'history.search': return { hits: [] };
            case 'cost.get': return getDemoCostData();
            case 'memory.list': return { projects: [], nextOffset: null };
            case 'usage.subscriptions': return { usage: demoSubscriptionUsage() };
            case 'subagent.list': return { subagents: {} };
            case 'session.tasks': return { tasks: [] };
            case 'msg.list': case 'msg.read': case 'msg.cancel': case 'msg.send':
            case 'mail.pause': case 'mail.resume':
                if (!demoMailOn()) throw mailDenied();
                return demoMailRequest(method, p);
            case 'agent.list': return demoMailOn() ? demoMailRequest(method, p) : { agents: [] };
            default: return null;
        }
    },
    async subscribe(params, onFrame) {
        if (!params.topics?.includes('msg.state')) return () => {};
        if (!demoMailOn()) throw mailDenied();
        let n = 0;
        return demoMailSubscribe(data => onFrame({ v: 1, type: 'event', sub: 'demo-mail', topic: 'msg.state', subject: data.to, cursor: `demo:mail:${++n}`, ts: data.at, data }));
    }
};
