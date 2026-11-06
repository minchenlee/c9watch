/** Per-window glass preferences in localStorage. The Settings page (G10)
 * writes `ui.glass`; until then it is set by hand or by a test. */
import { writable } from 'svelte/store';

export type GlassMode = 'off' | 'subtle' | 'full';

function read(key: string): string | null {
    try { return localStorage.getItem(key); } catch { return null; }
}
function write(key: string, value: string): void {
    try { localStorage.setItem(key, value); } catch { /* no storage */ }
}

const mode = read('ui.glass');
/** `ui.glass`: off / subtle / full. Anything else is subtle. */
export const glassMode = writable<GlassMode>(mode === 'off' || mode === 'full' ? mode : 'subtle');

/** `glass.sidebar = collapsed` keeps the side bar closed after a reload. */
export const sidebarCollapsed = writable(read('glass.sidebar') === 'collapsed');
sidebarCollapsed.subscribe(v => write('glass.sidebar', v ? 'collapsed' : 'open'));
