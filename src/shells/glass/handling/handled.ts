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

/** Agent ids with an `agent.retire` in flight (D42). One stop per agent at a
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

/** The panel notice when `agent.retire` returns an `orphaned` row. */
export const ORPHANED = '未確認結束，可再試一次。';
/** The panel notice when `agent.retire` returns a `failed` row: the agent
 * had already ended on its own, so nothing was stopped. */
export const ALREADY_FAILED = '沒有停止：這個 agent 在停止前已經失敗結束了。';
/** A core that cannot retire: older than the bridge, or a bridge that does
 * not expose `agent.retire`. There is no fallback to `session.stop`. */
const NO_RETIRE = new Set(['unknown_method', 'not_allowed', 'core_unavailable']);
/** Turns an `agent.retire` failure into what the panel shows. */
export function retireError(e: unknown): { notice: string } | unknown {
    const code = (e as { code?: unknown })?.code;
    if (typeof code === 'string' && NO_RETIRE.has(code)) return { notice: '沒有停止：這個 c9watch 核心不能結束 agent（不支援 agent.retire）。請更新 c9watch 後再試。' };
    return e;
}
