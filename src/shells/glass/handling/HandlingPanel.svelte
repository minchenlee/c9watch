<script lang="ts">
    // The one handling panel of the whole product (handoff §1, R7-SPEC §A.7).
    // The 需要你 page, a workspace page and the office open this same panel.
    // Its content: location, the ask, before / after, diff summary, evidence
    // with source and confidence, options, the 「以後直接做」 toggle (允許
    // only, D9 = C) and a receipt. The page owns the keys (keys.ts) and calls
    // `option`, `submit` and `openChat`. A send button without a handler is
    // off and says so. G5: the page passes `onSubmit` / `onNo` (a delayed
    // `msg.send` to a managed agent) and `onStop` (`agent.retire`, D42, which
    // ends in a receipt and cannot be undone).
    import { tick } from 'svelte';
    import OfflineBanner from './OfflineBanner.svelte';
    import { CANNOT_ANSWER, PREVIEW_ONLY, fieldStep, primaryState, showsFree, showsReject, waitText, type Answer, type HandlingDetail } from './detail';

    export let detail: HandlingDetail;
    export let answer: Answer;
    /** 1-based place in the queue as it was listed, and the queue length. */
    export let position: { index: number; total: number };
    export let next: { verb: string; title: string } | null = null;
    export let offline: { since: number } | null = null;
    export let now: number;
    /** The 「在需要你頁看全部」 link. Off on the 需要你 page itself. */
    export let allLink: string | null = null;
    /** A receipt replaces the content after an action that cannot be undone.
     * The receipt does not accept ⌘↵; only J goes on (R7-SPEC §A.2). */
    export let receipt: { title: string; rows: [string, string][] } | null = null;
    export let onSubmit: ((a: Answer) => void) | undefined = undefined;
    export let onNo: (() => void) | undefined = undefined;
    /** Opens the chat (⌘O). Without it the chat buttons are off. */
    export let onOpenChat: (() => Promise<void> | void) | undefined = undefined;
    /** 「以後直接做」: 'hidden' until core can answer permissions and keep
     * rules (Gap P, Gap R); 'preview' shows it as a design that saves nothing. */
    export let ruleMode: 'hidden' | 'preview' = 'hidden';
    export let onReconnect: (() => void) | undefined = undefined;
    export let onNext: (() => void) | undefined = undefined;
    export let onPrev: (() => void) | undefined = undefined;
    /** Put the cursor in the key field (給). The page passes false when the
     * item only came up because the page opened: a field that takes the
     * focus on load would eat the shell keys ([, ⌘N, ⌘K). */
    export let autofocus = true;
    /** Ends the agent normally (`agent.retire`). The panel asks once more
     * first. A rejection with `notice` shows that text as it is. */
    export let onStop: (() => Promise<void>) | undefined = undefined;
    /** ⌘Z in the empty key field (same rule as J / K, handoff test #7). */
    export let onUndo: (() => void) | undefined = undefined;
    /** Demo: says that nothing is really sent or stopped. */
    export let demo = false;

    let notice: string | null = null;
    let lastId = '';
    $: if (detail.id !== lastId) { lastId = detail.id; notice = null; confirmStop = false; }
    let confirmStop = false;
    /** An `agent.retire` for this item's agent is in flight. The page keeps
     * the lock per agent (handling/handled.ts), so it outlives this panel
     * and a switch to another item. */
    export let stopBusy = false;

    // Offline wording only when a handler could keep the answer for later.
    $: ps = primaryState(detail, answer, offline !== null && !!onSubmit);
    // A send with no handler: off, labelled as a preview.
    $: preview = !ps.chat && !onSubmit;
    $: primaryOff = ps.chat ? !onOpenChat : preview || ps.off;
    $: primaryText = !ps.chat && preview ? PREVIEW_ONLY : ps.text;
    $: free = showsFree(detail, answer);
    $: reject = showsReject(detail, answer);
    $: nOpt = detail.options?.length ?? 0;

    let box: HTMLTextAreaElement | null = null;

    /** Picks option n (keys 1–4). Opens the free or reason box when needed. */
    export async function option(n: number) {
        if (!detail.canAnswer || !detail.options || n < 1 || n > detail.options.length) return;
        const had = free || reject;
        answer.opt = n - 1;
        answer = answer;
        await tick();
        if (!had && (free || reject)) box?.focus();
    }
    export async function openChat() {
        if (!onOpenChat) return;
        notice = null;
        try { await onOpenChat(); } catch (e) { notice = `打不開對談：${(e as { message?: string })?.message ?? String(e)}`; }
    }
    /** ⌘↵: the main button. */
    export function submit() {
        if (receipt || confirmStop || stopBusy) return;
        if (ps.chat) { openChat(); return; }
        if (ps.off) return;
        if (onSubmit) onSubmit(answer);
    }
    function no() {
        if (!offline) onNo?.();
    }
    async function stop() {
        if (!onStop || stopBusy) return;
        notice = null;
        try { await onStop(); confirmStop = false; }
        catch (e) { const own = (e as { notice?: unknown })?.notice; notice = typeof own === 'string' ? own : `沒有停止：${(e as { msg?: string; message?: string })?.msg ?? (e as { message?: string })?.message ?? String(e)}`; }
    }
    $: sendNote = detail.canAnswer && onSubmit && !detail.sendBlock
        ? `送出後跳到下一件。10 秒後才真的送出，在那之前可以按「復原」（⌘Z）。`
        : '';

    /** Puts the cursor in the key field (給, R7-SPEC §A.3). */
    function focusField(el: HTMLInputElement, on: boolean) {
        if (on) el.focus();
        return { update(v: boolean) { if (v) el.focus(); } };
    }
    /** Handoff test #7: while the key field is empty, J / K still change item. */
    function fieldKeys(e: KeyboardEvent) {
        const step = fieldStep(e, answer.input);
        const fn = step === 1 ? onNext : step === -1 ? onPrev : step === 'undo' ? onUndo : undefined;
        if (fn) { e.preventDefault(); e.stopPropagation(); fn(); }
    }
