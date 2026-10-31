// Real c9p fixture engine; same reducer as the web page, over Unix transport.
// Gateway/browser acceptance still needs M-b.
import { C9pClient } from '../../packages/c9p-client/dist/index.js';
import { UnixTransport } from '../../packages/c9p-client/dist/transport-unix.js';
import { watchBoard } from './.smoke/examples/ts-web-board/board.js';
import assert from 'node:assert/strict';
const c = new C9pClient(() => new UnixTransport(process.argv[2]));
let timer;
try {
  await c.connect();
  const states = [];
  let complete, failed;
  const done = new Promise((resolve, reject) => { complete = resolve; failed = reject; });
  timer = setTimeout(() => failed(new Error('fixture events did not arrive')), 5000);
  const sub = await watchBoard(c, (sessions) => {
    states.push(sessions);
    if (states.length === 3) complete();
  }, failed);
  assert.deepEqual(states, [[]]);
  console.log('web board fixture: ready');
  await done;
  assert.equal(states[1][0].sessionKey, 'claudeCode:s1');
  assert.deepEqual(states[2], []);
  await sub.unsubscribe();
  console.log('web board fixture: ok');
} finally { clearTimeout(timer); c.close(); }
