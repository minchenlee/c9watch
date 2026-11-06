/** Workspaces: sessions and agents grouped by repo root (build plan §4 Gap W).
 * c9p has no workspace id, so the id is a hash of the normalized root.
 * When core adds `workspace.list` (or `workspaceId` on rows), replace this. */
import type { DemoWorkspace } from './gaps-demo';

/** A worktree folder inside a repo: `<repo>/.worktrees/<name>`,
 * `<repo>/.claude/worktrees/<name>` (Claude Code), `<repo>/.git/worktrees/<name>`.
 * Everything below the worktree also belongs to the repo. */
const WORKTREE = /\/(?:\.worktrees|\.claude\/worktrees|\.git\/worktrees)\/[^/]+(?:\/.*)?$/;

export function normalizePath(path: string): string {
    const p = path.trim().replace(/\\/g, '/').replace(/\/{2,}/g, '/');
    return p.length > 1 ? p.replace(/\/$/, '') : p;
}

/** The repo root of a session or agent path. A path with no known worktree
 * shape is its own root: c9p does not say where the git root is. */
export function repoRoot(path: string): string {
    return normalizePath(path).replace(WORKTREE, '');
}

/** FNV-1a 32-bit: stable across runs, so a local setting can key on it. */
export function workspaceId(root: string): string {
    let h = 0x811c9dc5;
    for (let i = 0; i < root.length; i++) h = Math.imul(h ^ root.charCodeAt(i), 0x01000193);
    return `ws_${(h >>> 0).toString(16).padStart(8, '0')}`;
}

export type Workspace = {
    id: string;
    root: string;
    name: string;
    /** Verifier lines: only the demo has them (Gap V). */
    verifier: string[] | null;
};

/** The workspace for a path. A demo workspace with the same root keeps its
 * demo id and name, so demo routes (`#/ws/c9watch`) stay valid. */
export function workspaceFor(path: string, demo: DemoWorkspace[] | null): Workspace {
    const root = repoRoot(path || '/');
    const hit = demo?.find(w => normalizePath(w.root) === root);
    if (hit) return { id: hit.id, root, name: hit.name, verifier: hit.verifier };
    return { id: workspaceId(root), root, name: root.split('/').filter(Boolean).pop() ?? root, verifier: null };
}
