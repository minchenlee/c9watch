import { writable } from 'svelte/store';
export type Shell = 'classic' | 'glass';
/** Glass is not shipped (D43). It is on only in `vite dev` and in builds made
 * with `VITE_C9_GLASS=1`. A release build is always Classic: a stored
 * `ui.shell = glass` is ignored and left as it is, and Classic shows no switch. */
export const glassAvailable = import.meta.env.DEV || import.meta.env.VITE_C9_GLASS === '1';
export const shell = writable<Shell>('classic');
export const settings = { 'ui.shell': shell };
export function initializeSettings() {
    if (!glassAvailable) return () => {};
    try { shell.set(localStorage.getItem('ui.shell') === 'glass' ? 'glass' : 'classic'); } catch { /* no storage */ }
    return shell.subscribe(value => { try { localStorage.setItem('ui.shell', value); } catch { /* no storage */ } });
}
