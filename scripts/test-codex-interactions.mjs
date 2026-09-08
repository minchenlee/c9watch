import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';
import { writable, get } from 'svelte/store';

const source = readFileSync(new URL('../src/lib/stores/codex-interactions.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source.replace(/^import .*;$/gm, ''), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
const exports = {};
const status = { Working: 'Working', WaitingForInput: 'WaitingForInput' };
new Function('exports','writable','get','invoke','SessionStatus',js)(exports,writable,get,()=>Promise.resolve([]),status);
const { mergeSnapshots, projectCodexSession, codexInteractions, canAnswer } = exports;
const request = {token:'q1',threadId:'A',turnId:'t',kind:'question',summary:'Choose',questions:[],submitted:false,answerable:true};
const snapshot = {endpoint:'e1',connected:true,pending:[request],statuses:{A:'waiting'},overflow:false};
const session = {id:'A',provider:'codex',status:'Working'};

test('Codex pending requests reuse Waiting and clear back to the authoritative status', () => {
 assert.equal(projectCodexSession(session,[snapshot]).status,'WaitingForInput');
 assert.equal(projectCodexSession(session,[snapshot]).pendingToolName,'Waiting for answer · 1');
 const cleared={...snapshot,pending:[],statuses:{A:'active'}};
 assert.equal(projectCodexSession(session,[cleared]).status,'Working');
 assert.equal(projectCodexSession({...session,provider:'claudeCode'},[snapshot]).status,'Working');
});
test('disconnect preserves a disabled stale card; reconnect replaces it without reviving old requests', () => {
 const lost=mergeSnapshots([snapshot],[]);
 assert.equal(lost[0].connected,false);assert.equal(lost[0].pending.length,1);
 codexInteractions.set(lost);assert.equal(canAnswer('e1',request),false);
 const fresh=mergeSnapshots(lost,[{...snapshot,pending:[],statuses:{A:'idle'}}]);
 assert.equal(fresh[0].pending.length,0);
 assert.equal(projectCodexSession(session,fresh).pendingToolName,undefined);
});
test('duplicate endpoints and submitted requests cannot be answered', () => {
 codexInteractions.set([snapshot]);assert.equal(canAnswer('e1',request),true);
 codexInteractions.set([snapshot,{...snapshot,endpoint:'e2'}]);assert.equal(canAnswer('e1',request),false);
 codexInteractions.set([{...snapshot,pending:[{...request,submitted:true}]}]);assert.equal(canAnswer('e1',request),false);
});
test('waiting flags without details show an honest fallback and stale endpoint retention is bounded', () => {
 const projected=projectCodexSession(session,[{...snapshot,pending:[]}]);
 assert.equal(projected.status,'WaitingForInput');assert.match(projected.pendingToolName,/details/);
 const previous=Array.from({length:32},(_,i)=>({...snapshot,endpoint:String(i)}));
 assert.equal(mergeSnapshots(previous,[]).length,16);
});

const card = readFileSync(new URL('../src/lib/components/CodexQuestionCard.svelte', import.meta.url), 'utf8');
const submit = card.slice(card.indexOf(' async function submit()'),card.indexOf('</script>'));
const cardJs = ts.transpileModule(`
export function harness(){
 const request={token:'q',threadId:'A',questions:[{id:'choice'},{id:'text'}]};const endpoint='e';
 let editable=true,complete=true,sending=false,submitted=false,unknown=false,notice='';
 const answers={choice:'Local',text:'中文'};const requests=[];
 const canAnswer=()=>true;const refreshCodexInteractions=()=>Promise.resolve();
 const invoke=(cmd,args)=>new Promise((resolve,reject)=>requests.push({cmd,args,resolve,reject}));
 ${submit}
 return {requests,submit:()=>{editable=!sending&&!submitted&&!unknown;return submit();},state:()=>({submitted,unknown,answers})};
}`,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText;
const cardExports={};new Function('exports',cardJs)(cardExports);
test('multi-question submit uses the answer endpoint once and IPC loss never retries automatically',async()=>{
 const h=cardExports.harness();const p=h.submit();await h.submit();
 assert.equal(h.requests.length,1);assert.equal(h.requests[0].cmd,'answer_codex_question');
 assert.deepEqual(h.requests[0].args.answers,{choice:{answers:['Local']},text:{answers:['中文']}});
 h.requests[0].reject(new Error('lost'));await p;await h.submit();
 assert.equal(h.requests.length,1);assert.equal(h.state().unknown,true);assert.equal(h.state().answers.text,'中文');
});

const formsJs=ts.transpileModule(readFileSync(new URL('../src/lib/codex-forms.ts',import.meta.url),'utf8'),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS}}).outputText;
const forms={};new Function('exports',formsJs)(forms);
test('MCP fields preserve titled choices and explicit false/zero values',()=>{
 assert.deepEqual(forms.formOptions({type:'string',enum:['a'],enumNames:['Alpha']}),[{value:'a',label:'Alpha'}]);
 assert.deepEqual(forms.formOptions({type:'array',items:{anyOf:[{const:'b',title:'Beta'}]}}),[{value:'b',label:'Beta'}]);
 const schema={type:'object',properties:{flag:{type:'boolean'},count:{type:'integer'}},required:['flag','count']};
 assert.equal(forms.formComplete(schema,{flag:false,count:0}),true);
 assert.equal(forms.formComplete(schema,{flag:false}),false);
 for(const url of ['file:///tmp/a','javascript:alert(1)','https://u:p@example.com'])assert.equal(forms.safeExternalUrl(url),false);
 assert.equal(forms.safeExternalUrl('https://example.com/complete'),true);
});
test('approval capability checks require the exact current token and allowed action',()=>{
 const approval={...request,kind:'command',answerable:false,actions:['decline','cancel']};
 codexInteractions.set([{...snapshot,pending:[approval]}]);
 assert.equal(exports.canDecide('e1',approval,'accept'),false);
 assert.equal(exports.canDecide('e1',approval,'decline'),true);
 assert.equal(exports.canDecide('e1',{...approval,token:'old'},'decline'),false);
 codexInteractions.set([{...snapshot,connected:false,pending:[approval]}]);
 assert.equal(exports.canDecide('e1',approval,'decline'),false);
});
test('disconnect retains turn progress without claiming a live stop capability',()=>{
 const turn={turnId:'t',status:'inProgress',stopping:false,plan:[],diff:'',diffTruncated:false};
 const previous={...snapshot,pending:[],turns:{A:turn}};
 const lost=mergeSnapshots([previous],[]);
 assert.equal(lost[0].connected,false);assert.equal(lost[0].turns.A.turnId,'t');
 const restored=mergeSnapshots(lost,[{...snapshot,pending:[],turns:{A:{...turn,status:'completed'}}}]);
 assert.equal(restored[0].turns.A.status,'completed');
});

// A slow transcript scan must not accumulate on every session/poll update.
test('subagent polling coalesces overlapping scans and recovers after failure', async () => {
 const source = readFileSync(new URL('../src/lib/stores/subagents.ts', import.meta.url), 'utf8');
 const js = ts.transpileModule(source.replace(/^import .*;$/gm, '') + '\nexport { refreshOnce };', { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
 const module = {}, requests = [];
 const invoke = () => new Promise((resolve, reject) => requests.push({resolve, reject}));
 new Function('exports','writable','derived','get','invoke','isTauri',js)(module,writable,()=>{},get,invoke,()=>true);
 const first = module.refreshOnce();
 await Promise.all([module.refreshOnce(),module.refreshOnce()]);
 assert.equal(requests.length,1);
 requests[0].reject(new Error('temporary scan failure')); await first;
 const next = module.refreshOnce(); assert.equal(requests.length,2);
 requests[1].resolve({}); await next;
});

test('an unloaded endpoint does not block a live Computer Use approval',()=>{
 const approval={...request,kind:'form',actions:['accept','decline','cancel']};
 const live={...snapshot,pending:[approval]};
 const unloaded={...snapshot,endpoint:'old',pending:[],statuses:{A:'notLoaded'},turns:{A:{turnId:'old',status:'completed'}}};
 assert.equal(exports.hasCodexThread(unloaded,'A'),false);
 codexInteractions.set([live,unloaded]);
 assert.equal(exports.canDecide('e1',approval,'accept'),true);
 assert.equal(exports.canDecide('old',approval,'accept'),false);
 codexInteractions.set([live,{...unloaded,statuses:{A:'active'}}]);
 assert.equal(exports.canDecide('e1',approval,'accept'),false);
 codexInteractions.set([live,{...unloaded,pending:[approval]}]);
 assert.equal(exports.canDecide('e1',approval,'accept'),false);
});

const approvalCard = readFileSync(new URL('../src/lib/components/CodexApprovalCard.svelte', import.meta.url), 'utf8');
const decideHandler = approvalCard.slice(approvalCard.indexOf(' async function decide('),approvalCard.indexOf(' async function openExternal('));
const approvalJs = ts.transpileModule(`
export function approvalHarness(){
 const request={token:'old',threadId:'A',kind:'form'},endpoint='e';
 const enabled=true,ready=()=>true,canDecide=()=>false;
 let notice='',refreshes=0,sends=0;
 const refreshCodexInteractions=()=>{refreshes++;return Promise.resolve();};
 const invoke=()=>{sends++;};
 ${decideHandler}
 return {decide,state:()=>({notice,refreshes,sends})};
}`,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const {approvalHarness}=await import('data:text/javascript;base64,'+Buffer.from(approvalJs).toString('base64'));
test('a changed approval request explains the failed click and refreshes without sending',async()=>{
 const h=approvalHarness();await h.decide('accept');
 assert.equal(h.state().sends,0);assert.equal(h.state().refreshes,1);
 assert.match(h.state().notice,/no response was sent/);
});
