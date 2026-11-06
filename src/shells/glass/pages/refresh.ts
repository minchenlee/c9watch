/** Re-reads page data while the page is open: every `everyMs`, when the
 * window gets focus, and when the page calls `kick()` because what it asks
 * for changed (for example the live sessions). Focus reads are at least
 * `minGapMs` after the last read; a focus inside the gap runs once at its
 * end. A `kick()` does not wait for the gap. One read at a time; reads asked
 * for during a read run once after it. `stop()` ends it (call it on unmount): no read
 * starts after that, and `alive()` tells a read in flight to drop its answer. */
export type Refresher = { kick(): void; stop(): void };

export function startRefresh(read: (alive: () => boolean) => Promise<void>, { everyMs = 60_000, minGapMs = 15_000, target = globalThis as unknown as EventTarget } = {}): Refresher {
    let stopped = false, reading = false, again = false, againNow = false, last = -Infinity;
    let wait: ReturnType<typeof setTimeout> | null = null;
    const alive = () => !stopped;
    async function run(now = false) {
        if (stopped) return;
        if (reading) { again = true; againNow ||= now; return; }
        const gap = last + minGapMs - Date.now();
        if (!now && gap > 0) { if (!wait) wait = setTimeout(() => { wait = null; void run(); }, gap); return; }
        if (wait) { clearTimeout(wait); wait = null; }
        reading = true; last = Date.now();
        try { await read(alive); } catch { /* the page keeps its last answer; the next tick retries */ }
        reading = false;
        if (again) { const n = againNow; again = againNow = false; void run(n); }
    }
    const timer = setInterval(() => void run(true), everyMs);
    const onFocus = () => void run();
    target.addEventListener?.('focus', onFocus);
    void run();
    return {
        kick: () => void run(true),
        stop() { stopped = true; clearInterval(timer); if (wait) clearTimeout(wait); target.removeEventListener?.('focus', onFocus); }
    };
}
