/**
 * Shared steps for the Classic visual tests: fixed clock, IPC mock,
 * no motion, loaded fonts, and a check that every command had a fixture.
 */
import { expect, type Page } from '@playwright/test';
import { FIXED_NOW } from '../classic/fixtures';
import { INIT_BUNDLE } from './global-setup';
import type { VisualOptions } from './ipc-mock';

/** Injected only in tests. Stops CSS motion and the text caret. */
const NO_MOTION_CSS = `
*, *::before, *::after {
	animation: none !important;
	transition: none !important;
	caret-color: transparent !important;
	scroll-behavior: auto !important;
}`;

export interface OpenOptions extends VisualOptions {
	/** Route to load. Default `/`. */
	path?: string;
	/** localStorage entries set before the app starts. */
	storage?: Record<string, string>;
	/** False: no Tauri mock, the page runs as the web (remote) client. */
	tauri?: boolean;
}

export async function openClassic(page: Page, opts: OpenOptions = {}): Promise<void> {
	const { path = '/', storage = {}, tauri = true, ...visual } = opts;

	// Fixed clock. Time stays paused, so no app timer fires unless a test
	// calls `advance`. This also freezes the demo-mode status simulation.
	// Install a little earlier: real time passes between the two calls, and
	// pauseAt cannot move the clock back.
	await page.clock.install({ time: FIXED_NOW - 10_000 });
	await page.clock.pauseAt(FIXED_NOW);

	await page.addInitScript(
		({ visual, storage, css }) => {
			window.__C9_VISUAL__ = visual;
			try {
				localStorage.clear();
				for (const [k, v] of Object.entries(storage)) localStorage.setItem(k, v);
			} catch {
				/* storage unavailable */
			}
			const addStyle = () => {
				const style = document.createElement('style');
				style.setAttribute('data-c9-visual', '');
				style.textContent = css;
				document.head.appendChild(style);
			};
			if (document.head) addStyle();
			else document.addEventListener('DOMContentLoaded', addStyle, { once: true });
		},
		{ visual, storage, css: NO_MOTION_CSS }
	);
	if (tauri) await page.addInitScript({ path: INIT_BUNDLE });

	await page.goto(path, { waitUntil: 'load' });
	await settle(page);
}

/** Runs paused app timers for `ms` of page time, then waits for a still page. */
export async function advance(page: Page, ms: number): Promise<void> {
	await page.clock.runFor(ms);
	await settle(page);
}

/** Waits for network, every bundled font, and short app timers. */
export async function settle(page: Page): Promise<void> {
	await page.waitForLoadState('networkidle');
	await page.evaluate(async () => {
		await Promise.all([...document.fonts].map((f) => f.load().catch(() => undefined)));
		await document.fonts.ready;
	});
	// Let pending promise chains (IPC answers, Svelte effects) finish.
	// Short timers (setTimeout 0, debounces) need a little page time too.
	// The clock is paused (it also fakes requestAnimationFrame), so only
	// runFor moves those timers. Each round trip also drains microtasks.
	for (let i = 0; i < 4; i++) await page.clock.runFor(50);
	await page.waitForLoadState('networkidle');
}

/** Fails when the app called a command that has no fixture. */
export async function expectNoUnhandledCommands(page: Page): Promise<void> {
	const unhandled = await page.evaluate(() => window.__C9_VISUAL_UNHANDLED__ ?? []);
	expect(unhandled, 'Tauri commands without a fixture').toEqual([]);
}

/** Screenshot of the viewport, 0 changed pixels allowed. */
export async function snap(page: Page, name: string): Promise<void> {
	await expectNoUnhandledCommands(page);
	// No hover state from the last click.
	await page.mouse.move(0, 0);
	await settle(page);
	await expect(page).toHaveScreenshot(`${name}.png`);
}

const TAB_KEYS = { MONITOR: '1', HISTORY: '2', COST: '3', MEMORY: '4', SETTINGS: '5' } as const;

/**
 * Opens a main-window tab with the app's own Cmd+1..5 shortcut. The tab bar
 * hides its buttons in a narrow window, so a click does not always work.
 */
export async function openTab(page: Page, tab: keyof typeof TAB_KEYS): Promise<void> {
	await page.keyboard.press(`ControlOrMeta+${TAB_KEYS[tab]}`);
	await settle(page);
}
