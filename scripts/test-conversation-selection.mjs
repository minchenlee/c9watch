import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { compileModule } from 'svelte/compiler';
import { flushSync } from 'svelte';
import ts from 'typescript';

// Execute the actual page's reactive loader with deferred backend responses.
const page = readFileSync(new URL('../src/routes/(app)/+page.svelte', import.meta.url), 'utf8');
const start = page.indexOf('\tlet conversationTarget');
const end = page.indexOf('\n\tfunction handleExpand', start);
const loader = page.slice(start, end);
const source = `
import { untrack } from 'svelte';
export function harness(initialProvider = 'codex') {
 let sessions = $state([{id:'same',provider:initialProvider}]);
 let expandedId = $state(initialProvider + ':same');
 const sessionKeyOf = s => s.provider + ':' + s.id;
 const providerSessionKey = (provider,id) => provider + ':' + id;
 const providerOf = s => s.provider;
 let expandedSession = $derived(sessions.find(s => sessionKeyOf(s) === expandedId) || null);
 let value = null;
 const currentConversation = {set(v) {value = v;}};
 const conversationError = {value:null,set(v) {this.value=v;}};
 const toolsLoadedFor = {value:null,set(v) {this.value=v;}};
 const get = s => s.value;
 const timers = new Map(); let timerId = 0;
 const setTimeout = fn => {timers.set(++timerId,fn);return timerId;};
 const clearTimeout = id => timers.delete(id);
 const listeners = new Map();
 const window = {addEventListener:(key,fn)=>listeners.set(key,fn),removeEventListener:key=>listeners.delete(key)};
 const document = {...window,visibilityState:'visible'};
 const isTauri=()=>true; let statusRefreshes=0; const refreshCodexInteractions=()=>{statusRefreshes++;return Promise.resolve();};
 const requests = [];
 const getConversation = (id,provider) => new Promise((resolve,reject) => requests.push({id,provider,resolve,reject}));
 const withConversationLoader = (_id,_provider,_kind,task) => task();
 ${loader.slice(0, loader.indexOf("\t$effect(() => {"))}
 const dispose = $effect.root(() => {
 ${loader.slice(loader.indexOf("\t$effect(() => {"))}
 });
 return {requests,dispose,retry:()=>retryConversation(),revision(modified,messageCount=0){sessions=sessions.map(s=>({...s,modified,messageCount}));},statusRefreshes:()=>statusRefreshes,wake(hidden=false){document.visibilityState=hidden?'hidden':'visible';listeners.get('focus')?.();listeners.get('visibilitychange')?.();},error:()=>conversationError.value,refresh(){const work=[...timers.values()];timers.clear();work.forEach(fn=>fn());},value:()=>value,poll:()=>{sessions=sessions.map(s=>({...s}));},select(provider){sessions=[{id:'same',provider}];expandedId=provider+':same';},close(){expandedId=null;}};
}`;
let code = compileModule(ts.transpileModule(source, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText, { filename: 'conversation-selection.svelte.js', generate: 'client' }).js.code;
code = code.replace(/from '([^']+)'/g, (_, spec) => `from '${import.meta.resolve(spec)}'`);
const { harness } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
const settle = async () => { await Promise.resolve(); await Promise.resolve(); flushSync(); };

test('initial failure is visible, retry succeeds, and stale errors are ignored', async () => {
 const h=harness('opencode');
 try {
  flushSync(); h.requests[0].reject(new Error('OpenCode HTTP 503')); await settle();
  assert.equal(h.error().key,'opencode:same'); assert.match(h.error().message, /503/); assert.equal(h.value(),null);
  h.retry(); flushSync(); assert.equal(h.error(),null); assert.equal(h.requests.length,2);
  const response={sessionId:'same',provider:'opencode',messages:[{content:'recovered'}]};
  h.requests[1].resolve(response); await settle(); assert.equal(h.value(),response);
  h.refresh(); h.select('codex'); flushSync();
  h.requests[2].reject(new Error('stale error')); await settle(); assert.equal(h.error(),null);
 } finally {h.dispose();}
});

