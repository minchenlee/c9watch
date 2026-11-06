<script lang="ts">
    // Glass classic card (direction §10, owner revision 2026-10-31): order
    // title → tag → branch → status → preview → buttons; square corners, 1px
    // border, white OPEN; auto height; at most 2 tags; buttons on hover or
    // focus, except on a card that needs you. Orange only for a needs-queue
    // item. No approve / reject here: the handling panel does that (G5).
    import type { C9pAnnotation } from '$lib/c9p-1';
    import { openSession } from '$lib/data/api';
    import type { MemberView } from '../model';
    import type { Need } from '../model/needs';
    import { routeHash } from '../nav';
    import PixelStatus from './PixelStatus.svelte';
    import AnnotationChips from './AnnotationChips.svelte';
    import { cardStatus, timeAgo } from './format';

    export let member: MemberView;
    /** This member's item in the needs queue, if any. */
    export let need: Need | null = null;
    export let annotations: C9pAnnotation[] = [];
    export let now: number;

    $: s = member.session;
    $: status = cardStatus(member, need?.verb ?? null);
    $: provider = member.agent?.provider ?? (s?.provider === 'claudeCode' ? 'claude' : s?.provider) ?? null;
    $: role = member.role === 'talk' ? '對談' : member.role;
    $: tags = [provider, role].filter((t): t is string => !!t).slice(0, 2);
    $: meta = [s ? `${s.messageCount} msg` : null, timeAgo(member.lastChange || member.seenAt, now)].filter(Boolean).join(' · ');
    // A queue item that is not just the detected session state (demo verbs,
    // attention, mail) names the ask; otherwise the session's own last line.
    $: preview = need && need.source !== 'session' ? need.title : s ? s.lastAssistantMessage || s.latestMessage || s.firstPrompt : member.agent?.stateReason ?? '';
    $: tool = need?.verb === '允許' && s?.pendingToolName ? toolLine(s.pendingToolName, s.pendingToolInput) : null;
    $: canOpen = !!s?.canOpen && member.live;

    function toolLine(name: string, input: unknown): string {
        const cmd = input && typeof input === 'object' && 'command' in input ? String((input as { command: unknown }).command) : null;
        return cmd ? `${name}: ${cmd}` : name;
    }

    let openError = '';
    async function open() {
        if (!s) return;
        openError = '';
        // The feed's SessionView rows are the data layer's Session rows.
        try { await openSession(s as unknown as Parameters<typeof openSession>[0]); }
        catch (e) { openError = e instanceof Error ? e.message : '打不開這個 session'; }
    }
</script>

<article
    class="card {status.tone}"
    class:done={!member.live}
    data-member={member.key}
    data-tone={status.tone}
    aria-label="{member.name}，{status.label}"
>
    <h3 class="tt" title={member.name}>{member.name}</h3>
    <div class="r">
        <span class="tags">{#each tags as t}<span class="tag">{t}</span>{/each}</span>
        <span class="mt">{meta}</span>
    </div>
    {#if s?.gitBranch}
        <div class="br" title={s.gitBranch}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                <line x1="6" y1="3" x2="6" y2="15" /><circle cx="18" cy="6" r="3" /><circle cx="6" cy="18" r="3" /><path d="M18 9a9 9 0 0 1-9 9" />
            </svg>
            <span>{s.gitBranch}</span>
        </div>
    {/if}
    <PixelStatus tone={status.tone} label={status.label} />
    {#if preview}<p class="pv" title={preview}>{preview}</p>{/if}
    {#if tool}<code class="tool" title={tool}>{tool}</code>{/if}
    <AnnotationChips items={annotations} {now} />
    {#if canOpen || need}
        <div class="ac" class:hov={!need}>
            {#if need}<a class="btn" href={routeHash({ page: 'needs' })}>在需要你處理</a>{/if}
            {#if canOpen}<button type="button" class="btn pri" on:click={open}>OPEN</button>{/if}
        </div>
    {/if}
    {#if openError}<p class="err" role="alert">{openError}</p>{/if}
</article>

<style>
    .card {
        display: grid;
        gap: var(--space-2);
        align-content: start;
        min-width: 0;
        padding: 14px var(--space-4);
        background: var(--surface-2);
        border: 1px solid rgb(255 255 255 / 0.10);
        border-radius: 0;
    }
    .card:hover, .card:focus-within { border-color: rgb(255 255 255 / 0.22); }
    .card.needs {
        border-color: rgb(255 106 26 / 0.55);
        box-shadow: 0 0 22px var(--status-permission-glow);
    }
    .card.done { background: var(--surface-1); }
    .card.done .tt, .card.done .pv { color: var(--text-3); }
    .tt {
        margin: 0;
        font: var(--type-heading);
        font-size: 14px;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        line-height: 1.35;
        color: var(--text-1);
        display: -webkit-box;
        -webkit-line-clamp: 2;
        line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
        overflow-wrap: anywhere;
    }
    .r { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); min-width: 0; }
    .tags { display: inline-flex; gap: var(--space-1); min-width: 0; overflow: hidden; }
    .tag {
        font-family: var(--font-pixel);
        font-weight: 500;
        font-size: 12px;
        line-height: 16px;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        color: var(--text-2);
        border: 1px solid rgb(255 255 255 / 0.14);
        padding: 1px 6px;
        white-space: nowrap;
    }
    .mt, .br { font: var(--type-meta); font-size: 12px; color: var(--text-3); white-space: nowrap; }
    .br { display: flex; align-items: center; gap: var(--space-1); min-width: 0; }
    .br svg { flex: none; }
    .br span { overflow: hidden; text-overflow: ellipsis; }
    .pv {
        margin: 0;
        font: var(--type-body);
        color: var(--text-2);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .tool {
        display: block;
        padding: 6px var(--space-2);
        background: rgb(0 0 0 / 0.35);
        font: var(--type-meta);
        font-size: 12px;
        color: var(--text-1);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .ac { display: flex; gap: 6px; justify-content: flex-end; flex-wrap: wrap; }
    .ac.hov { opacity: 0; transition: opacity 0.15s; }
    .card:hover .ac.hov, .card:focus-within .ac.hov { opacity: 1; }
    @media (hover: none) { .ac.hov { opacity: 1; } }
    @media (prefers-reduced-motion: reduce) { .ac.hov { transition: none; } }
    .btn {
        font-family: var(--font-data);
        font-size: 12px;
        line-height: 16px;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        text-decoration: none;
        background: none;
        border: 1px solid rgb(255 255 255 / 0.14);
        padding: var(--space-1) 10px;
        color: var(--text-2);
        cursor: pointer;
    }
    .btn:hover { color: var(--text-1); border-color: rgb(255 255 255 / 0.3); }
    .btn.pri { background: var(--text-1); border-color: var(--text-1); color: var(--surface-window-solid); }
    .err { margin: 0; font: var(--type-label); color: var(--status-error-text); }
</style>
