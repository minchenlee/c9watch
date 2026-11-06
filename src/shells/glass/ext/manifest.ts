/** Extension manifest `c9ext.json` v1 (glass build plan §3.4, as changed by
 * owner D41: R1–R12). Pure functions, no imports: the node unit test
 * (`scripts/test-glass-ext.mjs`) loads this file on its own.
 *
 * Only the UI part is used. The `core` block is reserved for the core
 * extension design: this file never runs it and never fails on it (R4). */

/** The UI contract this host speaks (R2: `ui.contract`). */
export const UI_CONTRACT = 'c9ui/1';
/** The manifest format this host reads (R1: top-level `c9ext`). */
export const MANIFEST_FORMAT = 1;
export const MAX_MANIFEST_BYTES = 64 * 1024;
export const MAX_PAGES = 16;
export const MAX_NAME = 64;
const ID_RE = /^[a-z][a-z0-9-]{0,31}$/;
// semver.org 2.0.0, the recommended regular expression.
const SEMVER_RE = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$/;

/** R8. */
export type ExtStatus = 'ok' | 'manifest_error' | 'version_mismatch' | 'shadowed' | 'dev_only';
/** `first`: repo `extensions/` (in the app bundle). `third`: `<engine home>/extensions/`. */
export type ExtSource = 'first' | 'third';

export type ExtPage = { id: string; title: string; entry: string; icon?: string };
export type Manifest = {
    c9ext: 1;
    id: string;
    name: string;
    version: string;
    minVersion?: { app?: string; apiMinor?: number };
    ui?: { contract: typeof UI_CONTRACT; pages: ExtPage[] };
    dev: boolean;
};

/** One directory found in an extensions root, before validation. */
export type ExtCandidate = {
    dir: string;
    source: ExtSource;
    /** The raw `c9ext.json` text; null when it is missing or too big to send. */
    manifest: string | null;
    /** Byte size of `c9ext.json` (lets the host refuse a big file it was not sent). */
    size?: number;
    /** URL prefix the files load from, ending in `/` (default per list source). */
    base?: string;
};

export type ExtEntry = {
    dir: string;
    source: ExtSource;
    status: ExtStatus;
    /** Why the status is not `ok`, in words for the settings list. */
    reason?: string;
    /** Set when the manifest is valid (every status except `manifest_error`). */
    manifest?: Manifest;
    hasUi: boolean;
    /** True when the manifest has a `core` block (R4: shown, never run). */
    hasCore: boolean;
    /** Problems in reserved parts (`core`) that do not stop the UI (R12). */
    warnings: string[];
    base: string;
};

/** What the host is: app version, the c9p minor it speaks, dev or prod build. */
export type HostEnv = { app: string; apiMinor: number; dev: boolean };

type Parsed = { ok: true; manifest: Manifest; hasCore: boolean; warnings: string[] } | { ok: false; reason: string };

export function isSemver(v: unknown): v is string {
    return typeof v === 'string' && v.length <= 256 && SEMVER_RE.test(v);
}

