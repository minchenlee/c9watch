import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
const binary = resolve(process.argv[2]);
const dir = mkdtempSync(join(tmpdir(), 'c9watch-claude-bridge-'));
const env = {...process.env, CLAUDE_CONFIG_DIR:dir};
const invoke = (args,input='') => spawnSync(binary,['usage-bridge',...args],{env,input,encoding:'utf8',timeout:10000});
try {
  const settings = {permissions:{deny:['fixture']},statusLine:{type:'command',command:"python3 -c 'import json,sys; print(json.load(sys.stdin)[\"model\"][\"display_name\"])' # preserved",padding:2}};
  writeFileSync(join(dir,'settings.json'),JSON.stringify(settings));
  assert.equal(invoke(['--install']).status,0);
  const installed = JSON.parse(readFileSync(join(dir,'settings.json')));
  assert.deepEqual(installed.permissions,settings.permissions);
  assert.equal(installed.statusLine.padding,2);
  assert.equal(invoke(['--install']).status,0);
  assert.equal(readdirSync(dir).filter(x=>x.includes('backup')).length,1);
  const now=Math.floor(Date.now()/1000);
  const input=JSON.stringify({model:{display_name:'TEST MODEL'},session_id:'DO NOT SAVE',rate_limits:{five_hour:{used_percentage:23.5,resets_at:now+3600},seven_day:{used_percentage:0,resets_at:now+7200}}});
  const forwarded=invoke(['--passthrough'],input);
  assert.equal(forwarded.status,0);assert.equal(forwarded.stdout,input);
  const path=join(dir,'c9watch/subscription-usage.json');
  const saved=readFileSync(path,'utf8');assert.ok(!saved.includes('DO NOT SAVE'));assert.ok(!saved.includes('TEST MODEL'));
  const existing=spawnSync('/bin/sh',['-c',installed.statusLine.command],{env,input,encoding:'utf8',timeout:10000});
  assert.equal(existing.status,0);assert.equal(existing.stdout.trim(),'TEST MODEL');
  assert.equal(invoke([], '{}').status,0);
  assert.deepEqual(JSON.parse(readFileSync(path)).rate_limits,{});
  assert.notEqual(invoke([], 'invalid json').status,0);
  assert.notEqual(invoke([], 'x'.repeat(1_048_577)).status,0);
  // A failed cache write must not break the existing status line's stdin.
  rmSync(path);rmSync(join(dir,'c9watch'),{recursive:true});writeFileSync(join(dir,'c9watch'),'blocked');
  const failed=invoke(['--passthrough'],input);assert.equal(failed.status,0);assert.equal(failed.stdout,input);
  console.log('PASS: isolated install, backup, idempotence, original status line, sanitized snapshots, missing quota, invalid/oversized input, write failure passthrough');
} finally { rmSync(dir,{recursive:true,force:true}); }
