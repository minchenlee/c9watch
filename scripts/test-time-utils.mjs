import assert from 'node:assert/strict';
import { test } from 'node:test';
import { formatTimeSince } from '../src/lib/time-utils.ts';

const ago = ms => new Date(Date.now() - ms).toISOString();

test('formatTimeSince uses the compact now/m/h/d format', () => {
  assert.equal(formatTimeSince(ago(5_000)), 'now');
  assert.equal(formatTimeSince(ago(5 * 60_000 + 500)), '5m');
  assert.equal(formatTimeSince(ago(3 * 3_600_000 + 500)), '3h');
  assert.equal(formatTimeSince(ago(2 * 86_400_000 + 500)), '2d');
});
