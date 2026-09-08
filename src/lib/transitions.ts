import { fly, fade as svelteFade, scale as svelteScale, slide as svelteSlide, type FlyParams, type FadeParams, type ScaleParams, type SlideParams } from 'svelte/transition';
import { cubicOut } from 'svelte/easing';
import { isTauri } from './ws';

// Native WebKit can suspend its animation timeline even after an intro starts,
// while DOM/AX updates continue. Native content must not depend on a transition
// finishing to become visible. Browser transitions remain animated when visible.
function skipMotion(node: Element): boolean {
	return isTauri() || node.ownerDocument.visibilityState === 'hidden' || prefersReducedMotion();
}

export function fade(node: Element, params: FadeParams = {}) {
	return skipMotion(node) ? { duration: 0, delay: 0 } : svelteFade(node, params);
}

export function scale(node: Element, params: ScaleParams = {}) {
	return skipMotion(node) ? { duration: 0, delay: 0 } : svelteScale(node, params);
}

export function slide(node: Element, params: SlideParams = {}) {
	return skipMotion(node) ? { duration: 0, delay: 0 } : svelteSlide(node, params);
}

function prefersReducedMotion(): boolean {
	if (typeof window === 'undefined' || !window.matchMedia) return false;
	return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

const EXPO_OUT = (t: number) => 1 - Math.pow(2, -10 * t);

// Cap cascade at this index. Past it, elements appear instantly — this keeps
// long lists (e.g. HISTORY's hundreds of rows) from scheduling hundreds of
// transitions on mount, which stalls the main thread and lags tab switches.
const CASCADE_CAP = 20;

// Staggered fly-in for list items. `i` is the index; delay is base + i * stride.
export function flyIn(
	node: Element,
	params: { index?: number; y?: number; duration?: number; stride?: number; base?: number } = {}
) {
	if (skipMotion(node)) {
		return { duration: 0, delay: 0 };
	}
	const { index = 0, y = 8, duration = 400, stride = 60, base = 0 } = params;
	if (index > CASCADE_CAP) {
		return { duration: 0, delay: 0 };
	}
	return fly(node, {
		y,
		duration,
		delay: base + index * stride,
		easing: EXPO_OUT,
		opacity: 0,
	} satisfies FlyParams);
}

// Plain fade that respects reduced motion.
export function fadeIn(node: Element, params: FadeParams = {}) {
	if (skipMotion(node)) {
		return { duration: 0, delay: 0 };
	}
	return fade(node, { duration: 320, easing: cubicOut, ...params });
}

// Staggered fly-in from the right (x-axis). Used for panels that slide in
// from the edge, e.g. right-column panels in the expanded overlay.
export function flyInX(
	node: Element,
	params: { index?: number; x?: number; duration?: number; stride?: number; base?: number } = {}
) {
	if (skipMotion(node)) {
		return { duration: 0, delay: 0 };
	}
	const { index = 0, x = 12, duration = 400, stride = 60, base = 0 } = params;
	if (index > CASCADE_CAP) {
		return { duration: 0, delay: 0 };
	}
	return fly(node, {
		x,
		duration,
		delay: base + index * stride,
		easing: EXPO_OUT,
		opacity: 0,
	} satisfies FlyParams);
}
