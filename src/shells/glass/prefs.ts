/** Per-window glass preferences in localStorage. The 設定 page (G10a)
 * writes them; a test may set them before the app starts. */
import { writable, type Writable } from 'svelte/store';

export type GlassMode = 'off' | 'subtle' | 'full';

function read(key: string): string | null {
    try { return localStorage.getItem(key); } catch { return null; }
}
function write(key: string, value: string): void {
    try { localStorage.setItem(key, value); } catch { /* no storage */ }
}
function remove(key: string): void {
    try { localStorage.removeItem(key); } catch { /* no storage */ }
}

/** Runs `fn` on each later value, not on the current one: this module loads
 * in Classic too (the root layout imports the glass shell), and it must not
 * write anything there. */
function onChange<T>(store: Writable<T>, fn: (v: T) => void): void {
    let first = true;
    store.subscribe(v => { if (first) { first = false; return; } fn(v); });
}

const mode = read('ui.glass');
/** `ui.glass`: off / subtle / full. Anything else is subtle. */
export const glassMode = writable<GlassMode>(mode === 'off' || mode === 'full' ? mode : 'subtle');
onChange(glassMode, v => write('ui.glass', v));

/** `glass.monthlyFee`: what the owner pays per month for all plans, in USD
 * (a host pref; core does not know it). null until set. */
export const monthlyFee = writable<number | null>(parseFee(read('glass.monthlyFee')));
onChange(monthlyFee, v => { if (v === null) remove('glass.monthlyFee'); else write('glass.monthlyFee', String(v)); });

/** A fee is a finite number ≥ 0 with at most 2 decimals; anything else is null. */
export function parseFee(text: string | null): number | null {
    if (text === null || !/^\s*\d+(\.\d{1,2})?\s*$/.test(text)) return null;
    const n = Number(text);
    return Number.isFinite(n) && n >= 0 ? n : null;
}

/** `glass.sidebar = collapsed` keeps the side bar closed after a reload. */
export const sidebarCollapsed = writable(read('glass.sidebar') === 'collapsed');
sidebarCollapsed.subscribe(v => write('glass.sidebar', v ? 'collapsed' : 'open'));
