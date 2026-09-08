#!/usr/bin/env python3
"""Isolated bridge checks. --live additionally performs short model turns."""
import argparse
import asyncio
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
from websockets.asyncio.client import unix_connect

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bridge", HERE / "codex-desktop-bridge.py")
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class Client:
    def __init__(self, read, write):
        self.read, self.write = read, write
        self.pending = {}
        self.events = asyncio.Queue(maxsize=1024)
        self.counter = 0
        self.task = asyncio.create_task(self.pump())

    async def pump(self):
        try:
            while raw := await self.read():
                value = json.loads(raw)
                if "method" not in value and value.get("id") in self.pending:
                    self.pending.pop(value["id"]).set_result(value)
                else:
                    await self.events.put(value)
        except Exception as e:
            for future in self.pending.values():
                if not future.done(): future.set_exception(e)

    async def call(self, method, params):
        self.counter += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.counter] = future
        await self.write(json.dumps({"id": self.counter, "method": method, "params": params}))
        value = await asyncio.wait_for(future, 15)
        assert "error" not in value, value
        return value["result"]

    async def initialize(self, name):
        await self.call("initialize", {"clientInfo": {"name": name, "version": "0.1.0"}})
        await self.write(json.dumps({"method": "initialized"}))


async def framing_checks():
    reader = asyncio.StreamReader()
    frames = []
    class WS:
        async def send(self, text): frames.append(text)
    original = '{"id":17,"result":{"decision":"decline"},"note":"中文\u2028`$()`"}'
    data = (original + '\r\n').encode()
    for chunk in [data[:5], data[5:25], data[25:]]: reader.feed_data(chunk)
    reader.feed_eof()
    await bridge.relay_input(reader, WS())
    assert frames == [original]
    assert bridge.server_args(['-c','model="gpt-5.4-mini"','app-server','--stdio','-c','example="a b"'], '/tmp/test.sock') == ['-c','model="gpt-5.4-mini"','app-server','-c','example="a b"','--listen','unix:///tmp/test.sock']
    truncated = asyncio.StreamReader(); truncated.feed_data(b'{"id":1}'); truncated.feed_eof()
    try: await bridge.relay_input(truncated, WS())
    except ValueError: pass
    else: raise AssertionError("EOF fragment was accepted")
    print('PASS argument preservation, fragmented UTF-8/CRLF, approval-decision frame unchanged, incomplete EOF rejected', flush=True)


async def completion(client, thread_id, turn_id, marker):
    text = ''
    async with asyncio.timeout(60):
        while True:
            value = await client.events.get()
            p = value.get('params', {})
            if p.get('threadId') != thread_id: continue
            if value.get('method') == 'item/agentMessage/delta': text += p.get('delta', '')
            if value.get('method') == 'turn/completed' and p['turn']['id'] == turn_id:
                assert p['turn']['status'] == 'completed', p['turn'].get('error')
                assert marker in text, text
                return


