/** Glass page navigation by URL hash. Glass replaces the SvelteKit `<slot/>`,
 * so it cannot use the `(app)` routes; a hash lets a reload or a Playwright
 * test open any page directly (`/#/ws/c9watch`). */
import { readable } from 'svelte/store';

export type GlassRoute =
    | { page: 'needs' }
    | { page: 'today' }
    | { page: 'usage' }
    | { page: 'settings' }
    | { page: 'ws'; id: string }
    | { page: 'ext'; extId: string; pageId: string };

export const DEFAULT_ROUTE: GlassRoute = { page: 'needs' };

/** Parses `#/needs`, `#/ws/<id>`, `#/ext/<extId>/<pageId>`, ...
 * Anything else is the default page. */
export function parseHash(hash: string): GlassRoute {
    const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
    const [page, a, b] = parts;
    if ((page === 'needs' || page === 'today' || page === 'usage' || page === 'settings') && parts.length === 1) return { page };
    if (page === 'ws' && a && parts.length === 2) return { page, id: a };
    if (page === 'ext' && a && b && parts.length === 3) return { page, extId: a, pageId: b };
    return DEFAULT_ROUTE;
}

export function routeHash(route: GlassRoute): string {
    const e = encodeURIComponent;
    switch (route.page) {
        case 'ws': return `#/ws/${e(route.id)}`;
        case 'ext': return `#/ext/${e(route.extId)}/${e(route.pageId)}`;
        default: return `#/${route.page}`;
    }
}

export function navigate(route: GlassRoute): void {
    location.hash = routeHash(route);
}

/** The current route; follows `hashchange`. */
export const route = readable<GlassRoute>(DEFAULT_ROUTE, set => {
    const update = () => set(parseHash(location.hash));
    update();
    addEventListener('hashchange', update);
    return () => removeEventListener('hashchange', update);
});
