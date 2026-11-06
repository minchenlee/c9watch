<script lang="ts">
    // 設定 (build plan G10a): Shell (classic / glass), Glass Off / Subtle /
    // Full, the monthly fee for 用量, 重新排列房間 (D7, not built yet), the
    // extension list (read-only, G6 registry) and 規則與授權 (D5, D9 = C:
    // only 允許 rules; core keeps no rules yet, Gap R).
    // This page stores `ui.glass`; the shell sets `data-glass` on
    // `.glass-root`, or `off` while Reduce Transparency is on (G10b).
    import { onMount } from 'svelte';
    import { shell } from '$lib/data/stores/settings';
    import { fixtureOn } from '../data';
    import { DEMO_RULES } from '../model/gaps-demo';
    import { glassMode, monthlyFee, parseFee, type GlassMode } from '../prefs';
    import { nativeGlass } from '../native';
    import { extensions, extensionsLoaded, loadExtensions } from '../ext/registry';
    import { describeEntry, type ExtEntry } from '../ext/manifest';
    import { codeParts } from './workspace-extras';

    onMount(() => { void loadExtensions(); });

    const MODES: { id: GlassMode; label: string; note: string }[] = [
        { id: 'off', label: 'Off', note: '不透明，最好讀' },
        { id: 'subtle', label: 'Subtle', note: '淡淡的玻璃（預設）' },
        { id: 'full', label: 'Full', note: '完整的玻璃' }
    ];

    // Glass → Classic unmounts glass at once. This Classic has no switch
    // back, so the page asks first and says how to come back.
    let confirmClassic = false;
    const toClassic = () => shell.set('classic');

    let feeText = $monthlyFee === null ? '' : String($monthlyFee);
    let feeError = '';
    let feeSaved = false;
    function saveFee() {
        const t = feeText.trim();
        if (t === '') { monthlyFee.set(null); feeError = ''; feeSaved = true; return; }
        const n = parseFee(t);
        if (n === null) { feeError = '請填 0 以上的數字，最多 2 位小數'; feeSaved = false; return; }
        monthlyFee.set(n); feeText = String(n); feeError = ''; feeSaved = true;
    }
    function feeKey(e: KeyboardEvent) {
        // An input method uses Enter to pick a word: not a save (IME guard).
        if (e.isComposing || e.keyCode === 229) return;
        if (e.key === 'Enter') { e.preventDefault(); saveFee(); }
    }

    const STATUS: Record<ExtEntry['status'], string> = {
        ok: '啟用', manifest_error: '設定檔有錯', version_mismatch: '版本不合', shadowed: '被蓋過', dev_only: '只在開發版'
    };
    const where = (e: ExtEntry) => (e.source === 'first' ? `app 內建 › extensions/${e.dir}/` : `engine home › extensions/${e.dir}/`);

    // D9 = C: the list shows 允許 rules only. Demo rules, fixture only.
    $: rules = $fixtureOn ? DEMO_RULES.filter(r => r.kind === '授權') : [];
</script>

