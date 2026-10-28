/**
 * Layout logic for the dense monitor view. No imports, so scripts/test-*.mjs
 * can transpile and test it directly.
 */

interface SessionLike {
	id: string;
	startedAtMs?: number | null;
}

export interface ProjectGroupLike<S extends SessionLike> {
	path: string;
	displayName: string;
	attention: S[];
	idle: S[];
	working: S[];
}

export interface DenseTile<S extends SessionLike> {
	path: string;
	displayName: string;
	/** Needs attention first, then idle, then working. */
	sessions: S[];
	/** 0 = something needs attention, 1 = something is idle, 2 = all working. */
	tier: 0 | 1 | 2;
}

/** Oldest session first, so rows keep their place while a session works. */
function byStart<S extends SessionLike>(a: S, b: S): number {
	const aStart = a.startedAtMs ?? Number.POSITIVE_INFINITY;
	const bStart = b.startedAtMs ?? Number.POSITIVE_INFINITY;
	if (aStart !== bStart) return aStart < bStart ? -1 : 1;
	return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/**
 * One tile per project. Tiles move only when a project changes tier, never
 * because a session wrote to its transcript, so the board stays put while
 * agents work and you can find a project where you last saw it.
 */
export function denseTiles<S extends SessionLike>(groups: ProjectGroupLike<S>[]): DenseTile<S>[] {
	return groups
		.map((group) => ({
			path: group.path,
			displayName: group.displayName,
			sessions: [
				...[...group.attention].sort(byStart),
				...[...group.idle].sort(byStart),
				...[...group.working].sort(byStart)
			],
			tier: (group.attention.length > 0 ? 0 : group.idle.length > 0 ? 1 : 2) as 0 | 1 | 2
		}))
		.sort(
			(a, b) =>
				a.tier - b.tier ||
				a.displayName.localeCompare(b.displayName) ||
				a.path.localeCompare(b.path)
		);
}

/** How many columns at least `minWidth` wide fit in `width`, between 1 and `max`. */
export function columnCount(width: number, minWidth: number, gap: number, max: number): number {
	if (width <= 0 || max <= 1) return 1;
	const fit = Math.floor((width + gap) / (minWidth + gap));
	return Math.max(1, Math.min(fit, max));
}

/**
 * Masonry: each item goes to the shortest column so far, ties to the leftmost.
 * The first `count` items therefore read left to right across the top, and the
 * columns end at about the same height.
 */
export function packColumns<T>(items: T[], count: number, height: (item: T) => number): T[][] {
	const columns: T[][] = Array.from({ length: Math.max(1, count) }, () => []);
	const heights = columns.map(() => 0);
	for (const item of items) {
		let shortest = 0;
		for (let i = 1; i < heights.length; i++) {
			if (heights[i] < heights[shortest]) shortest = i;
		}
		columns[shortest].push(item);
		heights[shortest] += height(item);
	}
	return columns;
}
