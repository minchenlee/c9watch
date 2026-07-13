import type { HistoryEntry, Session, SessionCostRecord, SessionProvider, SessionSurface } from './types';

export type ProviderFilter = 'all' | SessionProvider;
export type SessionAction = 'open' | 'stop' | 'rename' | 'conversation';
type ProviderRecord = Pick<Session, 'provider'> | Pick<HistoryEntry, 'provider'> | Pick<SessionCostRecord, 'provider'>;

export function providerOf(record: ProviderRecord): SessionProvider {
	return record.provider === 'codex' ? 'codex' : 'claudeCode';
}

export function providerLabel(provider: SessionProvider): string {
	return provider === 'codex' ? 'CODEX' : 'CLAUDE CODE';
}

export function surfaceLabel(surface?: SessionSurface): string | null {
	if (!surface || surface === 'unknown' || surface === 'claudeCode') return null;
	return surface.toUpperCase();
}

export function matchesProvider(record: ProviderRecord, filter: ProviderFilter): boolean {
	return filter === 'all' || providerOf(record) === filter;
}

export function providerFilterLabel(filter: ProviderFilter): string {
	if (filter === 'codex') return 'Codex';
	if (filter === 'claudeCode') return 'Claude Code';
	return 'All providers';
}

export function isHiddenInternalSession(session: Session): boolean {
	if (providerOf(session) !== 'codex') return false;
	if (session.agentKind === 'internal') return true;
	const kind = session.internalKind?.toLowerCase() ?? '';
	return kind.includes('guardian') || kind.includes('review');
}

export function isCodexSubagent(session: Session): boolean {
	return providerOf(session) === 'codex' && session.agentKind === 'subagent' && !isHiddenInternalSession(session);
}

export function codexParentId(session: Session): string | null {
	return session.parentThreadId ?? session.rootSessionId ?? null;
}

export function isTopLevelSession(session: Session): boolean {
	return !isHiddenInternalSession(session) && !isCodexSubagent(session);
}

export function canSessionAction(session: Session, action: SessionAction): boolean {
	const directCapability = {
		open: session.canOpen,
		stop: session.canStop,
		rename: session.canRename,
		conversation: undefined
	}[action];
	if (typeof directCapability === 'boolean') return directCapability;

	const caps = session.actionCapabilities ?? session.capabilities;
	const aliases = {
		open: ['open', 'canOpen'],
		stop: ['stop', 'canStop'],
		rename: ['rename', 'canRename'],
		conversation: ['conversation', 'canReadConversation']
	} as const;
	for (const key of aliases[action]) {
		const explicit = caps?.[key];
		if (typeof explicit === 'boolean') return explicit;
	}
	if (providerOf(session) === 'claudeCode') return true;
	return action === 'conversation';
}
