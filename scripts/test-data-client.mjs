import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
import { test } from 'node:test';
const require = createRequire(import.meta.url);
function load(file, dependencies = {}, extras = {}) {
    const exports = {};
    const source = readFileSync(new URL(file, import.meta.url), 'utf8');
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
        exports, require: name => name in dependencies ? dependencies[name] : require(name),
        console, setTimeout, clearTimeout, Date, Map, Set, ...extras
    });
    return exports;
}
const asJSON = value => JSON.parse(JSON.stringify(value));
const provider = load('../src/lib/provider.ts');
const fixtureTypes = load('../src/lib/types.ts');
const fx = load('../tests/visual/classic/fixtures/index.ts', { '../../../../src/lib/types': fixtureTypes });
const mapping = load('../src/lib/data/session.ts');
test('every recorded SessionView maps byte-for-byte to Classic with optional provider fields', () => {
    assert(fx.sessions.length >= 5);
    for (const session of fx.sessions) assert.deepEqual(asJSON(mapping.sessionFromView(session)), asJSON(session));
    const extended = { ...fx.sessions[0], provider: 'codex', parentThreadId: 'parent', sourceHealth: 'partial', futureField: { keep: true } };
    assert.deepEqual(asJSON(mapping.sessionFromView(extended)), asJSON(extended));
});
function apiWith(request, choose = () => ({request})) {
    return load('../src/lib/data/api.ts', {
        './client': { request, transport: choose, hasSelectedTransport: () => false }, './session': mapping, '../provider': provider,
        '../demo/mode': { isDemoMode: {} }, '../ws': { useWebSocket: () => false },
        'svelte/store': { get: () => false }, './platform': {}, './ws-legacy': {}
    });
}
test('paged conversation/history/memory keep all pages and Classic shapes', async () => {
    const calls=[];
    const api = apiWith(async (method, p) => {
        calls.push([method,p]);
        if (method === 'conversation.get') return { sessionId:'a',provider:'codex', messages:[{id:p.page.offset}],nextOffset:p.page.offset === 0 ? 1 : null };
        if (method === 'history.list') return { entries:[{sessionId:String(p.page.offset)}],nextOffset:p.page.offset === 0 ? 3 : null };
        return { projects:[{name:String(p.page.offset)}],nextOffset:p.page.offset === 0 ? 2 : null };
    });
    const conv = await api.getConversation('a','codex',true);
    assert.deepEqual(asJSON(conv),{sessionId:'a',provider:'codex',messages:[{id:0},{id:1}]});
    assert.equal(calls[0][1].sessionKey,'codex:a'); assert.equal(calls[0][1].includeTools,true);
    assert.equal((await api.getSessionHistory()).length,2);
    assert.equal((await api.getMemoryFiles()).length,2);
});
test('a failed or non-advancing page never looks like successful complete content', async () => {
    const api = apiWith(async (_m,p) => { if (p.page.offset) throw new Error('read failed'); return { messages:[{id:1}],nextOffset:1 }; });
    await assert.rejects(api.getConversation('a'),/read failed/);
    await assert.rejects(api.allPages(async () => ({items:[],nextOffset:0})),/non-advancing/);
});
test('session actions preserve the provider identity and never name stop by a raw pid', async () => {
    const calls=[]; const api=apiWith(async (m,p)=>{calls.push([m,p]);});
    await api.stopSession({id:'same',provider:'claudeCode',pid:55,sessionKey:'codex:wrong'});
    await api.renameSession('same','name','claudeCode');
    assert.deepEqual(asJSON(calls),[['session.stop',{sessionKey:'claudeCode:same',pid:55}],['session.rename',{sessionId:'same',newName:'name',provider:'claudeCode'}]]);
});
test('only core_unavailable permits the native fallback; denial retains the complete error', async () => {
    const calls=[]; let failure={code:'not_allowed',msg:'denied',hint:'needs scope',data:{needs:1}};
    const bridge=load('../src/lib/data/tauri-bridge.ts',{
        '@tauri-apps/api/core':{invoke:async(cmd,args)=>{calls.push([cmd,args]); if(cmd==='c9p_request')throw failure; return fx.sessions;},Channel:class{}},
        '@tauri-apps/api/event':{},'./transport':load('../src/lib/data/transport.ts')
    });
    await assert.rejects(bridge.tauriTransport.request('session.views',{}),e=>e.code===failure.code && e.hint===failure.hint && e.data.needs===1);
    assert.equal(calls.length,1);
    // Tauri ACL failures are unstructured strings. They must not widen into
    // local reads; the IPC capability gate prevents missing window grants.
    failure='c9p_request not allowed. Permissions associated with this command: allow-c9p-request';
    await assert.rejects(bridge.tauriTransport.request('session.views',{}),e=>e===failure);
    assert.equal(calls.length,2);
    failure={code:'core_unavailable'};
    const result=await bridge.tauriTransport.request('session.views',{});
    assert.deepEqual(asJSON(result.sessions),asJSON(fx.sessions));
    assert.equal(calls.at(-1)[0],'get_sessions');
});

