<script context="module" lang="ts">
    import type { Answer } from '../handling/detail';
    // Module state, so it stays when the person leaves the page and comes
    // back (handoff test #5). Memory only: quitting the app loses it.
    // One draft and one receipt per request instance (`Need.instance`).
    const drafts: Record<string, Answer> = {};
    type Receipt = { title: string; rows: [string, string][] };
    type Done = { id: string; send: string | null; verb: string; title: string; state: 'waiting' | 'sent' | 'stopped' };
    const kept: { receipts: Record<string, Receipt>; done: Done[] } = { receipts: {}, done: [] };
</script>

<script lang="ts">
    // The 需要你 page (build plan §G4–G5, R7-SPEC §C, needs-proto.html): the
    // queue (280px) next to the one handling panel.
    // G5 actions:
    // - Answer a managed agent: `msg.send` after 10 s (復原 = not sent yet).
    // - Stop a managed agent: `agent.retire` (D42), then a receipt (no undo).
    //   An `orphaned` result is no receipt: the panel says so and 停止 stays.
    // - Observed sessions and 允許: ⌘O only (`openSession`).
    // - Leaving this page, closing (hiding) or minimizing the window sends nothing: every
    //   waiting send is cancelled, the item goes back to the queue and the
    //   draft stays. The toast says so. The queue also checks before each
    //   send (`canSend`).
    import { onDestroy, onMount } from 'svelte';
    import { get } from 'svelte/store';
    import { dataError, fixtureOn, hideRetiredSession, model } from '../data';
    import { usePageKeys, type PageKeys } from '../keys';
    import { openSession, retireAgent } from '$lib/data/api';
    import { sessionFromView } from '$lib/data/session';
    import { sendMail } from '$lib/data/stores/mail';
    import { isDemoMode } from '$lib/data/demo/mode';
    import { DEMO_NEED_DETAILS, DEMO_OFFLINE_AGO_MS, DEMO_OFFLINE_KEY } from '../model/gaps-demo';
    import HandlingPanel from '../handling/HandlingPanel.svelte';
    import UndoToasts from '../handling/UndoToasts.svelte';
    import { answerBody, answerLabel, buildDetail, emptyAnswer, locate, noBody, waitText, type HandlingDetail } from '../handling/detail';
    import { SendQueue, stepFrom, type Pending, type QueueEvent } from '../handling/actions';
    import { ALREADY_FAILED, ORPHANED, handled, lockStop, markHandled, retireError, stopping, unlockStop, unmarkHandled } from '../handling/handled';
    import { getCurrentWindow } from '$lib/data/platform';

    // Demo details only while the glass fixture is on: live mode never shows them.
    $: demo = $fixtureOn ? DEMO_NEED_DETAILS : null;
    $: needs = $model.needs;
    $: details = needs.map(n => buildDetail(n, $model.workspaces, demo));
    $: order = details.map(d => d.id);

    // Handled marks use the request instance: a new request with the same
    // text is a new item. The shell forgets instances the model drops.
    $: hiddenIds = new Set(details.filter(d => $handled.has(d.instance)).map(d => d.id));

    let receipts = kept.receipts;
    $: kept.receipts = receipts;

    let currentId: string | null = null;
    // D42: a stop receipt does not depend on the live Need. After a retire
    // the agent row goes `retired` and the model drops the Need, so the page
    // keeps a snapshot of the item, its receipt, its place and its 「下一件」
    // until the person moves on (J / K, a click) or leaves the page.
    let shown: { detail: HandlingDetail; receipt: Receipt; index: number; total: number; nextId: string | null } | null = null;
    $: pinned = shown && currentId === shown.detail.id ? shown : null;
    $: current = pinned?.detail ?? details.find(d => d.id === currentId && (!hiddenIds.has(d.id) || receipts[d.instance])) ?? details.find(d => !hiddenIds.has(d.id)) ?? null;
    $: index = pinned ? pinned.index : current ? order.indexOf(current.id) : -1;
    $: total = pinned && !order.includes(pinned.detail.id) ? pinned.total : details.length;
    $: receipt = pinned?.receipt ?? (current ? receipts[current.instance] ?? null : null);
    $: nextId = pinned && !order.includes(pinned.detail.id) ? liveNext(pinned.nextId, 1) : current ? stepFrom(order, hiddenIds, current.id, 1) : null;
    $: next = nextId ? details.find(d => d.id === nextId) ?? null : null;
    $: currentNeed = current ? needs.find(x => x.id === current.id) ?? null : null;
    $: member = currentNeed ? locate(currentNeed, $model.workspaces).member : null;

    let answers = drafts;
    $: if (current && !answers[current.instance]) answers[current.instance] = emptyAnswer();

    // Handled this app run, oldest first. Their verbs are not orange: they
    // no longer need you (handoff 「實作前要改的錯」 #3).
    let done = kept.done;
    $: kept.done = done;
    const STATE_TEXT: Record<Done['state'], string> = { waiting: '還沒送出，可以復原', sent: '送出了', stopped: '已停止 · 不能復原' };

    let now = Date.now();
    let pending: Pending[] = [];
    // Every second while a toast counts down, otherwise every 30 s.
    const timer = setInterval(() => { if (pending.length || Date.now() - now >= 30_000) now = Date.now(); }, 1000);

    type Notice = { id: string; text: string; tone: 'info' | 'error' };
    let notices: Notice[] = [];
    let nNotice = 0;
    const noticeTimers = new Set<ReturnType<typeof setTimeout>>();
    function notify(text: string, tone: Notice['tone']) {
        const id = `n${++nNotice}`;
        notices = [...notices, { id, text, tone }];
        // An error stays until closed. Info ends on its own timer.
        if (tone === 'info') { const t = setTimeout(() => { noticeTimers.delete(t); dismiss(id); }, 5000); noticeTimers.add(t); }
    }
    const dismiss = (id: string) => (notices = notices.filter(n => n.id !== id));

    // A pending send's itemId is the request instance.
    const idOf = (instance: string) => details.find(d => d.instance === instance)?.id ?? null;
    let destroyed = false;
    function onQueue(e: QueueEvent) {
        const p = e.pending;
        if (e.kind === 'sent') {
            done = kept.done = done.map(x => (x.send === p.id ? { ...x, state: 'sent' as const } : x));
            delete answers[p.itemId];
            answers = answers;
            return;
        }
        done = kept.done = done.filter(x => x.send !== p.id);
        unmarkHandled(p.itemId);
        if (e.kind === 'cancelled') {
            // Leaving the page: nothing to say. Closing the window: say it
            // when the person comes back. The draft stays.
            if (!destroyed) notify(`沒送出：${p.label}。視窗關掉或縮小了，這件回到需要你，草稿還在。`, 'error');
            return;
        }
        if (e.kind === 'undone') {
            show(idOf(p.itemId));
            notify(`復原了：${p.label}。沒有送出。`, 'info');
        } else {
            notify(`沒送出：${p.label}。${e.error}。這件回到需要你，c9watch 不會自動再送。`, 'error');
        }
    }
    // Closing the window only hides it (src-tauri lib.rs), so the page and
    // its timers live on. A hidden window sends nothing: hiding cancels
    // every waiting send, and the queue checks again just before a send.
    let windowHidden = false;
    const canSend = () => !windowHidden && (typeof document === 'undefined' || document.visibilityState !== 'hidden');
    const queue = new SendQueue(onQueue, undefined, undefined, canSend);
    const stopQueue = queue.subscribe(l => (pending = l));
    function onHide() { windowHidden = true; queue.cancelAll(); }
    function onVisibility() {
        if (document.visibilityState === 'hidden') onHide();
        else windowHidden = false;
    }
    let unlistenClose: (() => void) | null = null;
    onMount(() => {
        document.addEventListener('visibilitychange', onVisibility);
        window.addEventListener('pagehide', onHide);
        // The Tauri close button (not in a plain browser).
        try {
            getCurrentWindow().listen('tauri://close-requested', onHide)
                .then(u => { if (destroyed) u(); else unlistenClose = u; })
                .catch(() => {});
        } catch { /* not in Tauri */ }
    });
    onDestroy(() => {
        // Leaving the page sends nothing (stated in the toast).
        destroyed = true;
        queue.cancelAll();
        if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibility);
        if (typeof window !== 'undefined') window.removeEventListener('pagehide', onHide);
        unlistenClose?.();
        stopQueue();
        clearInterval(timer);
        for (const t of noticeTimers) clearTimeout(t);
    });

    // A feed error means c9watch lost the core and so every agent. The time
    // is when this page first saw it. Reconnect restarts every feed.
    let offlineSince: number | null = null;
    $: if ($dataError != null && offlineSince === null) offlineSince = Date.now();
    $: if ($dataError == null) offlineSince = demoOffline($fixtureOn);
    function demoOffline(fixture: boolean): number | null {
        if (!fixture) return null;
        try { return localStorage.getItem(DEMO_OFFLINE_KEY) === '1' ? Date.now() - DEMO_OFFLINE_AGO_MS : null; } catch { return null; }
    }
    const reconnect = () => location.reload();

    // True once the person picked an item (click, J / K, a send). Only then
    // may the panel take the focus.
    let moved = false;
    function show(id: string | null) { moved = true; shown = null; currentId = id; }
    /** The snapshot's 「下一件」 while it is still listed and not handled,
     * else the first (J) or last (K) item still listed. */
    function liveNext(id: string | null, step: 1 | -1): string | null {
        return id && order.includes(id) && !hiddenIds.has(id) && step === 1 ? id : stepFrom(order, hiddenIds, null, step);
    }
    function go(step: 1 | -1) {
        // From a snapshot receipt whose Need is gone: its 「下一件」.
        if (pinned && !order.includes(pinned.detail.id)) { show(liveNext(pinned.nextId, step)); return; }
        const id = stepFrom(order, hiddenIds, current?.id ?? null, step);
        // From a receipt with nothing left: the empty page.
        if (id || (current && hiddenIds.has(current.id))) show(id);
    }

    /** Marks `d` handled and moves to the 「下一件」 the panel showed
     * (handoff test #1). */
    function finish(d: HandlingDetail, send: string | null, state: Done['state']) {
        const to = stepFrom(order, hiddenIds, d.id, 1);
        markHandled(d.instance);
        done = [...done, { id: d.id, send, verb: d.verb, title: d.title, state }];
        if (state !== 'stopped') show(to);
    }
    function schedule(d: HandlingDetail, label: string, body: string) {
        const n = needs.find(x => x.id === d.id);
        const to = n?.agentId;
        if (!d.managed || !to) return;
        const replyTo = n?.msgId ?? null;
        now = Date.now();
        // The person acts, so the window shows.
        windowHidden = false;
        const send = queue.schedule(d.instance, label, async () => {
            const error = await sendMail(to, body, replyTo);
            if (error) throw new Error(error);
        });
        finish(d, send, 'waiting');
    }
    function submit(a: Answer) {
        if (!current || offlineSince !== null) return;
        const body = answerBody(current, a);
        if (body) schedule(current, answerLabel(current, a), body);
    }
    function no() {
        if (!current || offlineSince !== null) return;
        const body = noBody(current);
        if (body) schedule(current, `${current.verb} ${current.title} · ${current.no}`, body);
    }
    const undo = (id?: string) => { queue.undo(id); };

    // ⌘O: the item's session, when it has one.
    $: chatOf = member?.session ? ((s) => () => openSession(sessionFromView(s)))(member.session) : undefined;
    // Stop: managed agents only (observed sessions get no stop button). D42:
    // 停止 ends the agent normally with `agent.retire {id}`; never
    // `session.stop`, and no fallback to it.
    async function stop(d: HandlingDetail, agentId: string) {
        // Second check, and the lock is taken before any await: one stop per
        // agent, whatever item the person moves to meanwhile.
        if (!lockStop(agentId)) return;
        // The item's place and 「下一件」 before the await: a feed refresh
        // during the call may drop the item from `order`.
        const place = { index: Math.max(0, order.indexOf(d.id)), total: details.length, nextId: stepFrom(order, hiddenIds, d.id, 1) };
        const sessionKey = needs.find(x => x.id === d.id)?.sessionKey ?? null;
        let row;
        try { row = await retireAgent(agentId); }
        catch (e) { throw retireError(e); }
        finally { unlockStop(agentId); }
        // Core could not confirm that every process is gone: no receipt, the
        // item stays, and the person can press 停止 again.
        if (row?.state === 'orphaned') throw { notice: ORPHANED };
        // A `failed` row comes back unchanged: the agent had already ended,
        // so this was no normal end. Any other state is not an end at all.
        if (row?.state === 'failed') { hideRetiredSession(sessionKey); throw { notice: ALREADY_FAILED }; }
        if (row?.state !== 'retired') throw { notice: `沒有停止：核心回報的狀態是 ${row?.state ?? '（沒有）'}，不是已結束。` };
        hideRetiredSession(sessionKey);
        const at = new Date();
        const rows: [string, string][] = [
            ['做了什麼', `正常結束了 ${d.who}`],
            ['agent', agentId],
            ['時間', `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`],
            ['能不能復原', '不能。要它再做事，請重新建立一個 agent。']
        ];
        if (get(isDemoMode)) rows.push(['示範模式', '沒有真的停止任何程式']);
        const r = { title: `已停止 ${d.who}`, rows };
        receipts = { ...receipts, [d.instance]: r };
        // The snapshot, only while the person still looks at this item.
        if (currentId === d.id || (current?.id === d.id)) {
            currentId = d.id;
            shown = { detail: d, receipt: r, ...place };
        }
        finish(d, null, 'stopped');
    }
    $: stopAgent = current?.managed && !receipt ? needs.find(x => x.id === current!.id)?.agentId ?? null : null;
    $: stopOf = current && stopAgent
        ? ((d: HandlingDetail, id: string) => () => stop(d, id))(current, stopAgent)
        : undefined;
    $: stopBusy = !!stopAgent && $stopping.has(stopAgent);

    let panel: HandlingPanel | undefined;
    const keys: PageKeys = {
        option: n => panel?.option(n),
        submit: () => panel?.submit(),
        openChat: () => panel?.openChat(),
        next: () => go(1),
        prev: () => go(-1),
        undo: () => undo()
    };
    // The receipt does not accept ⌘↵ (R7-SPEC §A.2).
    $: keys.receipt = !!receipt;
    onMount(() => usePageKeys(keys));

    const rowName = (id: string) => {
        const n = needs.find(x => x.id === id);
        const r = n ? locate(n, $model.workspaces).row : null;
        return r ? (r.kind === 'talk' ? `對談 · ${r.title}` : r.title) : '';
    };
    $: sortRule = needs.some(n => n.blocking != null) ? '依擋住的 agent 數排。一樣多時，等越久越前面。' : '等越久越前面。';
    $: visible = details.filter(d => !hiddenIds.has(d.id));
