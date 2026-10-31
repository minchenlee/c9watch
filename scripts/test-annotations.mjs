import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { test } from 'node:test';
function load() {
    const exports = {};
    const timers = [];
    const source = readFileSync(new URL('../src/lib/data/annotation-feed.ts', import.meta.url), 'utf8');
    vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText, {
        exports, require() { throw new Error('feed has no runtime dependencies'); }, Map, JSON,
        setTimeout(fn) { timers.push(fn); return timers.length; }, clearTimeout() {}
    });
    return { ...exports, timers };
}
const settle = async () => { for (let i=0;i<12;i++) await Promise.resolve(); };
const target = { kind:'session',sessionKey:'claudeCode:s' };
const row = (key,n) => ({target,key,value:{type:'number',v:n},writer:'local-unverified',sensitivity:'metadata',createdAt:1,updatedAt:1,expiresAt:null});
const change = (source,values) => ({ v:1,type:'event',topic:'annotation.changed',data:{target,source,values},cursor:'m:e:2' });
function values(map) { return [...map.values()].map(a=>[a.key,a.value?.v]); }
test('snapshot/watch race, source replacement and removed keys preserve other sources', async () => {
    const feed=load(); const seen=[]; let receive; let stops=0;
    const transport={async request(method) {assert.equal(method,'annotation.list');return {annotations:[row('ci/a',1),row('other/x',5)],cursor:'m:e:1'};},async subscribe(p,cb) {assert.equal(p.since,'m:e:1');receive=cb;cb(change('ci',{b:row('ci/b',2)}));return ()=>{stops++;};}};
    const stop=feed.annotationFeed(transport,m=>seen.push(values(m)));
    await settle();
    assert.deepEqual(seen.at(-1),[['other/x',5],['ci/b',2]]);
    receive(change('ci',{}));
    assert.deepEqual(seen.at(-1),[['other/x',5]]);
    receive({v:1,type:'event',topic:'annotation.removed',data:{target,key:'other/x',reason:'target_gone'}});
    assert.deepEqual(seen.at(-1),[]);
    stop();assert.equal(stops,1);
});
test('failure keeps data, expiry refresh replaces it, old callbacks and stopped requests are ignored', async () => {
    const feed=load(); const seen=[]; const callbacks=[]; const errors=[]; let n=0; let release;
    const transport={async request() {n++;if(n===2)throw new Error('offline');if(n===4)return await new Promise(r=>{release=r;});return {annotations:[row(`ci/${n}`,n)],cursor:`m:e:${n}`};},async subscribe(_p,cb) {callbacks.push(cb);return ()=>{};}};
    const stop=feed.annotationFeed(transport,m=>seen.push(values(m)),e=>errors.push(e));
    await settle();
    callbacks[0]({v:1,ok:false,error:{code:'cursor_expired'}});
    feed.timers.shift()();await settle();
    assert.deepEqual(seen.at(-1),[['ci/1',1]]);
    feed.timers.shift()();await settle();
    assert.deepEqual(seen.at(-1),[['ci/3',3]]);
    callbacks[0](change('ci',{stale:row('ci/stale',9)}));
    assert.deepEqual(seen.at(-1),[['ci/3',3]]);
    callbacks[1]({v:1,ok:false,error:{code:'cursor_expired'}});
    feed.timers.shift()();await settle();stop();
    release({annotations:[row('ci/late',8)],cursor:'m:e:4'});await settle();
    assert.deepEqual(seen.at(-1),[['ci/3',3]]);
    assert(errors.some(e=>e?.message==='offline'));
});
test('annotation identity includes kind, session and key; Classic has no annotation-store imports', () => {
    const {annotationId}=load();
    assert.notEqual(annotationId(row('ci/x',1)),annotationId({...row('ci/x',1),target:{kind:'session',sessionKey:'codex:s'}}));
    for (const dir of ['../src/lib/components/','../src/routes/']) {
        function inspect(url) {
            for (const entry of readdirSync(url,{withFileTypes:true})) {
                const child=new URL(entry.name+(entry.isDirectory()?'/':''),url);
                if(entry.isDirectory())inspect(child);
                else if(/\.(ts|svelte)$/.test(entry.name)) assert(!/from\s+['"][^'"]*(?:stores\/annotations|annotation-feed)['"]/.test(readFileSync(child,'utf8')),String(child));
            }
        }
        inspect(new URL(dir,import.meta.url));
    }
    assert.match(readFileSync(new URL('../tests/visual/classic/fixtures/index.ts',import.meta.url),'utf8'),/export const annotations/);
});
test('not_allowed is terminal for requests, subscription admission and active streams', async () => {
    for (const phase of ['request', 'subscribe', 'during-subscribe', 'stream']) {
        const feed = load(); const seen = []; const errors = [];
        const denied = {code:'not_allowed',msg:'annotation access denied',retryable:false};
        let requests = 0; let subscriptions = 0; let stops = 0; let receive;
        const transport = {
            async request() {
                requests++;
                if (phase === 'request') throw denied;
                return {annotations:[row('ci/x',1)],cursor:'m:e:1'};
            },
            async subscribe(_p, cb) {
                subscriptions++; receive = cb;
                if (phase === 'subscribe') throw denied;
                if (phase === 'during-subscribe') cb({v:1,ok:false,error:denied});
                return () => {stops++;};
            }
        };
        const stop = feed.annotationFeed(transport, m => seen.push(values(m)), e => errors.push(e));
        await settle();
        if (phase === 'stream') {
            receive({v:1,ok:false,error:denied});
            receive(change('ci',{late:row('ci/late',9)}));
            assert.deepEqual(seen.at(-1),[['ci/x',1]]);
        }
        assert.equal(errors.at(-1),denied,phase);
        assert.equal(feed.timers.length,0,phase);
        assert.equal(requests,1,phase);
        assert.equal(subscriptions,phase === 'request' ? 0 : 1,phase);
        if (phase === 'stream' || phase === 'during-subscribe') assert.equal(stops,1,phase);
        stop();
        if (phase === 'stream' || phase === 'during-subscribe') assert.equal(stops,1,phase);
    }
});
