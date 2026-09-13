#!/usr/bin/env python3
"""Read-only latency probe for an already running, separately verified QA app.

Obtain its ephemeral token from the native Mobile panel and set
C9WATCH_QA_WS_TOKEN. Verify that the expected QA PID owns TCP 9210 first.
Only loopback getConversation requests are sent; no model/session mutations.
No token or conversation content is written to the measurement output.
"""
import argparse
import asyncio
import hashlib
import json
import math
import os
import re
import statistics
import time
import uuid

from websockets.asyncio.client import connect


async def measure(session_id, samples):
    token = os.environ.get("C9WATCH_QA_WS_TOKEN", "")
    if not re.fullmatch(r"[a-fA-F0-9]{32}", token):
        raise ValueError("C9WATCH_QA_WS_TOKEN must be the current QA app token")
    timings = []
    digest = None
    size = count = progress = 0
    async with connect(
        "ws://127.0.0.1:9210/ws?token=" + token,
        open_timeout=5, close_timeout=2, max_size=16 * 1024 * 1024,
        max_queue=4, compression=None,
    ) as ws:
        for request_id in range(1, samples + 1):
            start = time.perf_counter()
            await ws.send(json.dumps({
                "type": "getConversation", "requestId": request_id,
                "sessionId": session_id, "provider": "codex", "includeTools": False,
            }))
            async with asyncio.timeout(10):
                while True:
                    response = json.loads(await ws.recv())
                    if response.get("type") == "conversationProgress":
                        progress += 1
                        continue
                    if response.get("requestId") != request_id:
                        continue
                    if response.get("type") != "conversation":
                        raise RuntimeError("Expected a correlated successful conversation response")
                    data = response["data"]
                    if data.get("sessionId") != session_id or data.get("provider") != "codex":
                        raise RuntimeError("Conversation identity mismatch")
                    timings.append((time.perf_counter() - start) * 1000)
                    payload = json.dumps(data, sort_keys=True, ensure_ascii=False).encode()
                    current = hashlib.sha256(payload).hexdigest()
                    if digest is not None and current != digest:
                        raise RuntimeError("Fixture changed during measurement; comparison invalid")
                    digest, size, count = current, len(payload), len(data["messages"])
                    break
    warm = timings[1:]
    return {
        "transport": "native-app-loopback-websocket", "samples": samples,
        "sessionId": session_id, "messages": count, "responseBytes": size,
        "contentSha256": digest, "firstMs": timings[0],
        "warmMedianMs": statistics.median(warm),
        "warmP95Ms": sorted(warm)[math.ceil(len(warm) * .95) - 1],
        "warmMaxMs": max(warm), "allMs": timings,
        "progressFrames": progress, "connectionClosed": True,
        "scope": "backend parse/serialize/transport, excludes WebKit render and OS cold-cache claims",
    }


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("session_id", type=lambda value: str(uuid.UUID(value)))
    parser.add_argument("--samples", type=int, default=20)
    args = parser.parse_args()
    if not 2 <= args.samples <= 100:
        parser.error("samples must be between 2 and 100")
    async with asyncio.timeout(90):
        result = await measure(args.session_id, args.samples)
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except Exception as error:
        # Never include a URL/token or response body in a failed diagnostic.
        raise SystemExit("Native history benchmark failed: " + type(error).__name__) from None
