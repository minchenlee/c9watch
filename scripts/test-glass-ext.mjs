// Glass extension host v1 (G6): the manifest rules R1–R12 (owner D41),
// the host object, and the dev file server's path checks.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import ts from 'typescript';
import { extensionRoots, listExtensions, resolveExtFile } from './vite-c9ext-dev.mjs';

async function load(rel) {
    const source = fs.readFileSync(new URL(rel, import.meta.url), 'utf8');
    const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
    assert(!/^import .* from /m.test(js), `${rel} has a runtime import`);
    return import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'));
}
const M = await load('../src/shells/glass/ext/manifest.ts');
const H = await load('../src/shells/glass/ext/host.ts');

const ENV = { app: '0.11.1', apiMinor: 15, dev: false };
const base = () => ({ c9ext: 1, id: 'demo', name: 'Demo', version: '1.0.0', ui: { contract: 'c9ui/1', pages: [{ id: 'main', title: 'Demo', entry: 'index.js' }] } });
const parse = (obj, dir = 'demo') => M.parseManifest(typeof obj === 'string' ? obj : JSON.stringify(obj), dir);
const reason = (obj, dir) => { const p = parse(obj, dir); assert.equal(p.ok, false, 'should fail'); return p.reason; };
const one = (obj, { dir = 'demo', source = 'third', env = ENV } = {}) => M.resolveExtensions([{ dir, source, manifest: JSON.stringify(obj) }], env)[0];

test('a valid manifest is ok and normalized', () => {
    const p = parse({ ...base(), extra: { anything: true } });
    assert.equal(p.ok, true);
    assert.deepEqual(p.manifest, { c9ext: 1, id: 'demo', name: 'Demo', version: '1.0.0', dev: false, ui: { contract: 'c9ui/1', pages: [{ id: 'main', title: 'Demo', entry: 'index.js' }] } });
    const e = one(base());
    assert.equal(e.status, 'ok');
    assert.equal(M.describeEntry(e), '可以使用');
});

test('R1: c9ext must be the JSON number 1; strict JSON only', () => {
    for (const v of [undefined, '1', 2, 0, 1.5, true, null]) {
        const m = base(); if (v === undefined) delete m.c9ext; else m.c9ext = v;
        assert.equal(reason(m), 'manifest 版本不支援', String(v));
        const e = one(m);
        assert.equal(e.status, 'manifest_error');
        assert.equal(M.describeEntry(e), 'manifest 版本不支援');
    }
    assert.equal(reason('{ "c9ext": 1, // comment\n "id": "demo" }'), 'manifest 不是有效的 JSON');
    assert.equal(reason('/* c */ ' + JSON.stringify(base())), 'manifest 不是有效的 JSON');
    assert.equal(reason(JSON.stringify(base()).replace(/}$/, ',}')), 'manifest 不是有效的 JSON');
    assert.equal(reason('[1]'), 'manifest 不是 JSON 物件');
    assert.equal(M.parseManifest(null, 'demo').reason, '找不到 c9ext.json');
});

test('R2: ui.contract must be c9ui/1; top-level contract is ignored', () => {
    assert.equal(parse({ ...base(), contract: 'c9ui/9' }).ok, true);
    assert.equal(parse({ ...base(), contract: 42 }).ok, true);
    const m = base(); delete m.ui.contract;
    assert.match(reason(m), /UI 合約不支援/);
    m.ui.contract = 'c9ui/2';
    assert.match(reason(m), /UI 合約不支援/);
    // The old §3.4 place for the contract does not count.
    const old = base(); delete old.ui.contract; old.contract = 'c9ui/1';
    assert.match(reason(old), /UI 合約不支援/);
});

test('R3: no ui is fine, no side bar entry, 「只有 core 部分」', () => {
    const m = base(); delete m.ui; m.core = { entry: 'core.js' };
    const e = one(m);
    assert.equal(e.status, 'ok');
    assert.equal(e.hasUi, false);
    assert.deepEqual(M.sidebarPages([e]), []);
    assert.equal(M.describeEntry(e), '只有 core 部分');
    // Without core either: still valid, still no entry.
    const bare = base(); delete bare.ui;
    assert.equal(one(bare).status, 'ok');
});

