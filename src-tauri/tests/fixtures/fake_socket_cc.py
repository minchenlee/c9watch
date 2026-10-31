#!/usr/bin/env python3
"""Synthetic CC 2.1.316 peer protocol for engine tests. No model/API calls.

Not a live capture. Shapes follow c1b/plan-m2.md and the 2026-10-31 note.
"""
import json
import os
import pathlib
import re
import select
import socket
import subprocess
import sys
import termios
import threading
import time
import tty

args = sys.argv[1:]
if args == ['--version']:
    print('2.1.316 (Claude Code)')
    sys.exit(0)

def arg(flag, default=''):
    return args[args.index(flag) + 1] if flag in args else default

sid = arg('--session-id', arg('--resume'))
settings = pathlib.Path(arg('--settings'))
home = settings.parent.parent
bindir = pathlib.Path(__file__).parent
agent = os.environ['C9_AGENT_ID']
sessions = pathlib.Path(os.environ.get('C9_CC_SESSIONS_DIR', str(bindir.parent / 'cc-sessions')))
sockdir = pathlib.Path(os.environ.get('C9_CC_SOCK_DIR', str(bindir.parent / 'socks')))
sessions.mkdir(parents=True, exist_ok=True)
transcript = bindir / (agent + '.jsonl')
transcript.touch()
lock = threading.Lock()

def hook(kind, **kw):
    data = dict(session_id=sid, transcript_path=str(transcript), **kw)
    subprocess.run([str(home / 'bin/c9watch-agent'), 'hook', kind, '--kind', 'claude'],
                   input=json.dumps(data).encode(), stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)

def append(entry):
    with lock, transcript.open('a') as f:
        f.write(json.dumps(entry) + '\n')
        f.flush()

def draw(text='Try "fix lint errors"'):
    rule = '─' * 40
    os.write(1, ('\033]0;✳ Claude Code\007\033[2J\033[H' + rule + '\r\n❯ ' + text + '\r\n' + rule + '\r\n  auto mode on\r\n  1 agent\r\n').encode())

def send_to(addr, frame):
    # The c9watch return address uses CC's percent-encoded uds form.
    from urllib.parse import unquote
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(4)
        s.connect(unquote(addr.removeprefix('uds:')))
        s.sendall((json.dumps(frame) + '\n').encode())
        time.sleep(.15)
        s.shutdown(socket.SHUT_WR)
        while s.recv(8192):
            pass

def receipt(frame, status, **kw):
    send_to(frame['from'], dict(type='control', action='peer_message_status', orig_msg_id=frame['msg_id'], status=status, **kw))

def process(body):
    with lock, (bindir / ('in-' + agent + '.log')).open('a') as f:
        f.write(body + '\n')
    hook('prompt-submit', prompt=body)
    match = re.search(r'^forward to (\S+)(?: with (.*))?$', body, re.M)
    if match:
        with (bindir / ('fwd-' + agent + '.out')).open('ab') as f:
            subprocess.run([str(bindir / 'c9watch'), 'send', match[1], match[2] or ('forwarded by ' + agent)], stdout=f, stderr=f)
    match = re.search(r'^reply to (\S+) about (\S+)', body, re.M)
    if match:
        with (bindir / ('reply-' + agent + '.out')).open('ab') as f:
            subprocess.run([str(bindir / 'c9watch'), 'send', match[1], '--reply-to', match[2], 'reply by ' + agent], stdout=f, stderr=f)
    hook('stop')
    draw()

def finish(frame, mode):
    body = frame['message']['content']
    if mode == 'no-evidence':
        return
    if mode.startswith('late-'):
        time.sleep(float(mode[5:]))
    append(dict(type='queue-operation', operation='enqueue', content=body))
    if mode == 'enqueue-only':
        return
    if mode.startswith('delay-'):
        time.sleep(float(mode[6:]))
    append(dict(type='user', turnOrigin='peer', origin=dict(kind='peer', msg_id=frame['msg_id'], **{'from': frame['from']}), uuid=frame['uuid'], message=dict(role='user', content=body)))
    # A real CC may not fire this hook mid-turn. The JSONL is independent.
    if mode != 'jsonl-only':
        process(body)
    if 'use SendMessage' in body:
        for _ in range(2):
            send_to(frame['from'], dict(type='user', message=dict(role='user', content='synthetic SendMessage reply')))

listener = socket.socket(socket.AF_UNIX)
path = sockdir / (str(os.getpid()) + '.sock')
listener.bind(str(path))
os.chmod(path, 0o600)
listener.listen()
pid_data = dict(sessionId=sid, messagingSocketPath=str(path))
if not (bindir / 'no-peer-protocol').exists():
    pid_data['peerProtocol'] = 1
(sessions / (str(os.getpid()) + '.json')).write_text(json.dumps(pid_data))

def serve():
    while True:
        conn, _ = listener.accept()
        try:
            conn.settimeout(5)
            data = bytearray()
            while True:
                chunk = conn.recv(8192)
                if not chunk:
                    break
                data.extend(chunk)
            frame = json.loads(data)
            modefile = bindir / ('socket-mode-' + agent)
            mode = modefile.read_text().strip() if modefile.exists() else 'normal'
            with lock, (bindir / ('frames-' + agent + '.jsonl')).open('a') as f:
                f.write(json.dumps(frame) + '\n')
            if mode == 'held':
                receipt(frame, 'held', status_detail='synthetic hold')
                conn.close()
                while not (bindir / ('release-' + agent)).exists():
                    time.sleep(.05)
                receipt(frame, 'delivered')
                finish(frame, 'normal')
            elif mode.startswith('dropped-'):
                receipt(frame, 'dropped', drop_reason=mode[8:])
            elif mode in ('denied', 'expired'):
                receipt(frame, mode, status_detail='refused')
            else:
                conn.close()  # Deliberately let evidence race the sender result.
                finish(frame, mode)
        except Exception as e:
            with (bindir / 'socket-errors.log').open('a') as f:
                f.write(repr(e) + '\n')
        finally:
            conn.close()

threading.Thread(target=serve, daemon=True).start()
hook('session-start', source='resume' if '--resume' in args else 'startup')
tty.setraw(0)
draw()
buf = b''
while True:
    if not select.select([0], [], [], .2)[0]:
        continue
    chunk = os.read(0, 4096)
    if not chunk:
        break
    buf += chunk
    if b'\x1b[201~' not in buf:
        continue
    body = buf.split(b'\x1b[200~', 1)[-1].split(b'\x1b[201~', 1)[0].decode()
    if b'\r' not in buf.split(b'\x1b[201~', 1)[1]:
        draw(body.splitlines()[0])
        continue
    buf = b''
    process(body)
