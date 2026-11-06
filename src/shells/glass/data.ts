/** Glass starts its own data. Classic starts its data in `(app)/+layout.svelte`,
 * which glass does not render. Only `src/lib/data/` feeds are used here,
 * never the Classic `$lib/stores/*`. */
import { writable } from 'svelte/store';
import type { C9pAgentRow, C9pSessionView } from '$lib/c9p-1';
import { request, transport } from '$lib/data/client';
import { sessionFeed } from '$lib/data/session-feed';
import { annotationFeed, type AnnotationMap } from '$lib/data/annotation-feed';
import { isDemoMode, isPersistedDemoMode } from '$lib/data/demo/mode';
import { demoGlassOn } from '$lib/data/demo';

export const sessions = writable<C9pSessionView[]>([]);
export const agents = writable<C9pAgentRow[]>([]);
export const annotations = writable<AnnotationMap>(new Map());
/** The last feed error; `null` while every feed is live. */
export const dataError = writable<unknown | null>(null);
/** True when the glass demo fixture answers (demo mode and `c9watch.demoGlass`).
 * Only then may the shell show fixture-only data such as demo workspaces. */
export const fixtureOn = writable(false);

/** Starts every glass feed. Returns the stop function; the shell calls it on
 * unmount, so a switch to Classic leaves no glass feed running. */
export function startGlassData(): () => void {
    const demo = isPersistedDemoMode();
    if (demo) isDemoMode.set(true);
    fixtureOn.set(demo && demoGlassOn());
    const t = transport();
    // The feed's Session rows are SessionView rows with a cast status.
    const stopSessions = sessionFeed(t, rows => sessions.set(rows as unknown as C9pSessionView[]), dataError.set);
    const stopAnnotations = annotationFeed(t, annotations.set, dataError.set);
    let stopped = false;
    // G0 reads the agent tree once. A live agent feed comes with the model (G2).
    request('agent.list', {}).then(r => { if (!stopped) agents.set(r?.agents ?? []); }, dataError.set);
    return () => { stopped = true; stopSessions(); stopAnnotations(); };
}
