import { fly, fade, type FlyParams, type FadeParams } from 'svelte/transition';
import { cubicOut } from 'svelte/easing';

function prefersReducedMotion(): boolean {
	if (typeof window === 'undefined' || !window.matchMedia) return false;
	return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

const EXPO_OUT = (t: number) => 1 - Math.pow(2, -10 * t);

// Staggered fly-in for list items. `i` is the index; delay is base + i * stride.
export function flyIn(
	node: Element,
	params: { index?: number; y?: number; duration?: number; stride?: number; base?: number } = {}
) {
	if (prefersReducedMotion()) {
		return fade(node, { duration: 0 });
	}
	const { index = 0, y = 8, duration = 220, stride = 25, base = 0 } = params;
	return fly(node, {
		y,
		duration,
		delay: base + Math.min(index, 20) * stride,
		easing: EXPO_OUT,
		opacity: 0,
	} satisfies FlyParams);
}

// Plain fade that respects reduced motion.
export function fadeIn(node: Element, params: FadeParams = {}) {
	if (prefersReducedMotion()) {
		return fade(node, { duration: 0 });
	}
	return fade(node, { duration: 180, easing: cubicOut, ...params });
}

// Staggered fly-in from the right (x-axis). Used for panels that slide in
// from the edge, e.g. right-column panels in the expanded overlay.
export function flyInX(
	node: Element,
	params: { index?: number; x?: number; duration?: number; stride?: number; base?: number } = {}
) {
	if (prefersReducedMotion()) {
		return fade(node, { duration: 0 });
	}
	const { index = 0, x = 12, duration = 220, stride = 25, base = 0 } = params;
	return fly(node, {
		x,
		duration,
		delay: base + Math.min(index, 20) * stride,
		easing: EXPO_OUT,
		opacity: 0,
	} satisfies FlyParams);
}