</script>

<div class="needs" data-needs={visible.length}>
    <aside class="queue" aria-label="需要你的事">
        {#if visible.length}<span class="rule">{sortRule}</span>{/if}
        {#each visible as d (d.id)}
            <button type="button" class="qi" class:on={d.id === current?.id} aria-current={d.id === current?.id ? 'true' : undefined} on:click={() => show(d.id)} data-need={d.id}>
                <span class="t"><span class="verb">{d.verb}</span>{d.title}</span>
                <span class="s">{[rowName(d.id), `等了 ${waitText(d.since, now)}`, d.reversible ? '' : '不能復原'].filter(Boolean).join(' · ')}</span>
            </button>
        {:else}
            <p class="empty">現在沒有事。</p>
        {/each}
        {#if done.length}
            <details class="handled" open>
                <summary>已處理 {done.length} 件</summary>
                <ul>{#each done as h, i (i)}<li data-done={h.id}><span class="verb done">{h.verb}</span>{h.title}<span class="st"> · {STATE_TEXT[h.state]}</span></li>{/each}</ul>
                <p class="redo">10 秒後就不能復原，只能「改」：送一則新指示給 agent。已經做的不會自動還原。</p>
            </details>
        {/if}
    </aside>
    <main class="view" aria-label="需要你">
        {#if current}
            <HandlingPanel
                bind:this={panel}
                detail={current}
                bind:answer={answers[current.instance]}
                position={{ index: index + 1, total }}
                next={next ? { verb: next.verb, title: next.title } : null}
                offline={offlineSince !== null ? { since: offlineSince } : null}
                {now}
                {receipt}
                autofocus={moved}
                onOpenChat={chatOf}
                onSubmit={current.canAnswer && current.managed ? submit : undefined}
                onNo={current.canAnswer && current.managed && current.no ? no : undefined}
                onStop={stopOf}
                {stopBusy}
                onUndo={() => undo()}
                demo={$isDemoMode}
                ruleMode={$fixtureOn ? 'preview' : 'hidden'}
                onReconnect={reconnect}
                onNext={() => go(1)}
                onPrev={() => go(-1)}
            />
        {:else}
            <div class="none">
                <h1>沒有需要你的事</h1>
                <p>agent 要你允許、決定、給東西或審查時，會出現在這裡。</p>
            </div>
        {/if}
        <UndoToasts {pending} {notices} {now} demo={$isDemoMode} onUndo={id => undo(id)} onDismiss={dismiss} />
    </main>
</div>

<style>
    .needs { display: grid; grid-template-columns: 280px minmax(0, 1fr); min-width: 0; min-height: 0; height: 100%; }
    .queue {
        display: flex; flex-direction: column; gap: var(--space-1); min-height: 0; overflow: auto;
        padding: var(--space-5) var(--space-3); border-right: 1px solid var(--divider); background: var(--surface-1);
    }
    .rule { margin: 2px var(--space-1) 10px; font-size: 12px; color: var(--text-3); }
    .qi {
        display: block; width: 100%; padding: 9px var(--space-3); border: 0; border-radius: var(--radius-md);
        background: transparent; font: var(--type-body); font-size: 13px; color: var(--text-2); text-align: left; cursor: pointer;
    }
    .qi:hover { background: var(--fill-hover); }
    .qi.on { background: var(--fill-active); color: var(--text-1); box-shadow: inset 2px 0 0 var(--text-1); }
    .t { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .s { display: block; font-size: 12px; color: var(--text-3); }
    .verb { margin-right: 4px; font-weight: 700; color: var(--status-permission-text); }
    .verb.done { color: var(--text-3); }
    .empty { margin: var(--space-1); font-size: 13px; color: var(--text-3); }
    .handled { margin: 10px var(--space-1) 0; font-size: 12px; color: var(--text-3); }
    .handled summary { cursor: pointer; text-decoration: underline; }
    .handled ul { margin: 6px 0 0; padding-left: var(--space-4); }
    .handled li { color: var(--text-2); }
    .handled .st { color: var(--text-3); }
    .redo { margin: 6px 0 0; }
    .view { min-width: 0; overflow: auto; padding: 28px var(--space-4) 60px var(--space-12); }
    .none h1 { margin: 0 0 var(--space-2); font: var(--type-title); color: var(--text-1); }
    .none p { margin: 0; font: var(--type-body); color: var(--text-3); }
    @media (max-width: 1100px) { .view { padding-left: var(--space-8); } }
    @media (max-width: 760px) {
        .needs { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto minmax(0, 1fr); }
        .queue { max-height: 40vh; border-right: 0; border-bottom: 1px solid var(--divider); }
        .view { padding-left: var(--space-4); }
    }
</style>
