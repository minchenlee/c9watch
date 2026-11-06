// Dev-only extension file server for the glass shell (glass build plan
// §3.2, G6). `vite dev` only: `configureServer` does not run in `vite build`
// or `vite preview`, so a production build never serves these files.
//
//   GET /c9ext/_list            → { extensions: ExtCandidate[] }  (see src/shells/glass/ext/manifest.ts)
//   GET /c9ext/<id>/<path...>   → a file from the first root that has <id>
//
// Roots, in order (R9: the first-party root wins a shared id):
//   first: <repo>/extensions/
//   third: $C9WATCH_ENGINE_HOME/extensions/ (default on macOS:
//          ~/Library/Application Support/c9watch/engine/extensions/)
// The host validates the manifests; this server only reads files.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const MAX_MANIFEST_BYTES = 64 * 1024;
const DIR_RE = /^[a-z][a-z0-9-]{0,31}$/;
/** @type {Record<string, string>} */
const TYPES = {
    '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.json': 'application/json',
    '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg', '.webp': 'image/webp',
    '.woff2': 'font/woff2', '.txt': 'text/plain; charset=utf-8', '.html': 'text/plain; charset=utf-8'
};

/** @typedef {{ source: 'first' | 'third', dir: string }} Root */

/** The two roots. `engineHome` null skips the third-party root.
 * @param {string} repoRoot @param {string | null} [engineHome] @returns {Root[]} */
export function extensionRoots(repoRoot, engineHome = defaultEngineHome()) {
    /** @type {Root[]} */
    const roots = [{ source: 'first', dir: path.join(repoRoot, 'extensions') }];
    if (engineHome) roots.push({ source: 'third', dir: path.join(engineHome, 'extensions') });
    return roots;
}

function defaultEngineHome() {
    if (process.env.C9WATCH_ENGINE_HOME) return process.env.C9WATCH_ENGINE_HOME;
    if (process.platform === 'darwin') return path.join(os.homedir(), 'Library', 'Application Support', 'c9watch', 'engine');
    return null;
}

/** `child` is strictly inside `parent` (both real paths). */
const inside = (/** @type {string} */ parent, /** @type {string} */ child) => child.startsWith(parent + path.sep);

/** The real path of `<root>/<id>`, or null when it is missing, not a
 * directory, or (through a symlink) not inside the root's real path. Such a
 * directory counts as absent: it neither lists nor shadows.
 * @param {Root} root @param {string} id @returns {string | null} */
function realExtDir(root, id) {
    try {
        const realRoot = fs.realpathSync(root.dir);
        const dir = fs.realpathSync(path.join(root.dir, id));
        return inside(realRoot, dir) && fs.statSync(dir).isDirectory() ? dir : null;
    } catch { return null; }
}

/** The real path of `<extDir>/<segs...>` when it is a file inside `extDir`.
 * @param {string} extDir @param {string[]} segs @returns {string | null} */
function realFileIn(extDir, segs) {
    try {
        const real = fs.realpathSync(path.join(extDir, ...segs));
        return inside(extDir, real) && fs.statSync(real).isFile() ? real : null;
    } catch { return null; }
}

/** Every extension directory in every root, with its raw manifest text.
 * Every path is checked by its real path (symlinks cannot leave the root).
 * @param {Root[]} roots */
export function listExtensions(roots) {
    /** @type {{ dir: string, source: Root['source'], manifest: string | null, size: number }[]} */
    const out = [];
    for (const root of roots) {
        /** @type {string[]} */
        let names = [];
        try { names = fs.readdirSync(root.dir, { withFileTypes: true }).filter(d => d.isDirectory() || d.isSymbolicLink()).map(d => d.name); } catch { continue; }
        for (const dir of names.sort()) {
            if (dir.startsWith('.')) continue;
            const extDir = realExtDir(root, dir);
            if (!extDir) continue;
            const file = realFileIn(extDir, ['c9ext.json']);
            let size = 0;
            /** @type {string | null} */
            let manifest = null;
            if (file) {
                try {
                    size = fs.statSync(file).size;
                    if (size <= MAX_MANIFEST_BYTES) manifest = fs.readFileSync(file, 'utf8');
                } catch { /* unreadable: the host reports a missing manifest */ }
            }
            out.push({ dir, source: root.source, manifest, size });
        }
    }
    return out;
}

/** The real path of `<root>/<id>/<rel>` in the first root that has `<id>`,
 * or null. Refuses ids the manifest rules reject, `..`, `%`, dot files, and
 * any path (symlinks too) that leaves the root or the extension directory.
 * `rel` is already URL-decoded.
 * @param {Root[]} roots @param {string} id @param {string} rel @returns {string | null} */
export function resolveExtFile(roots, id, rel) {
    if (!DIR_RE.test(id)) return null;
    const segs = rel.split('/');
    if (!rel || segs.some(s => !s || s === '.' || s === '..' || s.startsWith('.') || s.includes('\\') || s.includes('%') || s.includes('\0'))) return null;
    for (const root of roots) {
        const extDir = realExtDir(root, id);
        // R9: the first root that has the id owns it, file or not.
        if (extDir) return realFileIn(extDir, segs);
    }
    return null;
}

/** The Vite plugin. `repoRoot` is the folder that holds `extensions/`.
 * @param {{ repoRoot?: string, engineHome?: string | null }} [opts]
 * @returns {import('vite').Plugin} */
export function c9extDev({ repoRoot, engineHome } = {}) {
    return {
        name: 'c9ext-dev',
        apply: 'serve',
        configureServer(server) {
            const roots = () => extensionRoots(repoRoot ?? server.config.root, engineHome);
            server.middlewares.use('/c9ext', (req, res, next) => {
                if (req.method !== 'GET' && req.method !== 'HEAD') return next();
                let pathname;
                try { pathname = decodeURIComponent(new URL(req.url ?? '/', 'http://x').pathname); } catch { res.statusCode = 400; return res.end(); }
                res.setHeader('Cache-Control', 'no-store');
                if (pathname === '/_list') {
                    res.setHeader('Content-Type', 'application/json');
                    return res.end(JSON.stringify({ extensions: listExtensions(roots()) }));
                }
                const m = /^\/([^/]+)\/(.+)$/.exec(pathname);
                const file = m ? resolveExtFile(roots(), m[1], m[2]) : null;
                if (!file) { res.statusCode = 404; return res.end('not found'); }
                res.setHeader('Content-Type', TYPES[path.extname(file).toLowerCase()] ?? 'application/octet-stream');
                res.setHeader('X-Content-Type-Options', 'nosniff');
                if (req.method === 'HEAD') return res.end();
                fs.createReadStream(file).on('error', () => { res.statusCode = 500; res.end(); }).pipe(res);
            });
        }
    };
}
