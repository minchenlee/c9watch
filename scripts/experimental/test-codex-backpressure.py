#!/usr/bin/env python3
"""No-model stall/cleanup measurements for the actual Rust Desktop bridge.

Usage: python3 scripts/experimental/test-codex-backpressure.py PATH_TO_BINARY
Every child, socket and generated executable belongs to this disposable fixture.
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


async def fake_server():
    from websockets.asyncio.server import unix_serve
    socket = sys.argv[sys.argv.index('--listen') + 1].removeprefix('unix://')
    if os.environ['C9WATCH_QA_STALL'] == 'handshake':
        async def idle(reader, writer):
            await asyncio.sleep(30)
        async with await asyncio.start_unix_server(idle, socket):
            await asyncio.Future()
    else:
        async def flood(ws):
            frame = json.dumps({'method': 'test/large-notification', 'params': {'text': 'x' * (512 * 1024)}})
            try:
                for _ in range(128):
                    await ws.send(frame)
            except Exception:
                pass
            await asyncio.sleep(30)
        async with unix_serve(flood, socket, compression=None):
            await asyncio.Future()


def test(binary):
    outcomes = []
    with tempfile.TemporaryDirectory(prefix='c9-stall-') as tmp:
        tmp = Path(tmp)
        fake = tmp / 'fake-codex'
        fake.write_text(f'#!{sys.executable}\nimport runpy,sys\nsys.argv.insert(1,"--fake-server")\nrunpy.run_path({str(Path(__file__).resolve())!r},run_name="__main__")\n')
        fake.chmod(0o700)
        root = Path(f'/tmp/c9watch-codex-{os.geteuid()}')
        for mode in ['stdout', 'handshake']:
            before = set(root.glob('*/owner'))
            env = {**os.environ, 'C9WATCH_QA_STALL': mode}
            started = time.monotonic()
            proc = subprocess.Popen([binary, '--codex-desktop-bridge', str(fake), 'app-server', '--stdio'],
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
            runtime = None
            peak_rss_kib = 0
            try:
                while proc.poll() is None and time.monotonic() - started < 7:
                    fresh = set(root.glob('*/owner')) - before
                    if fresh:
                        runtime = next(iter(fresh)).parent
                    rss = subprocess.run(['ps', '-o', 'rss=', '-p', str(proc.pid)], capture_output=True, text=True)
                    if rss.returncode == 0 and rss.stdout.strip():
                        peak_rss_kib = max(peak_rss_kib, int(rss.stdout.strip()))
                    time.sleep(.05)
                bounded_exit = proc.poll() is not None
                elapsed = time.monotonic() - started
                if proc.poll() is None:
                    proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
                error = proc.stderr.read().decode()
                for _ in range(100):
                    if runtime is None or not runtime.exists():
                        break
                    time.sleep(.02)
                result = {'scenario': mode, 'boundedExit': bounded_exit, 'elapsedSeconds': round(elapsed, 3),
                          'runtimeDirectory': str(runtime) if runtime else None,
                          'peakBridgeRssKiB': peak_rss_kib, 'runtimeRemoved': runtime is not None and not runtime.exists(),
                          'exitCode': proc.returncode, 'error': error.strip()}
                print(json.dumps(result), flush=True)
                outcomes.append(result)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)
                for pipe in [proc.stdin, proc.stdout, proc.stderr]:
                    pipe.close()
    assert all(r['boundedExit'] and r['runtimeRemoved'] and 'timed out' in r['error'] for r in outcomes), outcomes


if __name__ == '__main__':
    if '--fake-server' in sys.argv:
        asyncio.run(fake_server())
    else:
        test(str(Path(sys.argv[1]).resolve()))
