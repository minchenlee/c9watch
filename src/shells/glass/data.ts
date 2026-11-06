/** Glass starts its own data. Classic starts its data in `(app)/+layout.svelte`,
 * which glass does not render. Only `src/lib/data/` feeds are used here,
 * never the Classic `$lib/stores/*`. */
import { writable } from 'svelte/store';
import type { C9pAgentRow, C9pSessionView } from '$lib/c9p-1';
import { request, transport } from '$lib/data/client';
import { sessionFeed } from '$lib/data/session-feed';
import { annotationFeed, type AnnotationMap } from '$lib/data/annotation-feed';
import { isDemoMode, isPersistedDemoMode } from '$lib/data/demo/mode';

export const sessions = writable<C9pSessionView[]>([]);
export const agents = writable<C9pAgentRow[]>([]);
export const annotations = writable<AnnotationMap>(new Map());
/** The last feed error; `null` while every feed is live. */
export const dataError = writable<unknown | null>(null);

/** Starts every glass feed. Returns the stop function. */
export function startGlassData(): () => void {
    if (isPersistedDemoMode()) isDemoMode.set(true);
    const t = transport();
    // The feed's Session rows are SessionView rows with a cast status.
    const stopSessions = sessionFeed(t, rows => sessions.set(rows as unknown as C9pSessionView[]), dataError.set);
    const stopAnnotations = annotationFeed(t, annotations.set, dataError.set);
    let stopped = false;
    // G0 reads the agent tree once. A live agent feed comes with the model (G2).
    request('agent.list', {}).then(r => { if (!stopped) agents.set(r?.agents ?? []); }, dataError.set);
    return () => { stopped = true; stopSessions(); stopAnnotations(); };
}
