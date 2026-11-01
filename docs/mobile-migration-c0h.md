# Remote access after the C0h hardening

This release locks down the WebSocket server on port 9210.
The changes below break old mobile and browser clients on purpose. Read this
page before you upgrade if you use c9watch from a phone, another browser,
or a script.

## What you must do

1. **Turn on LAN access if you use a phone on Wi-Fi.** Port 9210 now listens
   on this Mac only (`127.0.0.1` and `::1`). Go to **Settings → Remote access**
   and turn on **LAN access**.
2. **Pair each device again.** Open **Connect Mobile** on the desktop. Scan the
   QR code, or type the 8-character pairing code on the device.
3. **Turn on what the device may do.** Remote devices see session cards only.
   To read conversations or to open a terminal from the device, turn on
   **Conversations over remote** and **Open terminal from remote** in the same
   section.

## What changed for users

1. **Phones cannot connect by default.** Port 9210 binds loopback only. LAN
   access is an opt-in setting. When it is on, the server binds `[::]:9210`
   (all interfaces) instead of the loopback addresses.
2. **LAN traffic is not encrypted.** LAN access uses plain `ws://`. Anyone on
   the same network can read the pairing code, the device token and session
   content. The settings page shows this warning. Turn LAN access on only on a
   network you trust. A `wss://` transport is not part of this release; the
   listener code takes a bind plan so that a TLS listener can be added later.
3. **Old QR codes, bookmarks and saved URLs stop working.** Any URL with
   `?token=` is rejected. A device that has an old saved URL deletes it and
   shows "Pair again".
4. **Pairing codes replace tokens on screen.** The Connect Mobile window shows
   a one-time code and its QR code. A code expires after 5 minutes and works
   once. The QR code puts the code in the URL fragment (`#pair=CODE`), so it is
   not sent in the HTTP request or written to a log.
5. **Restarting the desktop app forgets paired devices.** Tokens live in
   memory only, as before. Persistent pairing comes with the engine database
   (C1a). **Forget paired devices** in Settings disconnects every device now.
6. **Remote devices get no conversation content by default.** With
   **Conversations over remote** off:
   - `getConversation` is refused.
   - Session pushes have no first prompt, latest message, last reply, summary,
     pending tool input or AI-generated title (`aiTitle`, `codexTitle`,
     `cursorTitle`). The session name, a title you set, status and the
     pending tool's name stay, so a card still says what it is and what it
     waits for.
   - A notification's title is the session's title or name, never the first
     prompt.
7. **Remote devices cannot open windows by default.** With **Open terminal
   from remote** off, `openSession` is refused and cards hide the open button.
8. **Stop is stricter.** c9watch stops only a Claude Code session that it
   detects, and only when the process start time still matches. A refused stop
   now shows the reason in a toast. `c9watch stop <pid>` refuses a pid that is
   not a detected session.
9. **The terminal and the debug console no longer show the access URL** or
   print a QR code.
10. **Remote images in Markdown do not load** in the desktop app or the web
    UI. The content security policy blocks them.

## Reaching the desktop without LAN access

- **SSH tunnel.** Forward the same port:
  `ssh -L 9210:127.0.0.1:9210 your-mac`, then open `http://localhost:9210`.
  The server checks the `Host` header, so a different local port (for example
  `-L 8000:127.0.0.1:9210`) is refused. Adding `localhost:8000` to **Extra
  allowed hosts** makes the server accept that `Host`; this setup is not
  tested.
- **Reverse proxies such as `tailscale serve`** are not supported in this
  release. The page is served over `https://` on port 443 there, and the web
  client and the content security policy expect `ws://` on port 9210.

## Third-party clients

The protocol changes are:

| Before | Now |
|---|---|
| `ws://host:9210/ws?token=T` | `ws://host:9210/ws`, then the first frame `{"type":"auth","token":"T"}` or `{"type":"pair","code":"CODE"}` within 5 s |
| connection opens at once | wait for `{"type":"authOk","clientId","capabilities"}` or `{"type":"paired","clientId","token","capabilities"}` before you send requests |
| — | close code `4400`: no, late or malformed first frame. `4401`: token refused; pair again, do not retry. `4403`: pairing code wrong, expired or used; a stored token is still valid |
| `{"type":"stopSession","pid":N}` | `{"type":"session.stop","addr":"claudeCode:<sessionId>","pid":N?}` |
| `{"type":"openSession","pid":N,"projectPath":P}` | `{"type":"openSession","addr":"<provider>:<sessionId>","pid":N?}`. The server uses the pid and project path of the session it detects. |

- `addr` is the session key (`sessionKey` in a session object). `pid` is
  optional. When you send it and it differs from the pid the server detects,
  the server refuses the request.
- `authOk` also carries `principal`, `"client:<clientId>"`: the name the
  server logs your actions under. It was added in C1a; a client can ignore it.
- `capabilities` is `{"conversation": bool, "open": bool}`. A request for a
  capability that is off gets
  `{"type":"error","code":"capabilityDisabled","capability":"conversation"|"open","message":...,"requestId":N}`.
- When the desktop changes a remote-access setting, every socket closes with
  code `1012`. Reconnect and authenticate again; the new `authOk` carries the
  new capabilities.
- The server checks `Host` on every route and `Origin` on `/ws`. A browser
  page from another origin gets `403`.

## Known trade-offs

- **Anyone on the LAN can use up a pairing code.** A code is cancelled after 5
  failed tries in total, counted across all connections (a per-connection
  count would let an attacker reconnect and try again). So a device on the
  same network can keep cancelling codes, and pairing then fails until you
  make a new code or turn LAN access off. This is a denial of service only; it
  does not let anyone in. We accept it because the alternative weakens the
  code.
- **`renameSession` and `getAwayActivity` are not gated.** Any paired device
  can rename a session (it writes the custom title file) and read the
  away-activity counts (files edited, commands run, subagents done; no
  content). Both are part of watching cards, so they stay available to every
  paired device on purpose.
- **A title you set can describe the conversation.** With conversations off,
  generated titles are removed, but `customTitle` and `sessionName` stay,
  because the card needs a name.

## Out of scope

- A native iOS app. The repo has none today; "mobile" is the web page that the
  desktop serves on port 9210. A future app with origin `tauri://localhost`
  would be refused by the `Origin` check until it is added there.
- `wss://` with a pinned certificate for LAN mode. The owner has not decided
  on it yet.
