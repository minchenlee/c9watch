// Build a shallow outline of a parsed workflow result for the RESULT panel's
// inline overview. One entry per top-level field (object) or element (array);
// no deep recursion — the full view handles depth.

export interface OutlineEntry {
	/** key name (objects) or index label like "[0]" (arrays) */
	key: string;
	/** 'string' | 'number' | 'boolean' | 'null' | 'array' | 'object' */
	kind: string;
	/** short human hint: a value preview, a count, etc. */
	hint: string;
}

const PREVIEW_MAX = 80;

function kindOf(v: unknown): string {
	if (v === null) return 'null';
	if (Array.isArray(v)) return 'array';
	return typeof v;
}

/** A one-line hint describing a value without rendering its full content. */
export function hintFor(v: unknown): string {
	const k = kindOf(v);
	if (k === 'array') {
		const n = (v as unknown[]).length;
		return `${n} ${n === 1 ? 'item' : 'items'}`;
	}
	if (k === 'object') {
		const n = Object.keys(v as object).length;
		return `${n} ${n === 1 ? 'field' : 'fields'}`;
	}
	if (k === 'string') {
		const s = v as string;
		const oneLine = s.replace(/\s+/g, ' ').trim();
		if (oneLine.length <= PREVIEW_MAX) return oneLine;
		return oneLine.slice(0, PREVIEW_MAX) + '…';
	}
	if (k === 'null') return 'null';
	return String(v);
}

/**
 * Top-level outline of a result value. Objects → one entry per key. Arrays →
 * one entry per element (labeled by index, hinted by element shape). Scalars →
 * a single entry. Returns [] for null/empty.
 */
export function outline(value: unknown): OutlineEntry[] {
	const k = kindOf(value);
	if (k === 'object') {
		return Object.entries(value as Record<string, unknown>).map(([key, v]) => ({
			key,
			kind: kindOf(v),
			hint: hintFor(v),
		}));
	}
	if (k === 'array') {
		return (value as unknown[]).map((v, i) => ({
			key: `[${i}]`,
			kind: kindOf(v),
			hint: hintFor(v),
		}));
	}
	if (value === null || value === undefined) return [];
	return [{ key: '', kind: k, hint: hintFor(value) }];
}
