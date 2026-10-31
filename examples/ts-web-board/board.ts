import { C9pClient, type C9pSessionView, type Subscription } from '../../packages/c9p-client/src/index.js';

/** This state reducer is shared by the browser page and the fixture smoke test. */
export async function watchBoard(client: C9pClient, render: (sessions: C9pSessionView[]) => void, onError: (error: Error) => void): Promise<Subscription> {
  const snapshot = await client.request('session.views', {});
  const sessions = new Map(snapshot.sessions.map((s) => [s.sessionKey, s]));
  render([...sessions.values()]);
  return client.subscribe<'session.view' | 'session.gone'>({
    since: snapshot.cursor, topics: ['session.view', 'session.gone'],
  }, (event) => {
    if (event.topic === 'session.view') sessions.set(event.data.sessionKey, event.data);
    else if (event.topic === 'session.gone') sessions.delete(event.data.sessionKey);
    render([...sessions.values()]);
  }, onError);
}
