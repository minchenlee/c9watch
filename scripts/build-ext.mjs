// `npm run build:ext` (glass build plan §3.2, G7): builds each first-party
// extension that has a source entry, `extensions/<id>/src/index.{ts,js}`,
// into one ES module, `extensions/<id>/dist/index.js` (Vite library mode).
// An extension without `src/` ships its files as they are (hello does).
// The app bundle takes all of `extensions/` (tauri.conf.json `resources`).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';

// C9EXT_ROOT overrides the folder (for a dry run outside the repo).
const root = process.env.C9EXT_ROOT ?? path.join(fileURLToPath(new URL('..', import.meta.url)), 'extensions');
const ids = fs.existsSync(root) ? fs.readdirSync(root).filter(d => !d.startsWith('.')).sort() : [];
let built = 0;
for (const id of ids) {
    const entry = ['ts', 'js'].map(e => path.join(root, id, 'src', `index.${e}`)).find(f => fs.existsSync(f));
    if (!entry) continue;
    await build({
        configFile: false,
        logLevel: 'warn',
        build: {
            outDir: path.join(root, id, 'dist'),
            emptyOutDir: true,
            lib: { entry, formats: ['es'], fileName: () => 'index.js' }
        }
    });
    built++;
    console.log(`build:ext ${id} → extensions/${id}/dist/index.js`);
}
if (!built) console.log('build:ext: no extension has src/index.{ts,js}; nothing to build');
