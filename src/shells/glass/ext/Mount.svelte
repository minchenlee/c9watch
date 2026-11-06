<script lang="ts">
    // Mounts one extension page in a shadow root (glass build plan §3.3).
    // The shadow root keeps CSS apart (UX, not security: D31). A page that
    // throws or never calls ready() leaves the host frame working: the
    // message replaces only this box (ui-as-extension §4.2).
    import { onDestroy, onMount } from 'svelte';
    import { createHost, READY_TIMEOUT_MS, type ExtModule, type HostContext, type HostControl, type HostTheme } from './host';
    import { entryUrl, type ExtEntry, type ExtPage } from './manifest';

    export let entry: ExtEntry;
    export let page: ExtPage;
    export let suspended = false;
    export let context: HostContext;
    export let theme: HostTheme;
    export let deps: { c9p: import('$lib/data/transport').Transport; navigate(hash: string): boolean; forwardKey(e: KeyboardEvent): void };

    let box: HTMLDivElement;
    let state: 'loading' | 'ready' | 'slow' | 'stopped' = 'loading';
    let problem = '';
    let ctl: HostControl | null = null;
    let handle: { destroy(): void } | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let resize: ResizeObserver | undefined;
    let gone = false;

    function stop(err: unknown) {
        console.error(`[c9ext ${entry.dir}] stopped`, err);
        state = 'stopped';
        problem = err instanceof Error ? err.message : String(err);
        clearTimeout(timer);
    }

    onMount(async () => {
        const root = box.attachShadow({ mode: 'open' });
        ctl = createHost({ id: entry.dir, version: entry.manifest?.version ?? '0.0.0' }, {
            ...deps,
            theme: () => theme,
            context: () => context,
            onReady: () => { if (state !== 'stopped') { state = 'ready'; clearTimeout(timer); } }
        });
        timer = setTimeout(() => { if (state === 'loading') state = 'slow'; }, READY_TIMEOUT_MS);
        try {
            const url = entryUrl(entry, page);
            // A runtime URL by design (D33 Q8: the same path as third-party
            // code). The boundary rule bans computed imports to keep Tauri
            // imports in src/lib/data; an extension URL cannot name a package.
            // eslint-disable-next-line no-restricted-syntax
            const mod = (await import(/* @vite-ignore */ url)) as Partial<ExtModule>;
            if (gone) return;
            if (typeof mod.mount !== 'function') throw new Error(`${page.entry} 沒有匯出 mount()`);
            const out = await mod.mount(root, ctl.host);
            if (gone) { try { out?.destroy?.(); } catch { /* already gone */ } return; }
            handle = out && typeof out.destroy === 'function' ? out : null;
            resize = new ResizeObserver(([e]) => ctl?.emit('resize', { width: e.contentRect.width, height: e.contentRect.height }));
            resize.observe(box);
            if (suspended) ctl.emit('suspend', null);
        } catch (err) {
            if (!gone) stop(err);
        }
    });

    onDestroy(() => {
        gone = true;
        clearTimeout(timer);
        resize?.disconnect();
        try { handle?.destroy(); } catch (err) { console.error(`[c9ext ${entry.dir}] destroy threw`, err); }
        ctl?.dispose();
        box?.shadowRoot?.replaceChildren();
    });

    // Lifecycle and context events. Each fires only on a change.
    let wasSuspended = suspended;
    $: if (ctl && suspended !== wasSuspended) { wasSuspended = suspended; ctl.emit(suspended ? 'suspend' : 'resume', null); }
    let lastContext = context;
    $: if (ctl && context !== lastContext) { lastContext = context; ctl.emit('context', { ...context }); }
    let lastTheme = theme;
    $: if (ctl && theme !== lastTheme) { lastTheme = theme; ctl.emit('theme', theme); }
</script>

<div class="ext-mount" data-ext-state={state}>
    <div class="ext-box" bind:this={box} data-c9ext-host={entry.dir} hidden={state === 'stopped'}></div>
    {#if state === 'loading'}
        <p class="state-msg">載入中…</p>
    {:else if state === 'slow'}
        <p class="state-msg" role="status">extension 沒有回應</p>
    {:else if state === 'stopped'}
        <div class="state-msg" role="alert">
            <p>extension 停止了</p>
            <p class="why">{problem}</p>
        </div>
    {/if}
</div>

<style>
    .ext-mount { position: relative; min-height: 0; }
    .ext-box[hidden] { display: none; }
    .why { margin-top: var(--space-1); font: var(--type-meta); color: var(--text-3); text-transform: none; }
    p { margin: 0; }
</style>
