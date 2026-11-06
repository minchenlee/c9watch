/** App-wide mail state for the MAIL tab and the session-card mail line.
 * The feed binds `transport()` when it starts, so it restarts on each demo
 * toggle (demo swaps the transport). */
import { derived, get, readable, writable } from 'svelte/store';
import { transport } from '../client';
import { isDemoMode } from '../demo/mode';
import { mailFeed, rowFromSend, seedPause, threadsOf, type MailFeed, type MailSnapshot } from '../mail-store';

const EMPTY: MailSnapshot = {
    status: 'loading', error: null, messages: new Map(), sessionKeys: new Map(), agentPause: new Map(), agentPauseAt: -Infinity
};
let feed: MailFeed | null = null;
/** Agent id → when this app run paused or resumed it (Date.now()). */
let pauseSetAt = new Map<string, number>();
let seededAt = -Infinity;

/** Mail ids this app run sent. Only these read `[you]`; other human mail
 * reads `terminal` (core cannot tell a GUI send from a script's). */
export const ownMail = writable<ReadonlySet<string>>(new Set());
/** Agents whose mail is paused: seeded from `agent.list` `mailPaused`
 * (c9p 1.15+), then kept by this app run's own pause, resume and send. */
export const pausedAgents = writable<ReadonlySet<string>>(new Set());
/** Agents whose pause state is known: listed by an engine that sends
 * `mailPaused`, or set by this app run. For any other agent (an older
 * engine) it may still be paused from an earlier run, so the MAIL tab
 * offers Resume too. */
export const pauseKnown = writable<ReadonlySet<string>>(new Set());

function takeList(s: MailSnapshot) {
    if (s.agentPauseAt === seededAt) return;
    seededAt = s.agentPauseAt;
    const next = seedPause(s, get(pausedAgents), get(pauseKnown), pauseSetAt);
    if (!next) return;
    pausedAgents.set(next.paused);
    pauseKnown.set(next.known);
}

export const mail = readable<MailSnapshot>(EMPTY, set => {
    let last: boolean | undefined;
    const stopDemo = isDemoMode.subscribe(demo => {
        if (demo === last) return;
        last = demo;
        feed?.stop();
        ownMail.set(new Set());
        pausedAgents.set(new Set());
        pauseKnown.set(new Set());
        pauseSetAt = new Map();
        seededAt = -Infinity;
        set(EMPTY);
        feed = mailFeed(transport(), s => { takeList(s); set(s); });
    });
    return () => { stopDemo(); feed?.stop(); feed = null; };
});

export const mailThreads = derived(mail, $m => threadsOf($m.messages.values()));
/** True once mail answered. The MAIL tab shows only then. */
export const mailAvailable = derived(mail, $m => $m.status === 'ready' || ($m.status === 'error' && $m.messages.size > 0));
/** Session key → that agent's thread, for the card line. */
export const mailBySessionKey = derived([mail, mailThreads], ([$m, $threads]) => {
    const out = new Map<string, (typeof $threads)[number]>();
    for (const t of $threads) {
        const key = $m.sessionKeys.get(t.agentId);
        if (key) out.set(key, t);
    }
    return out;
});

function message(error: unknown): string {
    if (error && typeof error === 'object' && 'msg' in error) return String((error as { msg: unknown }).msg);
    return error instanceof Error ? error.message : String(error);
}

/** Each action returns null on success, or the error text to show. */
/** `replyTo`: the request this mail answers (a 決定 from mail, glass G5). */
export async function sendMail(to: string, body: string, replyTo: string | null = null): Promise<string | null> {
    try {
        const result = await transport().request('msg.send', replyTo ? { to, body, replyTo } : { to, body });
        ownMail.update(s => new Set(s).add(result.msgId));
        feed?.upsert({ ...rowFromSend(result, body, Date.now()), replyTo });
        if (result.paused) {
            pauseSetAt.set(result.to.id, Date.now());
            pausedAgents.update(s => new Set(s).add(result.to.id));
            pauseKnown.update(s => new Set(s).add(result.to.id));
        }
        return null;
    } catch (e) { return message(e); }
}
export async function cancelMail(msgId: string): Promise<string | null> {
    try {
        await transport().request('msg.cancel', { msgId });
        const row = get(mail).messages.get(msgId);
        // Same engine time as the row: the engine's own `cancelled` event
        // (or any later state) still wins over this local guess.
        if (row) feed?.upsert({ ...row, state: 'cancelled', stateAt: row.stateAt, detail: null });
        return null;
    } catch (e) { return message(e); }
}
export async function setMailPaused(agentId: string, paused: boolean): Promise<string | null> {
    try {
        await transport().request(paused ? 'mail.pause' : 'mail.resume', { agent: agentId });
        pauseSetAt.set(agentId, Date.now());
        pausedAgents.update(s => { const n = new Set(s); if (paused) n.add(agentId); else n.delete(agentId); return n; });
        pauseKnown.update(s => new Set(s).add(agentId));
        return null;
    } catch (e) { return message(e); }
}

/** The agent the MAIL tab shows. The card mail line sets it. */
export const mailFocus = writable<string | null>(null);

/** The full body; `msg.list` rows carry only a 200-character preview. */
export async function readMailBody(msgId: string): Promise<{ body: string } | { error: string }> {
    try { return { body: (await transport().request('msg.read', { msgId })).body }; }
    catch (e) { return { error: message(e) }; }
}
