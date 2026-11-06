import { test, expect, type Page } from '@playwright/test';
import { fileURLToPath } from 'node:url';

// G10a 設定 › Extensions on `vite dev` (port 4181): the list the registry
// reads from the dev endpoint (repo `extensions/` first, then the fixture
// engine home), read-only. Runs on the glass demo fixture.
const STORAGE = { 'ui.shell': 'glass', demoMode: 'true', 'c9watch.demoGlass': '1' };
const out = (name: string) => fileURLToPath(new URL(`./.cache/${name}.png`, import.meta.url));

async function open(page: Page, hash: string) {
	await page.addInitScript((storage) => {
		if (sessionStorage.getItem('c9-test-seeded')) return;
		sessionStorage.setItem('c9-test-seeded', '1');
		localStorage.clear();
		for (const [k, v] of Object.entries(storage)) localStorage.setItem(k, v);
	}, STORAGE);
	await page.goto(`/${hash}`);
	await expect(page.locator('.glass-root')).toBeVisible();
}

test.describe('設定 › Extensions', () => {
	test('lists every extension with name, version, source, status and directory', async ({ page }) => {
		await page.setViewportSize({ width: 1100, height: 1200 });
		await open(page, '#/settings');
		const table = page.locator('[data-table="extensions"]');
		await expect(table).toBeVisible();
		const row = (dir: string, source: string) => table.locator(`tr[data-ext-row="${dir}"][data-source="${source}"]`);
		// First-party hello (dev build, so dev: true is listed as ok).
		const hello = row('hello', 'first');
		await expect(hello).toHaveAttribute('data-status', 'ok');
		await expect(hello.locator('td').nth(2)).toHaveText('第一方');
		await expect(hello.locator('td').nth(4)).toHaveText('app 內建 › extensions/hello/');
		await expect(hello.locator('td').nth(1)).toHaveText(/^\d+\.\d+\.\d+/);
		// Third-party fixtures: a shadowed copy, a bad manifest, a too-new one.
		await expect(row('hello', 'third')).toHaveAttribute('data-status', 'shadowed');
		await expect(row('hello', 'third').locator('td').nth(2)).toHaveText('第三方');
		await expect(row('bad-format', 'third')).toHaveAttribute('data-status', 'manifest_error');
		await expect(row('bad-format', 'third').locator('td').nth(1)).toHaveText('—');
		await expect(row('later', 'third')).toHaveAttribute('data-status', 'version_mismatch');
		await expect(row('later', 'third').locator('.st')).toHaveText('版本不合');
		await expect(row('throws', 'third').locator('td').nth(4)).toHaveText('engine home › extensions/throws/');
		// Read-only: no buttons or inputs in the list.
		await expect(table.locator('button, input')).toHaveCount(0);
		const rows = await table.locator('tbody tr').count();
		expect(rows).toBeGreaterThanOrEqual(5);
		await expect(page.locator('#s-ext')).toHaveText(new RegExp(`Extensions\\s*${rows}`));
		await page.mouse.move(0, 0);
		await page.locator('[data-table="extensions"]').screenshot({ path: out('g10-settings-extensions') });
	});
});
