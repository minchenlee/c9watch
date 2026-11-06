/** What the one handling panel shows for one item that needs you (R7-SPEC
 * §A.3–A.8). Pure, so `scripts/test-glass-handling.mjs` can test it.
 *
 * Live items get only what c9p knows: the verb, the title, the agent, the
 * wait and, for 允許, the pending tool call. Reason text, options, evidence
 * and before / after shots exist only in the demo gaps file, which the
 * caller passes only while the glass fixture is on. */
import type { Need } from '../model/needs';
import type { MemberView, RowView, WorkspaceView } from '../model';
import type { DemoFact, DemoNeedDetail, DemoOption, DemoShot, NeedVerb } from '../model/gaps-demo';

export type HandlingDetail = {
    id: string;
    verb: NeedVerb;
    title: string;
    /** The agent's name. */
    who: string;
    /** 「C9WATCH › 狀態用詞統一」: workspace › row. */
    where: string;
    since: number;
    why: string | null;
    code: string | null;
    facts: DemoFact[] | null;
    shots: DemoShot[] | null;
    options: DemoOption[] | null;
    /** The 「自己寫」 option (opens a text box), or -1. */
    freeIdx: number;
    freeLabel: string;
    /** The 「退回」 option (opens the reason box), or -1. */
    rejectIdx: number;
    rejectLabel: string;
    /** A one-line secret field (給). It gets the focus. */
    input: { label: string; placeholder: string } | null;
    /** A text box that is always there (an answer with no options). */
    text: { label: string; placeholder: string } | null;
    /** 「以後直接做」. D9 = C: only 允許 has it. */
    rule: { text: string; scope: string } | null;
    primary: string;
    /** The 「不」 answer as a plain button (拒絕, 我沒有這把 key). */
    no: string | null;
    note: string;
    reversible: boolean;
    /** False: c9watch cannot send this answer, so the main button opens the
     * chat (⌘O) and the panel says why. */
    canAnswer: boolean;
};

/** What the person chose or typed. Kept per item, so a draft stays when
 * they switch items. Typing never changes `opt`. */
export type Answer = { opt: number; free: string; reject: string; input: string; text: string; rule: boolean };
export const emptyAnswer = (): Answer => ({ opt: 0, free: '', reject: '', input: '', text: '', rule: false });

/** `chat`: the button opens the chat instead of sending. */
export type PrimaryState = { text: string; off: boolean; chat: boolean };

export const CANNOT_ANSWER = 'c9watch 還不能直接回答';

export type Located = { member: MemberView | null; workspace: WorkspaceView | null; row: RowView | null };

/** The member, workspace and row of an item. */
export function locate(need: Need, workspaces: WorkspaceView[]): Located {
    for (const w of workspaces) {
        for (const r of w.rows) {
            const m = r.members.find(x => x.key === need.memberKey);
            if (m) return { member: m, workspace: w, row: r };
        }
    }
    return { member: null, workspace: workspaces.find(w => w.id === need.workspaceId) ?? null, row: null };
}

const rowName = (r: RowView) => (r.kind === 'talk' ? `對談 · ${r.title}` : r.title);

/** Plain words for the `agent.attention` reasons c9p sends today. */
const REASONS: Record<string, string> = {
    auto_mode_denied: '自動模式擋下了一個動作',
    mail_paused: '這個 agent 的信件暫停了',
    mail_uncertain: '一封信不確定有沒有送到',
    mail_held: '一封信被扣住了'
};
function plainTitle(need: Need, m: MemberView | null): string {
    if (need.source === 'attention') return REASONS[need.title.split(':')[0]] ?? need.title;
    // A pending AskUserQuestion with no question text: G2 titles it by the tool name.
    if (need.permission === null && m?.session && need.title === m.session.pendingToolName) return m.session.latestMessage || need.title;
    return need.title;
}

function toolCode(need: Need, path: string): string | null {
    const p = need.permission;
    if (!p) return null;
    const i = (p.toolInput ?? {}) as Record<string, unknown>;
    const cmd = typeof i.command === 'string' ? i.command : null;
    const head = cmd ? `$ ${cmd}` : `${p.toolName}${need.title !== p.toolName ? ` ${need.title}` : ''}`;
    return path ? `${head}\n  在 ${path}` : head;
}

/** A managed (engine, PTY) agent reads mail, so c9watch can send it an
 * answer (G5, `msg.send`). An observed session answers only in its terminal. */
function managed(m: MemberView | null): boolean {
    return !!m?.agent && m.agent.source === 'engine' && m.agent.control === 'pty';
}

