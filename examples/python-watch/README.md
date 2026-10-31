> **Preview.** This example targets c9p 1.6. It refuses breaking method revisions.

# Python session watcher

Python 3.9 or later. Standard library only. Start an engine-enabled `c9watch engine` first. Then run:

```sh
python3 examples/python-watch/watch.py --socket '/absolute/core-home/engine-v1.sock'
```

Or set `C9WATCH_ENGINE_HOME` and omit `--socket`. The default is the production home for macOS or Linux. Development builds use a separate `engine-dev` home; set it explicitly. No process is auto-started by this script.

It sends `hello`, reads `session.views`, then subscribes from the snapshot cursor. It prints `session.view` events as NDJSON. `--since <cursor>` resumes a saved position. `--max-events 1` exits after one event for fixture checks. Without that option it waits until interrupted.

Persist the event cursor only after your own processing succeeds. On `cursor_expired`, restart without `--since` to read a fresh snapshot. Replace your application's state with that snapshot; the example only prints subsequent changes. It does not silently reset an expired cursor. Replay contains the latest live views, not every historical transition.
