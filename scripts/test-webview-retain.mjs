// IPC channels and list reorders in the desktop webview.
// The bridge test uses the real `@tauri-apps/api` Channel. Rust sends an `end`
// frame when it drops a channel (tauri ipc/channel.rs), and the Channel then
// unregisters its own callback. A progress frame can arrive after the command
// result, so the bridge must not unregister a request channel when it settles.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import ts from 'typescript';

const callbacks = new Map();
let nextId = 1;
let handler = async () => { throw new Error('no handler'); };
globalThis.window = {
	__TAURI_INTERNALS__: {
		transformCallback(fn, once = false) {
			const id = nextId++;
			callbacks.set(id, (data) => { if (once) callbacks.delete(id); return fn?.(data); });
			return id;
		},
		unregisterCallback(id) { callbacks.delete(id); },
		invoke: (cmd, args, options) => handler(cmd, args, options)
	}
};

// What Rust's `Drop for Channel` sends: one `end` frame after the last message.
function rustDrop(channel, sent = 0) {
	callbacks.get(channel.id)?.({ end: true, index: sent });
}
function rustSend(channel, index, message) {
	callbacks.get(channel.id)?.({ index, message });
}

const esm = (path) => ts.transpileModule(readFileSync(new URL(path, import.meta.url), 'utf8'), {
	compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 }
}).outputText;
const dataUrl = (code) => 'data:text/javascript;base64,' + Buffer.from(code).toString('base64');
const transportUrl = dataUrl(esm('../src/lib/data/transport.ts'));
const bridge = await import(dataUrl(esm('../src/lib/data/tauri-bridge.ts')
	.replaceAll("'@tauri-apps/api/core'", JSON.stringify(import.meta.resolve('@tauri-apps/api/core')))
	.replaceAll("'@tauri-apps/api/event'", JSON.stringify(import.meta.resolve('@tauri-apps/api/event')))
	.replaceAll("'./transport'", JSON.stringify(transportUrl))));
const { tauriTransport } = bridge;

test('a progress frame that arrives after the result still reaches onProgress', async () => {
	callbacks.clear();
	let channel;
	handler = async (cmd, args) => {
		assert.equal(cmd, 'c9p_request');
		channel = args.progress;
		// The command result reaches the webview before the frames it sent.
		setTimeout(() => { rustSend(channel, 0, { type: 'progress', bytesRead: 5, bytesTotal: 10 }); rustDrop(channel, 1); }, 0);
		return { messages: [] };
	};
	const frames = [];
	await tauriTransport.request('conversation.get', { sessionKey: 'claudeCode:x' }, { onProgress: (f) => frames.push(f) });
	await new Promise((r) => setTimeout(r, 5));
	assert.deepEqual(frames.map((f) => f.bytesRead), [5]);
	assert.equal(callbacks.size, 0, 'the end frame released the channel');
});

test('six days of fallback retries leave no IPC callbacks', async () => {
	callbacks.clear();
	// Visible main window, core unavailable: session.views and two subscribe
	// probes every 5 s. Rust drops each channel when the command returns.
	const TICKS = (6 * 24 * 60 * 60) / 5;
	handler = async (cmd, args) => {
		if (cmd === 'c9p_request' || cmd === 'c9p_subscribe') {
			rustDrop(args.progress ?? args.channel);
			throw { code: 'core_unavailable', msg: 'Core is not usable' };
		}
		if (cmd === 'get_sessions') return [];
		throw new Error(`unexpected command ${cmd}`);
	};
	for (let tick = 0; tick < TICKS; tick++) {
		await assert.rejects(tauriTransport.subscribe({ topics: ['session.view'] }, () => {}), (e) => e.code === 'core_unavailable');
		await assert.rejects(tauriTransport.subscribe({ topics: ['diag.detection'] }, () => {}), (e) => e.code === 'core_unavailable');
		assert.deepEqual((await tauriTransport.request('session.views', {})).sessions, []);
	}
	assert.equal(callbacks.size, 0);
});

test('an unsubscribed stream delivers no more frames and is released on drop', async () => {
	callbacks.clear();
	let channel;
	handler = async (cmd, args) => {
		if (cmd === 'c9p_subscribe') { channel = args.channel; return 'sub-1'; }
		if (cmd === 'c9p_unsubscribe') { rustDrop(channel, 1); return null; }
		throw new Error(`unexpected command ${cmd}`);
	};
	const seen = [];
	const stop = await tauriTransport.subscribe({ topics: ['session.view'] }, (f) => seen.push(f.n));
	rustSend(channel, 0, { n: 1 });
	stop();
	await new Promise((r) => setTimeout(r, 0));
	rustSend(channel, 1, { n: 2 });
	assert.deepEqual(seen, [1]);
	assert.equal(callbacks.size, 0);
});