test('R4: a core block is never an error; it is shown as 「含 core 部分」', () => {
    for (const core of [{}, { entry: 'x' }, 'weird', 42, [], null, { events: 'nope' }, { process: { cmd: 'rm -rf /' } }]) {
        const e = one({ ...base(), core });
        assert.equal(e.status, 'ok', JSON.stringify(core));
        assert.equal(e.hasCore, true);
        assert.equal(M.describeEntry(e), '含 core 部分');
    }
    assert.equal('core' in one({ ...base(), core: { a: 1 } }).manifest, false, 'core is not copied into the manifest');
});

test('R5: id pattern ^[a-z][a-z0-9-]{0,31}$', () => {
    for (const id of ['a', 'hello', 'office-map', 'a1', 'a'.repeat(32)]) assert.equal(parse({ ...base(), id }, id).ok, true, id);
    for (const id of ['', '1abc', '-a', 'Hello', 'a_b', 'a.b', 'a'.repeat(33), 'ä', 42]) assert.match(reason({ ...base(), id }, String(id)), /id 格式不對/, String(id));
});

test('R6: minVersion compares app and apiMinor only; core is ignored', () => {
    assert.equal(one({ ...base(), minVersion: { app: '0.11.1', apiMinor: 15 } }).status, 'ok');
    assert.equal(one({ ...base(), minVersion: { core: '99.0.0' } }).status, 'ok');
    assert.equal(one({ ...base(), minVersion: { core: 'not even semver' } }).status, 'ok');
    const app = one({ ...base(), minVersion: { app: '0.12.0' } });
    assert.equal(app.status, 'version_mismatch');
    assert.match(app.reason, /需要較新的 c9watch（0\.12\.0 以上）/);
    assert.equal(one({ ...base(), minVersion: { apiMinor: 16 } }).status, 'version_mismatch');
    assert.equal(one({ ...base(), minVersion: { app: '0.11.1-rc.1' } }).status, 'ok', 'a prerelease is older than the release');
    assert.match(reason({ ...base(), minVersion: { app: '1.0' } }), /minVersion\.app/);
    assert.match(reason({ ...base(), minVersion: { apiMinor: -1 } }), /apiMinor/);
    assert.match(reason({ ...base(), minVersion: { apiMinor: 1.5 } }), /apiMinor/);
    assert.match(reason({ ...base(), minVersion: 'new' }), /minVersion/);
});

test('R7: host.ext is exactly { id, version, call, on }; call refuses, on is a no-op', async () => {
    const ctl = H.createHost({ id: 'demo', version: '1.0.0' }, deps());
    assert.deepEqual(Object.keys(ctl.host.ext).sort(), ['call', 'id', 'on', 'version']);
    await assert.rejects(ctl.host.ext.call('office.seat', {}), e => e.code === 'refused');
    const off = ctl.host.ext.on('office.moved', () => assert.fail('never called'));
    assert.equal(typeof off, 'function');
    off();
    assert.ok(Object.isFrozen(ctl.host.ext));
    assert.deepEqual(Object.keys(ctl.host).sort(), ['c9p', 'context', 'ext', 'intent', 'keys', 'on', 'prefs', 'ready', 'theme', 'v']);
    assert.equal(ctl.host.v, 'c9ui/1');
});

test('R8: every status in the vocabulary', () => {
    const statuses = new Set(M.resolveExtensions([
        { dir: 'aa', source: 'first', manifest: JSON.stringify({ ...base(), id: 'aa' }) },
        { dir: 'aa', source: 'third', manifest: JSON.stringify({ ...base(), id: 'aa' }) },
        { dir: 'bb', source: 'third', manifest: '{' },
        { dir: 'cc', source: 'third', manifest: JSON.stringify({ ...base(), id: 'cc', minVersion: { app: '9.0.0' } }) },
        { dir: 'dd', source: 'first', manifest: JSON.stringify({ ...base(), id: 'dd', dev: true }) }
    ], ENV).map(e => e.status));
    assert.deepEqual([...statuses].sort(), ['dev_only', 'manifest_error', 'ok', 'shadowed', 'version_mismatch']);
});

