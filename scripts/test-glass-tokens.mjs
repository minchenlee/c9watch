// Glass token contrast (ui-refactor-direction §9.1): reads
// src/shells/glass/tokens.css and checks WCAG 2.x contrast of every text
// token on every surface it can sit on. Alpha is composited in sRGB, against
// the worst desktops (pure white and pure black), as the direction measured.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const css = readFileSync(new URL('../src/shells/glass/tokens.css', import.meta.url), 'utf8');

function block(selector) {
  const at = css.indexOf(`${selector} {`);
  assert.ok(at >= 0, `missing block ${selector}`);
  const body = css.slice(at, css.indexOf('}', at));
  return Object.fromEntries([...body.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(m => [m[1], m[2].trim()]));
}
const base = block('.glass-root');
const modes = {
  subtle: base,
  full: { ...base, ...block(".glass-root[data-glass='full']") },
  off: { ...base, ...block(".glass-root[data-glass='off']") }
};

// Color as [r, g, b, alpha], channels in 0..1 gamma sRGB.
function parse(tokens, value) {
  const ref = value.match(/^var\((--[\w-]+)\)$/);
  if (ref) return parse(tokens, tokens[ref[1]]);
  let m = value.match(/^#([0-9a-f]{6})$/i);
  if (m) return [0, 2, 4].map(i => parseInt(m[1].slice(i, i + 2), 16) / 255).concat(1);
  m = value.match(/^rgb\((\d+) (\d+) (\d+)(?: \/ ([\d.]+))?\)$/);
  if (m) return [+m[1] / 255, +m[2] / 255, +m[3] / 255, m[4] === undefined ? 1 : +m[4]];
  m = value.match(/^oklch\(([\d.]+) ([\d.]+) ([\d.]+)(?: \/ ([\d.]+))?\)$/);
  if (m) return [...oklchToSrgb(+m[1], +m[2], +m[3]), m[4] === undefined ? 1 : +m[4]];
  throw new Error(`cannot parse color ${value}`);
}
function oklchToSrgb(L, C, h) {
  const a = C * Math.cos((h * Math.PI) / 180), b = C * Math.sin((h * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const lin = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s
  ];
  return lin.map(x => Math.min(1, Math.max(0, x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055)));
}
const over = (fg, bg) => [0, 1, 2].map(i => fg[3] * fg[i] + (1 - fg[3]) * bg[i]).concat(1);
const luminance = c => {
  const [r, g, b] = c.slice(0, 3).map(x => (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

const DESKTOPS = { white: [1, 1, 1, 1], black: [0, 0, 0, 1] };
// Every surface text can sit on, stacked on the window tint (L0). L2 never
// sits on the bare desktop (direction §9.1 rule).
function surfaces(tokens, desk) {
  const c = name => parse(tokens, tokens[name]);
  const l0 = over(c('--surface-window'), desk);
  return {
    'L0 window': l0,
    'L1 surface-1': over(c('--surface-1'), l0),
    'L1 surface-1-hover': over(c('--surface-1-hover'), l0),
    'L1 surface-2': over(c('--surface-2'), l0),
    'L2 glass': over(c('--glass-bg'), l0)
  };
}
const TEXT = ['--text-1', '--text-2', '--text-3'];
const STATUS_TEXT = ['--status-working-text', '--status-permission-text', '--status-input-text', '--status-error-text'];

for (const [mode, tokens] of Object.entries(modes)) {
  for (const [deskName, desk] of Object.entries(DESKTOPS)) {
    test(`Glass ${mode}, ${deskName} desktop: text and status text ≥ 4.5:1 on every surface`, () => {
      for (const [surfaceName, surface] of Object.entries(surfaces(tokens, desk))) {
        for (const t of [...TEXT, ...STATUS_TEXT]) {
          const ratio = contrast(parse(tokens, tokens[t]), surface);
          assert.ok(ratio >= 4.5, `${t} on ${surfaceName}: ${ratio.toFixed(2)}`);
        }
      }
    });
  }
}

test('the math matches the direction §9.1 numbers', () => {
  const s = surfaces(base, DESKTOPS.white);
  const t3 = parse(base, base['--text-3']);
  // L0 0.90 on white: --text-3 4.72. L1 on L0: 5.4–5.6.
  assert.ok(Math.abs(contrast(t3, s['L0 window']) - 4.72) < 0.15, contrast(t3, s['L0 window']).toFixed(2));
  const l1 = contrast(t3, s['L1 surface-1']);
  assert.ok(l1 > 5.3 && l1 < 5.8, l1.toFixed(2));
});

test('--text-disabled stays below AA, so it cannot carry information', () => {
  const ratio = contrast(parse(base, base['--text-disabled']), surfaces(base, DESKTOPS.black)['L1 surface-1']);
  assert.ok(ratio < 4.5, ratio.toFixed(2));
});

test('working dot passes 3:1 as a non-text element, but not 4.5:1 as text', () => {
  const ratio = contrast(parse(base, base['--status-working']), surfaces(base, DESKTOPS.black)['L1 surface-1']);
  assert.ok(ratio >= 3 && ratio < 4.5, ratio.toFixed(2));
});

test('G10b: the L0 tint lets more vibrancy through as Off → Subtle → Full', () => {
  const alpha = mode => parse(modes[mode], modes[mode]['--surface-window'])[3];
  assert.equal(alpha('off'), 1, 'Off is opaque');
  assert.ok(alpha('subtle') < 1 && alpha('full') < alpha('subtle'), `subtle ${alpha('subtle')}, full ${alpha('full')}`);
});

test('G10b: html and body turn transparent only under html.glass-native', () => {
  const rule = css.match(/([^{}]*)\{\s*background: transparent;\s*\}/);
  assert.ok(rule, 'missing the transparent html/body rule');
  const selectors = rule[1].replace(/\/\*[\s\S]*?\*\//g, '').split(',').map(s => s.trim());
  assert.deepEqual(selectors, ['html.glass-native', 'html.glass-native body']);
});
