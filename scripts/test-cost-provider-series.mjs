// Golden checks for the COST tab provider series and window totals in
// src/lib/cost-semantics.ts. The reference functions are copies of the inline
// CostTracker.svelte code before PROVIDER_SERIES and summarizeCostDays existed,
// reduced to take the per-provider values (claudeCode, codex, cursor, pi).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
	PROVIDER_SERIES,
	providerSeriesBlocks,
	providerSeriesBreakdown,
	providerSeriesGradient,
	summarizeCostDays,
	summarizeCostSessions
} from '../src/lib/cost-semantics.ts';

function referenceGradient([claude, codex, cursor, pi]) {
	const providers = [
		{ color: 'var(--accent-amber)', value: claude },
		{ color: 'var(--accent-blue)', value: codex },
		{ color: 'var(--accent-purple)', value: cursor },
		{ color: 'var(--accent-green)', value: pi }
	];
	const total = providers.reduce((sum, provider) => sum + provider.value, 0);
	let offset = 0;
	const stops = providers.map(({ color, value }) => {
		const start = total > 0 ? (offset / total) * 100 : 0;
		offset += value;
		const end = total > 0 ? (offset / total) * 100 : 100;
		return `${color} ${start}% ${end}%`;
	});
	return `linear-gradient(to top, ${stops.join(', ')})`;
}

function referenceBreakdown(values, format) {
	const providers = [
		{ label: 'Claude Code', provider: 'claudeCode' },
		{ label: 'Codex', provider: 'codex' },
		{ label: 'Cursor', provider: 'cursor' },
		{ label: 'Pi', provider: 'pi' }
	];
	const total = values.reduce((sum, value) => sum + value, 0);
	const percentage = (value) => total > 0 ? `${((value / total) * 100).toFixed(0)}%` : '0%';
	return providers
		.map(({ label }, index) => `${label} ${format(values[index])} (${percentage(values[index])})`)
		.join(', ');
}

function referenceBlocks(fillPct, totalCols, [claude, codex, cursor, pi]) {
	const filled = Math.round((fillPct / 100) * totalCols);
	const providers = [
		{ type: 'claude', value: claude },
		{ type: 'codex', value: codex },
		{ type: 'cursor', value: cursor },
		{ type: 'pi', value: pi }
	];
	const total = providers.reduce((sum, provider) => sum + provider.value, 0);
	const arr = [];
	for (let i = 0; i < filled && total > 0; i++) {
		const midpoint = ((i + 0.5) / filled) * total;
		let cumulative = 0;
		const provider = providers.find(({ value }) => {
			cumulative += value;
			return midpoint <= cumulative;
		}) ?? providers[providers.length - 1];
		arr.push({ type: provider.type });
	}
	while (arr.length < totalCols) arr.push({ type: 'empty' });
	return arr;
}

function referenceWindowTotals(days) {
	const cost = days.reduce((sum, d) => sum + summarizeCostSessions(d.sessions).cost, 0);
	const tokens = days.reduce((sum, d) => sum + d.sessions.reduce((s, x) => s + (x.totalTokens || 0), 0), 0);
	const unpricedTokens = days.reduce((sum, d) => sum + summarizeCostSessions(d.sessions).unpricedTokens, 0);
	return { cost, tokens, unpricedTokens };
}

// Deterministic PRNG so failures reproduce.
function rng(seed) {
	let x = seed >>> 0;
	return () => {
		x = (x * 1664525 + 1013904223) >>> 0;
		return x / 2 ** 32;
	};
}

function valueSets() {
	const rand = rng(42);
	const sets = [
		[0, 0, 0, 0],
		[1, 0, 0, 0],
		[0, 0, 0, 5],
		[0.1, 0.2, 0.3, 0.4],
		[1e-9, 0, 1e9, 3],
		[1, 1, 1, 1]
	];
	for (let i = 0; i < 300; i++) {
		sets.push([0, 1, 2, 3].map(() => (rand() < 0.3 ? 0 : rand() * (rand() < 0.5 ? 10 : 5e6))));
	}
	return sets;
}

const fmtUsd = (n) => `$${n.toFixed(2)}`;
const fmtTok = (n) => `${Math.round(n)}`;

test('PROVIDER_SERIES keeps provider ids, css types and order', () => {
	assert.deepEqual(PROVIDER_SERIES.map((s) => s.provider), ['claudeCode', 'codex', 'cursor', 'pi']);
	assert.deepEqual(PROVIDER_SERIES.map((s) => s.type), ['claude', 'codex', 'cursor', 'pi']);
	assert.deepEqual(PROVIDER_SERIES.map((s) => s.label.toUpperCase()), ['CLAUDE CODE', 'CODEX', 'CURSOR', 'PI']);
	assert.deepEqual(PROVIDER_SERIES.filter((s) => s.alwaysInLegend).map((s) => s.provider), ['claudeCode', 'codex']);
});

test('providerSeriesGradient matches the old inline gradient', () => {
	for (const values of valueSets()) assert.equal(providerSeriesGradient(values), referenceGradient(values));
});

test('providerSeriesBreakdown matches the old inline label', () => {
	for (const values of valueSets()) {
		assert.equal(providerSeriesBreakdown(values, fmtUsd), referenceBreakdown(values, fmtUsd));
		assert.equal(providerSeriesBreakdown(values, fmtTok), referenceBreakdown(values, fmtTok));
	}
});

test('providerSeriesBlocks matches the old inline bar blocks', () => {
	const rand = rng(7);
	for (const values of valueSets()) {
		for (const cols of [1, 7, 40]) {
			for (const fill of [0, 2.5, 50, 99.9, 100, rand() * 100]) {
				assert.deepEqual(providerSeriesBlocks(values, fill, cols), referenceBlocks(fill, cols, values));
			}
		}
	}
});

test('summarizeCostDays is bit-identical to the old per-day window totals', () => {
	const rand = rng(1234);
	const providers = ['claudeCode', 'codex', 'cursor', 'pi', undefined];
	for (let run = 0; run < 50; run++) {
		const days = [];
		const dayCount = Math.floor(rand() * 10);
		for (let d = 0; d < dayCount; d++) {
			const sessions = [];
			const sessionCount = Math.floor(rand() * 6);
			for (let i = 0; i < sessionCount; i++) {
				const r = rand();
				sessions.push({
					provider: providers[Math.floor(rand() * providers.length)],
					cost: rand() * 13.37,
					totalTokens: rand() < 0.1 ? undefined : Math.floor(rand() * 1e6),
					costAvailable: r < 0.33 ? true : r < 0.66 ? false : undefined
				});
			}
			days.push({ date: `2026-10-${String(d + 1).padStart(2, '0')}`, sessions });
		}
		const actual = summarizeCostDays(days);
		const expected = referenceWindowTotals(days);
		assert.ok(Object.is(actual.cost, expected.cost), `cost ${actual.cost} != ${expected.cost}`);
		assert.ok(Object.is(actual.tokens, expected.tokens));
		assert.ok(Object.is(actual.unpricedTokens, expected.unpricedTokens));
	}
});
