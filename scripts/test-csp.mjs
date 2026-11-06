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
    // c9ext: is the extension file scheme (glass G7): files from disk, no inline.
    assert.equal(csp['script-src'], "'self' c9ext:");
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

test('c9ext: is allowed only where extensions load files', () => {
  const want = ['script-src', 'style-src', 'img-src', 'font-src', 'connect-src'];
  for (const csp of [security.csp, security.devCsp]) {
    for (const [directive, value] of Object.entries(csp)) {
      assert.equal(value.split(/\s+/).includes('c9ext:'), want.includes(directive), directive);
    }
  }
});

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

// Glass G10b: native glass is macOS only. tauri.macos.conf.json is a JSON
// merge patch, so its `windows` array replaces the base one: it must repeat
// both windows exactly and add only the three glass keys to `main`.
test('tauri.macos.conf.json repeats the base windows and adds glass to main only', () => {
  const mac = JSON.parse(readFileSync(join(root, 'src-tauri/tauri.macos.conf.json'), 'utf8'));
  assert.deepEqual(Object.keys(mac).filter(k => k !== '$schema'), ['app']);
  assert.deepEqual(Object.keys(mac.app), ['windows']);
  const [main, popover] = mac.app.windows;
  const glass = { transparent: true, windowEffects: { effects: ['sidebar'] }, theme: 'Dark' };
  assert.deepEqual(main, { ...config.app.windows[0], ...glass });
  assert.deepEqual(popover, config.app.windows[1]);
  assert.equal(mac.app.windows.length, config.app.windows.length);
  // Base (Linux, Windows): no glass keys on main, so Classic keeps the
  // system title bar theme and an opaque window.
  for (const key of Object.keys(glass)) assert.ok(!(key in config.app.windows[0]), `base main has ${key}`);
  // macOSPrivateApi stays in the base: tauri-build checks the `tauri`
  // features against the merged config on every platform, and the feature
  // is not target-specific (it is a no-op off macOS).
  assert.equal(config.app.macOSPrivateApi, true);
});