test('c9p event feed batches deltas, keeps last data on failure, and replaces on cursor expiry', async () => {
    const timers = new Map(); let timerId=0;
    const feed = load('../src/lib/data/session-feed.ts', { './session': mapping }, {
        setTimeout: fn => { timers.set(++timerId,fn); return timerId; }, clearTimeout: id => timers.delete(id)
    });
    let onFrame; let requests=0; let fail=false; let stops=0; const published=[];
    const t = {
        async request() { requests++; if(fail) throw new Error('failed'); return {cursor:`c${requests}`,sessions:requests===1?[{...fx.sessions[0],sessionKey:'claudeCode:first'}]:[]}; },
        async subscribe(p,cb) { assert.equal(p.since,`c${requests}`); onFrame=cb; return ()=>stops++; }
    };
    const settle = async () => { for(let i=0;i<8;i++) await Promise.resolve(); };
    const stop=feed.sessionFeed(t,rows=>published.push(asJSON(rows)));
    await settle(); assert.equal(published.length,1); assert.equal(published[0].length,1);
    // Open frame enums must not let progress/future/malformed frames become sessions.
    onFrame({type:'progress',bytesRead:1,bytesTotal:2});
    onFrame({type:'future-frame',topic:'session.view',data:{sessionKey:'unknown'}});
    onFrame({type:'event',topic:'session.view',data:null});
    for(let i=0;i<3;i++) onFrame({type:'event',topic:'session.view',data:{...fx.sessions[0],sessionKey:'claudeCode:next',messageCount:i}});
    assert.equal(published.length,1,'deltas are batched until a flush');
    for(const [id,fn] of timers) { timers.delete(id); fn(); }
    assert.equal(published.length,2); assert.equal(published[1].length,2); assert.equal(published[1][1].messageCount,2);
    fail=true; onFrame({ok:false,error:{code:'cursor_expired'}}); await settle();
    assert.equal(published.length,2,'failed replacement must retain previous data');
    fail=false; for(const [id,fn] of timers) { timers.delete(id); fn(); }
    await settle(); assert.equal(published.at(-1).length,0,'new snapshot replaces stale cards');
    assert(stops>=1); stop(); assert.equal(timers.size,0);
});

test('ws-c9p authenticates before hello, preserves errors, and never resends a mutation', async () => {
    const sockets=[]; let releaseAuth;
    const authed = new Promise(resolve=>releaseAuth=resolve);
    const types = load('../src/lib/c9p-1.ts');
    const transportTypes = load('../src/lib/data/transport.ts');
    class Socket {
        static OPEN=1; readyState=1; sent=[];
        constructor() { sockets.push(this); setTimeout(()=>this.onopen(),0); }
        send(text) {
            const request=JSON.parse(text); this.sent.push(request);
            queueMicrotask(()=>this.onmessage({data:JSON.stringify({v:1,id:request.id,ok:request.method==='hello',
                ...(request.method==='hello'?{result:{apiMajor:1,apiMinor:6,methods:{'session.stop':{}},breakingSince:[]}}:{error:{code:'not_allowed',msg:'denied',hint:'scope required',retryable:false,data:{scope:'session:stop'}}})
            })}));
        }
        close() { this.readyState=3; this.onclose?.(); }
    }
    const { C9pWebSocketTransport } = load('../src/lib/data/ws-c9p.ts', {'./transport':transportTypes,'../c9p-1':types,'./ws-c9p-auth':{}}, { WebSocket:Socket, TextEncoder });
    const transport = new C9pWebSocketTransport('ws://localhost/c9p',()=>authed);
    const connecting=transport.connect();
    await new Promise(resolve=>setTimeout(resolve,10));
    assert.equal(sockets[0].sent.length,0,'no hello or token before pinned-server authentication');
    releaseAuth(); await connecting;
    assert.equal(sockets[0].sent[0].method,'hello');
    await assert.rejects(transport.request('session.stop',{sessionKey:'claudeCode:a'}),e=>e.code==='not_allowed' && e.hint==='scope required' && e.data.scope==='session:stop');
    sockets[0].close();
    assert.equal(sockets[0].sent.filter(x=>x.method==='session.stop').length,1,'mutation is never replayed');
    transport.close();
});

