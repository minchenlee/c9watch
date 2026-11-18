// A visible desktop window whose core is down retries the session feed and the
// diagnostics feed every 5s. Each attempt registers a Tauri Channel callback.
// Those callbacks used to live until an `end` frame that never arrives.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { test } from 'node:test';

const FALLBACK_RETRY_MS = 5000;
const SIX_DAYS_S = 6 * 24 * 60 * 60;
const TICKS = Math.floor(SIX_DAYS_S / (FALLBACK_RETRY_MS / 1000));
// Visible main window, core unavailable: session.views, the session subscribe
// probe, and the diagnostics subscribe. A hidden popover does not retry.
const CALLBACKS_PER_TICK = 3;

function installCallbacks() {
	const callbacks = new Map();
	let next = 1;
	const window = {
		__TAURI_INTERNALS__: {
			transformCallback(fn) {
				const id = next++;
				callbacks.set(id, fn);
				return id;
			},
			unregisterCallback(id) { callbacks.delete(id); }
		}
	};
	return { callbacks, window };
}

function loadBridge(callbacks, window, invoke) {
	const exports = {};
	const source = readFileSync(new URL('../src/lib/data/tauri-bridge.ts', import.meta.url), 'utf8');
	const transport = {};
	vm.runInNewContext(ts.transpileModule(source, {
		compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS }
	}).outputText, {
		exports,
		window,
		require(name) {
			if (name === '@tauri-apps/api/core') return { invoke, Channel: class Channel {
				constructor(onmessage) {
					this.id = window.__TAURI_INTERNALS__.transformCallback(onmessage ?? (() => {}));
					this.onmessage = onmessage ?? (() => {});
				}
			} };
			if (name === '@tauri-apps/api/event') return {};
			if (name === './transport') return transport;
			throw new Error(`unexpected import ${name}`);
		}
	});
	return exports;
}

test('an unreleased fallback retry retains one callback per IPC call', () => {
	const { callbacks, window } = installCallbacks();
	const sample = 4000;
	const before = process.memoryUsage().heapUsed;
	const retained = [];
	for (let tick = 0; tick < sample; tick++) {
		for (let n = 0; n < CALLBACKS_PER_TICK; n++) {
			const channel = { pending: [] };
			channel.id = window.__TAURI_INTERNALS__.transformCallback((raw) => {
				channel.pending[raw?.index] = raw?.message;
			});
			retained.push(channel);
		}
	}
	assert.equal(callbacks.size, sample * CALLBACKS_PER_TICK);
	const perCallback = (process.memoryUsage().heapUsed - before) / callbacks.size;
	const sixDayCount = TICKS * CALLBACKS_PER_TICK;
	assert.equal(TICKS, 103680);
	assert.equal(sixDayCount, 311040);
	console.log(`unreleased: ${callbacks.size} callbacks in ${sample} ticks; six days retains ${sixDayCount}; ${perCallback.toFixed(0)} bytes/callback (${(perCallback * sixDayCount / 1024 / 1024).toFixed(0)} MiB extrapolated in this engine)`);
	assert.equal(retained.length, callbacks.size);
});

test('six days of fallback refreshes leave no IPC callbacks', async () => {
	const { callbacks, window } = installCallbacks();
	let subscribes = 0;
	let requests = 0;
	const bridge = loadBridge(callbacks, window, async (cmd) => {
		if (cmd === 'c9p_subscribe') { subscribes++; throw { code: 'core_unavailable', msg: 'Core is not usable' }; }
		if (cmd === 'c9p_request') { requests++; throw { code: 'core_unavailable', msg: 'The GUI is using its in-process fallback' }; }
		if (cmd === 'get_sessions') return [];
		if (cmd === 'get_detection_diagnostics') return { hiddenClaudeSessions: [] };
		throw new Error(`unexpected command ${cmd}`);
	});
	for (let tick = 0; tick < TICKS; tick++) {
		await assert.rejects(
			bridge.tauriTransport.subscribe({ topics: ['session.view', 'session.gone'] }, () => {}),
			(error) => error.code === 'core_unavailable'
		);
		const sessions = await bridge.tauriTransport.request('session.views', {});
		assert.deepEqual(sessions.sessions, []);
		assert.equal(sessions.cursor, null);
		await assert.rejects(
			bridge.tauriTransport.subscribe({ topics: ['diag.detection'] }, () => {}),
			(error) => error.code === 'core_unavailable'
		);
	}
	assert.equal(subscribes, TICKS * 2);
	assert.equal(requests, TICKS);
	assert.equal(callbacks.size, 0);
});

test('a native reorder does not start a Web Animations timeline', () => {
	const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');
	const motion = read('../src/lib/transitions.ts');
	assert.match(motion, /if \(skipMotion\(node\)\) return \{ duration: 0, delay: 0 \};/);
	assert.match(read('../src/routes/(app)/+page.svelte'), /animate:listFlip/);
	assert.match(read('../src/lib/components/DenseBoard.svelte'), /animate:listFlip/);
	assert.doesNotMatch(read('../src/routes/(app)/+page.svelte'), /animate:flip/);
	const animate = read('../node_modules/svelte/src/internal/client/dom/elements/transitions.js');
	assert.match(animate, /if \(!options\?\.duration && !options\?\.delay\)/);
	assert.match(animate, /element\.animate\(keyframes, \{ duration: delay, fill: 'forwards' \}\)/);
});