</script>

<section class="handling" aria-label="處理 {detail.verb}{detail.title}" data-verb={detail.verb}>
    {#if offline}
        <OfflineBanner since={offline.since} {now} {onReconnect} />
    {/if}
    <div class="prog">
        <span class="where">{detail.where}{detail.where ? ' · ' : ''}<span class="n">第 {position.index} 件，共 {position.total} 件</span></span>
        {#if allLink}<a class="link" href={allLink}>在需要你頁看全部</a>{/if}
    </div>
    <p class="nx">下一件：{#if next}<span class="verb">{next.verb}</span>{next.title}{:else}沒有了{/if}</p>

    {#if receipt}
        <span class="label">收據</span>
        <div class="card">
            <span class="meta">{detail.who} · {detail.where}</span>
            <h2>✓ {receipt.title}</h2>
            <dl class="kv">
                {#each receipt.rows as [k, v]}<dt>{k}</dt><dd>{v}</dd>{/each}
            </dl>
        </div>
        <div class="row">
            <button type="button" class="btn" disabled={!onOpenChat} on:click={openChat}>開啟對談<kbd>⌘O</kbd></button>
            <button type="button" class="btn primary" on:click={() => onNext?.()}>{next ? `下一件：${next.verb} ${next.title}` : '看全部處理完'}<kbd>J</kbd></button>
        </div>
        <span class="meta">收據頁不接受 ⌘↵，免得連按跳過。</span>
    {:else}
        <header class="ask">
            <span class="meta">{detail.who} · 等了 {waitText(detail.since, now)}</span>
            <h2><span class="verb">{detail.verb}</span>{detail.title}</h2>
            {#if detail.why}<p class="why">{detail.why}</p>{/if}
        </header>

        {#if detail.shots}
            <div class="shots">
                {#each detail.shots as shot}
                    <figure>
                        <div class="shot">
                            <span>{shot.title}</span>
                            {#each shot.rows as r}
                                <div class="r"><span>{r.name}</span><span><i class={r.tone ?? 'plain'}></i>{r.state}</span></div>
                            {/each}
                        </div>
                        <figcaption>{shot.caption}</figcaption>
                    </figure>
                {/each}
            </div>
        {/if}
        {#if detail.code}<pre class="code">{detail.code}</pre>{/if}
        {#if detail.facts}
            <div class="card">
                <b>c9watch 看到的</b>
                <div class="facts">
                    {#each detail.facts as f}
                        <div class="f"><span>{f.text}<span class="conf">{f.confidence}</span></span><span class="src">{f.source}</span></div>
                    {/each}
                </div>
            </div>
        {/if}

        {#if detail.canAnswer}
            {#key detail.id}
                {#if detail.input}
                    <label class="fld">
                        <span>{detail.input.label} · 游標已在框裡，直接 ⌘V。框是空的時候 J / K 照常換件。</span>
                        <input type="password" placeholder={detail.input.placeholder} autocomplete="off" bind:value={answer.input} use:focusField={autofocus} on:keydown={fieldKeys} />
                    </label>
                {/if}
            {/key}
            {#if detail.options}
                <fieldset class="opts">
                    <legend>選一個</legend>
                    {#each detail.options as op, k}
                        <label class="opt" class:on={answer.opt === k}>
                            <input type="radio" name="opt-{detail.id}" value={k} checked={answer.opt === k} on:change={() => option(k + 1)} />
                            <span class="b">
                                <span>{op.label}{#if op.by}<span class="meta"> · {op.by}</span>{/if}</span>
                                <span class="e">{op.effect}</span>
                            </span>
                            <kbd>{k + 1}</kbd>
                        </label>
                    {/each}
                </fieldset>
            {/if}
            {#if free}
                <label class="fld"><span>{detail.freeLabel}</span><textarea bind:this={box} bind:value={answer.free} placeholder="例如：先改 AGENTS，其他畫面等我看過再說"></textarea></label>
            {/if}
            {#if reject}
                <label class="fld"><span>{detail.rejectLabel}</span><textarea bind:this={box} bind:value={answer.reject} placeholder="例如：「等你審」在窄視窗會被截斷"></textarea></label>
            {/if}
            {#if detail.text}
                <label class="fld"><span>{detail.text.label}</span><textarea bind:value={answer.text} placeholder={detail.text.placeholder}></textarea></label>
            {/if}
        {/if}

        {#if detail.rule && ruleMode === 'preview'}
            <div class="grant" data-rule-preview>
                <span class="label">另外 · 設計預覽，不會儲存</span>
                <label class="sw">
                    <input type="checkbox" role="switch" bind:checked={answer.rule} />
                    <span><span>{detail.rule.text}</span><br /><span class="e">範圍：{detail.rule.scope}。</span></span>
                </label>
            </div>
        {/if}

        <div class="acts">
            {#if !detail.canAnswer}<p class="cannot" data-cannot>{CANNOT_ANSWER}</p>{/if}
            <span class="note">{[detail.note, sendNote, demo && (onSubmit || onStop) ? '示範模式：不會真的送出或停止。' : ''].filter(Boolean).join(' ')}</span>
            {#if notice}<span class="notice" role="status">{notice}</span>{/if}
            {#if confirmStop}
                <div class="confirm" role="group" aria-label="確認停止" data-confirm-stop>
                    <span>停止 {detail.who}？c9watch 會正常結束這個 agent 和它的程式，不能復原。</span>
                    <div class="row">
                        <button type="button" class="btn" disabled={stopBusy} on:click={() => (confirmStop = false)}>取消</button>
                        <button type="button" class="btn danger" disabled={stopBusy} on:click={stop} data-stop-confirm>{stopBusy ? '停止中…' : '停止（不能復原）'}</button>
                    </div>
                </div>
            {/if}
            <div class="row">
                {#if onStop}<button type="button" class="btn" disabled={confirmStop || stopBusy || !!offline} on:click={() => (confirmStop = true)} data-stop>{stopBusy ? '停止中…' : '停止這個 agent…'}</button>{/if}
                {#if detail.no}<button type="button" class="btn" disabled={!onNo || !!offline} on:click={no}>{detail.no}</button>{/if}
                {#if detail.canAnswer}<button type="button" class="btn" disabled={!onOpenChat} on:click={openChat}>開啟對談<kbd>⌘O</kbd></button>{/if}
                <button type="button" class="btn primary" disabled={primaryOff} on:click={submit} data-primary data-preview={preview || undefined}>
                    <span>{primaryText}</span><kbd>{ps.chat ? '⌘O' : '⌘↵'}</kbd>
                </button>
            </div>
        </div>
        <p class="help">鍵盤：{nOpt && detail.canAnswer ? `1–${nOpt} 選 · ` : ''}⌘↵ 執行主按鈕 · ⌘O 開啟對談 · J / K 換件 · ⌘Z 復原上一件 · 在文字框裡只有 ⌘↵ 和 Esc 有作用</p>
    {/if}
</section>

<style>
    .handling { max-width: 760px; display: flex; flex-direction: column; gap: var(--space-5); }
    .prog { display: flex; gap: var(--space-3); flex-wrap: wrap; align-items: baseline; font: var(--type-body); font-size: 13px; color: var(--text-2); }
    .prog .n { font: var(--type-meta); color: var(--text-2); }
    .nx { margin: calc(-1 * var(--space-4)) 0 0; font-size: 13px; color: var(--text-3); }
    .link { margin-left: auto; color: var(--text-2); font-size: 13px; }
    .meta { font-size: 13px; color: var(--text-3); }
    .label { font: var(--type-label); letter-spacing: 0.04em; color: var(--text-3); }
    .ask { display: flex; flex-direction: column; gap: 6px; }
    h2 { margin: 0; font: 600 24px/1.35 var(--font-ui); color: var(--text-1); text-wrap: balance; }
    .card h2 { font-size: 20px; }
    .verb { color: var(--status-permission-text); font-weight: 700; margin-right: 4px; }
    .why { margin: 0; max-width: 65ch; font-size: 15px; line-height: 1.75; color: var(--text-2); }
    .code {
        margin: 0;
        padding: var(--space-3) 14px;
        border: 1px solid var(--divider);
        border-radius: var(--radius-md);
        background: var(--surface-window-solid);
        font: var(--type-meta);
        font-size: 12.5px;
        color: var(--text-2);
        white-space: pre-wrap;
        overflow-x: auto;
    }
    .shots { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); }
    figure { margin: 0; }
    figcaption { margin-top: var(--space-1); font-size: 12px; color: var(--text-3); }
    .shot {
        display: flex; flex-direction: column; gap: 6px; padding: 10px;
        border: 1px solid var(--divider); border-radius: var(--radius-md); background: var(--surface-window-solid);
        font: var(--type-meta); font-size: 12px; color: var(--text-2);
    }
    .shot .r { display: flex; justify-content: space-between; padding: var(--space-1) 6px; border-radius: var(--radius-sm); background: rgb(255 255 255 / 0.03); }
    .shot i { display: inline-block; width: 8px; height: 8px; margin-right: 6px; }
    .shot i.plain { background: #c9ccd2; }
    .shot i.need { background: var(--status-permission); }
    .shot i.dim { background: #5a5f69; }
    .card {
        display: flex; flex-direction: column; gap: 10px; padding: 18px 20px;
        border: 1px solid var(--glass-stroke); border-radius: var(--radius-lg); background: var(--surface-1);
    }
    .facts { display: flex; flex-direction: column; gap: var(--space-2); }
    .f { display: flex; flex-direction: column; gap: 1px; }
    .src { font-size: 12.5px; color: var(--text-3); }
    .conf { margin-left: 6px; padding: 0 7px; border: 1px solid rgb(255 255 255 / 0.18); border-radius: 10px; font-size: 12px; color: var(--text-2); }
    .kv { display: grid; grid-template-columns: auto 1fr; gap: 6px 16px; margin: 0; font-size: 14px; }
    .kv dt { color: var(--text-3); }
    .kv dd { margin: 0; }
    .opts { display: flex; flex-direction: column; gap: var(--space-2); margin: 0; padding: 0; border: 0; }
    legend { margin-bottom: var(--space-2); font-size: 13px; font-weight: 600; color: var(--text-3); }
    .opt {
        display: flex; gap: var(--space-3); align-items: flex-start; padding: var(--space-3) 14px;
        border: 1px solid var(--glass-stroke); border-radius: 10px; cursor: pointer;
    }
    .opt.on { border-color: var(--text-2); background: var(--fill-active); }
    .opt input { margin-top: 5px; accent-color: var(--text-1); }
    .opt:focus-within { box-shadow: var(--focus-ring); }
    .b { flex: 1; display: flex; flex-direction: column; gap: 2px; }
    .e { font-size: 13px; color: var(--text-3); }
    .fld { display: flex; flex-direction: column; gap: 6px; }
    .fld > span { font-size: 13px; color: var(--text-3); }
    input[type='password'], textarea {
        padding: 10px var(--space-3);
        border: 1px solid rgb(255 255 255 / 0.18);
        border-radius: var(--radius-md);
        background: var(--surface-window-solid);
        font: var(--type-body);
        color: var(--text-1);
    }
    textarea { min-height: 64px; resize: vertical; }
    input:focus, textarea:focus { border-color: var(--text-2); }
    .grant { display: flex; flex-direction: column; gap: var(--space-2); padding-top: var(--space-4); border-top: 1px dashed rgb(255 255 255 / 0.16); }
    .sw { display: flex; gap: var(--space-3); align-items: flex-start; cursor: pointer; }
    .sw input {
        appearance: none; position: relative; flex: none; width: 32px; height: 18px; margin-top: 3px;
        border-radius: 9px; background: rgb(255 255 255 / 0.16); cursor: pointer;
    }
    .sw input::after {
        content: ''; position: absolute; left: 2px; top: 2px; width: 14px; height: 14px;
        border-radius: 50%; background: var(--text-3); transition: left 0.12s;
    }
    .sw input:checked { background: var(--text-1); }
    .sw input:checked::after { left: 16px; background: var(--surface-window-solid); }
    .sw .e { font-size: 12.5px; }
    .acts { display: flex; flex-direction: column; gap: 10px; }
    .note { font-size: 12.5px; color: var(--text-3); }
    .cannot { margin: 0; font: var(--type-body-l2); color: var(--text-1); }
    .notice { font-size: 13px; color: var(--text-2); }
    .row { display: flex; gap: var(--space-2); align-items: center; flex-wrap: wrap; }
    .btn {
        display: inline-flex; gap: 6px; align-items: center; padding: var(--space-2) 14px;
        border: 1px solid rgb(255 255 255 / 0.16); border-radius: var(--radius-md); background: transparent;
        font: var(--type-body); color: var(--text-1); cursor: pointer;
    }
    .btn:hover { border-color: var(--text-3); }
    .btn.primary { margin-left: auto; border-color: var(--text-1); background: var(--text-1); color: var(--surface-window-solid); font-weight: 600; }
    .btn.primary kbd { color: #555; }
    .btn:disabled { opacity: 0.45; cursor: not-allowed; }
    .btn.danger { border-color: rgb(255 92 92 / 0.6); color: var(--text-1); }
    .confirm {
        display: flex; flex-direction: column; gap: var(--space-2); padding: var(--space-3) 14px;
        border: 1px solid rgb(255 92 92 / 0.35); border-radius: var(--radius-md); background: rgb(255 92 92 / 0.06);
        font-size: 14px; color: var(--text-1);
    }
    .help { margin: 0; padding-top: var(--space-3); border-top: 1px solid var(--divider); font-size: 12px; color: var(--text-3); }
    @media (max-width: 760px) { .shots { grid-template-columns: 1fr; } }
    @media (prefers-reduced-motion: reduce) { .sw input::after { transition: none; } }
</style>