test('poll updates neither restart an in-flight load nor erase loaded messages', async () => {
 const h=harness();
 try {
  flushSync(); assert.equal(h.requests.length,1);
  for(let i=0;i<3;i++){h.poll();flushSync();}
  assert.equal(h.requests.length,1);
  const response={sessionId:'same',provider:'codex',messages:[{content:'hello'}]};
  h.requests[0].resolve(response);await settle();
  assert.equal(h.value(),response);
  h.poll();flushSync();assert.equal(h.value(),response);assert.equal(h.requests.length,1);
 } finally {h.dispose();}
});

test('provider switch rejects stale replies and reopening loads again', async () => {
 const h=harness();
 try {
  flushSync();h.select('cursor');flushSync();assert.equal(h.requests.length,2);
  h.requests[0].resolve({sessionId:'same',provider:'codex',messages:[]});await settle();assert.equal(h.value(),null);
  const response={sessionId:'same',provider:'cursor',messages:[{content:'cursor'}]};
  h.requests[1].resolve(response);await settle();assert.equal(h.value(),response);
  h.close();flushSync();assert.equal(h.value(),null);
  h.select('cursor');flushSync();assert.equal(h.requests.length,3);
 } finally {h.dispose();}
});

 test('open preview refreshes serially and discards a response after close', async () => {
 const h=harness('opencode');
 try {
  flushSync(); h.refresh(); assert.equal(h.requests.length,1);
  h.requests[0].resolve({sessionId:'same',provider:'opencode',messages:[{content:'old'}]});await settle();
  h.refresh();assert.equal(h.requests.length,2);
  h.refresh();assert.equal(h.requests.length,2);
  const updated={sessionId:'same',provider:'opencode',messages:[{content:'old'},{content:'reply'}]};
  h.requests[1].resolve(updated);await settle();assert.equal(h.value(),updated);
  h.refresh();assert.equal(h.requests.length,3);
  h.close();flushSync();h.requests[2].resolve(updated);await settle();
  assert.equal(h.value(),null);h.refresh();assert.equal(h.requests.length,3);
 } finally {h.dispose();}
});

const slidingSource = readFileSync(new URL('../src/lib/slidingWindow.svelte.ts', import.meta.url), 'utf8');
let slidingCode = compileModule(ts.transpileModule(slidingSource, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText, {filename:'sliding-window.svelte.js',generate:'client'}).js.code;
slidingCode = slidingCode.replace(/from '([^']+)'/g, (_, spec) => `from '${import.meta.resolve(spec)}'`);
const {createSlidingWindow, MAX_VISIBLE} = await import('data:text/javascript;base64,'+Buffer.from(slidingCode).toString('base64'));
test('following new replies advances the visible tail without unbounded rendering', () => {
 const sw=createSlidingWindow();sw.reset(2);
 const messages=Array.from({length:3},(_,i)=>({content:String(i)}));
 sw.followLatest(3);assert.equal(sw.sliceMessages(messages).at(-1).content,'2');
 sw.followLatest(1000);assert.equal(sw.endIndex,1000);assert.ok(sw.endIndex-sw.startIndex<=MAX_VISIBLE);
});

 test('load errors are scoped to selection and clear after successful retry', async () => {
 const h=harness();
 try {
  flushSync();h.requests[0].reject('Missing history segment');await settle();
  assert.deepEqual(h.error(),{key:'codex:same',message:'Missing history segment'});
  h.retry();h.select('cursor');flushSync();assert.equal(h.error(),null);
  h.requests[1].reject('late failure');await settle();assert.equal(h.error(),null);
  h.requests[2].reject('temporary');await settle();assert.equal(h.error().key,'cursor:same');
  h.retry();h.requests[3].resolve({sessionId:'same',provider:'cursor',messages:[]});await settle();
  assert.equal(h.error(),null);assert.ok(h.value());
 } finally {h.dispose();}
});

