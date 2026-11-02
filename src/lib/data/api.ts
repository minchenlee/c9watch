import type { Session, Conversation, AwayActivity, AwayRequest, HistoryEntry, DeepSearchHit, CostData, ProjectMemory, SessionProvider, DetectionDiagnostics } from '../types';
import { request, transport, hasSelectedTransport } from './client';
import { CapabilityDisabledError } from '../ws';
import { sessionFromView } from './session';
import { sessionKeyOf, providerSessionKey } from '../provider';
import { isDemoMode } from '../demo/mode';
import { useWebSocket } from '../ws';
import { get } from 'svelte/store';
import { localCommand } from './platform';
import { openLegacySession } from './ws-legacy';
export { CapabilityDisabledError } from '../ws';
export { getServerInfo, createPairingCode, forgetPairedClients, getRemoteAccess, setRemoteAccess, revealInFileManager, getDebugLogs } from './platform';
export type { ServerInfo, PairingCode, RemoteAccess } from './platform';

export async function getSessions(): Promise<Session[]> {
    return (await request('session.views', {})).sessions.map(sessionFromView);
}
// The wire model has open enums; Classic's retained model has closed unions.
// Keep payloads and extension values intact at this compatibility boundary.
export async function getDetectionDiagnostics(): Promise<DetectionDiagnostics | null> {
    return request('diag.detection', {}) as Promise<DetectionDiagnostics>;
}
/** Fetch every bounded page before replacing Classic's conversation/list.
 * Failure keeps the caller's last data; it never publishes a partial result. */
export async function allPages<T>(fetch: (offset: number) => Promise<{ items: T[]; nextOffset?: number | null }>): Promise<T[]> {
    const items: T[] = [];
    let offset = 0;
    for (;;) {
        const page = await fetch(offset);
        items.push(...page.items);
        const next = page.nextOffset;
        if (next == null) return items;
        if (!Number.isSafeInteger(next) || next <= offset) throw new Error('Core returned a non-advancing page');
        offset = next;
    }
}
export interface ConversationProgress { sessionId: string; provider?: SessionProvider; bytesRead: number; bytesTotal: number }
const progressListeners = new Set<(p: ConversationProgress) => void>();
/** How far each `getConversation` read has got, from the request's own
 * c9p `progress` frames. Returns the unsubscribe. */
export function onConversationProgress(listener: (p: ConversationProgress) => void): () => void {
    progressListeners.add(listener);
    return () => progressListeners.delete(listener);
}
export async function getConversation(sessionId: string, provider?: SessionProvider, includeTools = false): Promise<Conversation> {
    let resolvedProvider = provider;
    const source = transport();
    const onProgress = ({ bytesRead, bytesTotal }: { bytesRead: number; bytesTotal: number }) => {
        for (const listener of progressListeners) listener({ sessionId, provider, bytesRead, bytesTotal });
    };
    const messages = await allPages(async offset => {
        const page = await source.request('conversation.get', { sessionKey: providerSessionKey(provider, sessionId), provider, includeTools, page: { offset, limit: 1000 } }, { onProgress });
        resolvedProvider = (page.provider as SessionProvider | null | undefined) ?? resolvedProvider;
        return { items: page.messages as Conversation['messages'], nextOffset: page.nextOffset };
    });
    return { sessionId, provider: resolvedProvider, messages };
}
export async function getAwayActivity(requests: AwayRequest[]): Promise<Record<string, AwayActivity>> {
    return requests.length ? (await request('session.away', { requests })).activity : {};
}
export async function stopSession(session: Session): Promise<void> {
    await request('session.stop', { sessionKey: sessionKeyOf(session), pid: session.pid });
}
export async function openSession(session: Session): Promise<void> {
    if (get(isDemoMode)) return;
    if (hasSelectedTransport()) throw new CapabilityDisabledError('open', 'Opening session windows requires the desktop client');
    if (useWebSocket()) return openLegacySession(sessionKeyOf(session), session.pid);
    await localCommand('open_session', { pid: session.pid, projectPath: session.projectPath });
}
export async function renameSession(sessionId: string, newName: string, provider?: SessionProvider): Promise<void> {
    await request('session.rename', { sessionId, newName, provider });
}
export async function getSessionHistory(): Promise<HistoryEntry[]> {
    const source = transport();
    return allPages(async offset => {
        const p = await source.request('history.list', { page: { offset, limit: 1000 } });
        return { items: p.entries as HistoryEntry[], nextOffset: p.nextOffset };
    });
}
export async function deepSearchSessions(query: string, caseSensitive = false, wholeWord = false): Promise<DeepSearchHit[]> {
    return (await request('history.search', { query, caseSensitive, wholeWord })).hits as DeepSearchHit[];
}
export async function getCostData(): Promise<CostData | null> { return request('cost.get', {}) as Promise<CostData | null>; }
export async function getMemoryFiles(): Promise<ProjectMemory[]> {
    const source = transport();
    return allPages(async offset => {
        const p = await source.request('memory.list', { page: { offset, limit: 1000 } });
        return { items: p.projects as ProjectMemory[], nextOffset: p.nextOffset };
    });
}
export async function getSubscriptionUsage(): Promise<import('../subscription-usage').SubscriptionUsage[]> {
    return (await request('usage.subscriptions', {})).usage;
}
export async function getSubagents<T>(sessionIds: string[]): Promise<Record<string, T[]>> {
    return (await request('subagent.list', { sessionIds })).subagents as Record<string, T[]>;
}
export async function getSubagentTranscript<T>(parentSessionId: string, subagentId: string): Promise<T> {
    return request('subagent.transcript', { parentSessionId, subagentId }) as Promise<T>;
}
export async function getSessionTasks(sessionId: string): Promise<unknown[]> {
    return (await request('session.tasks', { sessionId })).tasks;
}
export async function getOpenCodeStatus<T>(): Promise<T> {
    return request('provider.opencode.status', {}) as Promise<T>;
}
export async function connectOpenCode(url: string, username: string, password: string | null): Promise<void> {
    await request('provider.opencode.connect', { url, username, password });
}
/** Native-only actions keep their explicit command map; data goes through c9p. */
export const dataCommand = localCommand;
