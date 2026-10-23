import { get, writable, type Writable } from 'svelte/store';
import { listen } from '@tauri-apps/api/event';
import { getConversation } from '$lib/api';
import { isTauri, useWebSocket, wsClient } from '$lib/ws';
import { providerSessionKey } from '$lib/provider';
import type { Conversation, SessionProvider } from '$lib/types';

export type ConversationLoadKind = 'conversation' | 'tools';

export interface ConversationLoadState {
	sessionId: string;
	provider?: SessionProvider;
	kind: ConversationLoadKind;
	bytesRead: number;
	bytesTotal: number;
	generation: number;
}

export interface ConversationProgressEvent {
	sessionId: string;
	provider?: SessionProvider;
	bytesRead: number;
	bytesTotal: number;
}

export const conversationLoad = writable<ConversationLoadState | null>(null);

/** Session whose in-memory conversation already includes tool dumps. */
export const toolsLoadedFor = writable<string | null>(null);

let progressListenerStarted = false;
let loadGeneration = 0;

export function conversationLoadPercent(state: ConversationLoadState | null): number | null {
	if (!state || state.bytesTotal <= 0) return null;
	return Math.min(100, Math.floor((state.bytesRead / state.bytesTotal) * 100));
}

export function conversationLoadLabel(state: ConversationLoadState | null): string | null {
	if (!state) return null;
	const kind = state.kind === 'tools' ? 'LOADING TOOLS' : 'LOADING';
	const percent = conversationLoadPercent(state);
	return percent == null ? kind : `${kind} · ${percent}%`;
}

function sameConversation(
	a: { sessionId: string; provider?: SessionProvider },
	b: { sessionId: string; provider?: SessionProvider }
): boolean {
	return providerSessionKey(a.provider, a.sessionId) === providerSessionKey(b.provider, b.sessionId);
}

export function isSessionLoading(
	sessionId: string,
	provider: SessionProvider | undefined,
	state: ConversationLoadState | null
): boolean {
	return !!state && sameConversation({ sessionId, provider }, state);
}

export function isToolsLoading(
	sessionId: string,
	provider: SessionProvider | undefined,
	state: ConversationLoadState | null
): boolean {
	return !!state && state.kind === 'tools' && sameConversation({ sessionId, provider }, state);
}

function applyProgress(payload: ConversationProgressEvent) {
	conversationLoad.update((state) => {
		if (!state || !sameConversation(payload, state)) return state;
		return {
			...state,
			bytesRead: payload.bytesRead,
			bytesTotal: payload.bytesTotal
		};
	});
}

export async function initConversationProgressListener() {
	if (progressListenerStarted) return;
	progressListenerStarted = true;

	if (useWebSocket()) {
		wsClient.on('conversationProgress', applyProgress);
		return;
	}
	if (isTauri()) {
		await listen<ConversationProgressEvent>('conversation-progress', (event) => {
			applyProgress(event.payload);
		});
	}
}

export async function withConversationLoader<T>(
	sessionId: string,
	provider: SessionProvider | undefined,
	kind: ConversationLoadKind,
	task: () => Promise<T>
): Promise<T> {
	const generation = ++loadGeneration;
	conversationLoad.set({ sessionId, provider, kind, bytesRead: 0, bytesTotal: 0, generation });
	try {
		return await task();
	} finally {
		if (get(conversationLoad)?.generation === generation) {
			conversationLoad.set(null);
		}
	}
}

export interface PreviewTarget {
	sessionId: string;
	provider?: SessionProvider;
}

/** True when a conversation pushed by the preview overlay belongs to the open entry. */
export function acceptsConversation(
	entry: PreviewTarget | null,
	conv: Pick<Conversation, 'sessionId' | 'provider'>
): boolean {
	return (
		!!entry &&
		providerSessionKey(entry.provider, entry.sessionId) ===
			providerSessionKey(conv.provider, conv.sessionId)
	);
}

export interface PreviewLoaderDeps {
	getConversation: typeof getConversation;
	withConversationLoader: typeof withConversationLoader;
	toolsLoadedFor: Writable<string | null>;
}

const defaultPreviewLoaderDeps: PreviewLoaderDeps = {
	getConversation,
	withConversationLoader,
	toolsLoadedFor
};

/**
 * Load the conversation for a HISTORY / COST preview overlay without tool
 * dumps. `apply` runs only when the response is still wanted: the request is
 * still current (`isCurrent`), the overlay has not already loaded tools for
 * this session, and the response is for the requested session.
 */
export async function loadPreviewConversation(
	entry: PreviewTarget,
	isCurrent: () => boolean,
	apply: (conv: Conversation) => void,
	deps: PreviewLoaderDeps = defaultPreviewLoaderDeps
): Promise<void> {
	deps.toolsLoadedFor.set(null);
	try {
		const requestedKey = providerSessionKey(entry.provider, entry.sessionId);
		const conv = await deps.withConversationLoader(entry.sessionId, entry.provider, 'conversation', () =>
			deps.getConversation(entry.sessionId, entry.provider, false)
		);
		if (!isCurrent()) return;
		if (get(deps.toolsLoadedFor) === requestedKey) return;
		if (providerSessionKey(conv.provider ?? entry.provider, conv.sessionId) !== requestedKey) return;
		apply(conv);
	} catch (e) {
		if (isCurrent()) console.error('Failed to load conversation:', e);
	}
}

/**
 * Load the conversation with tool dumps for a conversation overlay's
 * "Show Tools" toggle. `apply` runs, and `toolsLoadedFor` is set, only when
 * the overlay still wants the response (`isCurrent`: not disposed and still
 * showing the requested session) and the response is for the requested
 * session. Returns true when the tools were applied.
 */
export async function loadToolsConversation(
	target: PreviewTarget,
	isCurrent: () => boolean,
	apply: (conv: Conversation) => void,
	deps: PreviewLoaderDeps = defaultPreviewLoaderDeps
): Promise<boolean> {
	const requestedKey = providerSessionKey(target.provider, target.sessionId);
	try {
		const conv = await deps.withConversationLoader(target.sessionId, target.provider, 'tools', () =>
			deps.getConversation(target.sessionId, target.provider, true)
		);
		if (!isCurrent() ||
			providerSessionKey(conv.provider ?? target.provider, conv.sessionId) !== requestedKey
		) return false;
		apply(conv);
		deps.toolsLoadedFor.set(requestedKey);
		return true;
	} catch (error) {
		console.error('Failed to load tools:', error);
		return false;
	}
}
