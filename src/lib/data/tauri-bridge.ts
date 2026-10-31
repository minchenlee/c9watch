/** The only c9p IPC entry point. Legacy commands are used ONLY on the
 * explicit core_unavailable error; a denied/failed core read never widens
 * into a local filesystem read. The in-process fallback stays until v1. */
import { invoke, Channel } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Transport, StreamFrame } from './transport';
import type { C9pMethod, C9pMethods } from '../c9p-1';
import { C9pError } from './transport';

async function fallback(method: C9pMethod, p: any): Promise<any> {
    switch (method) {
        case 'session.views': return { sessions: await invoke('get_sessions'), cursor: null };
        case 'diag.detection': return invoke('get_detection_diagnostics');
        case 'conversation.get': {
            const sessionId = p.sessionKey.replace(/^(claudeCode|codex|cursor|pi|opencode):/, '');
            const c: any = await invoke('get_conversation', { sessionId, provider: p.provider, includeTools: p.includeTools });
            return { ...c, total: c.messages.length, offset: 0, nextOffset: null };
        }
        case 'session.away': return { activity: await invoke('get_away_activity', { requests: p.requests }) };
        case 'session.stop': return invoke('stop_session', { sessionKey: p.sessionKey, pid: p.pid });
        case 'session.rename': return invoke('rename_session', { sessionId: p.sessionId, newName: p.newName, provider: p.provider });
        case 'history.list': return { entries: await invoke('get_session_history'), nextOffset: null };
        case 'history.search': return { hits: await invoke('deep_search_sessions', { query: p.query, caseSensitive: p.caseSensitive, wholeWord: p.wholeWord }) };
        case 'cost.get': return invoke('get_cost_data');
        case 'memory.list': return { projects: await invoke('get_memory_files'), nextOffset: null };
        case 'subagent.list': return { subagents: await invoke('get_subagents', { sessionIds: p.sessionIds }) };
        case 'subagent.transcript': return invoke('get_subagent_transcript', { parentSessionId: p.parentSessionId, subagentId: p.subagentId });
        case 'session.tasks': return { tasks: await invoke('get_session_tasks', { sessionId: p.sessionId }) };
        case 'usage.subscriptions': return { usage: await invoke('get_subscription_usage') };
        case 'provider.opencode.status': return invoke('opencode_connection_status');
        case 'provider.opencode.connect': return invoke('opencode_connect', { url: p.url, username: p.username, password: p.password });
        case 'detect.recheck': return null; // native focus hook owns the fallback detector
        default: throw new C9pError('core_unavailable', 'Core is unavailable');
    }
}
export const tauriTransport: Transport = {
    async request<M extends C9pMethod>(method: M, params: C9pMethods[M]['params']) {
        try { return await invoke<C9pMethods[M]['result']>('c9p_request', { method, params }); }
        catch (error: any) {
            if (error?.code === 'core_unavailable') return fallback(method, params);
            if (error && typeof error.code === 'string') {
                throw Object.assign(new C9pError(error.code, error.msg ?? 'Core request failed', error.retryable), error);
            }
            throw error;
        }
    },
    async subscribe(params, onFrame) {
        let closed = false;
        const channel = new Channel<StreamFrame>();
        channel.onmessage = frame => { if (!closed) onFrame(frame); };
        let sub: string;
        try { sub = await invoke<string>('c9p_subscribe', { params, channel }); }
        catch (error) { closed = true; throw error; }
        return () => { if (!closed) { closed = true; void invoke('c9p_unsubscribe', { sub }).catch(() => {}); } };
    }
};

/** Fallback and core both publish the same complete snapshot events from Rust.
 * Webview stream consumers may also subscribe directly to raw c9p frames. */
export const listenSessions = (cb: (payload: any) => void) => listen('sessions-updated', e => cb(e.payload));
export const listenDiagnostics = (cb: (payload: any) => void) => listen('diagnostic-update', e => cb(e.payload));
export const listenProgress = (cb: (payload: any) => void) => listen('conversation-progress', e => cb(e.payload));
