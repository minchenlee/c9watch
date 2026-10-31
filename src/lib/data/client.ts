/** Typed c9p client shared by every shell and transport. */
import { get } from 'svelte/store';
import type { C9pMethod, C9pMethods } from '../c9p-1';
import { isDemoMode } from '../demo/mode';
import { useWebSocket } from '../ws';
import { tauriTransport } from './tauri-bridge';
import { legacyTransport } from './ws-legacy';
import { demoTransport } from './demo';
import type { Transport } from './transport';
let selected: Transport | null = null;
/** New browser clients install an authenticated ws-c9p transport explicitly. */
export function setTransport(transport: Transport | null) { selected = transport; }
export function hasSelectedTransport() { return selected !== null; }
export function transport(): Transport {
    if (get(isDemoMode)) return demoTransport;
    return selected ?? (useWebSocket() ? legacyTransport : tauriTransport);
}
export function request<M extends C9pMethod>(method: M, params: C9pMethods[M]['params']) {
    return transport().request(method, params);
}