test('R9: the first-party copy wins a shared id; the other is shadowed', () => {
    const list = M.resolveExtensions([
        { dir: 'office', source: 'third', manifest: JSON.stringify({ ...base(), id: 'office', name: 'Mine' }) },
        { dir: 'office', source: 'first', manifest: JSON.stringify({ ...base(), id: 'office', name: 'Built in' }) }
    ], ENV);
    assert.deepEqual(list.map(e => [e.source, e.status]), [['first', 'ok'], ['third', 'shadowed']]);
    assert.equal(M.sidebarPages(list).length, 1);
    assert.equal(M.sidebarPages(list)[0].source, 'first');
    // Shadowed even when the first-party copy is broken: the id belongs to it.
    const broken = M.resolveExtensions([
        { dir: 'office', source: 'first', manifest: '{' },
        { dir: 'office', source: 'third', manifest: JSON.stringify({ ...base(), id: 'office' }) }
    ], ENV);
    assert.deepEqual(broken.map(e => e.status), ['manifest_error', 'shadowed']);
});

test('R10: id must equal the directory name', () => {
    assert.match(reason(base(), 'other'), /和目錄名「other」不一樣/);
    assert.equal(one(base(), { dir: 'Demo' }).status, 'manifest_error');
});

test('R11: size ≤ 64 KiB, ui.pages ≤ 16, version is semver', () => {
    const big = { ...base(), pad: 'x'.repeat(64 * 1024) };
    assert.equal(reason(big), 'manifest 超過 64 KiB');
    assert.equal(M.parseManifest(null, 'demo', 70_000).reason, 'manifest 超過 64 KiB');
    assert.equal(M.parseManifest(JSON.stringify(base()), 'demo', 70_000).reason, 'manifest 超過 64 KiB', 'reported size counts too');
    // Multi-byte text counts in bytes, not characters.
    const cjk = { ...base(), pad: '字'.repeat(22_000) };
    assert.equal(reason(cjk), 'manifest 超過 64 KiB');
    const pages = n => Array.from({ length: n }, (_, i) => ({ id: `p${i}`, title: `P${i}`, entry: 'index.js' }));
    assert.equal(parse({ ...base(), ui: { contract: 'c9ui/1', pages: pages(16) } }).ok, true);
    assert.match(reason({ ...base(), ui: { contract: 'c9ui/1', pages: pages(17) } }), /最多 16 頁/);
    assert.match(reason({ ...base(), ui: { contract: 'c9ui/1', pages: [] } }), /至少要有 1 頁/);
    for (const v of ['1.0.0', '0.0.1-alpha.1', '1.2.3+build.5', '10.20.30-rc.1+x']) assert.equal(parse({ ...base(), version: v }).ok, true, v);
    for (const v of ['1', '1.0', 'v1.0.0', '01.0.0', '1.0.0-', '', 1, null]) assert.match(reason({ ...base(), version: v }), /semver/, String(v));
});

test('R12: core.events items are string | { name, subscribers? }, warnings only', () => {
    const ok = one({ ...base(), core: { events: ['office.seat', { name: 'office.move' }, { name: 'x', subscribers: ['a', 'b'] }] } });
    assert.equal(ok.status, 'ok');
    assert.deepEqual(ok.warnings, []);
    const loose = one({ ...base(), core: { events: [42, { subscribers: [] }, { name: 'x', subscribers: 'all' }, 'fine'] } });
    assert.equal(loose.status, 'ok');
    assert.deepEqual(loose.warnings, ['core.events[0] 格式不對', 'core.events[1] 格式不對', 'core.events[2] 格式不對']);
    assert.deepEqual(one({ ...base(), core: { events: {} } }).warnings, ['core.events 不是陣列']);
});

test('pages: ids unique, titles set, entries stay inside the directory', () => {
    const withPage = p => ({ ...base(), ui: { contract: 'c9ui/1', pages: [p] } });
    assert.equal(parse(withPage({ id: 'main', title: 'T', entry: 'dist/index.js', icon: 'map' })).ok, true);
    for (const entry of ['../x.js', '/abs.js', 'a/../b.js', './a.js', 'https://evil/x.js', 'c9ext://x/a.js', 'a\\b.js', 'a.js?x', '', 'a//b.js',
        '%2e%2e/hello/index.js', '.%2e/hello/index.js', '%2E%2E/x.js', 'a%2fb.js', 'a/%2e/b.js', '%5c..%5cx.js', 'a%20b.js']) {
        assert.match(reason(withPage({ id: 'main', title: 'T', entry })), /entry/, entry);
    }
    assert.match(reason(withPage({ id: 'Main', title: 'T', entry: 'a.js' })), /id 格式不對/);
    assert.match(reason(withPage({ id: 'main', title: '', entry: 'a.js' })), /title/);
    assert.match(reason({ ...base(), ui: { contract: 'c9ui/1', pages: [{ id: 'a', title: 'A', entry: 'a.js' }, { id: 'a', title: 'B', entry: 'b.js' }] } }), /兩個「a」/);
    assert.match(reason({ ...base(), dev: 'yes' }), /dev/);
    assert.match(reason({ ...base(), name: '' }), /name/);
    assert.equal(parse({ ...base(), name: 'n'.repeat(64) }).ok, true, '64 is the limit');
    assert.match(reason({ ...base(), name: 'n'.repeat(65) }), /name 必須是 1–64 字/);
});

