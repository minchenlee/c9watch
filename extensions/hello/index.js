// Hello: the smallest glass extension, for tests only (`dev: true`, never in
// a production build). It shows the session count, reads and writes one
// pref, sends a `navigate` intent, shows a theme token and the host context,
// and sends the keys it does not use back to the host.
export function mount(root, host) {
    root.innerHTML = `
        <style>
            :host { display: block; color: var(--text-1); font: var(--type-body); }
            .card { padding: var(--space-4); border: 1px solid var(--glass-stroke); border-radius: var(--radius-md); background: var(--surface-1); }
            dl { display: grid; grid-template-columns: max-content 1fr; gap: var(--space-2) var(--space-4); margin: 0 0 var(--space-4); }
            dt { color: var(--text-2); }
            dd { margin: 0; }
            button { font: var(--type-label); color: var(--text-1); background: var(--fill-hover); border: 1px solid var(--glass-stroke); border-radius: var(--radius-sm); padding: var(--space-1) var(--space-3); cursor: pointer; }
        </style>
        <div class="card">
            <dl>
                <dt>Sessions</dt><dd data-hello="sessions">…</dd>
                <dt>--text-1</dt><dd data-hello="token"></dd>
                <dt>Glass</dt><dd data-hello="glass"></dd>
                <dt>Needs item</dt><dd data-hello="needs">—</dd>
                <dt>Clicks</dt><dd data-hello="clicks">0</dd>
                <dt>State</dt><dd data-hello="state">running</dd>
            </dl>
            <button type="button" data-hello="click">Count a click</button>
            <button type="button" data-hello="go">Open 需要你</button>
            <input data-hello="input" aria-label="Type here" placeholder="Type here">
        </div>`;
    const $ = name => root.querySelector(`[data-hello="${name}"]`);
    const theme = t => { $('token').textContent = t.tokens['--text-1'] ?? ''; $('glass').textContent = t.glass; };
    theme(host.theme);
    const context = c => { $('needs').textContent = c.needsItemId ?? '—'; };
    context(host.context);

    let alive = true;
    host.c9p.request('session.views', {}).then(
        r => { if (alive) $('sessions').textContent = String(r?.sessions?.length ?? 0); },
        () => { if (alive) $('sessions').textContent = 'error'; }
    );

    const prefs = host.prefs.get();
    let clicks = typeof prefs?.clicks === 'number' ? prefs.clicks : 0;
    $('clicks').textContent = String(clicks);
    $('click').addEventListener('click', () => { clicks += 1; host.prefs.set({ clicks }); $('clicks').textContent = String(clicks); });
    $('go').addEventListener('click', async () => { $('state').textContent = `intent: ${await host.intent('navigate', { to: '#/needs' })}`; });

    // Hello uses no keys of its own: send every key back to the host.
    const onKey = e => host.keys.forward(e);
    root.addEventListener('keydown', onKey);

    const off = [
        host.on('theme', theme),
        host.on('context', context),
        host.on('suspend', () => { $('state').textContent = 'suspended'; }),
        host.on('resume', () => { $('state').textContent = 'running'; })
    ];
    host.ready();
    return {
        destroy() {
            alive = false;
            off.forEach(f => f());
            root.removeEventListener('keydown', onKey);
        }
    };
}
