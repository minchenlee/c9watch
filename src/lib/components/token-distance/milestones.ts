export interface Milestone {
	tokens: number;
	height: number;       // meters
	label: string;
	emoji: string;        // single char/emoji for ASCII scene
	category: 'building' | 'mountain' | 'sky' | 'space';
}

export const MILESTONES: Milestone[] = [
	{ tokens: 1_000,       height: 5,          label: 'Two-story house',        emoji: '⌂', category: 'building' },
	{ tokens: 10_000,      height: 50,         label: 'Statue of Liberty',      emoji: '🗽', category: 'building' },
	{ tokens: 30_000,      height: 146,        label: 'Great Pyramid of Giza',  emoji: '△', category: 'building' },
	{ tokens: 60_000,      height: 300,        label: 'Eiffel Tower',           emoji: '⚑', category: 'building' },
	{ tokens: 76_000,      height: 381,        label: 'Empire State Building',  emoji: '▏', category: 'building' },
	{ tokens: 100_000,     height: 508,        label: 'Taipei 101',             emoji: '█', category: 'building' },
	{ tokens: 166_000,     height: 828,        label: 'Burj Khalifa',           emoji: '▍', category: 'building' },
	{ tokens: 196_000,     height: 979,        label: 'Angel Falls',            emoji: '💧', category: 'mountain' },
	{ tokens: 256_000,     height: 1_281,      label: 'Mount Vesuvius',         emoji: '🌋', category: 'mountain' },
	{ tokens: 445_000,     height: 2_224,      label: 'Krubera Cave',           emoji: '🕳', category: 'mountain' },
	{ tokens: 584_000,     height: 2_917,      label: 'Mount Olympus',          emoji: '⛰', category: 'mountain' },
	{ tokens: 681_000,     height: 3_403,      label: 'Mount Etna',             emoji: '🌋', category: 'mountain' },
	{ tokens: 760_000,     height: 3_776,      label: 'Mt. Fuji',               emoji: '⛰', category: 'mountain' },
	{ tokens: 780_000,     height: 3_952,      label: 'Mt. Yushan',             emoji: '⛰', category: 'mountain' },
	{ tokens: 1_200_000,   height: 5_895,      label: 'Mt. Kilimanjaro',        emoji: '⛰', category: 'mountain' },
	{ tokens: 1_800_000,   height: 8_849,      label: 'Mt. Everest',            emoji: '⛰', category: 'mountain' },
	{ tokens: 2_000_000,   height: 10_000,     label: 'Cruising altitude',      emoji: '✈', category: 'sky' },
	{ tokens: 8_000_000,   height: 40_000,     label: "Baumgartner's skydive",  emoji: '🪂', category: 'sky' },
	{ tokens: 20_000_000,  height: 100_000,    label: 'Kármán line',            emoji: '─', category: 'space' },
	{ tokens: 80_000_000,  height: 400_000,    label: 'ISS',                    emoji: '🛰', category: 'space' },
	{ tokens: 7_000_000_000, height: 35_786_000, label: 'Geostationary orbit',  emoji: '📡', category: 'space' },
	{ tokens: 77_000_000_000, height: 384_400_000, label: 'The Moon',           emoji: '🌙', category: 'space' },
];

/** Convert token count to height in meters (1 token = 1 grain of rice = 5mm) */
export function tokensToHeight(tokens: number): number {
	return tokens * 0.005;
}

/** Format height with appropriate unit */
export function formatHeight(meters: number): string {
	if (meters < 1000) return `${Math.round(meters)}m`;
	if (meters < 100_000) return `${(meters / 1000).toFixed(1)}km`;
	return `${Math.round(meters / 1000).toLocaleString()}km`;
}

/** Find the highest milestone the user has passed */
export function getCurrentMilestone(tokens: number): Milestone | null {
	let last: Milestone | null = null;
	for (const m of MILESTONES) {
		if (tokens >= m.tokens) last = m;
		else break;
	}
	return last;
}

/** Find the next milestone the user hasn't reached yet */
export function getNextMilestone(tokens: number): Milestone | null {
	for (const m of MILESTONES) {
		if (tokens < m.tokens) return m;
	}
	return null;
}
