/** The glass keyboard contract (R7-SPEC §A.2). This is the only place that
 * reads key events in the glass shell.
 *
 * - The shell owns ⌘K (search), ⌘N (delegate hint), Esc and `[` (side bar).
 * - The current page registers 1–4, ⌘↵, ⌘O, J / K and ⌘Z (`usePageKeys`).
 * - In a text field only ⌘↵ and Esc act. Every other key is typing.
 * - A receipt page does not accept ⌘↵, so a double press cannot skip an item.
 * - While an input method composes, every key goes to the input method.
 * - Letters are matched by `KeyboardEvent.code` (`KeyJ`), not `key`, so J / K
 *   work under a Chinese input method too.
 * - A key from inside an extension (an element marked `data-c9ext-host`)
 *   goes to the extension first. The shell still takes ⌘K, ⌘N and Esc in
 *   the capture phase; the extension sends the keys it does not use back
 *   with `forwardKey` (glass build plan §3.3, D33 Q9).
 *
 * No imports: the node unit test loads this file on its own. */

export type Command =
    | 'search' | 'delegate' | 'sidebar' | 'escape'
    | 'option1' | 'option2' | 'option3' | 'option4'
    | 'submit' | 'openChat' | 'next' | 'prev' | 'undo';

/** The fields of a `KeyboardEvent` the contract reads. */
export type KeyLike = {
    code: string; metaKey?: boolean; ctrlKey?: boolean; altKey?: boolean; shiftKey?: boolean;
    isComposing?: boolean; keyCode?: number;
};

/** Shell keys. `escape` returns true when it closed something. */
export type ShellKeys = { search(): void; delegate(): void; sidebar(): void; escape(): boolean };

/** Page keys. `receipt: true` turns ⌘↵ off. `escape` returns true when it
 * closed something (a panel). */
export type PageKeys = {
    receipt?: boolean;
    option?(n: 1 | 2 | 3 | 4): void;
    submit?(): void; openChat?(): void; next?(): void; prev?(): void; undo?(): void;
    escape?(): boolean;
};

const DIGITS: Record<string, Command> = { Digit1: 'option1', Digit2: 'option2', Digit3: 'option3', Digit4: 'option4' };

/** Maps one key press to a command, or `null` when the contract ignores it. */
export function resolveKey(e: KeyLike, typing: boolean): Command | null {
    // keyCode 229: WebKit sends it for keys the input method takes.
    if (e.isComposing || e.keyCode === 229) return null;
    const mod = !!(e.metaKey || e.ctrlKey);
    if (e.altKey) return null;
    if (e.code === 'Escape' && !mod) return 'escape';
    if (mod && (e.code === 'Enter' || e.code === 'NumpadEnter') && !e.shiftKey) return 'submit';
    if (typing) return null;
    if (mod) {
        if (e.shiftKey) return null;
        switch (e.code) {
            case 'KeyK': return 'search';
            case 'KeyN': return 'delegate';
            case 'KeyO': return 'openChat';
            case 'KeyZ': return 'undo';
            default: return null;
        }
    }
    if (e.shiftKey) return null;
    if (e.code === 'KeyJ') return 'next';
    if (e.code === 'KeyK') return 'prev';
    if (e.code === 'BracketLeft') return 'sidebar';
    return DIGITS[e.code] ?? null;
}

type ElementLike = { tagName?: string; type?: string; isContentEditable?: boolean } | null | undefined;
const TEXT_INPUTS = new Set(['text', 'search', 'password', 'email', 'url', 'tel', 'number', '']);

/** True when key presses on `el` are typing. */
export function isTextField(el: ElementLike): boolean {
    if (!el) return false;
    if (el.isContentEditable) return true;
    const tag = (el.tagName ?? '').toLowerCase();
    if (tag === 'textarea') return true;
    return tag === 'input' && TEXT_INPUTS.has((el.type ?? '').toLowerCase());
}

/** Runs the command. Returns true when a handler took it. */
export function runCommand(cmd: Command, shell: ShellKeys, page: PageKeys | undefined): boolean {
    switch (cmd) {
        case 'search': shell.search(); return true;
        case 'delegate': shell.delegate(); return true;
        case 'sidebar': shell.sidebar(); return true;
        // Overlays sit above the page, so the shell closes first.
        case 'escape': return shell.escape() || !!page?.escape?.();
        case 'submit':
            if (!page?.submit || page.receipt) return false;
            page.submit(); return true;
        case 'option1': case 'option2': case 'option3': case 'option4':
            if (!page?.option) return false;
            page.option(Number(cmd.slice(6)) as 1 | 2 | 3 | 4); return true;
        default: {
            const fn = page?.[cmd];
            if (!fn) return false;
            fn(); return true;
        }
    }
}

// The registered pages, newest last. Only the newest one gets keys.
const pages: PageKeys[] = [];

/** Registers the current page's keys. Returns the unregister function, so a
 * Svelte page can pass it to `onMount` / `$effect` cleanup. */
export function usePageKeys(keys: PageKeys): () => void {
    pages.push(keys);
    return () => { const i = pages.lastIndexOf(keys); if (i >= 0) pages.splice(i, 1); };
}

type EventLike = KeyLike & { target?: unknown; composedPath?(): unknown[]; preventDefault(): void; stopPropagation(): void };
type NodeLike = { hasAttribute?(name: string): boolean } | null | undefined;

/** Keys the shell takes even from inside an extension. */
const SHELL_FIRST: ReadonlySet<Command> = new Set(['search', 'delegate', 'escape']);

/** The element the key really came from. In a shadow root `target` is the
 * shadow host; the composed path starts at the focused element. */
function origin(e: EventLike): unknown {
    return e.composedPath?.()[0] ?? e.target;
}
const fromExtension = (e: EventLike) => (e.composedPath?.() ?? []).some(n => (n as NodeLike)?.hasAttribute?.('data-c9ext-host'));

function run(e: EventLike, shell: ShellKeys, forwarded: boolean): Command | null {
    const at = origin(e);
    const typing = isTextField(at as ElementLike);
    const cmd = resolveKey(e, typing);
    if (!cmd) return null;
    if (!forwarded && !SHELL_FIRST.has(cmd) && fromExtension(e)) return null;
    const done = runCommand(cmd, shell, pages[pages.length - 1]);
    // Esc in a text field with nothing to close leaves the field.
    if (!done && cmd === 'escape' && typing) (at as { blur?: () => void }).blur?.();
    if (!done) return null;
    e.preventDefault();
    e.stopPropagation();
    return cmd;
}

/** Handles one keydown. Exported for the unit test. */
export function handleKey(e: EventLike, shell: ShellKeys): Command | null {
    return run(e, shell, false);
}

let installed: ShellKeys | null = null;

/** An extension sends back a key it does not use (`host.keys.forward`).
 * The shell then handles it as if it came from the page. */
export function forwardKey(e: EventLike): Command | null {
    return installed ? run(e, installed, true) : null;
}

/** Installs the contract in the capture phase of `target` (the window).
 * Returns the uninstall function. */
export function installKeys(target: EventTarget, shell: ShellKeys): () => void {
    const fn = (e: Event) => { handleKey(e as unknown as EventLike, shell); };
    target.addEventListener('keydown', fn, true);
    installed = shell;
    return () => { target.removeEventListener('keydown', fn, true); if (installed === shell) installed = null; };
}
