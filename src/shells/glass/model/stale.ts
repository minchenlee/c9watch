/** 「很久沒動靜」: a working agent whose state did not change for 20 minutes
 * (engine-impact item 23). Core has no such check, so the UI keeps a change
 * log: the last time each member's state signature changed. */
export const STALE_MS = 20 * 60_000;

export type ChangeLog = ReadonlyMap<string, { sig: string; at: number }>;
export type Observation = { key: string; sig: string; seenAt: number };

/** The next log. A new key starts at `seenAt` (the row's own timestamp), so a
 * reload does not make every member look fresh. A changed signature starts
 * at `now`. Keys not seen any more are dropped. */
export function trackChanges(prev: ChangeLog, seen: Observation[], now: number): Map<string, { sig: string; at: number }> {
    const next = new Map<string, { sig: string; at: number }>();
    for (const o of seen) {
        const old = prev.get(o.key);
        next.set(o.key, old?.sig === o.sig ? old : { sig: o.sig, at: old ? now : Math.min(o.seenAt, now) });
    }
    return next;
}

/** Only a member that claims to work can be stale; an idle one just waits. */
export function isStale(working: boolean, lastChange: number, now: number): boolean {
    return working && now - lastChange >= STALE_MS;
}
