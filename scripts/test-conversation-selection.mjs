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
export function harness() {
 let sessions = $state([{id:'same',provider:'codex'}]);
 let expandedId = $state('codex:same');
 const sessionKeyOf = s => s.provider + ':' + s.id;
 const providerSessionKey = (provider,id) => provider + ':' + id;
 const providerOf = s => s.provider;
 let expandedSession = $derived(sessions.find(s => sessionKeyOf(s) === expandedId) || null);
 let value = null;
 const currentConversation = {set(v) {value = v;}};
 const toolsLoadedFor = {value:null,set(v) {this.value=v;}};
 const get = s => s.value;
 const timers = new Map(); let timerId = 0;
 const setTimeout = fn => {timers.set(++timerId,fn);return timerId;};
 const clearTimeout = id => timers.delete(id);
 const requests = [];
 const getConversation = (id,provider) => new Promise(resolve => requests.push({id,provider,resolve}));
 const withConversationLoader = (_id,_provider,_kind,task) => task();
 const dispose = $effect.root(() => {
 ${loader}
 });
 return {requests,dispose,refresh(){const work=[...timers.values()];timers.clear();work.forEach(fn=>fn());},value:()=>value,poll:()=>{sessions=sessions.map(s=>({...s}));},select(provider){sessions=[{id:'same',provider}];expandedId=provider+':same';},close(){expandedId=null;}};
}`;
let code = compileModule(ts.transpileModule(source, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText, { filename: 'conversation-selection.svelte.js', generate: 'client' }).js.code;
code = code.replace(/from '([^']+)'/g, (_, spec) => `from '${import.meta.resolve(spec)}'`);
const { harness } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
const settle = async () => { await Promise.resolve(); await Promise.resolve(); flushSync(); };

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
 const h=harness();
 try {
  flushSync(); h.refresh(); assert.equal(h.requests.length,1);
  h.requests[0].resolve({sessionId:'same',provider:'codex',messages:[{content:'old'}]});await settle();
  h.refresh();assert.equal(h.requests.length,2);
  h.refresh();assert.equal(h.requests.length,2);
  const updated={sessionId:'same',provider:'codex',messages:[{content:'old'},{content:'reply'}]};
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
