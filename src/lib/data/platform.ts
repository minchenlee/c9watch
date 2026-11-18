/** Native-only actions and plugins. Shells import these wrappers. */
import { invoke } from '@tauri-apps/api/core';
import { listen as nativeListen, type EventCallback, type EventName, type Options } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';
import { get } from 'svelte/store';
import { isDemoMode } from '../demo/mode';
import { useWebSocket } from '../ws';
import type { LogEntry } from '../types';
export { isPermissionGranted, requestPermission };
export { openUrl } from '@tauri-apps/plugin-opener';
export { getVersion } from '@tauri-apps/api/app';
export { getCurrentWindow } from '@tauri-apps/api/window';
export { shareFile } from '@choochmeque/tauri-plugin-sharekit-api';
export interface ServerInfo {
    port: number; localIp: string; lanEnabled: boolean; bind: 'loopback' | 'lan'; lanAddresses: string[]
}
export interface PairingCode { code: string; expiresAt: number }
export interface RemoteAccess { lanEnabled: boolean; allowConversation: boolean; allowOpen: boolean; extraAllowedHosts: string[] }
export const getServerInfo = () => invoke<ServerInfo>('get_server_info');
export const createPairingCode = () => invoke<PairingCode>('create_pairing_code');
export const forgetPairedClients = () => invoke<void>('forget_paired_clients');
export const getRemoteAccess = () => invoke<RemoteAccess>('get_remote_access');
export const setRemoteAccess = (remote: RemoteAccess) => invoke<RemoteAccess>('set_remote_access', { remote });
export async function revealInFileManager(path: string) { if (!get(isDemoMode)) await invoke<void>('reveal_in_file_manager', { path }); }
export async function getDebugLogs(): Promise<LogEntry[]> { return useWebSocket() ? [] : invoke('get_debug_logs'); }
export async function localCommand<T = unknown>(command: string, p?: any): Promise<T> {
    switch (command) {
        case 'open_session': return invoke<T>('open_session', p);
        case 'show_main_window': return invoke<T>('show_main_window');
        case 'save_temp_image': return invoke<T>('save_temp_image', p);
        case 'get_notification_preferences': return invoke<T>('get_notification_preferences');
        case 'save_notification_preferences': return invoke<T>('save_notification_preferences', p);
        case 'test_native_notification': return invoke<T>('test_native_notification');
        case 'get_build_channel': return invoke<T>('get_build_channel');
        default: throw new Error(`Unsupported platform command: ${command}`);
    }
}
/** Focus recheck belongs to the data layer. Native fallback keeps its own hook. */
export async function listen<T>(event: EventName, handler: EventCallback<T>, options?: Options) {
    return nativeListen<T>(event, handler, options);
}
export async function initializeFocusRecheck() {
    const { request } = await import('./client');
    return nativeListen('tauri://focus', () => { void request('detect.recheck', {}).catch(() => {}); });
}