test('dev: true shows in a dev build only', () => {
    const m = { ...base(), dev: true };
    assert.equal(one(m).status, 'dev_only');
    assert.equal(one(m, { env: { ...ENV, dev: true } }).status, 'ok');
    assert.deepEqual(M.sidebarPages([one(m)]), []);
});

test('entry URLs use the list base', () => {
    const [e] = M.resolveExtensions([{ dir: 'demo', source: 'third', manifest: JSON.stringify(base()) }], ENV, 'c9ext://localhost/');
    assert.equal(M.entryUrl(e, e.manifest.ui.pages[0]), 'c9ext://localhost/demo/index.js');
    const [d] = M.resolveExtensions([{ dir: 'demo', source: 'third', manifest: JSON.stringify(base()), base: '/x/' }], ENV);
    assert.equal(M.entryUrl(d, d.manifest.ui.pages[0]), '/x/demo/index.js');
    const [n] = M.resolveExtensions([{ dir: 'demo', source: 'third', manifest: JSON.stringify({ ...base(), ui: { contract: 'c9ui/1', pages: [{ id: 'main', title: 'T', entry: 'dist/a b.js' }] } }) }], ENV);
    assert.equal(M.entryUrl(n, n.manifest.ui.pages[0]), '/c9ext/demo/dist/a%20b.js', 'segments are encoded');
});

test('compareSemver follows semver precedence', () => {
    const order = ['1.0.0-alpha', '1.0.0-alpha.1', '1.0.0-alpha.beta', '1.0.0-beta', '1.0.0-beta.2', '1.0.0-beta.11', '1.0.0-rc.1', '1.0.0', '1.0.1', '1.1.0', '2.0.0'];
    for (let i = 0; i + 1 < order.length; i++) {
        assert.equal(M.compareSemver(order[i], order[i + 1]), -1, `${order[i]} < ${order[i + 1]}`);
        assert.equal(M.compareSemver(order[i + 1], order[i]), 1);
    }
    assert.equal(M.compareSemver('1.0.0+a', '1.0.0+b'), 0);
});

// ── host ──────────────────────────────────────────────────────────────
function memoryStorage() {
    const m = new Map();
    return { getItem: k => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: k => m.delete(k), map: m };
}
function deps(over = {}) {
    const calls = [];
    return {
        calls,
        c9p: { request: async () => ({}), subscribe: async () => () => {} },
        theme: () => ({ tokens: { '--text-1': '#fff' }, glass: 'subtle' }),
        context: () => ({ needsItemId: 'n1' }),
        navigate: hash => { calls.push(['navigate', hash]); return hash === '#/needs'; },
        forwardKey: e => calls.push(['key', e.code]),
        onReady: () => calls.push(['ready']),
        storage: memoryStorage(),
        ...over
    };
}

test('prefs: one JSON blob per extension, ≤ 64 KiB', () => {
    const d = deps();
    const { host } = H.createHost({ id: 'demo', version: '1.0.0' }, d);
    assert.equal(host.prefs.get(), undefined);
    host.prefs.set({ floor: 2 });
    assert.equal(d.storage.map.get('c9ext.demo.prefs'), '{"floor":2}');
    assert.deepEqual(host.prefs.get(), { floor: 2 });
    assert.throws(() => host.prefs.set('x'.repeat(64 * 1024)), RangeError);
    assert.throws(() => host.prefs.set(() => 1), TypeError);
    assert.deepEqual(host.prefs.get(), { floor: 2 }, 'a refused write keeps the old blob');
    d.storage.map.set('c9ext.demo.prefs', '{broken');
    assert.equal(host.prefs.get(), undefined, 'a broken blob reads as nothing');
    host.prefs.set(undefined);
    assert.equal(d.storage.map.has('c9ext.demo.prefs'), false);
    // A storage that throws (private mode) is not the extension's problem.
    const throwing = { getItem() { throw new Error('denied'); }, setItem() { throw new Error('denied'); }, removeItem() { throw new Error('denied'); } };
    const t = H.createHost({ id: 'demo', version: '1.0.0' }, deps({ storage: throwing })).host;
    assert.equal(t.prefs.get(), undefined);
    t.prefs.set({ a: 1 });
});