async def run(live, approval=False, rust_binary=None, kill_bridge=False, image_test=False):
    await framing_checks()
    root = Path(tempfile.mkdtemp(prefix='c9b-', dir='/tmp'))
    runtime = root / 'run'
    stderr = open(root / 'stderr.log', 'wb')
    endpoints_root = Path(f'/tmp/c9watch-codex-{os.geteuid()}')
    before = set(endpoints_root.glob('*/ready'))
    command = ([str(Path(rust_binary).resolve()), '--codex-desktop-bridge', '/Applications/ChatGPT.app/Contents/Resources/codex'] if rust_binary else
        [sys.executable, str(HERE / 'codex-desktop-bridge.py'), '--binary', '/Applications/ChatGPT.app/Contents/Resources/codex', '--runtime-dir', str(runtime), '--'])
    process = await asyncio.create_subprocess_exec(*command, '-c', 'model="gpt-5.4-mini"', 'app-server', '--stdio',
        cwd=root, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=stderr, limit=bridge.MAX_RECORD + 1)
    clients = []
    child_pid = None
    try:
        async with asyncio.timeout(15):
            while True:
                if rust_binary:
                    fresh = set(endpoints_root.glob('*/ready')) - before
                    if len(fresh) == 1:
                        marker = fresh.pop(); runtime = marker.parent
                        ready = {'socket': str(runtime / 'server.sock'), 'pid': int(marker.read_text())}
                        break
                elif (runtime / 'ready.json').exists():
                    ready = json.loads((runtime / 'ready.json').read_text()); break
                assert process.returncode is None, f'Bridge failed: see {root}/stderr.log'
                await asyncio.sleep(.05)
        child_pid = ready['pid']
        assert stat.S_IMODE(runtime.stat().st_mode) == 0o700
        assert stat.S_IMODE(Path(ready['socket']).stat().st_mode) == 0o600
        async def write_stdio(text):
            process.stdin.write((text+'\n').encode()); await process.stdin.drain()
        owner = Client(process.stdout.readline, write_stdio); clients.append(owner)
        async with unix_connect(ready['socket'], uri='ws://localhost/', compression=None) as ws:
            sender = Client(ws.recv, ws.send); clients.append(sender)
            await asyncio.gather(owner.initialize('c9watch_owner_probe'), sender.initialize('c9watch_sender_probe'))
            # Both clients reuse ID 2; they must receive their own responses.
            config, loaded = await asyncio.gather(owner.call('config/read', {}), sender.call('thread/loaded/list', {}))
            assert config['config']['model'] == 'gpt-5.4-mini'
            assert loaded['data'] == []
            print('PASS installed Codex: isolated server, inherited config, two client ID spaces, 0700/0600 permissions', flush=True)
            if live:
                thread = await owner.call('thread/start', {'ephemeral':True, 'cwd':str(root), 'sandbox':'read-only', 'approvalPolicy':'on-request',
                    'developerInstructions':'Transport test only. Do not use tools. Reply with the requested marker.'})
                tid = thread['thread']['id']
                assert tid in (await sender.call('thread/loaded/list', {}))['data']
                for marker in ['BRIDGE_FIRST_OK', 'BRIDGE_SECOND_OK']:
                    turn = await sender.call('turn/start', {'threadId':tid,'input':[{'type':'text','text':f'Reply exactly {marker}. No tools.'}]})
                    await completion(owner, tid, turn['turn']['id'], marker)
                print('PASS two messages from secondary client; original stdio owner sees both same-session replies', flush=True)
                first = await sender.call('turn/start', {'threadId':tid,'input':[{'type':'text','text':'Write three short sentences about testing. No tools.'}]})
                second = await sender.call('turn/start', {'threadId':tid,'input':[{'type':'text','text':'Include BRIDGE_STEER_OK in your reply. No tools.'}]})
                assert first['turn']['id'] == second['turn']['id']
                await completion(owner, tid, first['turn']['id'], 'BRIDGE_STEER_OK')
                print('PASS active steering stays in same turn and reaches original stdio owner', flush=True)
            if image_test:
                import base64, struct, zlib
                def chunk(kind, data):
                    return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
                png = b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',16,16,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\xff\0\0'*16)*16))+chunk(b'IEND',b'')
                thread = await owner.call('thread/start', {'ephemeral':True, 'cwd':str(root), 'sandbox':'read-only', 'approvalPolicy':'never', 'developerInstructions':'Describe the supplied image. Do not use tools.'})
                tid=thread['thread']['id']
                turn=await sender.call('turn/start',{'threadId':tid,'input':[{'type':'text','text':'What is the dominant color? Reply with one lowercase English color word. No tools.'},{'type':'image','url':'data:image/png;base64,'+base64.b64encode(png).decode()}]})
                await completion(owner,tid,turn['turn']['id'],'red')
                print('PASS native image input: original owner receives correct image color reply',flush=True)
            if approval:
                thread = await owner.call('thread/start', {'ephemeral':True, 'cwd':str(root), 'sandbox':'read-only', 'approvalPolicy':'on-request', 'approvalsReviewer':'user',
                    'developerInstructions':'This is an approval routing test. Make exactly one shell tool call, requesting require_escalated permission, to run printf BRIDGE_APPROVAL_ONLY. Do not run any other command or modify any file. If denied, stop and say declined.'})
                tid = thread['thread']['id']
                approval_turn = await sender.call('turn/start', {'threadId':tid,'input':[{'type':'text','text':'Request user approval to run printf BRIDGE_APPROVAL_ONLY outside the sandbox, with require_escalated. This is solely to test permission routing. If denied, stop.'}]})
                async def approval_event(client):
                    async with asyncio.timeout(50):
                        while True:
                            v = await client.events.get()
                            if 'id' in v and 'method' in v: return v
                waits = {asyncio.create_task(approval_event(owner)):('stdio-owner',owner), asyncio.create_task(approval_event(sender)):('secondary-sender',sender)}
                done, pending = await asyncio.wait(waits, return_when=asyncio.FIRST_COMPLETED)
                receivers = []
                for task in done:
                    v = task.result(); name, client = waits[task]; receivers.append(name)
                    print('Approval received:',name,v.get('method'),flush=True)
                    assert v['method'] == 'item/commandExecution/requestApproval', v['method']
                    await client.write(json.dumps({'id':v['id'],'result':{'decision':'decline'}}))
                done2, pending2 = await asyncio.wait(pending,timeout=1)
                for task in done2:
                    v=task.result(); name,client=waits[task];receivers.append(name)
                    print('Approval also received:',name,v.get('method'),flush=True)
                    await client.write(json.dumps({'id':v['id'],'result':{'decision':'decline'}}))
                for task in pending2: task.cancel()
                await asyncio.gather(*pending2,return_exceptions=True)
                print('Approval routing observation:',','.join(receivers),flush=True)
                assert receivers == ['stdio-owner'], receivers
                await completion(owner, tid, approval_turn['turn']['id'], 'declined')
                print('PASS original owner declines approval; turn completes normally', flush=True)
        if kill_bridge:
            assert rust_binary, 'Crash cleanup check requires Rust bridge'
            process.kill()
        else:
            process.stdin.close()
        await asyncio.wait_for(process.wait(), 8)
        assert process.returncode == (-9 if kill_bridge else 0)
        async with asyncio.timeout(6):
            while runtime.exists(): await asyncio.sleep(.05)
        assert not runtime.exists()
        # SIGKILL delivery and orphan reaping can finish just after the bridge exits.
        async with asyncio.timeout(6):
            while True:
                try: os.kill(child_pid, 0)
                except ProcessLookupError: break
                await asyncio.sleep(.05)
        print('PASS ' + ('SIGKILL' if kill_bridge else 'stdin EOF') + ' cleans up owned server and endpoint', flush=True)
    finally:
        for client in clients: client.task.cancel()
        await asyncio.gather(*(c.task for c in clients), return_exceptions=True)
        if process.returncode is None:
            process.terminate(); await asyncio.wait_for(process.wait(), 8)
        stderr.close()
        print(f'Diagnostic directory: {root}', flush=True)


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--live',action='store_true');parser.add_argument('--approval',action='store_true');parser.add_argument('--rust-binary');parser.add_argument('--kill-bridge',action='store_true');parser.add_argument('--image',action='store_true')
    asyncio.run(run(parser.parse_args().live, parser.parse_args().approval, parser.parse_args().rust_binary, parser.parse_args().kill_bridge, parser.parse_args().image))
