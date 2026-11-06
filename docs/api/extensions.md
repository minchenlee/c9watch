> **Preview.** The manifest format (`c9ext: 1`) and the UI contract (`c9ui/1`) may change before c9p 1 leaves preview.

# Glass extensions

A glass extension adds pages to the glass shell. One extension is one directory with a `c9ext.json` manifest. In this version an extension has only a UI part. The manifest reserves a `core` block for a later core part, but nothing runs it yet.

## Trust

**An extension is trusted code.** Installing one is like installing an npm package. It runs in the glass window, in the same document as the host, and it gets the host's c9p transport. It can do everything the glass shell can do, and nothing more. c9watch does not sandbox it, sign it or review it. Install only extensions whose source you trust.

c9watch keeps three guarantees (D31):

1. Core still checks every connection (D8). An extension calls c9p through the host's bridge, so core sees the window's connection, not the extension.
2. Only you can put files in the extension directories. The third-party directory is inside core's write floor, so an agent that core starts cannot write there.
3. Gateway pairing and scope widening still need your native confirmation. An extension cannot call `gateway.*` writes.

The shadow root that holds an extension keeps its CSS apart from the host. It is not a security boundary.

## Where extensions live

| Kind | Directory | Notes |
| --- | --- | --- |
| First-party | `extensions/<id>/` in the repo, bundled into the app's resources | Signed with the app |
| Third-party | `<engine home>/extensions/<id>/` | Yours to add |

The engine home is the core home in [the API guide](README.md): on macOS `~/Library/Application Support/c9watch/engine/` (development builds: `engine-dev/`), or `C9WATCH_ENGINE_HOME` when it is set. Core does not create `extensions/`; make it yourself.

When a first-party and a third-party extension have the same id, the first-party one wins. The settings list shows the other one as `shadowed`.

## Install: add a directory and restart

1. Check the directory: `c9watch ext check path/to/my-ext`.
2. Copy it to `<engine home>/extensions/my-ext/`. The directory name must equal the manifest `id`.
3. Restart c9watch. The glass shell reads the extension list once per window.

To remove an extension, delete its directory and restart. There is no install, enable or update command in this version.

A symlink is allowed only when it stays inside the extension directory. An extension directory that is a symlink out of `extensions/` is ignored.

## Directory layout

```
my-ext/
├── c9ext.json     the manifest (required)
├── RECIPE.md      what it does, which c9p methods it reads, a screenshot (required to pass the check)
├── preview.png    a screenshot (required to pass the check)
└── index.js       a page entry, an ES module
```

The `c9ext://` file server sends only these file types: `.js`, `.mjs`, `.css`, `.json`, `.svg`, `.png`, `.woff2` and `.md`. It does not send a file or directory whose name starts with `.`.

`RECIPE.md` is for people who decide whether to install the extension. Say what it does, list every c9p method it calls and whether it writes, and show `preview.png`. See [`extensions/hello/RECIPE.md`](../../extensions/hello/RECIPE.md).

## Manifest

`c9ext.json` is strict JSON: no comments, no trailing commas. It is at most 64 KiB. Unknown fields are ignored.

```json
{
  "c9ext": 1,
  "id": "my-ext",
  "name": "My extension",
  "version": "0.1.0",
  "minVersion": { "app": "0.12.0", "apiMinor": 15 },
  "ui": {
    "contract": "c9ui/1",
    "pages": [{ "id": "main", "title": "My page", "entry": "index.js", "icon": "grid" }]
  }
}
```

