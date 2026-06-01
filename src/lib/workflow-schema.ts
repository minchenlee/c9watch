import type { ResultSchema, FieldMeta } from './types';

/** Jaccard overlap of two key sets. */
function jaccard(a: string[], b: string[]): number {
	const sa = new Set(a);
	const sb = new Set(b);
	let inter = 0;
	for (const k of sa) if (sb.has(k)) inter++;
	const union = sa.size + sb.size - inter;
	return union === 0 ? 0 : inter / union;
}

/**
 * Best schema for an array element's keys, by key-set overlap. The script often
 * renames/wraps/extends agent output, so we match on keys, not on name.
 * Returns null below the confidence threshold (generic render then applies).
 */
export function matchSchema(
	elemKeys: string[],
	schemas: ResultSchema[],
	threshold = 0.5
): ResultSchema | null {
	let best: ResultSchema | null = null;
	let bestScore = 0;
	for (const s of schemas) {
		const score = jaccard(elemKeys, s.keys);
		if (score > bestScore) {
			bestScore = score;
			best = s;
		}
	}
	return bestScore >= threshold ? best : null;
}

export interface TableLayout {
	/** scalar columns shown inline, in order */
	columns: FieldMeta[];
	/** at most one long-text field rendered as an expandable per-row body */
	bodyKey: string | null;
	/** true when the table layout is unsuitable — caller should fall back to cards */
	fallback: boolean;
}

const SCALAR_TYPES = new Set(['string', 'number', 'integer', 'boolean', '']);
const COLUMN_VALUE_MAX = 60;

/**
 * Decide table columns from the matched schema + observed rows. Scalar props
 * with consistently short values become columns; the single longest-average
 * string prop becomes the expandable body. Falls back when there is nothing
 * tabular to show, or too many columns to read.
 */
export function pickColumns(schema: ResultSchema, rows: Record<string, unknown>[]): TableLayout {
	const avgLen = (key: string): number => {
		let total = 0;
		let n = 0;
		for (const r of rows) {
			const v = r[key];
			if (typeof v === 'string') {
				total += v.length;
				n++;
			}
		}
		return n === 0 ? 0 : total / n;
	};

	const columns: FieldMeta[] = [];
	const longCandidates: { key: string; len: number }[] = [];

	for (const p of schema.props) {
		if (!SCALAR_TYPES.has(p.ty)) continue; // arrays/objects are not columns
		const isStringy = p.ty === 'string' || p.ty === '';
		if (isStringy && avgLen(p.name) > COLUMN_VALUE_MAX) {
			longCandidates.push({ key: p.name, len: avgLen(p.name) });
		} else {
			columns.push(p);
		}
	}

	longCandidates.sort((a, b) => b.len - a.len);
	const bodyKey = longCandidates.length ? longCandidates[0].key : null;

	const fallback = columns.length === 0 || columns.length > 6;

	return { columns, bodyKey, fallback };
}

/** Whole-token badge color (mirrors JsonWidget.badgeColor). */
const RED = new Set(['bug', 'error', 'fail', 'failed', 'failure', 'critical', 'high', 'reject', 'rejected', 'blocked', 'broken', 'crash']);
const AMBER = new Set(['warn', 'warning', 'medium', 'uncertain', 'partial', 'pending', 'skip', 'skipped', 'unknown', 'nit']);
const GREEN = new Set(['ok', 'pass', 'passed', 'done', 'complete', 'completed', 'confirmed', 'low', 'success', 'good', 'green', 'resolved']);

export function badgeColor(v: string): 'red' | 'amber' | 'green' | 'neutral' {
	const toks = v.toLowerCase().split(/[\s_\-/,]+/).filter(Boolean);
	if (toks.some((t) => RED.has(t))) return 'red';
	if (toks.some((t) => AMBER.has(t))) return 'amber';
	if (toks.some((t) => GREEN.has(t))) return 'green';
	return 'neutral';
}

/** A schema prop is badge-worthy when it has an enum of short strings. */
export function isEnumProp(p: FieldMeta): boolean {
	return !!p.enumVals && p.enumVals.length > 0;
}
