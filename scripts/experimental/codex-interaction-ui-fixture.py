#!/usr/bin/env python3
"""Temporary, local-only native UI fixture. Never contacts Codex or writes transcripts.
Pass a visible Codex thread UUID to project synthetic cards onto that session in QA.
"""
import asyncio,json,os,sys,uuid,signal,base64
from pathlib import Path
from websockets.asyncio.server import unix_serve

async def main():
    thread=str(uuid.UUID(sys.argv[1]))
    root=Path(f'/tmp/c9watch-codex-{os.geteuid()}');root.mkdir(mode=0o700,exist_ok=True)
    directory=root/str(uuid.uuid4());directory.mkdir(mode=0o700)
    token=str(uuid.uuid4())
    request={'token':token,'threadId':thread,'turnId':'native-ui-fixture','kind':'question','summary':'QA fixture: choose and type','submitted':False,'answerable':True,'questions':[
        {'id':'mode','header':'Mode','question':'QA fixture — choose a test mode','options':[{'label':'Local','description':'Test only; no real Codex request'},{'label':'Remote','description':'Alternative test choice'}],'isOther':True,'isSecret':False},
        {'id':'details','header':'Details','question':'QA fixture — enter a short answer','options':[],'isOther':False,'isSecret':False}]}
    def card(kind,details,actions):
        return {'token':str(uuid.uuid4()),'threadId':thread,'turnId':'native-ui-fixture','kind':kind,'summary':'QA fixture — no real Codex operation','submitted':False,'answerable':False,'questions':[],'details':details,'actions':actions}
    approval=card('command',{'command':'echo QA_ONLY','cwd':'/tmp','reason':'QA fixture — no command will execute'},['accept','decline','cancel'])
    file=card('file',{'changes':[{'path':'/tmp/qa.txt','kind':{'type':'update'},'diff':'-old\n+new'}]},['accept','decline','cancel'])
    permission=card('permission',{'choices':[{'id':'network','label':'Network access'},{'id':'read:0','label':'read: /tmp/qa'}],'permissions':{'network':{'enabled':True},'fileSystem':{'read':['/tmp/qa']}}},['grant','deny'])
    form=card('form',{'serverName':'QA fixture','mode':'form','message':'Test form — no remote submission','supportedForm':True,'requestedSchema':{'type':'object','required':['label','count','ok'],'properties':{'label':{'type':'string','minLength':1},'count':{'type':'integer','minimum':0},'ok':{'type':'boolean'},'mode':{'type':'string','oneOf':[{'const':'local','title':'Local fixture'},{'const':'remote','title':'Remote fixture'}]}}}},['accept','decline','cancel'])
    url=card('form',{'serverName':'QA fixture','mode':'url','message':'URL fixture — opening is optional','url':'https://example.com/','safeUrl':True},['accept','decline','cancel'])
    empty=card('form',{'serverName':'QA fixture','mode':'form','message':'Zero-field fixture — no remote submission','supportedForm':True,'requestedSchema':{'type':'object','properties':{}}},['accept','decline','cancel'])
    state={'endpoint':directory.name,'connected':True,'pending':[request,approval,file,permission,form,url],'statuses':{thread:'waiting'},'overflow':False,'turns':{thread:{'turnId':'native-ui-fixture','status':'inProgress','stopping':False,'plan':[{'step':'Review synthetic cards','status':'inProgress'},{'step':'Check clearing and stop','status':'pending'}],'explanation':'QA fixture only','diff':'-old\n+new','diffTruncated':False}}}
    state['pending'].append(empty)
    (directory/'image.png').write_bytes((Path(__file__).resolve().parents[2]/'src-tauri/icons/32x32.png').read_bytes())
    stop=asyncio.Event()
    async def messaging(ws):
        async for raw in ws:
            value=json.loads(raw)
            method=value.get('method')
            if method=='initialize':result={}
            elif method=='initialized':continue
            elif method=='thread/loaded/list':result={'data':[thread],'nextCursor':None}
            elif method in ['turn/start','turn/steer'] and value.get('params',{}).get('threadId')==thread:
                if method=='turn/steer' and value['params'].get('expectedTurnId')!='native-ui-fixture':
                    await ws.send(json.dumps({'id':value['id'],'error':{'code':-32600,'message':'QA turn changed'}}));continue
                with (directory/'messages.jsonl').open('a') as log:log.write(json.dumps(value,ensure_ascii=False)+'\n')
                result={'turnId':'native-ui-fixture'} if method=='turn/steer' else {'turn':{'id':'native-ui-fixture'}}
            else:
                await ws.send(json.dumps({'id':value.get('id'),'error':{'code':-32601,'message':'QA fixture rejects unsupported operation'}}));continue
            await ws.send(json.dumps({'id':value['id'],'result':result}))
    async def client(reader,writer):
        try:
            value=json.loads(await asyncio.wait_for(reader.readline(),3))
            if value.get('op')=='snapshot': result=state
            elif value.get('op')=='answer' and value.get('token')==token and value.get('threadId')==thread:
                assert set(value['answers'])=={'mode','details'}
                (directory/'answer.json').write_text(json.dumps(value['answers'],ensure_ascii=False))
                state['pending']=[p for p in state['pending'] if p['token']!=token]
                result={'status':'submitted','detail':'QA answer received; no message was sent to Codex.'}
            elif value.get('op')=='decide' and value.get('threadId')==thread:
                target=next((p for p in state['pending'] if p['token']==value.get('token')),None)
                if target and value.get('action') in target.get('actions',[]):
                    with (directory/'decisions.jsonl').open('a') as log:log.write(json.dumps(value,ensure_ascii=False)+'\n')
                    state['pending']=[p for p in state['pending'] if p['token']!=target['token']]
                    result={'status':'submitted','detail':'QA decision received; nothing was sent to Codex.'}
                else:result={'status':'not_sent','detail':'QA request is no longer pending'}
            elif value.get('op')=='interrupt' and value.get('threadId')==thread and value.get('turnId')=='native-ui-fixture':
                state['turns'][thread]['status']='interrupted';state['pending']=[];state['statuses'][thread]='idle'
                result={'status':'submitted','detail':'QA turn stopped; no real turn was interrupted.'}
            else:result={'status':'not_sent','detail':'QA fixture rejected the request'}
            writer.write((json.dumps(result)+'\n').encode());await writer.drain()
        finally:writer.close()
    server=await unix_serve(messaging,directory/'server.sock',compression=None,max_size=8*1024*1024)
    control=await asyncio.start_unix_server(client,directory/'interactions.sock')
    for name in ['server.sock','interactions.sock']:os.chmod(directory/name,0o600)
    (directory/'ready').write_text(str(os.getpid()))
    print(str(directory),flush=True)
    loop=asyncio.get_running_loop()
    for sig in (signal.SIGTERM,signal.SIGINT):loop.add_signal_handler(sig,stop.set)
    try:
        await asyncio.wait_for(stop.wait(),1200)
    finally:
        server.close();control.close();await server.wait_closed();await control.wait_closed()
        # Preserve the synthetic answer evidence, remove only this fixture's sockets/marker.
        for name in ['ready','server.sock','interactions.sock']:(directory/name).unlink(missing_ok=True)
        if not any(directory.iterdir()):directory.rmdir()
if __name__=='__main__':asyncio.run(main())
