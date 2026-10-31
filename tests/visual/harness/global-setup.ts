/**
 * Bundles the IPC mock (and its fixtures) into one IIFE file that
 * Playwright injects with `page.addInitScript`. Runs once per test run.
 */
import { build } from 'vite';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
export const INIT_BUNDLE = path.resolve(here, '../.cache/ipc-mock.js');

export default async function globalSetup() {
	await build({
		configFile: false,
		logLevel: 'warn',
		build: {
			outDir: path.dirname(INIT_BUNDLE),
			emptyOutDir: true,
			minify: false,
			lib: {
				entry: path.resolve(here, 'ipc-mock.ts'),
				formats: ['iife'],
				name: 'c9VisualIpcMock',
				fileName: () => path.basename(INIT_BUNDLE)
			}
		}
	});
}
