import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { publicSchema } from './public-c9p-schema.mjs';
const input = JSON.parse(readFileSync(new URL('../docs/api/c9p-1.json', import.meta.url), 'utf8'));
const expected = publicSchema(input);
if (process.argv[2]) {
  const actual = JSON.parse(execFileSync(process.argv[2], ['api', 'schema', '--public'], { encoding: 'utf8', maxBuffer: 4 << 20 }));
  assert.deepEqual(actual, expected, 'Rust and SDK public projections differ');
}
function walk(v) {
  if (!v || typeof v !== 'object') return;
  assert.notEqual(v['x-visibility'], 'internal');
  assert.notEqual(v['x-stability'], 'internal');
  if (Array.isArray(v.required)) for (const key of v.required) assert(Object.hasOwn(v.properties ?? {}, key), `missing required property ${key}`);
  if (v.$ref) assert(expected.$defs[v.$ref.slice(8)], `dangling ref ${v.$ref}`);
  if (typeof v.description === 'string') assert(!/\b(?:A:\d+|C1[ab]-\d+|[DQN]\d+)|§\d/.test(v.description), v.description);
  Object.values(v).forEach(walk);
}
walk(expected);
for (const group of ['methods', 'topics']) {
  for (const [name, entry] of Object.entries(expected[group])) {
    assert(entry['x-authz'], `missing public authority: ${group}.${name}`);
    assert.deepEqual(entry['x-authz'], input[group][name]['x-authz'], `changed public authority: ${group}.${name}`);
  }
}
assert(!expected.topics['legacy.inbox']);
assert(!Object.keys(expected.methods).some((m) => m.startsWith('hook.')));
assert(!expected.methods['task.done']); assert(!expected.methods['agent.handoff']);
console.log(`public schema: ok (${Object.keys(expected.methods).length} methods, ${Object.keys(expected.topics).length} topics)`);
