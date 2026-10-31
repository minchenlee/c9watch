import { writable } from 'svelte/store';
/** M-f owns population and the annotation contract. Classic never reads it. */
export const annotations = writable<ReadonlyMap<string, unknown>>(new Map());
