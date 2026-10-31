import { writable } from 'svelte/store';
export type Shell = 'classic' | 'glass';
/** The glass shell is reserved for the later UI milestone. */
export const shell = writable<Shell>('classic');
export const settings = { 'ui.shell': shell };
export function initializeSettings() {
    try { shell.set(localStorage.getItem('ui.shell') === 'glass' ? 'glass' : 'classic'); } catch { /* no storage */ }
    return shell.subscribe(value => { try { localStorage.setItem('ui.shell', value); } catch { /* no storage */ } });
}
