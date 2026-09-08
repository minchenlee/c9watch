#!/usr/bin/env python3
"""No-model integration test: actual Rust bridge + fake owner server + control socket."""
import asyncio, json, os, sys, tempfile
from pathlib import Path
from websockets.asyncio.server import unix_serve

THREAD='00000000-0000-4000-8000-000000000001'

def question(id):
    return {'id':id,'method':'item/tool/requestUserInput','params':{'threadId':THREAD,'turnId':'turn-test','itemId':id,'isBlocking':True,'questions':[
        {'id':'choice','header':'Mode','question':'Choose a test mode','options':[{'label':'Local','description':'On this computer'},{'label':'Remote','description':'Another host'}]},
        {'id':'details','header':'Details','question':'Test details','options':None}]}}

async def fake_server():
    socket=sys.argv[sys.argv.index('--listen')+1].removeprefix('unix://')
    async def client(ws):
        replies=[]
        async for raw in ws:
            value=json.loads(raw)
            if value.get('method')=='initialize':
                await ws.send(json.dumps({'id':value['id'],'result':{}}))
                await ws.send(json.dumps(question('q1')))
                await ws.send(json.dumps(question('q2')))
                await ws.send(json.dumps({'method':'thread/status/changed','params':{'threadId':THREAD,'status':{'type':'active','activeFlags':['waitingOnUserInput']}}}))
            elif value.get('method')=='test/actions':
                events=[
                    {'method':'turn/started','params':{'threadId':THREAD,'turn':{'id':'actions-turn'}}},
                    {'method':'turn/plan/updated','params':{'threadId':THREAD,'turnId':'actions-turn','explanation':'QA only','plan':[{'step':'Test controls','status':'inProgress'}]}},
                    {'method':'turn/diff/updated','params':{'threadId':THREAD,'turnId':'actions-turn','diff':'-old\n+new'}},
                    {'method':'item/started','params':{'threadId':THREAD,'turnId':'actions-turn','item':{'id':'file-item','type':'fileChange','changes':[{'path':'/tmp/qa.txt','kind':{'type':'update'},'diff':'-old\n+new'}]}}},
                    {'id':'command','method':'item/commandExecution/requestApproval','params':{'threadId':THREAD,'turnId':'actions-turn','itemId':'command-item','command':'echo QA_ONLY','cwd':'/tmp'}},
                    {'id':'file','method':'item/fileChange/requestApproval','params':{'threadId':THREAD,'turnId':'actions-turn','itemId':'file-item'}},
                    {'id':'permission','method':'item/permissions/requestApproval','params':{'threadId':THREAD,'turnId':'actions-turn','itemId':'permissions-item','permissions':{'network':{'enabled':True},'fileSystem':{'read':['/tmp/qa']}}}},
                    {'id':'form','method':'mcpServer/elicitation/request','params':{'threadId':THREAD,'turnId':'actions-turn','serverName':'qa','mode':'form','message':'QA only','requestedSchema':{'type':'object','properties':{'count':{'type':'integer','minimum':0},'ok':{'type':'boolean'}},'required':['count','ok']}}},
                    {'id':'url','method':'mcpServer/elicitation/request','params':{'threadId':THREAD,'turnId':'actions-turn','serverName':'qa','mode':'url','message':'QA URL - not opened','url':'https://example.com/qa','elicitationId':'url1'}}
                ]
                for event in events:await ws.send(json.dumps(event))
                await ws.send(json.dumps({'id':value['id'],'result':{}}))
            elif value.get('method')=='turn/interrupt':
                assert value['params']=={'threadId':THREAD,'turnId':'actions-turn'}
                await ws.send(json.dumps({'id':value['id'],'result':{}}))
                await ws.send(json.dumps({'method':'turn/completed','params':{'threadId':THREAD,'turn':{'id':'actions-turn','status':'interrupted'}}}))
            elif value.get('method')=='test/report':
                await ws.send(json.dumps({'id':value['id'],'result':replies}))
            elif value.get('method')=='test/cancel':
                await ws.send(json.dumps({'method':'turn/completed','params':{'threadId':THREAD,'turn':{'id':'turn-test','status':'interrupted'}}}))
                await ws.send(json.dumps({'id':value['id'],'result':{}}))
            elif 'result' in value:
                replies.append(value)
                await ws.send(json.dumps({'method':'serverRequest/resolved','params':{'threadId':THREAD,'requestId':value['id']}}))
    async with unix_serve(client,socket,compression=None):
        await asyncio.Future()

