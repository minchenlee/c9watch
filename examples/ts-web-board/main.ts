import { C9pClient } from '../../packages/c9p-client/src/index.js';
import { WsTransport, pair } from '../../packages/c9p-client/src/transport-ws.js';
import { watchBoard } from './board.js';

const form = document.querySelector<HTMLFormElement>('form')!;
const status = document.querySelector<HTMLOutputElement>('output')!;
const list = document.querySelector<HTMLUListElement>('ul')!;
let client: C9pClient | undefined;
let connecting = false;
const submit = form.querySelector<HTMLButtonElement>('button')!;
form.addEventListener('submit', async (event) => {
  event.preventDefault();
  if (connecting) return;
  connecting = true; submit.disabled = true;
  client?.close(); list.replaceChildren();
  const data = new FormData(form);
  const url = String(data.get('gateway'));
  const code = String(data.get('code'));
  status.textContent = 'Pairing…';
  try {
    // Memory only. Closing/reloading this page forgets the credentials.
    const credentials = await pair(url, code);
    const codeInput = form.elements.namedItem('code') as HTMLInputElement;
    codeInput.value = '';
    client = new C9pClient(() => new WsTransport(url, credentials), {
      onDisconnect: (error) => { status.textContent = error.message + '. Pair again to reconnect.'; },
    });
    const hello = await client.connect();
    if (hello.machineId !== credentials.machineId) throw new Error('Machine identity changed');
    await watchBoard(client, (sessions) => {
      const items = sessions.map((session) => {
        const item = document.createElement('li');
        item.textContent = `${session.sessionName || session.sessionKey} — ${session.status}`;
        return item;
      });
      list.replaceChildren(...items);
      status.textContent = `${sessions.length} sessions`;
    }, (error) => { client?.close(); status.textContent = error.message + '. Pair again for a fresh snapshot.'; });
  } catch (error) {
    client?.close(); status.textContent = error instanceof Error ? error.message : String(error);
  } finally { connecting = false; submit.disabled = false; }
});
window.addEventListener('pagehide', () => client?.close());
