import type { Session, SessionProvider, SessionSurface } from './types';

export type ProviderFilter = 'all' | SessionProvider;
export type SessionAction = 'open' | 'stop' | 'rename' | 'conversation';
type ProviderRecord = { provider?: SessionProvider | null };

const KNOWN_PROVIDERS: SessionProvider[] = ['claudeCode', 'codex', 'cursor'];

export function providerOf(record: ProviderRecord): SessionProvider {
	return record.provider && KNOWN_PROVIDERS.includes(record.provider)
		? record.provider
		: 'claudeCode';
}

/** Build the identity used by maps and UI selection instead of raw IDs. */
export function providerSessionKey(provider: SessionProvider | undefined, id: string): string {
	const normalized = provider && KNOWN_PROVIDERS.includes(provider) ? provider : 'claudeCode';
	return `${normalized}:${id}`;
}

export function sessionKeyOf(session: Pick<Session, 'id' | 'provider' | 'sessionKey'>): string {
	// Derive the key from the canonical provider/id pair.  Do not trust a
	// stale or malformed serialized sessionKey: it is a compatibility field,
	// while provider + id is the identity boundary that prevents collisions.
	return providerSessionKey(providerOf(session), session.id);
}

export function providerLabel(provider: SessionProvider): string {
	if (provider === 'codex') return 'CODEX';
	if (provider === 'cursor') return 'CURSOR';
	return 'CLAUDE CODE';
}

export function surfaceLabel(surface?: SessionSurface): string | null {
	if (!surface || surface === 'unknown' || surface === 'claudeCode' || surface === 'cursor') {
		return null;
	}
	return surface.toUpperCase();
}

export function matchesProvider(record: ProviderRecord, filter: ProviderFilter): boolean {
	return filter === 'all' || providerOf(record) === filter;
}

export function providerFilterLabel(filter: ProviderFilter): string {
	if (filter === 'codex') return 'Codex';
	if (filter === 'claudeCode') return 'Claude Code';
	if (filter === 'cursor') return 'Cursor';
	return 'All providers';
}

export function isHiddenInternalSession(session: Session): boolean {
	if (providerOf(session) !== 'codex') return false;
	if (session.agentKind === 'internal') return true;
	const kind = session.internalKind?.toLowerCase() ?? '';
	return kind.includes('guardian') || kind.includes('review');
}

export function isCodexSubagent(session: Session): boolean {
	const provider = providerOf(session);
	return (provider === 'codex' || provider === 'cursor') && session.agentKind === 'subagent' && !isHiddenInternalSession(session);
}

export interface CodexHierarchy {
	topLevelIds: Set<string>;
	subagentsByParent: Map<string, Session[]>;
}

/**
 * Flatten visible Codex/Cursor descendants beneath their nearest visible root.
 * If every referenced ancestor has expired or is missing, the highest
 * surviving subagent is promoted so no normal agent disappears from the UI.
 */
export function resolveCodexHierarchy(sessions: Session[]): CodexHierarchy {
	const byId = new Map(sessions.map((session) => [sessionKeyOf(session), session]));
	const topLevelIds = new Set(
		sessions
			.filter((session) => !isHiddenInternalSession(session) && !isCodexSubagent(session))
			.map((session) => sessionKeyOf(session))
	);
	const subagentsByParent = new Map<string, Session[]>();

	function nextExistingAncestor(session: Session): Session | null {
		for (const id of [session.parentThreadId, session.rootSessionId]) {
			if (!id || id === session.id) continue;
			const ancestor = byId.get(providerSessionKey(providerOf(session), id));
			if (ancestor) return ancestor;
		}
		return null;
	}

	for (const session of sessions) {
		if (!isCodexSubagent(session)) continue;

		let cursor = session;
		let anchor: Session = session;
		const visited = new Set([sessionKeyOf(session)]);
		while (true) {
			const ancestor = nextExistingAncestor(cursor);
			if (!ancestor || visited.has(sessionKeyOf(ancestor))) break;
			visited.add(sessionKeyOf(ancestor));
			if (!isHiddenInternalSession(ancestor) && !isCodexSubagent(ancestor)) {
				anchor = ancestor;
				break;
			}
			if (isCodexSubagent(ancestor)) anchor = ancestor;
			cursor = ancestor;
		}

		if (sessionKeyOf(anchor) === sessionKeyOf(session)) {
			topLevelIds.add(sessionKeyOf(session));
			continue;
		}
		const group = subagentsByParent.get(sessionKeyOf(anchor)) ?? [];
		group.push(session);
		subagentsByParent.set(sessionKeyOf(anchor), group);
	}

	return { topLevelIds, subagentsByParent };
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
