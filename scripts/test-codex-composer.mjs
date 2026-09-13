import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

// Exercise the component's actual send/update handlers with deferred IPC.
const source = readFileSync(new URL('../src/lib/components/CodexMessageComposer.svelte', import.meta.url), 'utf8');
const update = source.slice(source.indexOf('\tfunction update('), source.indexOf('\n\tasync function check('));
const attach = source.slice(source.indexOf('\tasync function attach('), source.indexOf('\n\tfunction paste('));
const send = source.slice(source.indexOf('\tasync function send('), source.indexOf('</script>', source.indexOf('\tasync function send(')));
const js = ts.transpileModule(`
export function harness() {
 const drafts = new Map();
 const empty = {text:'',pending:false,notice:'',unknown:false};
 let key='codex:A', sessionId='A', draft=empty;
 let available=true, checking=false, tooLong=false, attaching=false, draftFull=false;
 class FileReader { readAsDataURL(file) { this.result=file.url; queueMicrotask(()=>this.onload()); } }
 let active=false, turn=null, live=[];
 const requests=[];
 const invoke=(command,args)=>new Promise((resolve,reject)=>requests.push({command,args,resolve,reject}));
 ${update}
 ${attach}
 ${send}
 function sync(){draft=drafts.get(key)??empty;}
 return {
  requests, drafts,
  running(id){active=true;turn={turnId:id};live=[{}];},
  stale(){live=[];},
  images(images){sync();update({images});sync();},
  attach(files){sync();return attach(files);},
  write(text){sync();update({text});sync();},
  select(id){sessionId=id;key='codex:'+id;sync();},
  send(){sync();const result=send();sync();return result;},
 };
}`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const { harness } = await import('data:text/javascript;base64,' + Buffer.from(js).toString('base64'));

test('attachments are validated before preview and late validation stays with its session', async () => {
 const h=harness();h.write('original');
 const pending=h.attach([{name:'fixture.png',type:'image/png',size:3,url:'data:image/png;base64,YWJj'}]);
 await new Promise(setImmediate);
 assert.equal(h.requests[0].command,'validate_codex_images');
 assert.equal(h.drafts.get('codex:A').images,undefined);
 h.select('B');h.write('other');h.requests[0].resolve();await pending;
 assert.equal(h.drafts.get('codex:A').images.length,1);
 assert.equal(h.drafts.get('codex:B').images,undefined);
});

test('invalid or oversized attachments never become previews or send requests', async () => {
 const h=harness();h.write('keep');
 const pending=h.attach([{name:'corrupt.png',type:'image/png',size:3,url:'data:image/png;base64,YWJj'}]);
 await new Promise(setImmediate);h.requests[0].reject('Invalid image');await pending;
 assert.match(h.drafts.get('codex:A').notice,/Invalid image/);
 assert.equal(h.drafts.get('codex:A').images,undefined);
 await h.attach([{name:'large.png',type:'image/png',size:4*1024*1024+1}]);
 assert.equal(h.requests.length,1);
 assert.equal(h.drafts.get('codex:A').text,'keep');
});

test('double click sends once and a late receipt updates only its original session', async () => {
 const h=harness();h.write('第一行\n`$()`');const pending=h.send();await h.send();
 assert.equal(h.requests.length,1);
 assert.deepEqual(h.requests[0].args,{sessionId:'A',text:'第一行\n`$()`'});
 h.select('B');h.write('B draft');
 h.requests[0].resolve({status:'accepted',detail:'Accepted'});await pending;
 assert.equal(h.drafts.get('codex:A').text,'');
 assert.equal(h.drafts.get('codex:B').text,'B draft');
});

test('unknown delivery survives reopening and blocks another send', async () => {
 const h=harness();h.write('keep me');const pending=h.send();
 h.requests[0].resolve({status:'unknown',detail:'Check original session'});await pending;
 h.select('B');h.select('A');await h.send();
 assert.equal(h.requests.length,1);assert.equal(h.drafts.get('codex:A').text,'keep me');
});

test('active send binds the observed turn identity and a rejection never falls back to start', async () => {
 const h=harness();h.write('steer');h.running('original-turn');const pending=h.send();
 assert.equal(h.requests[0].args.expectedTurnId,'original-turn');
 h.requests[0].resolve({status:'rejected',detail:'Turn changed'});await pending;
 assert.equal(h.requests.length,1);assert.equal(h.drafts.get('codex:A').text,'steer');
});

test('stale active snapshots retain turn identity instead of starting a successor', async () => {
 const h=harness();h.write('steer');h.running('old-turn');h.stale();const pending=h.send();
 assert.equal(h.requests[0].args.expectedTurnId,'old-turn');
 h.requests[0].resolve({status:'not_sent',detail:'Not loaded'});await pending;
 assert.equal(h.requests.length,1);assert.equal(h.drafts.get('codex:A').text,'steer');
});

test('IPC failure retains the draft without automatic retry', async () => {
 const h=harness();h.write('keep me');const pending=h.send();
 h.requests[0].reject(new Error('IPC closed'));await pending;await h.send();
 assert.equal(h.requests.length,1);assert.equal(h.drafts.get('codex:A').unknown,true);
});

test('image-only send uses native images and clears attachments only after acceptance', async () => {
 const h=harness();const image={name:'test.png',url:'data:image/png;base64,AAAA'};
 h.images([image]);const pending=h.send();
 assert.deepEqual(h.requests[0].args,{sessionId:'A',text:'',images:[image.url]});
 h.requests[0].resolve({status:'accepted',detail:'Accepted'});await pending;
 assert.deepEqual(h.drafts.get('codex:A').images,[]);
});
test('unknown delivery preserves image attachments', async () => {
 const h=harness();const image={name:'test.png',url:'data:image/png;base64,AAAA'};
 h.images([image]);const pending=h.send();
 h.requests[0].resolve({status:'unknown',detail:'Check session'});await pending;
 assert.deepEqual(h.drafts.get('codex:A').images,[image]);
 await h.send();assert.equal(h.requests.length,1);
});

test('capacity preserves unsent text and images and reclaims only empty drafts', () => {
 const h=harness();
 for(let i=0;i<20;i++){h.select(String(i));h.write('unsent '+i);}
 h.select('0');h.images([{name:'x.png',url:'data:image/png;base64,AAAA'}]);
 h.select('20');h.write('overflow');
 assert.equal(h.drafts.size,20);
 assert.equal(h.drafts.has('codex:20'),false);
 assert.equal(h.drafts.get('codex:0').text,'unsent 0');
 h.select('0');h.write('');
 h.select('20');h.write('still full');
 assert.equal(h.drafts.has('codex:20'),false);
 h.select('1');h.write('');
 h.select('20');h.write('now fits');
 assert.equal(h.drafts.get('codex:20').text,'now fits');
 assert.equal(h.drafts.get('codex:0').images.length,1);
});

test('definite not-sent and rejected receipts retain drafts and allow manual retry', async () => {
 for(const status of ['not_sent','rejected']) {
  const h=harness();h.write('retry me');let pending=h.send();
  h.requests[0].resolve({status,detail:'Not delivered'});await pending;
  assert.equal(h.drafts.get('codex:A').unknown,false);
  assert.equal(h.drafts.get('codex:A').text,'retry me');
  pending=h.send();assert.equal(h.requests.length,2);
  h.requests[1].resolve({status:'accepted',detail:'Accepted'});await pending;
 }
});

const stopHandler = source.slice(source.indexOf(' async function stop()'), source.indexOf('\n\tfunction autosize'));
const stopJs = ts.transpileModule(`
export function stopHarness() {
 const stops = new Map(); let stopDisabled=false;
 let notice='';const update=patch=>{notice=patch.notice;};
 let turn={turnId:'one'}, snapshot={endpoint:'local'}, sessionId='A', stopKey='local:A:one';
 const requests=[]; const invoke=(command,args)=>new Promise((resolve,reject)=>requests.push({command,args,resolve,reject}));
 const refreshCodexInteractions=()=>Promise.resolve();
 ${stopHandler}
 return {stop,stops,requests,notice:()=>notice,disable(){stopDisabled=true;},next(){turn={turnId:'two'};stopKey='local:A:two';}};
}`, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const {stopHarness}=await import('data:text/javascript;base64,'+Buffer.from(stopJs).toString('base64'));
test('stop is sent once for the exact turn and late receipts do not disable a new turn',async()=>{
 const h=stopHarness();const first=h.stop();await h.stop();assert.equal(h.requests.length,1);
 assert.deepEqual(h.requests[0].args,{endpoint:'local',threadId:'A',turnId:'one'});
 h.next();h.requests[0].resolve({status:'submitted',detail:'Stopping'});await first;
 const next=h.stop();assert.equal(h.requests.length,2);h.requests[1].reject(Error('lost'));await next;
 await h.stop();assert.equal(h.requests.length,2);assert.equal(h.stops.get('local:A:two').status,'unknown');
});
test('unavailable stop cannot invoke the backend',async()=>{
 const h=stopHarness();h.disable();await h.stop();assert.equal(h.requests.length,0);
});
test('definite rejected and not-sent stop receipts remain retryable',async()=>{
 for(const status of ['rejected','not_sent']) {
  const h=stopHarness();const first=h.stop();
  h.requests[0].resolve({status,detail:'failed'});await first;
  const retry=h.stop();assert.equal(h.requests.length,2);
  assert.deepEqual(h.requests[1].args,{endpoint:'local',threadId:'A',turnId:'one'});
  h.requests[1].resolve({status:'submitted',detail:'Stopping'});await retry;
 }
});

test('stop retention is bounded without evicting uncertain turn identities',async()=>{
 const h=stopHarness();for(let i=0;i<1024;i++)h.stops.set(String(i),{status:'unknown'});
 await h.stop();assert.equal(h.requests.length,0);assert.equal(h.stops.size,1024);
 assert.match(h.notice(),/Stop history is full/);
});
