#!/usr/bin/env python3
"""Preview c9p/1: print session.view changes using only the Python stdlib."""
import argparse
import json
import os
import socket
import sys
from pathlib import Path


def default_socket():
    home = os.environ.get('C9WATCH_ENGINE_HOME')
    runtime = os.environ.get('XDG_RUNTIME_DIR', '')
    if not home and sys.platform != 'darwin' and os.path.isabs(runtime):
        return str(Path(runtime) / 'c9watch/engine-v1.sock')
    if not home:
        base = (Path.home() / 'Library/Application Support' if sys.platform == 'darwin'
                else Path(os.environ.get('XDG_STATE_HOME', Path.home() / '.local/state')))
        home = base / 'c9watch/engine'
    return str(Path(home) / 'engine-v1.sock')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--socket', default=default_socket())
    parser.add_argument('--since', help='last successfully processed cursor')
    parser.add_argument('--max-events', type=int, default=0, help='0 watches forever')
    args = parser.parse_args()
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(10)
        sock.connect(args.socket)
        stream = sock.makefile('rb')

        def read():
            line = stream.readline((1 << 20) + 1)
            if not line.endswith(b'\n') or len(line) - 1 > (1 << 20):
                raise RuntimeError('closed connection or oversized frame')
            frame = json.loads(line)
            if frame.get('v') != 1:
                raise RuntimeError('unsupported c9p major')
            if frame.get('ok') is False:
                error = frame.get('error', {})
                raise RuntimeError(f"{error.get('code')}: {error.get('msg')}")
            return frame

        def call(request_id, method, params):
            sock.sendall((json.dumps({'v': 1, 'id': request_id, 'method': method,
                                     'params': params}) + '\n').encode())
            frame = read()
            if frame.get('id') != request_id or frame.get('ok') is not True:
                raise RuntimeError('unexpected response')
            return frame['result']

        hello = call('1', 'hello', {'apiMajor': 1, 'apiMinor': 6,
                     'client': {'kind': 'example', 'name': 'python-watch'}, 'want': []})
        required = {'session.views', 'events.subscribe'}
        if required.intersection(hello.get('breakingSince', [])):
            raise RuntimeError('breaking preview change; update this example')
        if not required.issubset(hello.get('methods', {})):
            raise RuntimeError('core lacks session views; update core')
        snapshot = call('2', 'session.views', {})
        # Read the state first, then replay changes after its cursor (no race).
        call('3', 'events.subscribe', {'since': args.since or snapshot['cursor'],
                                     'topics': ['session.view']})
        sock.settimeout(None)
        count = 0
        while True:
            frame = read()
            if frame.get('type') != 'event' or frame.get('sub') != '3':
                continue
            if frame.get('topic') != 'session.view':
                continue
            print(json.dumps(frame, ensure_ascii=False), flush=True)
            # Persist frame['cursor'] after your processing succeeds.
            count += 1
            if args.max_events and count >= args.max_events:
                return


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