export function buildDetail(need: Need, workspaces: WorkspaceView[], demo: DemoNeedDetail[] | null = null): HandlingDetail {
    const { member, workspace, row } = locate(need, workspaces);
    const who = member?.name ?? need.agentId ?? need.sessionKey ?? '';
    const where = [workspace?.name, row ? rowName(row) : null].filter(Boolean).join(' › ');
    const d = demo?.find(x => x.agentId === need.agentId) ?? null;
    const talk = member?.role === 'talk';
    const base: HandlingDetail = {
        id: need.id, verb: need.verb, title: plainTitle(need, member), who, where, since: need.since,
        why: null, code: null, facts: null, shots: null, options: null,
        freeIdx: -1, freeLabel: '', rejectIdx: -1, rejectLabel: '', input: null, text: null, rule: null,
        primary: '送出', no: null, note: '', reversible: true, canAnswer: true
    };
    // D9 = C: 「以後直接做」 only on 允許, written to the project's permissions.
    if (need.verb === '允許') {
        base.rule = talk
            ? { text: '以後在這個對談裡，完全相同的指令直接允許。', scope: '只有這個對談、只有一字不差的指令' }
            : { text: `以後 ${who} 要跑完全相同的指令時，直接允許。`, scope: `只有 ${who}、只有一字不差的指令` };
        // Gap P / D40 Q1: c9p has no method that answers a permission prompt.
        base.canAnswer = false;
        base.reversible = false;
        base.no = null;
        base.note = '按 ⌘O 打開對談，在終端機回答。';
    }
    if (d) {
        return {
            ...base,
            why: d.why, code: d.code ?? null, facts: d.facts ?? null, shots: d.shots ?? null, options: d.options ?? null,
            freeIdx: d.freeIdx ?? -1, freeLabel: d.freeLabel ?? '', rejectIdx: d.rejectIdx ?? -1, rejectLabel: d.rejectLabel ?? '',
            input: d.input ?? null, primary: d.primary ?? base.primary, no: base.canAnswer ? d.no ?? null : null,
            note: base.canAnswer ? d.note ?? '' : base.note, reversible: d.reversible
        };
    }
    // Live: only c9p facts.
    const s = member?.session ?? null;
    if (need.verb === '允許') {
        const path = s?.projectPath ?? member?.path ?? '';
        base.why = s?.latestMessage || null;
        base.code = toolCode(need, path);
        base.facts = [{ text: `要用的工具：${need.permission?.toolName ?? '不明'}`, source: '來源：Claude Code 的權限提示', confidence: '確定' }];
        return base;
    }
    if (managed(member)) {
        base.text = need.verb === '決定'
            ? { label: `你要回 ${who} 的話`, placeholder: '寫下你的回答' }
            : { label: `給 ${who} 的新指示`, placeholder: '寫下要它怎麼做' };
        base.why = s?.latestMessage || null;
        return base;
    }
    base.why = s?.latestMessage && s.latestMessage !== base.title ? s.latestMessage : null;
    base.canAnswer = false;
    base.note = '按 ⌘O 打開對談，在終端機回答。';
    return base;
}

const isFree = (d: HandlingDetail, a: Answer) => d.freeIdx >= 0 && a.opt === d.freeIdx;
const isReject = (d: HandlingDetail, a: Answer) => d.rejectIdx >= 0 && a.opt === d.rejectIdx;
export const showsFree = isFree;
export const showsReject = isReject;

/** The main button always says what ⌘↵ will do (R7-SPEC §A.2–A.3, §A.8). */
export function primaryState(d: HandlingDetail, a: Answer, offline: boolean): PrimaryState {
    if (!d.canAnswer) return { text: '在終端機回答', off: false, chat: true };
    const off = (text: string): PrimaryState => ({ text, off: true, chat: false });
    if (d.input && !a.input.trim()) return off('先貼上 key');
    if (isFree(d, a) && !a.free.trim()) return off('先寫你的回答');
    if (isReject(d, a) && !a.reject.trim()) return off('先寫退回原因');
    if (d.text && !a.text.trim()) return off('先寫你的回答');
    if (offline) return { text: '先存著，連上後送出', off: false, chat: false };
    let text = d.primary;
    if (d.rejectIdx >= 0) text = isReject(d, a) ? '退回並送出' : d.primary;
    else if (d.options) text = isFree(d, a) ? '送出我寫的回答' : '送出：' + d.options[a.opt].label.replace(/，.*/, '');
    if (d.rule && a.rule) text += '，並以後直接做';
    return { text, off: false, chat: false };
}

/** 「3 分」, 「1 小時 5 分」, 「不到 1 分」. */
export function waitText(since: number, now: number): string {
    const min = Math.floor(Math.max(0, now - since) / 60_000);
    if (min < 1) return '不到 1 分';
    if (min < 60) return `${min} 分`;
    const h = Math.floor(min / 60), m = min % 60;
    return m ? `${h} 小時 ${m} 分` : `${h} 小時`;
}

/** The label of a send button that has no handler yet (G4 is read-only). */
export const PREVIEW_ONLY = '預覽版：還不能從這裡送出';

/** Handoff test #7: in the empty key field, J / K still change item. Same
 * IME guard as keys.ts: while an input method composes (or WebKit reports
 * keyCode 229), the key belongs to the input method. */
export function fieldStep(e: { code: string; isComposing?: boolean; keyCode?: number; metaKey?: boolean; ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean }, value: string): 1 | -1 | 0 {
    if (value || e.isComposing || e.keyCode === 229 || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return 0;
    return e.code === 'KeyJ' ? 1 : e.code === 'KeyK' ? -1 : 0;
}
