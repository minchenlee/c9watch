/** Handling state that outlives the 需要你 page (memory only; a closed
 * app loses it). Keyed by the request instance (`Need.instance`), so a new
 * request from the same agent is never hidden by an old one. */
import { writable } from 'svelte/store';
import type { Need } from '../model/needs';

/** Request instances handled this app run. The queue, J / K and the side
 * bar count skip them. */
export const handled = writable<ReadonlySet<string>>(new Set());
export const markHandled = (instance: string) => handled.update(s => new Set(s).add(instance));
export const unmarkHandled = (instance: string) => handled.update(s => { const n = new Set(s); n.delete(instance); return n; });
/** Forgets the instances the model no longer lists. The shell calls it on
 * every model change, so it runs on every page. */
export function pruneHandled(needs: Need[]) {
    handled.update(s => {
        const live = new Set(needs.map(n => n.instance));
        return [...s].every(i => live.has(i)) ? s : new Set([...s].filter(i => live.has(i)));
    });
}

/** Session keys with a `session.stop` in flight. One stop per session at a
 * time, whatever item or page the person moves to meanwhile. */
export const stopping = writable<ReadonlySet<string>>(new Set());
let inFlight = new Set<string>();
/** Takes the stop lock for `key` before any await. False: a stop runs. */
export function lockStop(key: string): boolean {
    if (inFlight.has(key)) return false;
    inFlight = new Set(inFlight).add(key);
    stopping.set(inFlight);
    return true;
}
export function unlockStop(key: string) {
    inFlight = new Set(inFlight);
    inFlight.delete(key);
    stopping.set(inFlight);
}