/** Compares two valid semver strings by precedence (build metadata ignored). */
export function compareSemver(a: string, b: string): number {
    const pa = SEMVER_RE.exec(a), pb = SEMVER_RE.exec(b);
    if (!pa || !pb) throw new Error('not semver');
    for (let i = 1; i <= 3; i++) {
        const d = Number(pa[i]) - Number(pb[i]);
        if (d) return Math.sign(d);
    }
    const ra = pa[4], rb = pb[4];
    if (ra === rb) return 0;
    if (ra === undefined) return 1;   // 1.0.0 > 1.0.0-rc
    if (rb === undefined) return -1;
    const xa = ra.split('.'), xb = rb.split('.');
    for (let i = 0; i < Math.max(xa.length, xb.length); i++) {
        if (xa[i] === undefined) return -1;
        if (xb[i] === undefined) return 1;
        const na = /^\d+$/.test(xa[i]), nb = /^\d+$/.test(xb[i]);
        if (na && nb) { const d = Number(xa[i]) - Number(xb[i]); if (d) return Math.sign(d); continue; }
        if (na !== nb) return na ? -1 : 1;
        if (xa[i] !== xb[i]) return xa[i] < xb[i] ? -1 : 1;
    }
    return 0;
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const byteLength = (s: string) => new TextEncoder().encode(s).length;

/** A page entry is a relative path inside the extension directory. No
 * `%` at all, so no encoded `..` (`%2e%2e`) can survive URL normalization. */
function isEntryPath(v: unknown): v is string {
    if (typeof v !== 'string' || !v || v.length > 200) return false;
    if (v.startsWith('/') || /[\\%:?#]/.test(v)) return false;
    return v.split('/').every(seg => seg !== '' && seg !== '.' && seg !== '..');
}

/** R12: `core.events` items are `string | { name, subscribers? }`. Loose:
 * only warnings, because G6 never reads the core block (R4). */
function coreWarnings(core: unknown): string[] {
    if (!isObj(core) || core.events === undefined) return [];
    if (!Array.isArray(core.events)) return ['core.events 不是陣列'];
    const out: string[] = [];
    core.events.forEach((ev, i) => {
        if (typeof ev === 'string' && ev) return;
        if (isObj(ev) && typeof ev.name === 'string' && ev.name
            && (ev.subscribers === undefined || (Array.isArray(ev.subscribers) && ev.subscribers.every(s => typeof s === 'string')))) return;
        out.push(`core.events[${i}] 格式不對`);
    });
    return out;
}

/** Parses and checks one `c9ext.json`. `dir` is the directory name (R10). */
export function parseManifest(text: string | null, dir: string, size?: number): Parsed {
    const bad = (reason: string): Parsed => ({ ok: false, reason });
    if (text === null) return (size ?? 0) > MAX_MANIFEST_BYTES ? bad('manifest 超過 64 KiB') : bad('找不到 c9ext.json');
    if (byteLength(text) > MAX_MANIFEST_BYTES || (size ?? 0) > MAX_MANIFEST_BYTES) return bad('manifest 超過 64 KiB');
    let raw: unknown;
    // JSON.parse is strict JSON: comments and trailing commas fail.
    try { raw = JSON.parse(text); } catch { return bad('manifest 不是有效的 JSON'); }
    if (!isObj(raw)) return bad('manifest 不是 JSON 物件');
    // R1: a JSON number 1, nothing else ("1", 1.5, 2, missing).
    if (raw.c9ext !== MANIFEST_FORMAT) return bad('manifest 版本不支援');
    // R5, R10.
    if (typeof raw.id !== 'string' || !ID_RE.test(raw.id)) return bad('id 格式不對（小寫字母開頭，只能有小寫字母、數字、-，最多 32 字）');
    if (raw.id !== dir) return bad(`id「${raw.id}」和目錄名「${dir}」不一樣`);
    if (typeof raw.name !== 'string' || !raw.name.trim() || raw.name.length > MAX_NAME) return bad(`name 必須是 1–${MAX_NAME} 字的字串`);
    // R11.
    if (!isSemver(raw.version)) return bad('version 不是有效的 semver');
    if (raw.dev !== undefined && typeof raw.dev !== 'boolean') return bad('dev 必須是 true 或 false');

    // R6: only app and apiMinor are compared; core (and anything else) is ignored.
    let minVersion: Manifest['minVersion'];
    if (raw.minVersion !== undefined) {
        const mv = raw.minVersion;
        if (!isObj(mv)) return bad('minVersion 必須是物件');
        if (mv.app !== undefined && !isSemver(mv.app)) return bad('minVersion.app 不是有效的 semver');
        if (mv.apiMinor !== undefined && !(Number.isInteger(mv.apiMinor) && (mv.apiMinor as number) >= 0)) return bad('minVersion.apiMinor 必須是 0 以上的整數');
        minVersion = {};
        if (mv.app !== undefined) minVersion.app = mv.app as string;
        if (mv.apiMinor !== undefined) minVersion.apiMinor = mv.apiMinor as number;
    }

    // R2, R3, R11.
    let ui: Manifest['ui'];
    if (raw.ui !== undefined) {
        const u = raw.ui;
        if (!isObj(u)) return bad('ui 必須是物件');
        if (u.contract !== UI_CONTRACT) return bad(`UI 合約不支援（需要 "${UI_CONTRACT}"）`);
        if (!Array.isArray(u.pages) || u.pages.length === 0) return bad('ui.pages 至少要有 1 頁');
        if (u.pages.length > MAX_PAGES) return bad(`ui.pages 最多 ${MAX_PAGES} 頁`);
        const pages: ExtPage[] = [];
        const seen = new Set<string>();
        for (const [i, p] of u.pages.entries()) {
            if (!isObj(p)) return bad(`ui.pages[${i}] 必須是物件`);
            if (typeof p.id !== 'string' || !ID_RE.test(p.id)) return bad(`ui.pages[${i}].id 格式不對`);
            if (seen.has(p.id)) return bad(`ui.pages 有兩個「${p.id}」`);
            seen.add(p.id);
            if (typeof p.title !== 'string' || !p.title.trim() || p.title.length > 40) return bad(`ui.pages[${i}].title 必須是 1–40 字的字串`);
            if (!isEntryPath(p.entry)) return bad(`ui.pages[${i}].entry 必須是目錄內的相對路徑`);
            if (p.icon !== undefined && (typeof p.icon !== 'string' || p.icon.length > 40)) return bad(`ui.pages[${i}].icon 必須是字串`);
            pages.push(p.icon === undefined ? { id: p.id, title: p.title, entry: p.entry } : { id: p.id, title: p.title, entry: p.entry, icon: p.icon });
        }
        ui = { contract: UI_CONTRACT, pages };
    }

    // R4: `core` is never read beyond the loose R12 check. Top-level
    // `contract` (R2) and every unknown field are ignored.
    const hasCore = raw.core !== undefined;
    const manifest: Manifest = { c9ext: 1, id: raw.id, name: raw.name, version: raw.version as string, dev: raw.dev === true };
    if (minVersion) manifest.minVersion = minVersion;
    if (ui) manifest.ui = ui;
    return { ok: true, manifest, hasCore, warnings: hasCore ? coreWarnings(raw.core) : [] };
}

/** `version_mismatch` reason, or null when this host is new enough (R6). */
export function versionProblem(m: Manifest, env: HostEnv): string | null {
    const mv = m.minVersion;
    if (!mv) return null;
    if (mv.app !== undefined && isSemver(env.app) && compareSemver(env.app, mv.app) < 0) return `需要較新的 c9watch（${mv.app} 以上）`;
    if (mv.apiMinor !== undefined && env.apiMinor < mv.apiMinor) return `需要較新的 c9watch（c9p 1.${mv.apiMinor} 以上）`;
    return null;
}

/** Validates every candidate and applies R9 (first-party wins a shared id).
 * Order: first-party, then third-party, each by directory name. `base` is
 * the URL prefix for candidates that do not name their own. */
export function resolveExtensions(candidates: ExtCandidate[], env: HostEnv, base = '/c9ext/'): ExtEntry[] {
    const sorted = [...candidates].sort((a, b) => (a.source === b.source ? (a.dir < b.dir ? -1 : a.dir > b.dir ? 1 : 0) : a.source === 'first' ? -1 : 1));
    const firstDirs = new Set(sorted.filter(c => c.source === 'first').map(c => c.dir));
    const seen = new Set<string>();
    const out: ExtEntry[] = [];
    for (const c of sorted) {
        // Two lists can name the same directory twice; keep the first.
        const key = `${c.source}/${c.dir}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const entryBase = c.base ?? base;
        const p = parseManifest(c.manifest, c.dir, c.size);
        if (!p.ok) {
            out.push({ dir: c.dir, source: c.source, status: 'manifest_error', reason: p.reason, hasUi: false, hasCore: false, warnings: [], base: entryBase });
            continue;
        }
        const e: ExtEntry = { dir: c.dir, source: c.source, status: 'ok', manifest: p.manifest, hasUi: !!p.manifest.ui, hasCore: p.hasCore, warnings: p.warnings, base: entryBase };
        const tooOld = versionProblem(p.manifest, env);
        if (c.source === 'third' && firstDirs.has(c.dir)) { e.status = 'shadowed'; e.reason = '被內建的同名 extension 蓋過'; }
        else if (p.manifest.dev && !env.dev) { e.status = 'dev_only'; e.reason = '只在開發版顯示'; }
        else if (tooOld) { e.status = 'version_mismatch'; e.reason = tooOld; }
        out.push(e);
    }
    return out;
}

/** The page entries the side bar shows: `ok` extensions with a UI, first-party first. */
export function sidebarPages(entries: ExtEntry[]): { extId: string; page: ExtPage; source: ExtSource }[] {
    return entries.flatMap(e => (e.status === 'ok' && e.manifest?.ui ? e.manifest.ui.pages.map(page => ({ extId: e.dir, page, source: e.source })) : []));
}

/** One line for the settings list (G10 shows it). */
export function describeEntry(e: ExtEntry): string {
    if (e.status !== 'ok') return e.reason ?? e.status;
    if (!e.hasUi) return '只有 core 部分';
    return e.hasCore ? '含 core 部分' : '可以使用';
}

/** URL of a page entry. Ids and paths were checked by `parseManifest`;
 * each segment is still encoded, so no segment can mean `..` or `/`. */
export function entryUrl(e: ExtEntry, page: ExtPage): string {
    return `${e.base}${encodeURIComponent(e.dir)}/${page.entry.split('/').map(encodeURIComponent).join('/')}`;
}
