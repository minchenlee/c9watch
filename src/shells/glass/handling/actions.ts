/** What the handling panel does (build plan §G5). Pure, so
 * `scripts/test-glass-handling.mjs` can test it. No imports.
 *
 * - An answer to a managed agent is a `msg.send`. It waits 10 s first:
 *   復原 (⌘Z) inside the 10 s means it was never sent (engine-impact 37).
 * - Each waiting send has its own timer. A new send, a receipt or a notice
 *   never ends or replaces another one (handoff tests #2, #3).
 * - Leaving the 需要你 page or closing the window sends nothing: the page
 *   calls `cancelAll` when it goes away, and a closed window runs no timer.
 *   The toast says so. The draft stays in memory, so the item comes back
 *   with what was typed (until the window closes).
 * - A send that fails is not sent again. The item goes back to the queue
 *   (engine-impact 36: an unsure result goes back to the person). */

export const UNDO_MS = 10_000;

export type PendingState = 'waiting' | 'sending';
export type Pending = {
    /** Unique per send. */
    id: string;
    /** The item this send answers. */
    itemId: string;
    label: string;
    /** Local time the send starts. */
    deadline: number;
    state: PendingState;
};
export type QueueEvent =
    | { kind: 'sent'; pending: Pending }
    | { kind: 'failed'; pending: Pending; error: string }
    | { kind: 'undone'; pending: Pending }
    | { kind: 'cancelled'; pending: Pending };

type Timers = {
    now(): number;
    set(fn: () => void, ms: number): unknown;
    clear(handle: unknown): void;
};
const realTimers: Timers = {
    now: () => Date.now(),
    set: (fn, ms) => setTimeout(fn, ms),
    clear: h => clearTimeout(h as ReturnType<typeof setTimeout>)
};

const message = (e: unknown) =>
    e && typeof e === 'object' && 'msg' in e ? String((e as { msg: unknown }).msg) : e instanceof Error ? e.message : String(e);

/** The delayed sends of one page. */
export class SendQueue {
    private items: { p: Pending; fire: () => Promise<void>; timer: unknown }[] = [];
    private listeners = new Set<(list: Pending[]) => void>();
    private n = 0;
    constructor(private onEvent: (e: QueueEvent) => void = () => {}, private timers: Timers = realTimers, private delay = UNDO_MS) {}

    /** The sends that are waiting or running, oldest first. */
    get list(): Pending[] { return this.items.map(x => ({ ...x.p })); }
    subscribe(fn: (list: Pending[]) => void): () => void {
        this.listeners.add(fn);
        fn(this.list);
        return () => { this.listeners.delete(fn); };
    }
    private publish() { const l = this.list; for (const fn of this.listeners) fn(l); }

    /** Waits `delay` ms, then calls `fire`. Returns the send id. */
    schedule(itemId: string, label: string, fire: () => Promise<void>): string {
        const id = `send-${++this.n}`;
        const p: Pending = { id, itemId, label, deadline: this.timers.now() + this.delay, state: 'waiting' };
        const item = { p, fire, timer: null as unknown };
        item.timer = this.timers.set(() => void this.run(id), this.delay);
        this.items.push(item);
        this.publish();
        return id;
    }
    private async run(id: string) {
        const item = this.items.find(x => x.p.id === id);
        if (!item || item.p.state !== 'waiting') return;
        item.p.state = 'sending';
        this.publish();
        try {
            await item.fire();
            this.drop(id);
            this.onEvent({ kind: 'sent', pending: { ...item.p } });
        } catch (e) {
            this.drop(id);
            this.onEvent({ kind: 'failed', pending: { ...item.p }, error: message(e) });
        }
    }
    private drop(id: string) {
        const i = this.items.findIndex(x => x.p.id === id);
        if (i >= 0) { this.items.splice(i, 1); this.publish(); }
    }
    /** 復原: the send `id`, or the newest one that still waits. A send that
     * already started cannot be undone. Returns what was undone. */
    undo(id?: string): Pending | null {
        const waiting = this.items.filter(x => x.p.state === 'waiting');
        const item = id ? waiting.find(x => x.p.id === id) : waiting[waiting.length - 1];
        if (!item) return null;
        this.timers.clear(item.timer);
        this.drop(item.p.id);
        const p = { ...item.p };
        this.onEvent({ kind: 'undone', pending: p });
        return p;
    }
    /** Ends every waiting send without sending it (the page goes away). */
    cancelAll(): Pending[] {
        const out: Pending[] = [];
        for (const item of [...this.items]) {
            if (item.p.state !== 'waiting') continue;
            this.timers.clear(item.timer);
            this.drop(item.p.id);
            out.push({ ...item.p });
            this.onEvent({ kind: 'cancelled', pending: { ...item.p } });
        }
        return out;
    }
    /** True while `itemId` has a send that waits or runs. */
    has(itemId: string): boolean { return this.items.some(x => x.p.itemId === itemId); }
}

/** Seconds left, rounded up, never below 0. */
export const secondsLeft = (p: Pick<Pending, 'deadline'>, now: number) => Math.max(0, Math.ceil((p.deadline - now) / 1000));

/** The next item to show from `fromId`, `step` = 1 (J) or -1 (K), in the
 * order the queue was listed. Handled items are skipped (handoff test #4).
 * `fromId` may itself be handled (a receipt). Null: nothing else is left. */
export function stepFrom(order: string[], handled: ReadonlySet<string>, fromId: string | null, step: 1 | -1): string | null {
    const n = order.length;
    if (!n) return null;
    const at = fromId ? order.indexOf(fromId) : -1;
    const start = at < 0 ? (step === 1 ? -1 : n) : at;
    for (let k = 1; k <= n; k++) {
        const id = order[(((start + step * k) % n) + n) % n];
        if (id !== fromId && !handled.has(id)) return id;
    }
    return null;
}