test('intents: navigate and focus-needs-you; the rest refused', async () => {
    const d = deps();
    const { host, dispose } = H.createHost({ id: 'demo', version: '1.0.0' }, d);
    assert.equal(await host.intent('navigate', { to: '#/needs' }), 'done');
    assert.equal(await host.intent('navigate', { to: '#/nowhere' }), 'refused');
    assert.equal(await host.intent('navigate', {}), 'refused');
    assert.equal(await host.intent('focus-needs-you', {}), 'done');
    assert.equal(await host.intent('open-session', { sessionKey: 'x' }), 'refused');
    assert.equal(await host.intent('open-handling', { needsItemId: 'x' }), 'refused');
    assert.equal(await host.intent('format-disk', {}), 'refused');
    dispose();
    assert.equal(await host.intent('navigate', { to: '#/needs' }), 'refused');
});

test('events: a throwing listener does not stop the others; dispose ends all', () => {
    const d = deps();
    const ctl = H.createHost({ id: 'demo', version: '1.0.0' }, d);
    const got = [];
    const errors = console.error; console.error = () => {};
    try {
        ctl.host.on('context', () => { throw new Error('boom'); });
        const off = ctl.host.on('context', v => got.push(v));
        ctl.host.on('nope', () => got.push('never'));
        ctl.emit('context', 1);
        off();
        ctl.emit('context', 2);
        ctl.host.on('theme', v => got.push(v));
        ctl.dispose();
        ctl.emit('theme', 3);
    } finally { console.error = errors; }
    assert.deepEqual(got, [1]);
    ctl.host.ready(); ctl.host.keys.forward({ code: 'KeyJ' });
    assert.deepEqual(d.calls, [], 'ready and forward are no-ops after dispose');
});

test('ready, keys and live getters reach the shell', () => {
    let ctx = { needsItemId: 'n1' };
    const d = deps({ context: () => ctx });
    const { host } = H.createHost({ id: 'demo', version: '1.0.0' }, d);
    host.ready();
    host.keys.forward({ code: 'KeyJ' });
    assert.deepEqual(d.calls, [['ready'], ['key', 'KeyJ']]);
    assert.equal(host.context.needsItemId, 'n1');
    ctx = { needsItemId: 'n2' };
    assert.equal(host.context.needsItemId, 'n2');
    host.context.needsItemId = 'mine';
    assert.equal(host.context.needsItemId, 'n2', 'the extension gets a copy');
    assert.equal(host.theme.tokens['--text-1'], '#fff');
    assert.equal(H.READY_TIMEOUT_MS, 5000);
});

// ── dev file server ──────────────────────────────────────────────────
test('dev server: lists both roots and serves only files inside an extension', () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'c9ext-'));
    try {
        const repo = path.join(tmp, 'repo'), home = path.join(tmp, 'home');
        const put = (p, s) => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, s); };
        put(path.join(repo, 'extensions/hello/c9ext.json'), '{"c9ext":1}');
        put(path.join(repo, 'extensions/hello/index.js'), 'first');
        put(path.join(repo, 'extensions/hello/.secret'), 'x');
        put(path.join(home, 'extensions/hello/index.js'), 'third');
        put(path.join(home, 'extensions/hello/c9ext.json'), '{}');
        put(path.join(home, 'extensions/mine/c9ext.json'), 'x'.repeat(70_000));
        put(path.join(home, 'extensions/mine/index.js'), 'mine');
        put(path.join(home, 'outside.js'), 'outside');
        fs.symlinkSync(path.join(home, 'outside.js'), path.join(home, 'extensions/mine/link.js'));
        const roots = extensionRoots(repo, home);
        const list = listExtensions(roots);
        assert.deepEqual(list.map(c => [c.source, c.dir, c.manifest === null ? null : c.manifest.length]), [['first', 'hello', 11], ['third', 'hello', 2], ['third', 'mine', null]]);
        assert.equal(list[2].size, 70_000, 'a big manifest is not sent, only its size');
        const read = (id, rel) => { const f = resolveExtFile(roots, id, rel); return f && fs.readFileSync(f, 'utf8'); };
        assert.equal(read('hello', 'index.js'), 'first', 'first-party root wins (R9)');
        assert.equal(read('mine', 'index.js'), 'mine');
        for (const [id, rel] of [['hello', '../hello/index.js'], ['hello', '.secret'], ['mine', 'link.js'], ['mine', '../../outside.js'], ['Mine', 'index.js'], ['..', 'outside.js'], ['hello', ''], ['hello', 'nope.js'], ['mine', 'a//b']]) {
            assert.equal(resolveExtFile(roots, id, rel), null, `${id}/${rel}`);
        }
        assert.deepEqual(listExtensions(extensionRoots(repo, path.join(tmp, 'missing'))).map(c => c.dir), ['hello'], 'a missing root is skipped');
        assert.equal(resolveExtFile(roots, 'mine', '%2e%2e/hello/index.js'), null, 'no % in a served path');
    } finally { fs.rmSync(tmp, { recursive: true, force: true }); }
});

