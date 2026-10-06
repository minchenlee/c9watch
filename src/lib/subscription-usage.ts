export interface UsageWindow {
	label: string;
	usedPercent: number;
	resetsAt: number | null;
	/** Full window length; absent from older desktop builds and for windows with no fixed length. */
	windowSeconds?: number | null;
}
export interface SubscriptionUsage {
	provider: string;
	name: string;
	plan: string | null;
	windows: UsageWindow[];
	updatedAt: number | null;
	message: string | null;
}
export function demoSubscriptionUsage(): SubscriptionUsage[] {
	const now = Math.floor(Date.now() / 1000);
	return [
		{ provider: 'claudeCode', name: 'Claude Code', plan: 'Max', updatedAt: now, message: null, windows: [
			{ label: '5-hour', usedPercent: 23.5, resetsAt: now + 7200, windowSeconds: 18000 },
			{ label: 'Weekly', usedPercent: 67, resetsAt: now + 259200, windowSeconds: 604800 }
		] },
		{ provider: 'codex', name: 'Codex', plan: 'Pro', updatedAt: now, message: null, windows: [
			{ label: '5-hour', usedPercent: 38, resetsAt: now + 7200, windowSeconds: 18000 },
			{ label: 'Weekly', usedPercent: 62, resetsAt: now + 259200, windowSeconds: 604800 }
		] },
		{ provider: 'cursor', name: 'Cursor', plan: 'Pro', updatedAt: now, message: null, windows: [
			{ label: 'Included usage', usedPercent: 62, resetsAt: now + 604800, windowSeconds: 2592000 },
			{ label: 'Auto', usedPercent: 84, resetsAt: now + 604800, windowSeconds: 2592000 },
			{ label: 'API', usedPercent: 31, resetsAt: now + 604800, windowSeconds: 2592000 },
			{ label: 'On-demand spend', usedPercent: 0, resetsAt: now + 604800, windowSeconds: 2592000 }
		] }
	];
}
