/** App-wide mail state for the MAIL tab and the session-card mail line.
 * The feed binds `transport()` when it starts, so it restarts on each demo
 * toggle (demo swaps the transport). */
import { derived, get, readable, writable } from 'svelte/store';
import { transport } from '../client';
import { isDemoMode } from '../demo/mode';
import { mailFeed, rowFromSend, threadsOf, type MailFeed, type MailSnapshot } from '../mail-store';

const EMPTY: MailSnapshot = { status: 'loading', error: null, messages: new Map(), sessionKeys: new Map() };
let feed: MailFeed | null = null;

/** Mail ids this app run sent. Only these read `[you]`; other human mail
 * reads `terminal` (core cannot tell a GUI send from a script's). */
export const ownMail = writable<ReadonlySet<string>>(new Set());
/** Agents this app run paused. No core read returns pause state yet, so this
 * is lost on reload. */
export const pausedAgents = writable<ReadonlySet<string>>(new Set());

export const mail = readable<MailSnapshot>(EMPTY, set => {
    let last: boolean | undefined;
    const stopDemo = isDemoMode.subscribe(demo => {
        if (demo === last) return;
        last = demo;
        feed?.stop();
        ownMail.set(new Set());
        pausedAgents.set(new Set());
        set(EMPTY);
        feed = mailFeed(transport(), set);
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
export async function sendMail(to: string, body: string): Promise<string | null> {
    try {
        const result = await transport().request('msg.send', { to, body });
        ownMail.update(s => new Set(s).add(result.msgId));
        feed?.upsert(rowFromSend(result, body, Date.now()));
        if (result.paused) pausedAgents.update(s => new Set(s).add(result.to.id));
        return null;
    } catch (e) { return message(e); }
}
export async function cancelMail(msgId: string): Promise<string | null> {
    try {
        await transport().request('msg.cancel', { msgId });
        const row = get(mail).messages.get(msgId);
        if (row) feed?.upsert({ ...row, state: 'cancelled', stateAt: Math.max(Date.now(), row.stateAt), detail: null });
        return null;
    } catch (e) { return message(e); }
}
export async function setMailPaused(agentId: string, paused: boolean): Promise<string | null> {
    try {
        await transport().request(paused ? 'mail.pause' : 'mail.resume', { agent: agentId });
        pausedAgents.update(s => { const n = new Set(s); if (paused) n.add(agentId); else n.delete(agentId); return n; });
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
