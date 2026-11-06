/** The extension list (glass build plan §3.2, G6).
 *
 * - Tauri: `localCommand('ext_list')` (G7 adds it). Files load from
 *   `c9ext://localhost/<id>/`. Until G7 lands the command fails and the
 *   list is empty (or, in `tauri dev`, comes from the Vite endpoint).
 * - Browser dev (`vite dev`): `GET /c9ext/_list` from
 *   `scripts/vite-c9ext-dev.mjs`. Files load from `/c9ext/<id>/`.
 * - A production build in a browser has no extensions.
 *
 * Both lists send raw manifest text (`ExtCandidate`); the host validates it
 * in one place (`manifest.ts`). Classic never imports this file (D33 Q10). */
import { writable } from 'svelte/store';
import { C9P_API_MINOR } from '$lib/c9p-1';
import { localCommand } from '$lib/data/platform';
import { isTauri } from '$lib/data/ws-legacy-client';
import { resolveExtensions, type ExtCandidate, type ExtEntry, type HostEnv } from './manifest';

declare const __C9_APP_VERSION__: string;

export const hostEnv: HostEnv = {
    app: typeof __C9_APP_VERSION__ === 'string' ? __C9_APP_VERSION__ : '0.0.0',
    apiMinor: C9P_API_MINOR,
    dev: import.meta.env.DEV
};

/** Every extension found, with its status (R8). */
export const extensions = writable<ExtEntry[]>([]);
/** True once the list was read (it may be empty). */
export const extensionsLoaded = writable(false);

const isCandidate = (c: unknown): c is ExtCandidate => {
    const x = c as Partial<ExtCandidate> | null;
    return !!x && typeof x.dir === 'string' && (x.source === 'first' || x.source === 'third')
        && (x.manifest === null || typeof x.manifest === 'string')
        && (x.size === undefined || typeof x.size === 'number')
        && (x.base === undefined || (typeof x.base === 'string' && x.base.endsWith('/')));
};
const candidates = (r: unknown): ExtCandidate[] | null => {
    const list = (r as { extensions?: unknown } | null)?.extensions;
    return Array.isArray(list) ? list.filter(isCandidate) : null;
};

async function fromTauri(): Promise<ExtCandidate[] | null> {
    try { return candidates(await localCommand('ext_list')); } catch { return null; }
}
async function fromVite(): Promise<ExtCandidate[] | null> {
    if (!import.meta.env.DEV) return null;
    try {
        const res = await fetch('/c9ext/_list', { cache: 'no-store' });
        return res.ok ? candidates(await res.json()) : null;
    } catch { return null; }
}

let loading: Promise<void> | null = null;

/** Reads the list once per window (D31 Q13: add a directory, then restart). */
export function loadExtensions(): Promise<void> {
    loading ??= (async () => {
        let list: ExtCandidate[] | null = null;
        let base = '/c9ext/';
        if (isTauri()) { list = await fromTauri(); base = 'c9ext://localhost/'; }
        if (!list) { list = await fromVite(); base = '/c9ext/'; }
        extensions.set(resolveExtensions(list ?? [], hostEnv, base));
        extensionsLoaded.set(true);
    })();
    return loading;
}
