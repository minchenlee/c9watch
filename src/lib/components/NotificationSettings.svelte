<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { notificationPermission, checkNotificationPermission } from '$lib/stores/sessions';
    type Preferences = { enabled: boolean; replyReady: boolean; questions: boolean; permissions: boolean; detail: 'brief' | 'detailed'; sound: boolean; cooldownSeconds: number };
    let prefs = $state<Preferences | null>(null);
    let saved = $state('');
    let busy = $state(false);
    let status = $state('');
    let error = $state('');
    let dirty = $derived(prefs !== null && JSON.stringify(prefs) !== saved);
    async function load() {
        error = '';
        try {
            prefs = await invoke<Preferences>('get_notification_preferences');
            saved = JSON.stringify(prefs);
            await checkNotificationPermission();
        } catch (e) { error = String(e); }
    }
    onMount(load);
    async function save() {
        if (!prefs) return;
        busy = true; error = ''; status = '';
        try {
            prefs = await invoke<Preferences>('save_notification_preferences', { preferences: prefs });
            saved = JSON.stringify(prefs); status = 'Saved';
        } catch (e) { error = String(e); }
        finally { busy = false; }
    }
    async function test() {
        busy = true; error = ''; status = '';
        try {
            await invoke('test_native_notification');
            status = 'Test requested. Check Notification Center; macOS notification settings and Focus control delivery.';
        } catch (e) { error = String(e); }
        finally { busy = false; }
    }
</script>