<div class="settings" data-settings>
    <header><h1>設定</h1></header>

    <section aria-labelledby="s-look">
        <h2 class="psec" id="s-look">外觀</h2>
        <div class="box">
            <div class="field">
                <span class="lbl" id="s-shell">介面</span>
                <div class="seg" role="radiogroup" aria-labelledby="s-shell">
                    <button type="button" role="radio" aria-checked={!confirmClassic ? 'false' : 'true'} class:on={confirmClassic} data-shell="classic" on:click={() => (confirmClassic = true)}>Classic</button>
                    <button type="button" role="radio" aria-checked={confirmClassic ? 'false' : 'true'} class:on={!confirmClassic} data-shell="glass" on:click={() => (confirmClassic = false)}>Glass（預覽）</button>
                </div>
                {#if confirmClassic}
                    <div class="confirm" role="alert" data-confirm-classic>
                        <p>切到 Classic 會馬上換掉這個畫面。這一版的 Classic 還沒有切回 Glass 的開關：要回來，得在開發者工具設 <code>localStorage['ui.shell'] = 'glass'</code> 再重新載入。</p>
                        <div class="row">
                            <button type="button" class="primary" on:click={toClassic} data-confirm="classic">切到 Classic</button>
                            <button type="button" on:click={() => (confirmClassic = false)}>留在 Glass</button>
                        </div>
                    </div>
                {/if}
            </div>
            <div class="field">
                <span class="lbl" id="s-glass">玻璃效果</span>
                <div class="seg" role="radiogroup" aria-labelledby="s-glass">
                    {#each MODES as m (m.id)}
                        <button type="button" role="radio" aria-checked={$glassMode === m.id} class:on={$glassMode === m.id} data-glass-btn={m.id} on:click={() => glassMode.set(m.id)}>{m.label}</button>
                    {/each}
                </div>
                {#if $nativeGlass.reduceTransparency}
                    <p class="note" data-reduce-transparency>系統開了「減少透明度」，所以現在是 Off。關掉之後會回到你選的 {MODES.find(m => m.id === $glassMode)?.label}。</p>
                {:else}
                    <p class="note">{MODES.find(m => m.id === $glassMode)?.note}。{$nativeGlass.vibrancy ? '' : '這個視窗沒有原生玻璃，只有浮層會變。'}</p>
                {/if}
            </div>
        </div>
    </section>

    <section aria-labelledby="s-usage">
        <h2 class="psec" id="s-usage">用量</h2>
        <div class="box">
            <div class="field">
                <label class="lbl" for="s-fee">每月訂閱費合計（USD）</label>
                <div class="row">
                    <input id="s-fee" type="text" inputmode="decimal" placeholder="例如 200" bind:value={feeText} on:keydown={feeKey} on:input={() => (feeSaved = false)} on:blur={saveFee} aria-invalid={feeError ? 'true' : undefined} aria-describedby="s-fee-note" data-fee />
                    <button type="button" on:click={saveFee}>儲存</button>
                    {#if feeSaved && !feeError}<span class="ok" role="status">已儲存</span>{/if}
                </div>
                <p class="note" id="s-fee-note">{feeError || '用量頁的「實付」用這個數字按天分攤。只存在這台電腦。留空就不算實付。'}</p>
            </div>
        </div>
    </section>

    <section aria-labelledby="s-office">
        <h2 class="psec" id="s-office">辦公室</h2>
        <div class="box">
            <div class="field">
                <span class="lbl">重新排列房間</span>
                <div class="row"><button type="button" disabled data-rearrange>重新排列房間</button></div>
                <p class="note">還不能用。辦公室平面圖 extension 做好之後才會有房間。</p>
            </div>
        </div>
    </section>

    <section aria-labelledby="s-ext">
        <h2 class="psec" id="s-ext">Extensions <span>{$extensions.length}</span></h2>
        {#if !$extensionsLoaded}
            <p class="state-msg">讀取中</p>
        {:else if !$extensions.length}
            <p class="state-msg">沒有安裝 extension</p>
        {:else}
            <table class="tbl" data-table="extensions">
                <thead><tr><th>名稱</th><th>版本</th><th>來源</th><th>狀態</th><th>目錄</th></tr></thead>
                <tbody>
                    {#each $extensions as e (e.source + '/' + e.dir)}
                        <tr data-ext-row={e.dir} data-source={e.source} data-status={e.status}>
                            <td class="t">{e.manifest?.name ?? e.dir}</td>
                            <td class="mono">{e.manifest?.version ?? '—'}</td>
                            <td>{e.source === 'first' ? '第一方' : '第三方'}</td>
                            <td><span class="st" class:bad={e.status !== 'ok'}>{STATUS[e.status]}</span><span class="why">{describeEntry(e)}</span></td>
                            <td class="mono dir">{where(e)}</td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {/if}
        <p class="note">唯讀。新增或移除 extension：放進或拿出目錄，再重新開 c9watch。第三方的目錄在 engine home（macOS 預設 <code>~/Library/Application Support/c9watch/engine</code>）。</p>
    </section>

    <section aria-labelledby="s-rules" data-section="rules">
        <h2 class="psec" id="s-rules">規則與授權</h2>
        {#if rules.length}
            <p class="preview" data-preview>設計預覽，不會儲存</p>
            <ul class="list">
                {#each rules as r (r.scope + r.text)}
                    <li class="rule">
                        <span class="t">{#each codeParts(r.text) as part}{#if part.code}<code>{part.text}</code>{:else}{part.text}{/if}{/each}</span>
                        <span class="mono meta">{r.kind} · {r.scope} · 上次用到 {r.lastUsed} · 共 {r.count} 次</span>
                        <button type="button" disabled>撤銷</button>
                    </li>
                {/each}
            </ul>
        {:else}
            <p class="state-msg" data-rules-empty>還沒有規則</p>
            <p class="note">c9watch 現在還不能記住「以後直接允許」。之後的版本會把所有直接允許的指令列在這裡，可以在這裡撤銷。</p>
        {/if}
    </section>
</div>

<style>
    .settings { display: grid; gap: var(--space-8); min-width: 0; max-width: 880px; }
    h1 { margin: 0; font: var(--type-title); color: var(--text-1); }
    section { display: grid; gap: var(--space-3); min-width: 0; }
    .psec {
        display: flex;
        align-items: center;
        gap: 10px;
        margin: 0;
        font: var(--type-section);
        letter-spacing: 0.1em;
        text-transform: uppercase;
        color: var(--text-1);
    }
    .psec::before { content: ''; width: 8px; height: 8px; background: var(--status-idle); }
    .psec span { color: var(--text-3); }
    .box { display: grid; background: var(--surface-1); }
    .field { display: grid; gap: var(--space-2); padding: 14px var(--space-4); border-bottom: 1px solid var(--divider); }
    .field:last-child { border-bottom: 0; }
    .lbl { font: var(--type-label); color: var(--text-2); }
    .note { margin: 0; font: var(--type-label); color: var(--text-3); }
    .note code, .confirm code, .rule code { font: var(--type-meta); color: var(--text-1); overflow-wrap: anywhere; }
    .mono { font: var(--type-meta); }
    .row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
    .seg { display: flex; flex-wrap: wrap; gap: var(--space-1); }
    button {
        min-height: 32px;
        padding: 0 var(--space-3);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-sm);
        background: none;
        font: var(--type-label);
        color: var(--text-2);
        cursor: pointer;
    }
    button:hover:not(:disabled) { background: var(--fill-hover); }
    button.on, button.primary { background: var(--fill-active); color: var(--text-1); }
    button:disabled { color: var(--text-disabled); cursor: default; }
    button:focus-visible, input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
    input {
        width: 140px;
        min-height: 32px;
        padding: 0 var(--space-2);
        border: 1px solid var(--glass-stroke);
        border-radius: var(--radius-sm);
        background: var(--surface-2);
        font: var(--type-meta);
        color: var(--text-1);
    }
    input[aria-invalid='true'] { border-color: var(--status-error); }
    .ok { font: var(--type-label); color: var(--text-3); }
    .confirm { display: grid; gap: var(--space-2); padding: var(--space-3); border: 1px solid var(--glass-stroke); border-radius: var(--radius-md); }
    .confirm p { margin: 0; font: var(--type-body); color: var(--text-2); }
    .tbl { width: 100%; border-collapse: collapse; background: var(--surface-1); font: var(--type-body); }
    .tbl th { text-align: left; font: var(--type-label); color: var(--text-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); }
    .tbl td { padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); color: var(--text-2); vertical-align: top; }
    .tbl tr:last-child td { border-bottom: 0; }
    .t { color: var(--text-1); }
    .st { display: block; color: var(--text-1); }
    .st.bad { color: var(--status-error-text); }
    .why { display: block; font: var(--type-label); color: var(--text-3); }
    .dir { overflow-wrap: anywhere; font-size: 12px; color: var(--text-3); }
    .preview { margin: 0; font: var(--type-label); color: var(--status-input-text); }
    .list { margin: 0; padding: 0; list-style: none; background: var(--surface-1); }
    .rule { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: var(--space-1) var(--space-3); padding: 10px var(--space-4); border-bottom: 1px solid var(--divider); }
    .rule .meta { grid-column: 1; font-size: 12px; color: var(--text-3); }
    .rule button { grid-row: 1 / span 2; grid-column: 2; align-self: center; }
    .rule:last-child { border-bottom: 0; }
    @media (max-width: 700px) {
        .tbl th, .tbl td { padding: 8px var(--space-2); }
    }
</style>
