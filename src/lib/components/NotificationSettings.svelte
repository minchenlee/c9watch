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
    <div class="heading"><div><h2 id="notification-heading">Notifications</h2><p class="intro">Choose what reaches you. Changes apply when you save.</p></div><span class="platform">macOS native</span></div>
    {#if prefs}
        <div class="settings-layout">
            <fieldset disabled={busy}>
                <div class="row master"><span>Notifications<small>For all providers on this Mac.</small></span><button class="choice" class:active={prefs.enabled} aria-label="Enable notifications" aria-pressed={prefs.enabled} onclick={() => { if (prefs) prefs.enabled = !prefs.enabled; }}>{prefs.enabled ? 'On' : 'Off'}</button></div>
                {#if !prefs.enabled}<p class="off-note">Notifications are off. Your choices below will be kept.</p>{/if}
                <div class="setting-group events" role="group" aria-labelledby="event-heading">
                    <h3 id="event-heading">Notify me when</h3>
                    <div class="event-row"><span>A reply is ready</span><button class="choice" class:active={prefs.replyReady} aria-label="A reply is ready" aria-pressed={prefs.replyReady} onclick={() => { if (prefs) prefs.replyReady = !prefs.replyReady; }}>{prefs.replyReady ? 'On' : 'Off'}</button></div>
                    <div class="event-row"><span>An agent asks a question</span><button class="choice" class:active={prefs.questions} aria-label="An agent asks a question" aria-pressed={prefs.questions} onclick={() => { if (prefs) prefs.questions = !prefs.questions; }}>{prefs.questions ? 'On' : 'Off'}</button></div>
                    <div class="event-row"><span>A tool needs permission</span><button class="choice" class:active={prefs.permissions} aria-label="A tool needs permission" aria-pressed={prefs.permissions} onclick={() => { if (prefs) prefs.permissions = !prefs.permissions; }}>{prefs.permissions ? 'On' : 'Off'}</button></div>
                </div>
                <div class="setting-group">
                    <h3>Content detail</h3>
                    <div class="detail-options" role="group" aria-label="Content detail">
                        <button class="choice" class:active={prefs.detail === 'brief'} aria-pressed={prefs.detail === 'brief'} onclick={() => { if (prefs) prefs.detail = 'brief'; }}>Brief</button>
                        <button class="choice" class:active={prefs.detail === 'detailed'} aria-pressed={prefs.detail === 'detailed'} onclick={() => { if (prefs) prefs.detail = 'detailed'; }}>Detailed</button>
                    </div>
                    <p class="hint">{prefs.detail === 'brief' ? 'Event and session only.' : 'Includes conversation text and commands.'}</p>
                </div>
                <div class="row"><span>Play a sound<small>Allow sounds in macOS notification settings too.</small></span><button class="choice" class:active={prefs.sound} aria-label="Play a sound" aria-pressed={prefs.sound} onclick={() => { if (prefs) prefs.sound = !prefs.sound; }}>{prefs.sound ? 'On' : 'Off'}</button></div>
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
    h2 { margin: 0 0 var(--space-sm); font: 600 13px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .1em; }
    h3 { font: 600 13px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .1em; margin: 0 0 var(--space-sm); }
    p { margin: 0; }
    .intro, .hint, small { color: var(--text-secondary); font-size: 13px; line-height: 1.55; }
    .platform { font-family: var(--font-mono); color: var(--text-secondary); font-size: 12px; white-space: nowrap; border: 1px solid var(--border-default); padding: 4px 8px; }
    .settings-layout { display: grid; grid-template-columns: minmax(0, 1fr) 280px; gap: var(--space-2xl); align-items: start; }
    fieldset { border: 0; padding: 0; margin: 0; min-width: 0; }
    .row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-xl); padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
    .master { padding-top: 0; }
    small { display: block; margin-top: 4px; }
    .setting-group { padding: var(--space-lg) 0; border-bottom: 1px solid var(--border-default); }
    /* History pressed-button and segmented-option surfaces; text labels remain readable Sans. */
    .event-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-lg); padding: var(--space-xs) 0; }
    .detail-options { display: inline-flex; border: 1px solid var(--border-default); margin-bottom: var(--space-sm); }
    button.choice { padding: 4px var(--space-sm); min-width: 42px; border: 1px solid var(--border-default); font-family: var(--font-pixel); font-size: 11px; letter-spacing: .05em; text-transform: uppercase; color: var(--text-secondary); }
    .detail-options button.choice { border: 0; }
    button.choice:hover:not(:disabled) { background: rgba(255,255,255,.08); color: var(--text-primary); }
    button.choice.active { background: rgba(255,255,255,.1); color: var(--text-primary); }
    .number-control { display: flex; align-items: center; gap: var(--space-sm); color: var(--text-secondary); }
    input[type=number] { width: 72px; color: var(--text-primary); background: var(--bg-elevated); border: 1px solid var(--border-default); padding: var(--space-sm) var(--space-md); font: 13px/1.5 var(--font-mono); }
    aside { padding: var(--space-lg); background: var(--bg-card); position: sticky; top: 0; }
    .preview { background: var(--bg-card-hover); border-radius: var(--radius-sm); padding: var(--space-lg); display: flex; flex-direction: column; gap: 7px; font-size: 13px; line-height: 1.5; margin: var(--space-xl) 0; overflow-wrap: anywhere; }
    .preview-app { display: flex; gap: var(--space-sm); align-items: center; color: var(--text-secondary); margin-bottom: 4px; font-size: 12px; }
    .app-icon { background: var(--bg-base); color: var(--text-primary); font-family: var(--font-mono); padding: 3px 5px; border-radius: var(--radius-sm); }
    .preview-time { margin-left: auto; }
    .preview strong { font-size: 15px; }
    .preview p { color: var(--text-secondary); }
    .test { width: 100%; display: flex; justify-content: space-between; margin-bottom: 12px; }
    /* Cost scale-trigger action; no fixed form-wide height. */
    button { cursor: pointer; color: var(--text-secondary); background: transparent; border: 1px solid var(--border-default); padding: 4px var(--space-sm); font: 11px/1.5 var(--font-pixel); text-transform: uppercase; letter-spacing: .05em; transition: color var(--transition-fast), border-color var(--transition-fast); }
    button:hover:not(:disabled) { color: var(--text-primary); border-color: var(--text-muted); }
    button.primary { color: var(--text-primary); border-color: var(--text-secondary); }
    button:disabled { opacity: .5; cursor: default; }
    :is(button, input):focus-visible { outline: 1px solid var(--border-focus); outline-offset: 0; }
    .save-bar { display: flex; align-items: center; justify-content: space-between; gap: var(--space-lg); margin-top: var(--space-xl); padding: var(--space-lg) 0; border-top: 1px solid var(--text-muted); }
    .save-state { color: var(--text-secondary); font-size: 14px; }
    .off-note, .permission-note, .feedback { color: var(--text-secondary); font-size: 13px; line-height: 1.6; margin-top: 14px; }
    .error { color: var(--accent-red); font-size: 14px; overflow-wrap: anywhere; margin-top: 12px; }
    @media (max-width: 1180px) { .settings-layout { grid-template-columns: minmax(0, 1fr); gap: var(--space-xl); } aside { position: static; } .preview { max-width: 420px; } .test { width: auto; gap: var(--space-xl); } }
    @media (max-width: 520px) { .heading { align-items: flex-start; flex-direction: column; gap: var(--space-sm); } .detail-options { flex-wrap: wrap; } .row { gap: var(--space-md); } .save-bar { flex-wrap: wrap; } }
    @media (prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
