// Checks that the WebSocket client token never leaks: the Rust backend does
// not write it (or a value that carries it) to a log or the terminal, no
// URL carries it, and ServerInfo does not hand it to the UI. A token grants
// full remote control, so it must not end up in debug.log, stderr, a QR
// code, browser history or a Referer.
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

// Every way the backend writes text out: the debug_log helpers, the std
// print macros, dbg!, and the tracing / log crate macros (qualified or not).
const loggingCall =
  /\b(log_[a-z_]+|e?println!|e?print!|dbg!|print_qr|(?:tracing|log)::[a-z_]+!|(?:trace|debug|info|warn|error)!)\s*\(/g;
// Names that are, or carry, a credential.
const secretArg = /token|http_url|ws_url|server_info|ServerInfo|pairing_code|PairingCode|AuthReply|Paired/;

test('no log or print call takes the access token or URL', () => {
  const leaks = [];
  for (const { path, text } of sources) {
    for (const m of text.matchAll(loggingCall)) {
      const args = callArgs(text, m.index + m[0].length - 1);
      if (/token/i.test(args) || secretArg.test(args)) {
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

test('the leak scan sees every logging form', () => {
  const sample = [
    'debug_log::log_info(&format!("{}", ws_url));',
    'eprintln!("{:?}", server_info);',
    'dbg!(server_info(&server));',
    'tracing::info!("{:?}", ServerInfo { port });',
    'log::debug!("{}", token);',
    'warn!("{}", auth_token);',
  ];
  for (const line of sample) {
    const m = [...line.matchAll(loggingCall)][0];
    assert.ok(m, `not seen as a log call: ${line}`);
    const args = callArgs(line, m.index + m[0].length - 1);
    assert.ok(/token/i.test(args) || secretArg.test(args), `not flagged: ${line}`);
  }
});

const libRs = readFileSync(join(root, 'src-tauri/src/lib.rs'), 'utf8');
const api = readFileSync(join(root, 'src/lib/data/platform.ts'), 'utf8');

test('ServerInfo carries no token and no token-bearing URL', () => {
  const rust = libRs.match(/pub struct ServerInfo \{([\s\S]*?)\n\}/);
  assert.ok(rust, 'ServerInfo not found in lib.rs');
  assert.doesNotMatch(rust[1], /\btoken\b|ws_url/);
  const ts = api.match(/export interface ServerInfo \{([\s\S]*?)\n\}/);
  assert.ok(ts, 'ServerInfo not found in api.ts');
  assert.doesNotMatch(ts[1], /\btoken\b|wsUrl/);
});

test('no URL carries a token, in the backend or the frontend', () => {
  const frontend = files(join(root, 'src'), ['.ts', '.svelte']).map((path) => ({
    path: relative(root, path),
    text: readFileSync(path, 'utf8'),
  }));
  const hits = [...sources, ...frontend]
    .filter(({ text }) => /[?&]token=/.test(text))
    .map(({ path }) => path);
  assert.deepEqual(hits, []);
});

test('the WS handler reads no token from the query string', () => {
  const web = readFileSync(join(root, 'src-tauri/src/web_server.rs'), 'utf8');
  assert.doesNotMatch(web, /struct WsQuery|Query<WsQuery>/);
  assert.doesNotMatch(web, /auth_token/);
});
