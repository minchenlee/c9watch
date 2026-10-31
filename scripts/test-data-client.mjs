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

test('gateway auth sends an audience-bound HMAC and verifies the pinned Ed25519 key', async () => {
    const { webcrypto, createHmac } = await import('node:crypto');
    const key = await webcrypto.subtle.generateKey('Ed25519',true,['sign','verify']);
    const serverKey=Buffer.from(await webcrypto.subtle.exportKey('raw',key.publicKey)).toString('hex');
    const token='a1'.repeat(32), serverNonce='b2'.repeat(32);
    const auth=load('../src/lib/data/ws-c9p-auth.ts',{}, {crypto:webcrypto,TextEncoder});
    class Socket {
        listeners=new Map(); sent=[];
        addEventListener(name,fn) { this.listeners.set(name,fn); }
        removeEventListener(name,fn) { if(this.listeners.get(name)===fn)this.listeners.delete(name); }
        emit(value) { this.listeners.get('message')?.({data:JSON.stringify(value)}); }
        send(text) {
            this.sent.push(text); assert(!text.includes(token));
            const {auth:request}=JSON.parse(text);
            const payload=Buffer.concat([Buffer.from(serverNonce,'hex'),Buffer.from(request.clientNonce,'hex'),Buffer.from('machine-1')]);
            assert.equal(request.mac,createHmac('sha256',Buffer.from(token,'hex')).update(payload).digest('hex'));
            const nonces=payload.subarray(0,64);
            void webcrypto.subtle.sign('Ed25519',key.privateKey,nonces).then(sig=>this.emit({sig:Buffer.from(sig).toString('hex')}));
        }
    }
    const socket=new Socket();
    const done=auth.hexGatewayAuthenticator({clientId:'client-1',token,serverKey,machineId:'machine-1'})(socket);
    socket.emit({challenge:serverNonce}); await done;
    assert.equal(socket.sent.length,1);
    assert.equal(socket.listeners.size,0);
    const bad=new Socket(); bad.send=function(){this.emit({sig:'00'.repeat(64)});};
    const rejected=auth.hexGatewayAuthenticator({clientId:'client-1',token,serverKey,machineId:'machine-1'})(bad);
    bad.emit({challenge:serverNonce});
    await assert.rejects(rejected,/does not match/);
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
