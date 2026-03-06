/**
 * API wrapper — automatically dispatches to Tauri IPC or WebSocket
 * depending on the runtime environment.
 */

import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import type { Session, Conversation, HistoryEntry, DeepSearchHit, CostData, ProjectMemory, LogEntry } from './types';
import { isDemoMode } from './demo/mode';
import { getDemoSessions, demoConversations } from './demo/data';
import { wsClient, useWebSocket } from './ws';

/**
 * Get all active Claude Code sessions
 */
export async function getSessions(): Promise<Session[]> {
	if (get(isDemoMode)) return getDemoSessions();

	if (useWebSocket()) {
		return await wsClient.request<Session[]>('getSessions');
	}
	return await invoke<Session[]>('get_sessions');
}

/**
 * Get the full conversation history for a specific session
 */
export async function getConversation(sessionId: string): Promise<Conversation> {
	if (get(isDemoMode)) {
		return demoConversations[sessionId] ?? { sessionId, messages: [] };
	}

	if (useWebSocket()) {
		return await wsClient.request<Conversation>('getConversation', { sessionId });
	}
	return await invoke<Conversation>('get_conversation', { sessionId });
}

/**
 * Stop a running session by terminating the process
 * On Unix: Sends SIGTERM for graceful termination
 * On Windows: Uses taskkill for process termination
 */
export async function stopSession(pid: number): Promise<void> {
	if (get(isDemoMode)) return;

	if (useWebSocket()) {
		await wsClient.request('stopSession', { pid });
		return;
	}
	await invoke<void>('stop_session', { pid });
}

/**
 * Open the terminal or IDE window for a session
 */
export async function openSession(pid: number, projectPath: string): Promise<void> {
	if (get(isDemoMode)) return;

	if (useWebSocket()) {
		await wsClient.request('openSession', { pid, projectPath });
		return;
	}
	await invoke<void>('open_session', { pid, projectPath });
}

/**
 * Rename a session title
 */
export async function renameSession(sessionId: string, newName: string): Promise<void> {
	if (get(isDemoMode)) return;

	if (useWebSocket()) {
		await wsClient.request('renameSession', { sessionId, newName });
		return;
	}
	await invoke<void>('rename_session', { sessionId, newName });
}

/**
 * Server connection info (desktop/Tauri only)
 */
export interface ServerInfo {
	token: string;
	port: number;
	localIp: string;
	wsUrl: string;
}

export async function getServerInfo(): Promise<ServerInfo> {
	return await invoke<ServerInfo>('get_server_info');
}

/**
 * Get all inactive session history from ~/.claude/history.jsonl
 * (Desktop/Tauri only — returns empty array on mobile/browser)
 */
export async function getSessionHistory(): Promise<HistoryEntry[]> {
	if (get(isDemoMode)) return [];
	if (useWebSocket()) return [];
	return await invoke<HistoryEntry[]>('get_session_history');
}

/**
 * Deep search session JSONL files for a query string.
 * Returns session IDs of matching sessions.
 * (Desktop/Tauri only — returns empty array on mobile/browser)
 */
export async function deepSearchSessions(query: string): Promise<DeepSearchHit[]> {
	if (get(isDemoMode)) return [];
	if (useWebSocket()) return [];
	return await invoke<DeepSearchHit[]>('deep_search_sessions', { query });
}

/**
 * Get historical cost data aggregated by day, project, and model.
 * (Desktop/Tauri only — returns null on mobile/browser)
 */
export async function getCostData(): Promise<CostData | null> {
	if (get(isDemoMode)) return null;
	if (useWebSocket()) return null;
	return await invoke<CostData>('get_cost_data');
}

/**
 * Get all memory files from ~/.claude/projects/{project}/memory/*.md
 * (Desktop/Tauri only — returns empty array on mobile/browser)
 */
export async function getMemoryFiles(): Promise<ProjectMemory[]> {
	if (get(isDemoMode)) return [];
	if (useWebSocket()) return [];
	return await invoke<ProjectMemory[]>('get_memory_files');
}

/**
 * Open a directory in the system file manager (Finder on macOS)
 */
export async function revealInFileManager(path: string): Promise<void> {
	if (get(isDemoMode)) return;
	await invoke<void>('reveal_in_file_manager', { path });
}

/**
 * Get debug log entries from the ring buffer.
 * (Desktop/Tauri only)
 */
export async function getDebugLogs(): Promise<LogEntry[]> {
	if (useWebSocket()) return [];
	return await invoke<LogEntry[]>('get_debug_logs');
}
