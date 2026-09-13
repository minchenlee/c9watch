#!/usr/bin/env python3
"""Experimental transport only. NOT a drop-in CODEX_CLI_PATH launcher yet.

Requires Python 3.11+ and websockets. Keeps original CLI arguments, except
replacing app-server's stdio listener with a private Unix WebSocket listener.
Never synthesizes initialize, thread identity, approval, or user messages.
"""
import argparse
import asyncio
import contextlib
import json
import os
from pathlib import Path
import signal
import sys

from websockets.asyncio.client import unix_connect

MAX_RECORD = 8 * 1024 * 1024


def server_args(original, socket):
    # This prototype accepts exactly app-server launches, not daemon/subcommands.
    index = original.index("app-server")
    before, after = original[:index], original[index + 1:]
    result = []
    i = 0
    while i < len(after):
        arg = after[i]
        if arg == "--listen":
            if i + 1 >= len(after) or after[i + 1] != "stdio://":
                raise ValueError("Only the default stdio listener can be replaced")
            i += 2
            continue
        if arg == "--stdio" or arg == "--listen=stdio://":
            i += 1
            continue
        if arg.startswith("--listen=") or arg in {"daemon", "proxy", "generate-ts", "generate-json-schema"}:
            raise ValueError("Unsupported app-server invocation")
        result.append(arg)
        i += 1
    return before + ["app-server"] + result + ["--listen", "unix://" + str(socket)]


async def relay_input(reader, ws):
    pending = bytearray()
    while chunk := await reader.read(65536):
        pending.extend(chunk)
        while (end := pending.find(b"\n")) >= 0:
            if end > MAX_RECORD:
                raise ValueError("JSONL record exceeds size limit")
            line = bytes(pending[:end]).removesuffix(b"\r")
            del pending[:end + 1]
            if line.strip():
                await ws.send(line.decode("utf-8"))
        if len(pending) > MAX_RECORD:
            raise ValueError("JSONL record exceeds size limit")
    if pending.strip():
        raise ValueError("Incomplete JSONL record at EOF")


async def relay_output(ws, writer):
    async for message in ws:
        if not isinstance(message, str):
            raise ValueError("Unexpected binary frame")
        # Codex frames contain one JSON object; serialize framing without changing content.
        json.loads(message)
        writer.write(message.encode("utf-8") + b"\n")
        await writer.drain()


async def run(args):
    runtime = Path(args.runtime_dir)
    runtime.mkdir(mode=0o700)  # Fail on existing path; no stale or shared endpoint reuse.
    socket = runtime / "server.sock"
    child = None
    tasks = []
    try:
        original = args.args[1:] if args.args[:1] == ["--"] else args.args
        command = server_args(original, socket)
        child = await asyncio.create_subprocess_exec(args.binary, *command,
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.DEVNULL,
            start_new_session=True)
        async with asyncio.timeout(10):
            while not socket.exists():
                if child.returncode is not None:
                    raise RuntimeError("Codex exited before opening its listener")
                await asyncio.sleep(.025)
        os.chmod(socket, 0o600)
        async with unix_connect(str(socket), uri="ws://localhost/", compression=None,
                                max_size=MAX_RECORD, max_queue=4) as ws:
            ready = runtime / "ready.json"
            ready.write_text(json.dumps({"socket": str(socket), "pid": child.pid}))
            loop = asyncio.get_running_loop()
            reader = asyncio.StreamReader(limit=MAX_RECORD + 1)
            await loop.connect_read_pipe(lambda: asyncio.StreamReaderProtocol(reader), sys.stdin.buffer)
            transport, protocol = await loop.connect_write_pipe(asyncio.streams.FlowControlMixin, sys.stdout.buffer)
            writer = asyncio.StreamWriter(transport, protocol, None, loop)
            tasks = [asyncio.create_task(relay_input(reader, ws)),
                     asyncio.create_task(relay_output(ws, writer)),
                     asyncio.create_task(child.wait())]
            done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            for task in done:
                task.result()
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        if child is not None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(child.pid, signal.SIGTERM)
            try:
                await asyncio.wait_for(child.wait(), 3)
            except asyncio.TimeoutError:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(child.pid, signal.SIGKILL)
                await child.wait()
        for file in [runtime / "ready.json", socket]:
            file.unlink(missing_ok=True)
        runtime.rmdir()


async def entry(args):
    task = asyncio.create_task(run(args))
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, task.cancel)
    with contextlib.suppress(asyncio.CancelledError):
        await task


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--runtime-dir", required=True)
    parser.add_argument("args", nargs=argparse.REMAINDER)
    try:
        asyncio.run(entry(parser.parse_args()))
    except Exception as error:
        print(f"c9watch bridge: {error}", file=sys.stderr)
        sys.exit(1)
