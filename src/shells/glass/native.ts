/** Native glass (build plan G10b). The GUI crate's `native_glass` command
 * says whether the window has macOS vibrancy behind the WebView and whether
 * the system's Reduce Transparency is on. Outside Tauri (vite dev, the
 * browser demo) the command fails and both stay false, so glass keeps the
 * opaque body it has today.
 *
 * Reduce Transparency changes in System Settings, outside the app, so the
 * shell reads it again each time the window gets focus (no NSWorkspace
 * observer: one IPC read per focus is enough). */
import { derived, writable } from 'svelte/store';
import { localCommand } from '$lib/data/platform';
import { glassMode, type GlassMode } from './prefs';

export interface NativeGlass { vibrancy: boolean; reduceTransparency: boolean }

const NONE: NativeGlass = { vibrancy: false, reduceTransparency: false };
export const nativeGlass = writable<NativeGlass>(NONE);

/** The mode `.glass-root` shows: Reduce Transparency forces Off over the
 * user's pref (direction §2.1). The pref itself is not changed. */
export const effectiveGlass = derived([glassMode, nativeGlass], ([m, n]): GlassMode => (n.reduceTransparency ? 'off' : m));

async function read(): Promise<void> {
    try {
        const v = await localCommand<Partial<NativeGlass> | null>('native_glass');
        nativeGlass.set({ vibrancy: v?.vibrancy === true, reduceTransparency: v?.reduceTransparency === true });
    } catch { nativeGlass.set(NONE); }
}

/** `html.glass-native` makes html and body transparent (tokens.css), so the
 * native material shows through `.glass-root`. Classic never has the class:
 * the shell removes it when it unmounts. */
const CLASS = 'glass-native';

/** Starts the reads and the html class. Returns the stop function. */
export function startNativeGlass(win: Window = window): () => void {
    const html = win.document.documentElement;
    const unsub = nativeGlass.subscribe(n => html.classList.toggle(CLASS, n.vibrancy));
    const onFocus = () => { void read(); };
    const onVisible = () => { if (win.document.visibilityState === 'visible') void read(); };
    win.addEventListener('focus', onFocus);
    win.document.addEventListener('visibilitychange', onVisible);
    void read();
    return () => {
        win.removeEventListener('focus', onFocus);
        win.document.removeEventListener('visibilitychange', onVisible);
        unsub();
        html.classList.remove(CLASS);
    };
}