test('legacy remote transport keeps capability gating and the old request names', async () => {
    const calls=[];
    const legacy=load('../src/lib/data/ws-legacy.ts', {
        '../ws':{wsClient:{request:async(type,data)=>{calls.push([type,data]); return type==='getSessions'?fx.sessions:{};}}},
        './transport':load('../src/lib/data/transport.ts')
    });
    assert.deepEqual(asJSON((await legacy.legacyTransport.request('session.views',{})).sessions),asJSON(fx.sessions));
    await legacy.legacyTransport.request('session.stop',{sessionKey:'claudeCode:a',pid:2});
    await legacy.openLegacySession('claudeCode:a',2);
    assert.equal(calls[0][0],'getSessions');
    assert.deepEqual(asJSON(calls[1]),['session.stop',{addr:'claudeCode:a',pid:2}]);
    assert.deepEqual(asJSON(calls[2]),['openSession',{addr:'claudeCode:a',pid:2}]);
    await assert.rejects(legacy.legacyTransport.request('agent.create',{}),e=>e.code==='not_allowed');
});

test('a mode/transport switch between pages cannot mix real and demo content', async () => {
    let selected; let choices=0; const offsets=[];
    const real = { async request(_method,p) { offsets.push(p.page.offset); selected=demo; return {provider:'claudeCode',messages:[{id:p.page.offset}],nextOffset:p.page.offset===0?1:null}; } };
    const demo = { async request() { throw new Error('Mixed transport'); } };
    selected=real;
    const api=apiWith(()=>{},()=>{choices++; return selected;});
    const result=await api.getConversation('a');
    assert.equal(result.messages.length,2); assert.deepEqual(offsets,[0,1]); assert.equal(choices,1);
});

test('gateway auth signs with the client key and verifies the pinned Ed25519 server key', async () => {
    const { webcrypto, createPrivateKey, createPublicKey, sign, verify } = await import('node:crypto');
    // Same seeds and bytes as src-tauri/core/src/gateway/pubkey.rs cross_language_vectors.
    const seeded=seed=>createPrivateKey({key:Buffer.concat([Buffer.from('302e020100300506032b657004220420','hex'),Buffer.alloc(32,seed)]),format:'der',type:'pkcs8'});
    const machine=seeded(7), client=seeded(9);
    const serverKey=createPublicKey(machine).export({format:'der',type:'spki'}).subarray(-32).toString('hex');
    const privateKey=await webcrypto.subtle.importKey('pkcs8',client.export({format:'der',type:'pkcs8'}),'Ed25519',false,['sign']);
    const clientId='cli_01jbz3k6x8q2m9v4r7t5w0y1zc', serverNonce='b2'.repeat(32);
    const msg=(label,clientNonce)=>Buffer.concat([Buffer.from(label),Buffer.from([0]),Buffer.from(serverNonce,'hex'),Buffer.from(clientNonce,'hex'),
        Buffer.from([64]),Buffer.from(serverKey),Buffer.from([30]),Buffer.from(clientId)]);
    assert.equal(sign(null,msg('c9p-gateway auth-client v1','c3'.repeat(32)),client).toString('hex'),
        'c8434983a6da58624add2bdc334f4f3f1f306c5a4177da50e8d1df4352e033d587bb5d3a26027cc0690fe4bcdf66276bc41a9c0474111fd6f7f168424a73e901');
    const auth=load('../src/lib/data/ws-c9p-auth.ts',{}, {crypto:webcrypto,TextEncoder});
    const pairing={clientId,privateKey,serverKey,machineId:serverKey};
    class Socket {
        listeners=new Map(); sent=[];
        addEventListener(name,fn) { this.listeners.set(name,fn); }
        removeEventListener(name,fn) { if(this.listeners.get(name)===fn)this.listeners.delete(name); }
        emit(value) { this.listeners.get('message')?.({data:JSON.stringify(value)}); }
        send(text) {
            this.sent.push(text);
            const {auth:request}=JSON.parse(text);
            assert.deepEqual(Object.keys(request).sort(),['clientId','clientNonce','sig']);
            assert(verify(null,msg('c9p-gateway auth-client v1',request.clientNonce),createPublicKey(client),Buffer.from(request.sig,'hex')));
            this.emit({sig:sign(null,msg('c9p-gateway auth-server v1',request.clientNonce),machine).toString('hex')});
        }
    }
    const socket=new Socket();
    const done=auth.hexGatewayAuthenticator(pairing)(socket);
    await new Promise(r=>setTimeout(r,0));
    socket.emit({challenge:serverNonce}); await done;
    assert.equal(socket.sent.length,1);
    assert.equal(socket.listeners.size,0);
    const bad=new Socket(); bad.send=function(){this.emit({sig:'00'.repeat(64)});};
    const rejected=auth.hexGatewayAuthenticator(pairing)(bad);
    await new Promise(r=>setTimeout(r,0));
    bad.emit({challenge:serverNonce});
    await assert.rejects(rejected,/does not match/);
    for (const challenge of [serverNonce.toUpperCase(), serverNonce.slice(2), 'nonce']) {
        const s=new Socket();
        const p=auth.hexGatewayAuthenticator(pairing)(s);
        await new Promise(r=>setTimeout(r,0));
        s.emit({challenge}); await assert.rejects(p,/Invalid gateway challenge/);
        assert.equal(s.sent.length,0);
    }
    await assert.rejects(auth.hexGatewayAuthenticator({...pairing,clientId:'client-1'})(new Socket()),/client id/);
});

