/**
 * Test-only init script. Playwright injects the bundled form of this file
 * before any app code runs. It answers the real Tauri command names from the
 * recorded fixtures with `@tauri-apps/api/mocks`. Core results enter through
 * the real data layer; Classic contains no test-only paths.
 *
 * Options come from `window.__C9_VISUAL__`, which an earlier init script sets.
 */
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import * as fx from '../classic/fixtures';

export interface VisualOptions {
	/** Tauri window label: `main` or `popover`. */
	window?: 'main' | 'popover';
	/** `full` answers every fixture; `empty` has no live sessions. */
	sessions?: 'full' | 'empty';
	/** When set, `stop_session` rejects with this message. */
	stopError?: string | null;
}

declare global {
	interface Window {
		__C9_VISUAL__?: VisualOptions;
		/** Commands the harness has no fixture for. The tests assert it stays empty. */
		__C9_VISUAL_UNHANDLED__?: string[];
	}
}

const options: VisualOptions = window.__C9_VISUAL__ ?? {};
const unhandled: string[] = [];
window.__C9_VISUAL_UNHANDLED__ = unhandled;

type Args = Record<string, unknown> | undefined;

function handle(cmd: string, args: Args): unknown {
	const live = options.sessions === 'empty' ? [] : fx.sessions;
	switch (cmd) {
        case 'c9p_request': {
            const p = (args?.params ?? {}) as Record<string, any>;
            const id = String(p.sessionKey ?? '').replace(/^(claudeCode|codex|cursor|pi|opencode):/, '');
            switch (args?.method) {
                case 'session.views': return { sessions: live, cursor: 'visual:epoch:1' };
                case 'diag.detection': return fx.diagnostics;
                case 'conversation.get': return { ...(fx.conversations[id] ?? { sessionId: id, provider: p.provider, messages: [] }), nextOffset: null };
                case 'session.away': return { activity: handle('get_away_activity', p) };
                case 'session.stop': return handle('stop_session', p);
                case 'session.rename': return null;
                case 'history.list': return { entries: fx.history, nextOffset: null };
                case 'history.search': return { hits: [] };
                case 'cost.get': return fx.cost;
                case 'memory.list': return { projects: fx.memory, nextOffset: null };
                case 'usage.subscriptions': return { usage: fx.subscriptionUsage };
                case 'subagent.list': return { subagents: options.sessions === 'empty' ? {} : fx.subagents };
                case 'subagent.transcript': return { messages: [] };
                case 'session.tasks': return { tasks: fx.tasks[String(p.sessionId)] ?? [] };
                case 'provider.opencode.status': return fx.opencodeStatus;
                case 'provider.opencode.connect': case 'detect.recheck': return null;
                default: unhandled.push(`c9p:${String(args?.method)}`); return null;
            }
        }
        case 'c9p_subscribe': return 'visual-sub';
        case 'c9p_unsubscribe': return null;
		case 'get_sessions':
			return live;
		case 'get_detection_diagnostics':
			return fx.diagnostics;
		case 'get_conversation': {
			const id = String(args?.sessionId);
			return fx.conversations[id] ?? { sessionId: id, provider: args?.provider, messages: [] };
		}
		case 'get_away_activity': {
			const requests = (args?.requests ?? []) as { sessionKey: string }[];
			const out: Record<string, unknown> = {};
			for (const { sessionKey } of requests) {
				if (fx.awayActivity[sessionKey]) out[sessionKey] = fx.awayActivity[sessionKey];
			}
			return out;
		}
		case 'get_subagents':
			return options.sessions === 'empty' ? {} : fx.subagents;
		case 'get_session_tasks':
			return fx.tasks[String(args?.sessionId)] ?? [];
		case 'get_subagent_transcript':
			return { messages: [] };
		case 'get_session_history':
			return fx.history;
		case 'deep_search_sessions':
			return [];
		case 'get_cost_data':
			return fx.cost;
		case 'get_memory_files':
			return fx.memory;
		case 'get_subscription_usage':
			return fx.subscriptionUsage;
		case 'get_notification_preferences':
		case 'save_notification_preferences':
			return fx.notificationPreferences;
		case 'opencode_connection_status':
			return fx.opencodeStatus;
		case 'get_remote_access':
		case 'set_remote_access':
			return fx.remoteAccess;
		case 'get_server_info':
			return fx.serverInfo;
		case 'get_debug_logs':
			return [];
		case 'get_build_channel':
			// `dev` skips the startup update check, so no update banner shows.
			return 'dev';
		case 'stop_session':
			if (options.stopError) throw options.stopError;
			return null;
		case 'open_session':
		case 'rename_session':
		case 'show_main_window':
		case 'reveal_in_file_manager':
		case 'test_native_notification':
			return null;
		case 'plugin:app|version':
			return fx.appVersion;
		case 'plugin:window|is_fullscreen':
			return false;
		case 'plugin:notification|is_permission_granted':
			return true;
		default:
			unhandled.push(cmd);
			return null;
	}
}

mockWindows(options.window ?? 'main');
mockIPC((cmd, args) => handle(cmd, args as Args), { shouldMockEvents: true });
