import { test } from '@playwright/test';
import { openClassic, snap } from '../harness/classic';

// Without Tauri, Classic runs as the web (remote) client and asks to pair.
test('web client connection screen', async ({ page }) => {
	await openClassic(page, { tauri: false });
	await snap(page, 'web-connection');
});