test('native unsubscribe is idempotent and drops queued channel frames', async () => {
    let channel; const calls=[],seen=[];
    const bridge=load('../src/lib/data/tauri-bridge.ts',{
        '@tauri-apps/api/core':{invoke:async(cmd)=>{calls.push(cmd); return 'sub-1';},Channel:class {constructor(){channel=this;}}},
        '@tauri-apps/api/event':{},'./transport':load('../src/lib/data/transport.ts')
    });
    const stop=await bridge.tauriTransport.subscribe({topics:['session.view']},frame=>seen.push(frame));
    channel.onmessage({type:'event'}); stop(); stop(); channel.onmessage({type:'event'});
    assert.equal(seen.length,1); assert.equal(calls.filter(cmd=>cmd==='c9p_unsubscribe').length,1);
});

const settle = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function fakeTimers() {
    const timers = new Map(); let id = 0;
    return { timers, setTimeout: (fn, ms) => { timers.set(++id, { fn, ms }); return id; }, clearTimeout: t => timers.delete(t),
        run() { for (const [t, { fn }] of [...timers]) { timers.delete(t); fn(); } } };
}
const sessionFeedModule = clock => load('../src/lib/data/session-feed.ts', { './session': mapping }, clock);

test('a stream error frame other than cursor_expired retries; core_unavailable backs off to 5 s', async () => {
    const clock = fakeTimers(); const feed = sessionFeedModule(clock);
    let onFrame; let requests = 0; const errors = []; const published = [];
    const t = { async request() { requests++; return { cursor: `c${requests}`, sessions: [] }; },
        async subscribe(p, cb) { if (p.since === undefined) return () => {}; assert.equal(p.since, `c${requests}`, 'resume from the snapshot cursor'); onFrame = cb; return () => {}; } };
    const stop = feed.sessionFeed(t, rows => published.push(rows), e => errors.push(e?.code ?? e));
    await settle(); assert.deepEqual(errors, [null]); assert.equal(published.length, 1);
    onFrame({ v: 1, id: 's', ok: false, error: { code: 'core_unavailable' } });
    assert.deepEqual(errors, [null, 'core_unavailable']);
    assert.deepEqual([...clock.timers.values()].map(x => x.ms), [5000]);
    onFrame({ type: 'event', topic: 'session.view', data: { ...fx.sessions[0], sessionKey: 'claudeCode:late' } });
    clock.run(); await settle();
    assert.equal(requests, 2); assert.deepEqual(errors.at(-1), null);
    assert(!published.some(rows => rows.some(s => s.sessionKey === 'claudeCode:late')), 'a frame of the ended stream is dropped');
    stop();
});

