import type { C9pMethod } from '../c9p-1';
import type { Transport } from './transport';
import { getDemoSessions, getDemoHistoryEntries, getDemoCostData, demoConversations, getDemoAwayActivity, getDemoDiagnostics } from '../demo/data';
import { demoSubscriptionUsage } from '../subscription-usage';
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
            default: return null;
        }
    },
    async subscribe() { return () => {}; }
};