async def test(binary):
    with tempfile.TemporaryDirectory(prefix='c9-interaction-') as tmp:
        tmp=Path(tmp)
        fake=tmp/'fake-codex'
        fake.write_text(f'#!{sys.executable}\nimport runpy,sys\nsys.argv.insert(1,"--fake-server")\nrunpy.run_path({str(Path(__file__).resolve())!r},run_name="__main__")\n')
        fake.chmod(0o700)
        root=Path(f'/tmp/c9watch-codex-{os.geteuid()}')
        before=set(root.glob('*/ready'))
        proc=await asyncio.create_subprocess_exec(binary,'--codex-desktop-bridge',str(fake),'app-server','--stdio',stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
        runtime=None
        async def send(v):
            proc.stdin.write((json.dumps(v)+'\n').encode());await proc.stdin.drain()
        async def until(predicate):
            async with asyncio.timeout(5):
                while True:
                    raw=await proc.stdout.readline();assert raw,'bridge closed'
                    v=json.loads(raw)
                    if predicate(v):return v
        async def control(v):
            reader,writer=await asyncio.open_unix_connection(runtime/'interactions.sock')
            try:
                writer.write((json.dumps(v)+'\n').encode());await writer.drain()
                return json.loads(await asyncio.wait_for(reader.readline(),5))
            finally:
                writer.close();await writer.wait_closed()
        try:
            async with asyncio.timeout(10):
                while True:
                    fresh=set(root.glob('*/ready'))-before
                    if fresh:
                        runtime=next(iter(fresh)).parent
                        if (runtime/'interactions.sock').exists():break
                    await asyncio.sleep(.02)
            await send({'id':1,'method':'initialize','params':{}})
            await until(lambda v:v.get('method')=='thread/status/changed')
            snapshot=await control({'op':'snapshot'})
            assert len(snapshot['pending'])==2,snapshot
            assert snapshot['statuses'][THREAD]=='waiting'
            q1,q2=snapshot['pending']
            answers={'choice':{'answers':['Local']},'details':{'answers':['中文\nsecond line']}}
            reply=await control({'op':'answer','token':q1['token'],'threadId':THREAD,'answers':answers})
            assert reply['status']=='submitted',reply
            await until(lambda v:v.get('method')=='serverRequest/resolved')
            await send({'id':'q1','result':{'answers':answers}}) # late owner duplicate
            await send({'id':'q2','result':{'answers':answers}}) # owner wins second request
            await until(lambda v:v.get('method')=='serverRequest/resolved')
            reply=await control({'op':'answer','token':q2['token'],'threadId':THREAD,'answers':answers})
            assert reply['status']=='not_sent',reply
            await send({'id':2,'method':'test/report','params':{}})
            report=await until(lambda v:v.get('id')==2)
            assert [v['id'] for v in report['result']]==['q1','q2'],report
            assert report['result'][0]['result']['answers']==answers
            assert not (await control({'op':'snapshot'}))['pending']
            await send({'id':3,'method':'test/cancel','params':{}})
            await until(lambda v:v.get('id')==3)
            assert (await control({'op':'snapshot'}))['statuses'][THREAD]=='idle'
            await send({'id':4,'method':'test/actions','params':{}})
            await until(lambda v:v.get('id')==4)
            snapshot=await control({'op':'snapshot'})
            assert snapshot['turns'][THREAD]['plan'][0]['step']=='Test controls'
            assert snapshot['turns'][THREAD]['diff']=='-old\n+new'
            current={r['kind']+('URL' if r.get('details',{}).get('mode')=='url' else ''):r for r in snapshot['pending']}
            for kind,action,inputs in [('command','accept',{}),('file','accept',{'reviewed':True}),('permission','grant',{'selected':['read:0']}),('form','accept',{'content':{'count':0,'ok':False}}),('formURL','cancel',{})]:
                request=current[kind]
                result=await control({'op':'decide','token':request['token'],'threadId':THREAD,'action':action,'input':inputs})
                assert result['status']=='submitted',result
                await until(lambda v:v.get('method')=='serverRequest/resolved')
            await send({'id':'command','result':{'decision':'accept'}}) # late duplicate must be suppressed
            await send({'id':5,'method':'test/report','params':{}})
            report=(await until(lambda v:v.get('id')==5))['result']
            assert [v['id'] for v in report]==['q1','q2','command','file','permission','form','url'],report
            assert report[4]['result']=={'permissions':{'fileSystem':{'read':['/tmp/qa']}},'scope':'turn'}
            assert report[5]['result']=={'action':'accept','content':{'count':0,'ok':False}}
            assert report[6]['result']=={'action':'cancel','content':None}
            bad=await control({'op':'interrupt','threadId':THREAD,'turnId':'stale'})
            assert bad['status']=='not_sent'
            stopped=await control({'op':'interrupt','threadId':THREAD,'turnId':'actions-turn'})
            assert stopped['status']=='submitted',stopped
            await until(lambda v:v.get('method')=='turn/completed' and v['params']['turn']['id']=='actions-turn')
            snapshot=await control({'op':'snapshot'})
            assert snapshot['turns'][THREAD]['status']=='interrupted'
            assert not snapshot['pending']
            proc.stdin.close();await asyncio.wait_for(proc.wait(),8)
            assert proc.returncode==0,(await proc.stderr.read()).decode()
            assert not runtime.exists(),runtime
            print('PASS owner transport: questions, multi-answer payload, both race orders, resolved, command/file/permission/MCP decisions, stop/plan/diff, cancellation, EOF cleanup')
        finally:
            if proc.returncode is None:proc.kill();await proc.wait()

if __name__=='__main__':
    asyncio.run(fake_server() if '--fake-server' in sys.argv else test(str(Path(sys.argv[1]).resolve())))
