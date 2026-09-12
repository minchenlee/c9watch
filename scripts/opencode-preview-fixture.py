#!/usr/bin/env python3
"""Local read-only OpenCode fixture for repeatable native QA (no real model/API).

Run: python3 scripts/opencode-preview-fixture.py --port 64647
Control: curl -X POST http://127.0.0.1:64647/__fixture -H 'Content-Type: application/json'
         -d '{"messageError":503}'
Ready without expiring the old root: {"busy":false,"fresh":true}
Reset errors / update text: {"messageError":0,"revision":1}
Only /__fixture accepts writes; every OpenCode-shaped endpoint is GET-only.
"""
import argparse
import base64
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=64647)
    args = parser.parse_args()
    state = {"error": 0, "messageError": 0, "revision": 0, "busy": True,
             "deleted": False, "delay": 0, "messages": 24, "auth": False, "fresh": False}
    metrics = {"requests": 0, "active": 0, "peak": 0, "paths": {}}
    lock = threading.Lock()

    def sessions():
        def row(sid, directory, title, parent=None, updated=None):
            return {"id": sid, "directory": directory, "title": title, "parentID": parent,
                    "time": {"created": 1000, "updated": int(time.time() * 1000) if updated is None else updated}}
        if state["deleted"]:
            return []
        return [row("ses_root", "/fixture/a", "OpenCode English preview", updated=None if state["fresh"] else 0),
                row("ses_root", "/fixture/中文", "OpenCode 中文預覽"),
                row("ses_child", "/fixture/a", "Read-only child", "ses_root"),
                row("ses_expired", "/fixture/a", "Expired idle (must not appear)", updated=0)]

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def reply(self, value, code=200, headers=None):
            body = json.dumps(value, ensure_ascii=False).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for key, value in (headers or {}).items():
                self.send_header(key, value)
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_POST(self):
            if self.path != "/__fixture":
                self.reply({"error": "read-only fixture"}, 405)
                return
            if int(self.headers.get("Content-Length", 0)) > 4096:
                self.reply({"error": "control too large"}, 413)
                return
            update = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
            with lock:
                for key, value in update.items():
                    if key in state:
                        state[key] = value
                result = dict(state)
            self.reply(result)

        def do_GET(self):
            parsed = urlparse(self.path)
            query = parse_qs(parsed.query)
            path = parsed.path
            if path == "/__metrics":
                with lock:
                    result = {**metrics, "state": dict(state)}
                self.reply(result)
                return
            with lock:
                metrics["requests"] += 1
                metrics["active"] += 1
                metrics["peak"] = max(metrics["peak"], metrics["active"])
                metrics["paths"][path] = metrics["paths"].get(path, 0) + 1
                current = dict(state)
            try:
                time.sleep(current["delay"])
                if current["auth"] and self.headers.get("Authorization") != "Basic " + base64.b64encode(b"opencode:fixture").decode():
                    self.reply({"error": "fixture authentication required"}, 401)
                    return
                if current["error"]:
                    self.reply({"error": "fixture outage"}, current["error"])
                    return
                if path == "/global/health":
                    self.reply({"healthy": True, "version": "fixture-2026-09-12"})
                    return
                if path == "/session":
                    self.reply(sessions())
                    return
                directory = query.get("directory", ["/fixture/a"])[0]
                if path == "/session/status":
                    self.reply({"ses_root": {"type": "busy"}} if current["busy"] and directory == "/fixture/a" else {})
                    return
                segments = path.strip("/").split("/")
                if len(segments) < 2 or segments[0] != "session":
                    self.reply({"error": "unknown fixture route"}, 404)
                    return
                sid = segments[1]
                detail = next((s for s in sessions() if s["id"] == sid and s["directory"] == directory), None)
                if not detail:
                    self.reply({"error": "not found"}, 404)
                    return
                if len(segments) == 2:
                    self.reply(detail)
                elif segments[2] == "children":
                    self.reply([s for s in sessions() if s["directory"] == directory and s["parentID"] == sid])
                elif segments[2] == "message":
                    if current["messageError"]:
                        self.reply({"error": "fixture conversation outage"}, current["messageError"])
                        return
                    count = current["messages"]
                    before = min(count, int(query.get("before", [str(count)])[0]))
                    limit = int(query.get("limit", ["20"])[0])
                    start = max(0, before - limit)
                    rows = [{"info": {"id": f"msg_{i}", "role": "user" if i % 2 == 0 else "assistant",
                                      "time": {"created": 1000 + i}},
                             "parts": [{"type": "text", "text": f"{i + 1}. English preview / 中文內容 · {directory} · live revision {current['revision']}"}]}
                            for i in range(start, before)]
                    self.reply(rows, headers={"X-Next-Cursor": str(start)} if start else None)
                else:
                    self.reply({"error": "unsupported"}, 404)
            finally:
                with lock:
                    metrics["active"] -= 1

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(json.dumps({"url": f"http://127.0.0.1:{server.server_port}", "fixture": True}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