test('dev server: symlinks cannot leave the root or the extension directory', () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'c9ext-'));
    try {
        const put = (p, s) => { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, s); };
        const repo = path.join(tmp, 'repo'), home = path.join(tmp, 'home'), outside = path.join(tmp, 'outside');
        put(path.join(outside, 'secrets.json'), '{"secret":1}');
        put(path.join(outside, 'c9ext.json'), '{"c9ext":1,"id":"demo"}');
        put(path.join(outside, 'index.js'), 'outside');
        put(path.join(repo, 'extensions/real/index.js'), 'real');
        // 1. An extension directory that is a symlink out of the root.
        fs.mkdirSync(path.join(repo, 'extensions'), { recursive: true });
        fs.symlinkSync(outside, path.join(repo, 'extensions/demo'));
        // 2. A manifest that is a symlink out of its extension.
        put(path.join(home, 'extensions/leak/index.js'), 'leak');
        fs.symlinkSync(path.join(outside, 'secrets.json'), path.join(home, 'extensions/leak/c9ext.json'));
        // 3. A served file that is a symlink out of its extension (to a sibling).
        fs.symlinkSync(path.join(repo, 'extensions/real/index.js'), path.join(home, 'extensions/leak/sibling.js'));
        // A symlink that stays inside its extension is fine.
        fs.symlinkSync(path.join(home, 'extensions/leak/index.js'), path.join(home, 'extensions/leak/alias.js'));
        // An engine home that is itself a symlink is fine: the root's real path counts.
        fs.symlinkSync(home, path.join(tmp, 'home-link'));
        for (const roots of [extensionRoots(repo, home), extensionRoots(repo, path.join(tmp, 'home-link'))]) {
            const list = listExtensions(roots);
            assert.deepEqual(list.map(c => [c.source, c.dir]), [['first', 'real'], ['third', 'leak']], 'the symlinked-out dir is not listed');
            assert.equal(list[1].manifest, null, 'the symlinked-out manifest is not read');
            assert.ok(!JSON.stringify(list).includes('secret'));
            assert.equal(resolveExtFile(roots, 'demo', 'secrets.json'), null, 'dir symlink out');
            assert.equal(resolveExtFile(roots, 'demo', 'index.js'), null);
            assert.equal(resolveExtFile(roots, 'leak', 'c9ext.json'), null, 'manifest symlink out');
            assert.equal(resolveExtFile(roots, 'leak', 'sibling.js'), null, 'file symlink out');
            assert.equal(fs.readFileSync(resolveExtFile(roots, 'leak', 'alias.js'), 'utf8'), 'leak');
            assert.equal(fs.readFileSync(resolveExtFile(roots, 'real', 'index.js'), 'utf8'), 'real');
        }
    } finally { fs.rmSync(tmp, { recursive: true, force: true }); }
});

test('the hello extension is valid and dev-only', () => {
    const text = fs.readFileSync(new URL('../extensions/hello/c9ext.json', import.meta.url), 'utf8');
    const [prod] = M.resolveExtensions([{ dir: 'hello', source: 'first', manifest: text }], ENV);
    assert.equal(prod.status, 'dev_only');
    const [dev] = M.resolveExtensions([{ dir: 'hello', source: 'first', manifest: text }], { ...ENV, dev: true });
    assert.equal(dev.status, 'ok');
    assert.equal(dev.manifest.ui.pages[0].entry, 'index.js');
});
