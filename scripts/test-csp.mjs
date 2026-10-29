// Checks the desktop webview Content Security Policy in tauri.conf.json.
// The webview renders markdown from session logs with {@html}, so a strict
// CSP is the last line of defense against injected scripts and exfiltration.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const root = fileURLToPath(new URL('..', import.meta.url));
const config = JSON.parse(readFileSync(join(root, 'src-tauri/tauri.conf.json'), 'utf8'));
const security = config.app.security;

function files(dir, exts) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...files(path, exts));
    else if (exts.some((ext) => name.endsWith(ext))) out.push(path);
  }
  return out;
}

const sources = (directive) => (security.csp[directive] ?? '').split(/\s+/).filter(Boolean);

test('the desktop webview has a CSP in both build and dev', () => {
  assert.equal(typeof security.csp, 'object');
  assert.notEqual(security.csp, null);
  assert.equal(typeof security.devCsp, 'object');
  assert.notEqual(security.devCsp, null);
});

test('scripts cannot run inline or from eval', () => {
  for (const csp of [security.csp, security.devCsp]) {
    assert.equal(csp['script-src'], "'self'");
    assert.doesNotMatch(csp['default-src'], /unsafe/);
    assert.equal(csp['object-src'], "'none'");
    assert.equal(csp['base-uri'], "'none'");
  }
});

test('Tauri does not add a style nonce that would disable unsafe-inline', () => {
  // A nonce in style-src makes the browser ignore 'unsafe-inline', which
  // breaks Svelte transitions and the style attribute in app.html.
  assert.ok(security.dangerousDisableAssetCspModification.includes('style-src'));
  assert.ok(!security.dangerousDisableAssetCspModification.includes('script-src'));
});

test('dev CSP differs from the build CSP only by the Vite dev server', () => {
  const { 'connect-src': devConnect, ...devRest } = security.devCsp;
  const { 'connect-src': connect, ...rest } = security.csp;
  assert.deepEqual(devRest, rest);
  assert.equal(devConnect, `${connect} ws://localhost:1420 http://localhost:1420`);
});

// Every absolute URL or url() in the frontend, with why the CSP allows it.
// A new entry here means a new network source: check the CSP before adding.
const allowedUrls = [
  // Same-origin fonts: font-src 'self'.
  { re: /^url\('\/fonts\/[\w-]+\.woff2'\)$/ },
  // Release notes for the updater: connect-src https://api.github.com.
  { re: /^https:\/\/api\.github\.com\/repos\/minchenlee\/c9watch\/releases\// },
  // Default OpenCode server address. The backend connects to it over IPC.
  { re: /^http:\/\/127\.0\.0\.1:4096$/ },
  // Mobile access URL shown as text and a data: QR code; never fetched.
  { re: /^http:\/\/\$\{info\.localIp\}:\$\{info\.port\}\// },
  // Another desktop's page address in the "Other host" hint; text only, never fetched.
  { re: /^http:\/\/(\$\{host\}|host:9210)\/(<\/code)?$/ },
  // Documentation links in comments.
  { re: /^https:\/\/(svelte\.dev|v2\.tauri\.app)\// },
];

test('every URL the frontend references is covered by the CSP', () => {
  assert.ok(sources('connect-src').includes('https://api.github.com'));
  assert.ok(sources('font-src').includes("'self'"));
  const unknown = [];
  for (const path of files(join(root, 'src'), ['.ts', '.svelte', '.css'])) {
    const text = readFileSync(path, 'utf8');
    for (const m of text.matchAll(/https?:\/\/[^'"`\s)>]*|url\([^)]*\)/g)) {
      if (!allowedUrls.some(({ re }) => re.test(m[0]))) {
        unknown.push(`${relative(root, path)}: ${m[0]}`);
      }
    }
  }
  assert.deepEqual(unknown, []);
});