test('native fallback events apply only while c9p is unavailable', async () => {
    const live = load('../src/lib/data/live.ts', { './session-feed': sessionFeedModule(fakeTimers()) });
    let report, nativeApply, unlistened = 0, listens = 0; const applied = [];
    const stop = live.withNativeFallback(
        (apply, failed) => { report = { apply, failed }; return () => {}; },
        async cb => { listens++; nativeApply = cb; return () => unlistened++; },
        v => applied.push(v));
    report.failed({ code: 'core_unavailable' }); report.failed({ code: 'core_unavailable' });
    await settle(); assert.equal(listens, 1, 'one native listener per fallback period');
    nativeApply('fallback-1');
    report.failed(null); report.apply('c9p-1');
    nativeApply('queued-before-switch');
    await settle(); assert.equal(unlistened, 1);
    report.failed({ code: 'transport' }); await settle();
    assert.equal(listens, 1, 'a transport error is not the fallback');
    assert.deepEqual(applied, ['fallback-1', 'c9p-1']);
    stop();
});

test('diagnostics subscribe before the read and replay buffered events after it', async () => {
    const clock = fakeTimers();
    const live = load('../src/lib/data/live.ts', { './session-feed': sessionFeedModule(clock) }, clock);
    const order = []; let onFrame, release; const applied = [];
    const t = {
        async subscribe(p, cb) { order.push(['subscribe', p.topics, p.since ?? null]); onFrame = cb; return () => order.push(['stop']); },
        request(method) { order.push(['request', method]); return new Promise(r => release = r); }
    };
    const errors = [];
    const stop = live.diagnosticsFeed(t, d => applied.push(d.n), e => errors.push(e?.code ?? e));
    await settle();
    assert.deepEqual(asJSON(order), [['subscribe', ['diag.detection'], null], ['request', 'diag.detection']]);
    onFrame({ type: 'event', topic: 'diag.detection', data: { n: 2 } });
    onFrame({ type: 'event', topic: 'diag.detection', data: { n: 3 } });
    release({ n: 1 }); await settle();
    assert.deepEqual(applied, [1, 2, 3], 'snapshot first, then the events that raced it, in order');
    onFrame({ type: 'event', topic: 'diag.detection', data: { n: 4 } });
    onFrame({ type: 'event', topic: 'other', data: { n: 9 } });
    assert.deepEqual(applied, [1, 2, 3, 4]);
    onFrame({ v: 1, ok: false, error: { code: 'core_unavailable' } });
    assert.deepEqual(errors, [null, 'core_unavailable']);
    assert.deepEqual([...clock.timers.values()].map(x => x.ms), [5000]);
    stop(); assert.equal(clock.timers.size, 0);
});

test('conversation progress comes from the request channel, or native events in fallback', async () => {
    const channels = []; let failCore = false; let nativeCb; let unlistened = 0;
    const bridge = load('../src/lib/data/tauri-bridge.ts', {
        '@tauri-apps/api/core': { invoke: async (cmd, args) => {
            if (cmd === 'c9p_request') {
                if (failCore) throw { code: 'core_unavailable' };
                args.progress.onmessage({ v: 1, type: 'progress', id: 'r', bytesRead: 5, bytesTotal: 10 });
                return { messages: [] };
            }
            nativeCb({ payload: { sessionId: 'other', bytesRead: 9, bytesTotal: 9 } });
            nativeCb({ payload: { sessionId: 'abc', bytesRead: 7, bytesTotal: 10 } });
            return { sessionId: 'abc', messages: [] };
        }, Channel: class { constructor() { channels.push(this); } } },
        '@tauri-apps/api/event': { listen: async (name, cb) => { assert.equal(name, 'conversation-progress'); nativeCb = cb; return () => unlistened++; } },
        './transport': load('../src/lib/data/transport.ts')
    });
    const seen = [];
    await bridge.tauriTransport.request('conversation.get', { sessionKey: 'claudeCode:abc' }, { onProgress: f => seen.push(f.bytesRead) });
    failCore = true;
    await bridge.tauriTransport.request('conversation.get', { sessionKey: 'claudeCode:abc' }, { onProgress: f => seen.push(f.bytesRead) });
    assert.deepEqual(seen, [5, 7]); assert.equal(unlistened, 1);
});

