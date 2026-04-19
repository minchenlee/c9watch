export function formatTimeSince(isoTimestamp: string): string {
	const now = Date.now();
	const then = new Date(isoTimestamp).getTime();
	const diffMins = Math.floor((now - then) / 60000);
	const diffHours = Math.floor((now - then) / 3600000);
	const diffDays = Math.floor((now - then) / 86400000);
	if (diffMins < 1) return 'now';
	if (diffMins < 60) return `${diffMins}m`;
	if (diffHours < 24) return `${diffHours}h`;
	return `${diffDays}d`;
}

export function formatDurationMs(ms: number | null | undefined): string {
	if (ms == null) return '';
	if (ms < 1000) return `${ms}ms`;
	const s = ms / 1000;
	if (s < 60) return `${s.toFixed(1)}s`;
	const m = Math.floor(s / 60);
	const rs = Math.round(s - m * 60);
	return `${m}m ${rs}s`;
}
