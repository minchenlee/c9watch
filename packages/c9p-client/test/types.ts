// Compile-only contract checks, never executed against a core.
import { C9pClient, type C9pSessionStatus, type C9pAgentRow, type TopicEvent } from '../src/index.js';

const future: C9pSessionStatus = 'new-status-from-a-later-minor';
const futureRole: C9pAgentRow['role'] = 'future-role';
void future; void futureRole;

async function contracts(client: C9pClient, event: TopicEvent<'session.view' | 'session.gone'>) {
  const result = await client.request('session.views', {});
  const cursor: string = result.cursor;
  if (event.topic === 'session.view') {
    const pid: number = event.data.pid;
    void pid;
  }
  // @ts-expect-error private methods are excluded
  await client.request('hook.stop', {});
  // @ts-expect-error unknown methods are excluded
  await client.request('not.a.method', {});
  // @ts-expect-error session rename needs its required fields
  await client.request('session.rename', {});
  // @ts-expect-error private agent fields are excluded
  const internal: C9pAgentRow['resumeCount'] = 1;
  void internal; void cursor;
}
void contracts;