test('a snapshot without a cursor is the fallback, never a recovered core stream', async () => {
    const clock = fakeTimers(); const feed = sessionFeedModule(clock);
    const subscribes = []; const errors = []; const published = [];
    const t = { async request() { return { cursor: null, sessions: [{ ...fx.sessions[0], sessionKey: 'claudeCode:native' }] }; },
        async subscribe(p) { subscribes.push(p.since ?? null); return () => {}; } };
    const stop = feed.sessionFeed(t, rows => published.push(rows), e => errors.push(e?.code ?? e));
    await settle();
    assert.equal(published.length, 0, 'native rows are not published as a live core snapshot');
    assert.deepEqual(errors, ['core_unavailable']);
    assert.deepEqual(subscribes, [], 'no live-only stream on a null cursor');
    stop();
});

test('a batch queued before core_unavailable is dropped, not published over the fallback', async () => {
    const clock = fakeTimers(); const feed = sessionFeedModule(clock);
    let onFrame; let down = false; const published = [];
    const t = { async request() { if (down) throw { code: 'core_unavailable' }; return { cursor: 'c1', sessions: [] }; },
        async subscribe(p, cb) { if (down) throw { code: 'core_unavailable' }; onFrame = cb; return () => {}; } };
    const stop = feed.sessionFeed(t, rows => published.push(rows));
    await settle(); assert.equal(published.length, 1);
    onFrame({ type: 'event', topic: 'session.view', data: { ...fx.sessions[0], sessionKey: 'claudeCode:stale' } });
    down = true;
    onFrame({ v: 1, ok: false, error: { code: 'core_unavailable' } });
    clock.run(); await settle(); clock.run(); await settle();
    assert.equal(published.length, 1, 'the stale core delta never reaches the store');
    stop();
});

test('recovery probes core before the snapshot read, and waits while the window is hidden', async () => {
    const clock = fakeTimers();
    const doc = { hidden: false, listeners: new Set(), addEventListener(_, fn) { this.listeners.add(fn); }, removeEventListener(_, fn) { this.listeners.delete(fn); } };
    const feed = load('../src/lib/data/session-feed.ts', { './session': mapping }, { ...clock, document: doc });
    const calls = []; let up = false;
    const t = { async request() { calls.push('read'); return { cursor: up ? 'c' : null, sessions: [] }; },
        async subscribe(p) { calls.push(p.since ? 'subscribe' : 'probe'); if (!up) throw { code: 'core_unavailable' }; return () => {}; } };
    const errors = [];
    const stop = feed.sessionFeed(t, () => {}, e => errors.push(e?.code ?? e));
    await settle(); calls.length = 0;
    clock.run(); await settle();
    assert.deepEqual(calls, ['probe'], 'core down: no snapshot read (no native scan)');
    doc.hidden = true; calls.length = 0;
    clock.run(); await settle();
    assert.deepEqual(calls, [], 'hidden: the retry waits');
    up = true; doc.hidden = false; for (const fn of [...doc.listeners]) fn();
    await settle();
    assert.deepEqual(calls, ['probe', 'read', 'subscribe']);
    assert.equal(errors.at(-1), null);
    stop();
});

test('entering the fallback reads the native snapshot once the listener is in place', async () => {
    const live = load('../src/lib/data/live.ts', { './session-feed': sessionFeedModule(fakeTimers()) });
    let report; const applied = []; const order = [];
    const stop = live.withNativeFallback(
        (apply, failed) => { report = { apply, failed }; return () => {}; },
        async () => { order.push('listen'); return () => {}; },
        v => applied.push(v),
        async () => { order.push('read'); return 'native-cached'; });
    report.failed({ code: 'core_unavailable' }); await settle();
    assert.deepEqual(order, ['listen', 'read']);
    assert.deepEqual(applied, ['native-cached']);
    stop();
});

test('a native event during the fallback snapshot read applies after it, never under it', async () => {
    const live = load('../src/lib/data/live.ts', { './session-feed': sessionFeedModule(fakeTimers()) });
    let report, emit, finishRead; const applied = [];
    const stop = live.withNativeFallback(
        (apply, failed) => { report = { apply, failed }; return () => {}; },
        async listener => { emit = listener; return () => {}; },
        v => applied.push(v),
        () => new Promise(resolve => { finishRead = resolve; }));
    report.failed({ code: 'core_unavailable' }); await settle();
    emit('B-newer'); await settle();
    assert.deepEqual(applied, []);
    finishRead('A-snapshot'); await settle();
    assert.deepEqual(applied, ['A-snapshot', 'B-newer']);
    emit('C'); await settle();
    assert.deepEqual(applied, ['A-snapshot', 'B-newer', 'C']);
    stop();
});
