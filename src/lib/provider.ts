import type { Session, SessionProvider, SessionSurface } from './types';

export type ProviderFilter = 'all' | SessionProvider;
export type SessionAction = 'open' | 'stop' | 'rename' | 'conversation';
type ProviderRecord = { provider?: SessionProvider | null };

/**
 * The single provider registry, in display order. `label` is the uppercase
 * badge text; `filterLabel` is the title-case name used by the provider filter.
 */
export const PROVIDERS = [
	{ id: 'claudeCode', label: 'CLAUDE CODE', filterLabel: 'Claude Code' },
	{ id: 'codex', label: 'CODEX', filterLabel: 'Codex' },
	{ id: 'cursor', label: 'CURSOR', filterLabel: 'Cursor' },
	{ id: 'pi', label: 'PI', filterLabel: 'Pi' },
	{ id: 'opencode', label: 'OPENCODE', filterLabel: 'OpenCode' }
] as const satisfies ReadonlyArray<{ id: SessionProvider; label: string; filterLabel: string }>;

const KNOWN_PROVIDERS: readonly string[] = PROVIDERS.map((p) => p.id);
export const ALL_PROVIDERS_LABEL = 'All providers';

export function isKnownProvider(value: unknown): value is SessionProvider {
	return typeof value === 'string' && KNOWN_PROVIDERS.includes(value);
}

function providerEntry(provider: string) {
	return PROVIDERS.find((p) => p.id === provider);
}

export function providerOf(record: ProviderRecord): SessionProvider {
	return isKnownProvider(record.provider) ? record.provider : 'claudeCode';
}

/** Build the identity used by maps and UI selection instead of raw IDs. */
export function providerSessionKey(provider: SessionProvider | undefined, id: string): string {
	const normalized = isKnownProvider(provider) ? provider : 'claudeCode';
	return `${normalized}:${id}`;
}

export function sessionKeyOf(session: Pick<Session, 'id' | 'provider' | 'sessionKey'>): string {
	// Derive the key from the canonical provider/id pair.  Do not trust a
	// stale or malformed serialized sessionKey: it is a compatibility field,
	// while provider + id is the identity boundary that prevents collisions.
	return providerSessionKey(providerOf(session), session.id);
}

export function providerLabel(provider: SessionProvider): string {
	return (providerEntry(provider) ?? PROVIDERS[0]).label;
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
	return providerEntry(filter)?.filterLabel ?? ALL_PROVIDERS_LABEL;
}

export function isHiddenInternalSession(session: Session): boolean {
	if (providerOf(session) !== 'codex') return false;
	if (session.agentKind === 'internal') return true;
	const kind = session.internalKind?.toLowerCase() ?? '';
	return kind.includes('guardian') || kind.includes('review');
}

export function isCodexSubagent(session: Session): boolean {
	const provider = providerOf(session);
	return (provider === 'codex' || provider === 'cursor' || provider === 'opencode') && session.agentKind === 'subagent' && !isHiddenInternalSession(session);
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

/** Exact native return requires a backend-declared tty mechanism. */
export function sessionReturnKind(session: Session): 'native' | 'project' | 'application' | 'conversation' {
	if (!canSessionAction(session, 'open')) return 'conversation';
	switch (session.openTarget) {
		case 'terminal': return 'native';
		case 'project': return 'project';
		case 'application': return 'application';
		default: return 'conversation';
	}
}
