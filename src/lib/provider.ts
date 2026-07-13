import type { Session, SessionProvider, SessionSurface } from './types';

export type ProviderFilter = 'all' | SessionProvider;
export type SessionAction = 'open' | 'stop' | 'rename' | 'conversation';
type ProviderRecord = { provider?: SessionProvider | null };

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

export interface CodexHierarchy {
	topLevelIds: Set<string>;
	subagentsByParent: Map<string, Session[]>;
}

/**
 * Flatten visible Codex descendants beneath their nearest visible root.
 * If every referenced ancestor has expired or is missing, the highest
 * surviving subagent is promoted so no normal agent disappears from the UI.
 */
export function resolveCodexHierarchy(sessions: Session[]): CodexHierarchy {
	const byId = new Map(sessions.map((session) => [session.id, session]));
	const topLevelIds = new Set(
		sessions
			.filter((session) => !isHiddenInternalSession(session) && !isCodexSubagent(session))
			.map((session) => session.id)
	);
	const subagentsByParent = new Map<string, Session[]>();

	function nextExistingAncestor(session: Session): Session | null {
		for (const id of [session.parentThreadId, session.rootSessionId]) {
			if (!id || id === session.id) continue;
			const ancestor = byId.get(id);
			if (ancestor) return ancestor;
		}
		return null;
	}

	for (const session of sessions) {
		if (!isCodexSubagent(session)) continue;

		let cursor = session;
		let anchor: Session = session;
		const visited = new Set([session.id]);
		while (true) {
			const ancestor = nextExistingAncestor(cursor);
			if (!ancestor || visited.has(ancestor.id)) break;
			visited.add(ancestor.id);
			if (!isHiddenInternalSession(ancestor) && !isCodexSubagent(ancestor)) {
				anchor = ancestor;
				break;
			}
			if (isCodexSubagent(ancestor)) anchor = ancestor;
			cursor = ancestor;
		}

		if (anchor.id === session.id) {
			topLevelIds.add(session.id);
			continue;
		}
		const group = subagentsByParent.get(anchor.id) ?? [];
		group.push(session);
		subagentsByParent.set(anchor.id, group);
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