test('foreground refresh is single-flight, skips hidden windows and cleans up on close',async()=>{
 const h=harness();try{
  flushSync();h.wake();assert.equal(h.requests.length,1);
  h.requests[0].resolve({sessionId:'same',provider:'codex',messages:[]});await settle();
  h.wake(true);assert.equal(h.requests.length,1);
  h.wake();assert.ok(h.statusRefreshes()>0);assert.equal(h.requests.length,2);
  for(let i=0;i<100;i++){h.wake();h.refresh();}
  assert.equal(h.requests.length,2);
  h.requests[1].resolve({sessionId:'same',provider:'codex',messages:[{content:'fresh'}]});await settle();
  assert.equal(h.value().messages[0].content,'fresh');
  h.close();flushSync();h.wake();assert.equal(h.requests.length,2);
 }finally{h.dispose();}
});

test('local providers load once and allow manual retry without scheduling full reparses', async () => {
 for (const provider of ['claudeCode', 'codex', 'cursor', 'pi']) {
  const h = harness(provider);
  try {
   flushSync();
   h.requests[0].reject(new Error('temporary read failure')); await settle();
   h.refresh(); assert.equal(h.requests.length, 1, provider);
   h.retry(); flushSync(); assert.equal(h.requests.length, 2, provider);
   const response = {sessionId:'same', provider, messages:[{content:'loaded'}]};
   h.requests[1].resolve(response); await settle();
   for (let i = 0; i < 3; i++) { h.poll(); flushSync(); h.refresh(); }
   assert.equal(h.requests.length, 2, provider);
   assert.equal(h.value(), response);
  } finally { h.dispose(); }
 }
});

test('Codex revision changes coalesce while unchanged session polls never reparse', async () => {
 const h=harness(); try {
  flushSync();
  for(let i=0;i<100;i++){h.revision(String(i));flushSync();}
  assert.equal(h.requests.length,1);
  h.requests[0].resolve({sessionId:'same',provider:'codex',messages:[{content:'old'}]});await settle();
  assert.equal(h.requests.length,2); // one trailing read for all changed revisions
  const updated={sessionId:'same',provider:'codex',messages:[{content:'new'}]};
  h.requests[1].resolve(updated);await settle();
  for(let i=0;i<100;i++){h.poll();h.refresh();flushSync();}
  assert.equal(h.requests.length,2);assert.equal(h.value(),updated);
  h.revision('99',1);flushSync();assert.equal(h.requests.length,3);
  h.revision('100',2);flushSync();h.close();flushSync();
  h.requests[2].resolve(updated);await settle();
  assert.equal(h.requests.length,3);assert.equal(h.value(),null);
 }finally{h.dispose();}
});

test('hidden Codex revision is refreshed on focus and manual retry stays single-flight', async () => {
 const h=harness();try{
  flushSync();for(let i=0;i<100;i++)h.retry();assert.equal(h.requests.length,1);
  const old={sessionId:'same',provider:'codex',messages:[{content:'old'}]};
  h.requests[0].resolve(old);await settle();
  h.wake(true);h.revision('changed');flushSync();assert.equal(h.requests.length,1);
  h.wake();assert.equal(h.requests.length,2);assert.equal(h.value(),old);
  h.requests[1].reject('temporary');await settle();assert.equal(h.value(),old);
  h.retry();for(let i=0;i<100;i++)h.retry();assert.equal(h.requests.length,3);
  assert.equal(h.value(),old);
 }finally{h.dispose();}
});

test('Codex refresh signals do not add background reads to other local providers', async () => {
 for(const provider of ['claudeCode','cursor','pi']){
  const h=harness(provider);try{
   flushSync();h.requests[0].resolve({sessionId:'same',provider,messages:[]});await settle();
   h.revision('different',20);flushSync();h.wake();h.refresh();
   assert.equal(h.requests.length,1,provider);assert.equal(h.statusRefreshes(),0,provider);
  }finally{h.dispose();}
 }
});