<section aria-labelledby="notification-heading">
    <div class="heading"><div><h2 id="notification-heading">Notifications</h2><p class="intro">Choose what reaches you, and how much you see.</p></div><span class="platform">macOS native</span></div>
    {#if prefs}
        <div class="settings-layout">
            <fieldset disabled={busy}>
                <label class="row master"><span><strong>Enable notifications</strong><small>For all providers on this Mac.</small></span><input type="checkbox" role="switch" aria-label="Enable notifications" bind:checked={prefs.enabled} /></label>
                {#if !prefs.enabled}<p class="off-note">Notifications are off. Your choices below will be kept.</p>{/if}
                <div class="setting-group events" role="group" aria-labelledby="event-heading">
                    <h3 id="event-heading">Notify me when</h3>
                    <label><input type="checkbox" bind:checked={prefs.replyReady} /><span>A reply is ready</span></label>
                    <label><input type="checkbox" bind:checked={prefs.questions} /><span>An agent asks a question</span></label>
                    <label><input type="checkbox" bind:checked={prefs.permissions} /><span>A tool needs permission</span></label>
                </div>
                <div class="setting-group">
                    <h3>Content detail</h3>
                    <div class="detail-options" role="group" aria-label="Content detail">
                        <label class:selected={prefs.detail === 'brief'}><input type="radio" name="notification-detail" value="brief" bind:group={prefs.detail} /><span>Brief<small>Event and session</small></span></label>
                        <label class:selected={prefs.detail === 'detailed'}><input type="radio" name="notification-detail" value="detailed" bind:group={prefs.detail} /><span>Detailed<small>Includes a preview</small></span></label>
                    </div>
                    <p class="hint">Detailed notifications can include conversation text and commands.</p>
                </div>
                <label class="row"><span>Play a sound<small>Allow sounds in macOS notification settings too.</small></span><input type="checkbox" role="switch" aria-label="Play a sound" bind:checked={prefs.sound} /></label>
                <div class="row cooldown"><label for="notification-cooldown">Cooldown<small>Between the same event in a session.<br />Use 0 for no delay.</small></label><div class="number-control"><input id="notification-cooldown" type="number" min="0" max="600" step="1" bind:value={prefs.cooldownSeconds} aria-label="Cooldown in seconds" /><span>sec</span></div></div>
                {#if !Number.isInteger(prefs.cooldownSeconds) || prefs.cooldownSeconds < 0 || prefs.cooldownSeconds > 600}<p class="error" role="alert">Enter a whole number from 0 to 600 seconds.</p>{/if}
            </fieldset>
            <aside aria-label="Notification preview and test">
                <h3>Notification preview</h3>
                <p class="hint">{prefs.detail === 'detailed' ? 'Event, session, and a little context.' : 'Just the event and session.'}</p>
                <div class="preview" aria-label="Example notification"><div class="preview-app"><span class="app-icon" aria-hidden="true">c9</span><span>c9watch</span><span class="preview-time">now</span></div><strong>Improve notification settings</strong><span>Reply ready · Codex · c9watch</span>{#if prefs.detail === 'detailed'}<p>Added event filters and notification previews. The changes are ready for review.</p>{/if}</div>
                <button class="test" onclick={test} disabled={busy || dirty}>Send test notification <span aria-hidden="true">↗</span></button>
                <p class="hint">{dirty ? 'Save your changes to test this configuration.' : 'Uses your saved detail and sound choices. Works even when notifications are off.'}</p>
                {#if $notificationPermission !== 'granted'}<p class="permission-note">No notification? Check System Settings → Notifications → c9watch, and your Focus mode.</p>{/if}
            </aside>
        </div>
        <div class="save-bar"><span class="save-state" role="status">{busy ? 'Working…' : dirty ? 'Unsaved changes' : 'All changes saved'}</span><button class="primary" onclick={save} disabled={busy || !dirty || !Number.isInteger(prefs.cooldownSeconds) || prefs.cooldownSeconds < 0 || prefs.cooldownSeconds > 600}>Save preferences</button></div>
    {:else if !error}<p class="hint">Loading notification preferences…</p>{/if}
    {#if status && !dirty && status !== 'Saved'}<p role="status" class="feedback">{status}</p>{/if}
    {#if error}<p role="alert" class="error">{error}</p>{#if !prefs}<button onclick={load}>Retry</button>{/if}{/if}
</section>

<style>
    section { color: var(--text-primary); font-size: 14px; line-height: 1.5; }
    .heading { display: flex; align-items: baseline; justify-content: space-between; gap: var(--space-lg); margin-bottom: var(--space-xl); }
    h2 { margin: 0 0 6px; font-size: 18px; font-weight: 600; letter-spacing: -.02em; }
    h3 { font-size: 14px; font-weight: 600; margin: 0 0 12px; }
    p { margin: 0; }
    .intro, .hint, small { color: var(--text-description); font-size: 13px; line-height: 1.55; }
    .platform { font-family: var(--font-mono); color: var(--text-description); font-size: 12px; white-space: nowrap; border: 1px solid var(--border-default); padding: 4px 8px; }
    .settings-layout { display: grid; grid-template-columns: minmax(0, 1fr) 280px; gap: var(--space-2xl); align-items: start; }
    fieldset { border: 0; padding: 0; margin: 0; min-width: 0; }
    .row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-xl); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
    .master { padding-top: 0; }
    small { display: block; margin-top: 4px; }
    .setting-group { padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
    .events label { width: fit-content; max-width: 100%; padding-right: var(--space-md); display: flex; align-items: center; gap: var(--space-md); min-height: 32px; cursor: pointer; }
    input[type=checkbox], input[type=radio] { appearance: none; -webkit-appearance: none; width: 16px; height: 16px; flex-shrink: 0; border: 1px solid var(--text-secondary); background: var(--bg-base); display: inline-grid; place-content: center; margin: 0; cursor: pointer; }
    input[type=checkbox]:checked, input[type=radio]:checked { background: var(--text-primary); border-color: var(--text-primary); }
    input[type=checkbox]:not([role=switch]):checked { background: var(--bg-base); }
    input[type=checkbox]:not([role=switch]):checked::before { content: ''; width: 6px; height: 6px; background: var(--text-primary); }
    input[type=radio] { border-radius: var(--radius-sm); }
    input[type=radio]:checked::before { content: ''; width: 6px; height: 6px; border-radius: var(--radius-sm); background: var(--bg-base); }
    input[role=switch] { width: 32px; height: 18px; border-radius: var(--radius-sm); display: flex; align-items: center; justify-content: flex-start; padding: 2px; background: var(--border-default); }
    input[role=switch]::before { content: ''; display: block; width: 12px; height: 12px; border-radius: var(--radius-sm); background: var(--text-description); }
    input[role=switch]:checked { background: var(--text-primary); }
    input[role=switch]:checked::before { content: ''; background: var(--bg-base); transform: translateX(13px); }
    .detail-options { display: flex; gap: var(--space-sm); margin-bottom: 12px; }
    .detail-options label { display: flex; flex: 0 1 160px; align-items: center; gap: var(--space-sm); border: 1px solid var(--text-muted); padding: var(--space-sm) var(--space-md); cursor: pointer; }
    .detail-options label.selected { background: var(--bg-card-hover); border-color: var(--text-primary); }
    .detail-options small { font-size: 12px; }
    .number-control { display: flex; align-items: center; gap: var(--space-sm); color: var(--text-description); }
    input[type=number], button { color: var(--text-primary); background: var(--bg-card); border: 1px solid var(--text-muted); border-radius: var(--radius-sm); padding: 6px var(--space-md); font: inherit; font-size: 12px; min-height: 32px; }
    input[type=number] { width: 64px; }
    aside { padding: var(--space-lg); background: var(--bg-card); position: sticky; top: 0; }
    .preview { background: var(--bg-card-hover); border-radius: var(--radius-sm); padding: var(--space-lg); display: flex; flex-direction: column; gap: 7px; font-size: 13px; line-height: 1.5; margin: var(--space-xl) 0; overflow-wrap: anywhere; }
    .preview-app { display: flex; gap: var(--space-sm); align-items: center; color: var(--text-description); margin-bottom: 4px; font-size: 12px; }
    .app-icon { background: var(--bg-base); color: var(--text-primary); font-family: var(--font-mono); padding: 3px 5px; border-radius: var(--radius-sm); }
    .preview-time { margin-left: auto; }
    .preview strong { font-size: 14px; }
    .preview p { color: var(--text-description); }
    .test { width: 100%; display: flex; justify-content: space-between; margin-bottom: 12px; }
    button { cursor: pointer; font-family: var(--font-mono); font-size: 12px; transition: background var(--transition-fast), border-color var(--transition-fast); }
    button:hover:not(:disabled), .events label:hover, .detail-options label:hover { background: var(--bg-card-hover); }
    button.primary { background: var(--text-primary); color: var(--bg-base); border-color: var(--text-primary); font-weight: 600; }
    button.primary:hover:not(:disabled) { background: var(--text-description); }
    button:disabled { color: var(--text-secondary); border-color: var(--border-default); background: var(--bg-elevated); cursor: default; }
    fieldset:disabled { opacity: .65; }
    :is(button, input):focus-visible { outline: 2px solid var(--text-primary); outline-offset: 4px; }
    .save-bar { display: flex; align-items: center; justify-content: space-between; gap: var(--space-lg); margin-top: var(--space-xl); padding: var(--space-lg) 0; border-top: 1px solid var(--text-muted); }
    .save-state { color: var(--text-description); font-size: 14px; }
    .off-note, .permission-note, .feedback { color: var(--text-description); font-size: 13px; line-height: 1.6; margin-top: 14px; }
    .error { color: var(--accent-red); font-size: 14px; overflow-wrap: anywhere; margin-top: 12px; }
    @media (max-width: 1180px) { .settings-layout { grid-template-columns: minmax(0, 1fr); gap: var(--space-xl); } aside { position: static; } .preview { max-width: 420px; } .test { width: auto; gap: var(--space-xl); } }
    @media (max-width: 520px) { .heading { align-items: flex-start; flex-direction: column; gap: var(--space-sm); } .detail-options { flex-wrap: wrap; } .detail-options label { flex-basis: auto; } .row { gap: var(--space-md); } .save-bar { flex-wrap: wrap; } }
    @media (prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
