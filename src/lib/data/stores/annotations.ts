/** Opt-in L1 state for annotation-aware clients. Classic never imports or
 * subscribes to this store, so it creates no L1 requests or UI changes. */
import { readable, writable } from 'svelte/store';
import { transport } from '../client';
import { annotationFeed, type AnnotationMap } from '../annotation-feed';
import type { Transport } from '../transport';
export function createAnnotationsStore(t: Transport) {
    const error = writable<unknown | null>(null);
    const annotations = readable<AnnotationMap>(new Map(), set => annotationFeed(t, set, error.set));
    return { annotations, error: { subscribe: error.subscribe } };
}
const error = writable<unknown | null>(null);
export const annotationsError = { subscribe: error.subscribe };
export const annotations = readable<AnnotationMap>(new Map(), set => annotationFeed(transport(), set, error.set));
