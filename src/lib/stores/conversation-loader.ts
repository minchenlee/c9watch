import { get, writable } from 'svelte/store';
import { listen } from '@tauri-apps/api/event';
import { isTauri, useWebSocket, wsClient } from '$lib/ws';
import { providerSessionKey } from '$lib/provider';
import type { SessionProvider } from '$lib/types';

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
