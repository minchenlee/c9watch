// Checks that the Rust backend never writes the WebSocket access token
// to a log or the terminal. The token grants full remote control, so it
// must not end up in debug.log, stderr, or a terminal QR code.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const root = fileURLToPath(new URL('..', import.meta.url));

function files(dir, exts) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...files(path, exts));
    else if (exts.some((ext) => name.endsWith(ext))) out.push(path);
  }
  return out;
}

const sources = files(join(root, 'src-tauri/src'), ['.rs']).map((path) => ({
  path: relative(root, path),
  text: readFileSync(path, 'utf8'),
}));

// Returns the text between the open paren at `start` and its matching close
// paren. Skips string literals so a "(" inside a message does not count.
function callArgs(text, start) {
  let depth = 0;
  for (let i = start; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) if (text[i] === '\\') i++;
    } else if (ch === '(') depth++;
    else if (ch === ')' && --depth === 0) return text.slice(start + 1, i);
  }
  return text.slice(start + 1);
}

const loggingCall = /\b(log_[a-z_]+|eprintln!|println!|print_qr)\s*\(/g;

test('no log or print call takes the access token or URL', () => {
  const leaks = [];
  for (const { path, text } of sources) {
    for (const m of text.matchAll(loggingCall)) {
      const args = callArgs(text, m.index + m[0].length - 1);
      if (/token|http_url/i.test(args)) {
        const line = text.slice(0, m.index).split('\n').length;
        leaks.push(`${path}:${line} ${m[1]}(${args.trim().slice(0, 80)})`);
      }
    }
  }
  assert.deepEqual(leaks, []);
});

test('the backend does not print terminal QR codes', () => {
  const users = sources.filter(({ text }) => text.includes('qr2term')).map(({ path }) => path);
  assert.deepEqual(users, []);
  assert.doesNotMatch(readFileSync(join(root, 'src-tauri/Cargo.toml'), 'utf8'), /qr2term/);
});
