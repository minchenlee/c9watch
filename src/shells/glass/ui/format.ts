/** Pure helpers for the glass classic card, pixel status, pixel bar and
 * annotation chips (direction §10, core plan §8.2). No DOM, no stores:
 * `scripts/test-glass-ui.mjs` loads this file directly. */
import type { C9pAnnotation } from '$lib/c9p-1';
import type { NeedVerb } from '../model/gaps-demo';

// ---- Status ----------------------------------------------------------------

/** The status a card shows. Orange (`needs`) only for an item in the needs
 * queue: the classic rule "only 需要你 is orange". */
export type Tone = 'needs' | 'working' | 'stale' | 'waiting' | 'done' | 'idle';

/** Pixel status text. Latin only: Geist Pixel Square has no CJK glyphs.
 * 「沒回應」 (redesign §8.14) is NO RESPONSE. */
const VERB_LABEL: Record<NeedVerb, string> = {
    允許: 'APPROVAL',
    決定: 'DECISION',
    審: 'REVIEW',
    給: 'NEEDS KEY',
    處理: 'BLOCKED'
};
const TONE_LABEL: Record<Exclude<Tone, 'needs'>, string> = {
    working: 'WORKING',
    stale: 'NO RESPONSE',
    waiting: 'WAITING',
    done: 'DONE',
    idle: 'IDLE'
};

export type StatusInput = {
    status: 'needs' | 'working' | 'waiting' | 'done' | 'unknown';
    stale: boolean;
};

/** Card tone and pixel label. `verb` is the member's needs-queue verb, if any. */
export function cardStatus(m: StatusInput, verb: NeedVerb | null): { tone: Tone; label: string } {
    if (verb) return { tone: 'needs', label: VERB_LABEL[verb] };
    // A detected NeedsAttention with no queue item (the queue is the only
    // source of orange) still asks for a person.
    if (m.status === 'needs') return { tone: 'needs', label: 'NEEDS YOU' };
    const tone: Tone = m.status === 'working' ? (m.stale ? 'stale' : 'working') : m.status === 'unknown' ? 'idle' : m.status;
    return { tone, label: TONE_LABEL[tone as Exclude<Tone, 'needs'>] };
}

// ---- Pixel bar -------------------------------------------------------------

/** Splits `blocks` cells over `counts` in proportion (largest remainder,
 * like Classic's StatusBar). All zero: every cell stays empty. */
export function allocateBlocks(counts: number[], blocks: number): number[] {
    const total = counts.reduce((a, b) => a + b, 0);
    if (total <= 0 || blocks <= 0) return counts.map(() => 0);
    const exact = counts.map(c => (c / total) * blocks);
    const out = exact.map(Math.floor);
    let left = blocks - out.reduce((a, b) => a + b, 0);
    const order = exact.map((e, i) => [e - out[i], i] as const).sort((a, b) => b[0] - a[0] || a[1] - b[1]);
    for (const [, i] of order) { if (left-- <= 0) break; out[i]++; }
    return out;
}

/** Cells that fit a track: 8px cells, 2px gap. */
export function blockColumns(width: number): number {
    return Math.max(1, Math.floor((width + 2) / 10));
}

// ---- Numbers and time ------------------------------------------------------

/** `4m`, `2h`, `3d`; `now` under a minute. */
export function timeAgo(t: number, now: number): string {
    const s = Math.max(0, Math.round((now - t) / 1000));
    if (s < 60) return 'now';
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 48) return `${h}h`;
    return `${Math.floor(h / 24)}d`;
}

/** `12`, `4.2K`, `0.4M`. Millions with one decimal (the design's unit). */
export function formatTokens(n: number): string {
    if (!Number.isFinite(n) || n <= 0) return '0';
    if (n >= 100_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1000) return `${(n / 1000).toFixed(1)}K`;
    return String(Math.round(n));
}

export function formatUsd(n: number): string {
    return `$${(Number.isFinite(n) ? n : 0).toFixed(2)}`;
}

export function formatDuration(ms: number): string {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
    const h = Math.floor(m / 60);
    return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
}

function formatBytes(n: number): string {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = n, i = 0;
    while (Math.abs(v) >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return `${i ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

// ---- Annotation chips ------------------------------------------------------

/** The annotations on one session, ordered by `namespace/name` (§8.2). */
export function annotationsFor(all: Iterable<C9pAnnotation>, sessionKey: string | null): C9pAnnotation[] {
    if (!sessionKey) return [];
    return [...all]
        .filter(a => a.target.kind === 'session' && a.target.sessionKey === sessionKey)
        .sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
}

/** The chip label: `label`, else the key's name part. */
export function annotationLabel(a: C9pAnnotation): string {
    return a.label?.trim() || a.key.slice(a.key.indexOf('/') + 1) || a.key;
}

/** The chip value, rendered by `type`, then `format`, then `unit`.
 * A client that does not know a format shows the plain value. */
export function annotationValue(a: C9pAnnotation, now: number): string {
    if (a.redacted || !a.value) return '隱藏';
    const unit = a.unit?.trim() ? ` ${a.unit.trim()}` : '';
    const v = a.value;
    switch (v.type) {
        case 'bool': return v.v ? '是' : '否';
        case 'string': return v.v + unit;
        case 'duration': return formatDuration(v.ms);
        case 'money': return `${v.amount} ${v.currency}`;
        case 'number': {
            const n = v.v;
            switch (a.format) {
                case 'integer': return Math.round(n).toLocaleString('en-US') + unit;
                // The value is the percent itself (42 → 42%), not a ratio.
                case 'percent': return `${Number(n.toFixed(1))}%`;
                case 'bytes': return formatBytes(n);
                case 'relative-time': return timeAgo(n, now);
                default: return (Number.isInteger(n) ? String(n) : String(Number(n.toFixed(2)))) + unit;
            }
        }
        default: return '';
    }
}