| Field | Required | Rule |
| --- | --- | --- |
| `c9ext` | Yes | The JSON number `1`. Not the string `"1"`. |
| `id` | Yes | `^[a-z][a-z0-9-]{0,31}$`. Must equal the directory name. |
| `name` | Yes | 1 to 64 characters, not only white space. |
| `version` | Yes | A [semver 2.0.0](https://semver.org) version. |
| `minVersion` | No | An object. `app`: the oldest c9watch app version, as semver. `apiMinor`: the oldest c9p 1 minor, an integer of 0 or more. `core` is reserved for the core part and ignored. |
| `ui` | No | Without `ui` the extension adds no page. |
| `ui.contract` | With `ui` | `"c9ui/1"`. |
| `ui.pages` | With `ui` | 1 to 16 pages. |
| `ui.pages[].id` | Yes | Same rule as `id`. Unique in the extension. |
| `ui.pages[].title` | Yes | 1 to 40 characters, not only white space. |
| `ui.pages[].entry` | Yes | A relative path inside the extension directory, at most 200 characters. No leading `/`, no empty, `.` or `..` segment, no `\`, `%`, `:`, `?` or `#`. |
| `ui.pages[].icon` | No | A string of at most 40 characters. |
| `dev` | No | `true` or `false`. When `true`, only development builds show the extension. |
| `core` | No | Reserved for the core part. The glass shell does not run it and does not fail on it. |

Character counts are UTF-16 code units, as in JavaScript.

Each extension gets one status:

| Status | Meaning |
| --- | --- |
| `ok` | Loaded |
| `manifest_error` | The manifest breaks a rule above |
| `version_mismatch` | This c9watch is older than `minVersion` |
| `shadowed` | A first-party extension has the same id |
| `dev_only` | `dev: true` in a production build |

## `c9watch ext check`

```sh
c9watch ext check path/to/my-ext
c9watch ext check path/to/my-ext --json --pretty
```

The check uses the glass shell's manifest rules and gives the same reason text. It also checks that:

- every page entry is a file inside the directory, also after symlinks are resolved;
- the file server sends every page entry;
- `RECIPE.md` exists and `preview.png` is a PNG file, both inside the directory.

It prints `PASS` or `FAIL`, then one line per `error`, `warning` and `note`. Exit code 0 means pass; 1 means fail. A warning (for example, a bad `core.events` item, or an entry that is not `.js` or `.mjs`) does not fail the check. A note says what the host will do, for example that `dev: true` hides the extension in production builds, or that this c9watch is older than `minVersion`.

The check does not run the extension.

## The `c9ui/1` API

A page entry is an ES module that exports `mount`:

```ts
export function mount(root: ShadowRoot, host: C9uiHost): { destroy(): void } | void;
```

The host calls `mount` with an empty shadow root. It may return a promise. The host calls `destroy` when the page is replaced.

```ts
interface C9uiHost {
  v: 'c9ui/1';
  ext: {
    id: string;
    version: string;
    call(method: string, params?: unknown): Promise<never>;   // reserved: rejects with { code: 'refused' }
    on(event: string, cb: (v: unknown) => void): () => void;  // reserved: does nothing
  };
  c9p: Transport;            // request(method, params) and subscribe(...), as the host uses them
  readonly theme: { tokens: Record<string, string>; glass: 'off' | 'subtle' | 'full' };
  readonly context: { workspaceId?: string; goalId?: string; sessionKey?: string; needsItemId?: string };
  prefs: { get(): unknown; set(blob: unknown): void };
  intent(name: 'open-session' | 'open-handling' | 'navigate' | 'focus-needs-you',
         args?: Record<string, unknown>): Promise<'done' | 'refused' | 'cancelled'>;
  on(event: 'context' | 'theme' | 'resize' | 'suspend' | 'resume', cb: (v: unknown) => void): () => void;
  ready(): void;
  keys: { forward(e: KeyboardEvent): void };
}
```

| Part | Use |
| --- | --- |
| `c9p` | The host's c9p transport. The extension may call the same methods as the glass window. In demo mode it gets demo data. |
| `theme` | Theme tokens are CSS custom properties. They reach the shadow root, so CSS can use `var(--text-1)`. `theme.tokens` gives the values for SVG or canvas code. |
| `context` | What the user has selected in the host. Listen to `context` for changes. |
| `prefs` | One JSON value per extension, at most 64 KiB. `set` throws a `RangeError` when the value is too big and a `TypeError` when JSON cannot hold it. `set(undefined)` removes it. |
| `intent` | Asks the host to act. In this version `navigate` (`{ to: '#/needs' }`) and `focus-needs-you` work; the others return `refused`. |
| `on` | Host events. Stop animations and subscriptions on `suspend`; start them again on `resume`. Returns an unsubscribe function. |
| `ready` | Call it within 5 seconds of `mount`. |
| `keys.forward` | Send each key the page does not use back to the host, so J/K and the other host keys keep working. |

The type source is [`src/shells/glass/ext/host.ts`](../../src/shells/glass/ext/host.ts). The smallest working example is [`extensions/hello/`](../../extensions/hello/).

## Share an extension

Publish the directory as a GitHub repository with the topic **`c9watch-extension`**, so people can find it. The repository root can be the extension directory itself. There is no registry and no signing.
