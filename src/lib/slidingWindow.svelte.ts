import { tick } from 'svelte';
import type { Message } from '$lib/types';

/** Tuning constants for the sliding window. */
export const BATCH_SIZE = 200;
export const MAX_VISIBLE = 400;
const SCROLL_THRESHOLD = 500;

/**
 * Creates a sliding window controller for rendering large message lists.
 *
 * Only MAX_VISIBLE messages are kept in the DOM at once.  When the user
 * scrolls near the top or bottom edge the window expands by BATCH_SIZE and
 * trims the opposite end so the total never exceeds MAX_VISIBLE.
 */
export function createSlidingWindow() {
	let startIndex = $state(0);
	let endIndex = $state(0);
	let loading = $state(false);

	function reset(totalMessages: number, initialStart?: number) {
		endIndex = totalMessages;
		startIndex = initialStart ?? Math.max(0, totalMessages - BATCH_SIZE);
	}

	function sliceMessages(messages: Message[]): Message[] {
		return messages.slice(startIndex, endIndex);
	}

	async function loadOlder(container: HTMLElement) {
		if (loading || startIndex <= 0) return;
		loading = true;

		const prevHeight = container.scrollHeight;
		startIndex = Math.max(0, startIndex - BATCH_SIZE);
		if (endIndex - startIndex > MAX_VISIBLE) {
			endIndex = startIndex + MAX_VISIBLE;
		}

		await tick();
		container.scrollTop += container.scrollHeight - prevHeight;
		loading = false;
	}

	async function loadNewer(container: HTMLElement, totalMessages: number) {
		if (loading || endIndex >= totalMessages) return;
		loading = true;

		endIndex = Math.min(totalMessages, endIndex + BATCH_SIZE);
		if (endIndex - startIndex > MAX_VISIBLE) {
			const prevHeight = container.scrollHeight;
			startIndex = endIndex - MAX_VISIBLE;
			await tick();
			container.scrollTop += container.scrollHeight - prevHeight;
		}

		loading = false;
	}

	function handleScroll(container: HTMLElement, totalMessages: number) {
		if (loading) return;
		if (container.scrollTop < SCROLL_THRESHOLD && startIndex > 0) {
			loadOlder(container);
		}
		const distFromBottom =
			container.scrollHeight - container.scrollTop - container.clientHeight;
		if (distFromBottom < SCROLL_THRESHOLD && endIndex < totalMessages) {
			loadNewer(container, totalMessages);
		}
	}

	function expandToIndex(index: number, totalMessages: number) {
		if (index < startIndex || index >= endIndex) {
			startIndex = Math.max(0, index - Math.floor(MAX_VISIBLE / 2));
			endIndex = Math.min(totalMessages, startIndex + MAX_VISIBLE);
		}
	}

	async function scrollToIndex(
		index: number,
		container: HTMLElement,
		totalMessages: number,
		options?: { behavior?: ScrollBehavior; block?: ScrollLogicalPosition }
	) {
		expandToIndex(index, totalMessages);
		await tick();
		const target = container.querySelector(
			`[data-msg-index="${index}"]`
		) as HTMLElement | null;
		if (target) {
			target.scrollIntoView({
				behavior: options?.behavior ?? 'smooth',
				block: options?.block ?? 'start',
			});
		}
	}

	async function clearBeforeClose(totalMessages: number) {
		startIndex = totalMessages;
		await tick();
	}

	return {
		get startIndex() { return startIndex; },
		set startIndex(v: number) { startIndex = v; },
		get endIndex() { return endIndex; },
		set endIndex(v: number) { endIndex = v; },
		get loading() { return loading; },
		reset,
		sliceMessages,
		loadOlder,
		loadNewer,
		handleScroll,
		expandToIndex,
		scrollToIndex,
		clearBeforeClose,
	};
}
