/** M-b public-key challenge-response adapter (D15). All wire byte strings
 * use lowercase hex; see docs/api/gateway.md before integrating another codec.
 * The client proves its own Ed25519 key; no shared secret crosses the
 * WebSocket or is stored by core. */
export interface GatewayPairing { clientId: string; privateKey: CryptoKey; serverKey: string; machineId: string }
function bytes(hex: unknown, length: number, what: string): Uint8Array {
    if (typeof hex !== 'string' || hex.length !== length * 2 || !/^[0-9a-f]*$/.test(hex)) throw new Error(`Invalid gateway ${what}`);
    return Uint8Array.from(hex.match(/../g)!, pair => parseInt(pair, 16));
}
const hex = (bytes: Uint8Array) => Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
const CLIENT_ID = /^cli_[0-9a-hjkmnp-tv-z]{26}$/;
/** `label || 0x00 || serverNonce || clientNonce || (len8 || UTF-8 field)*`. */
function message(label: string, serverNonce: Uint8Array, clientNonce: Uint8Array, ...fields: string[]): Uint8Array {
    const utf8 = (s: string) => new TextEncoder().encode(s);
    const parts = [utf8(label), new Uint8Array([0]), serverNonce, clientNonce];
    for (const f of fields.map(utf8)) parts.push(new Uint8Array([f.byteLength]), f);
    const result = new Uint8Array(parts.reduce((n, p) => n + p.byteLength, 0));
    let offset = 0;
    for (const part of parts) { result.set(part, offset); offset += part.byteLength; }
    return result;
}
function authFrame(socket: WebSocket): Promise<any> {
    return new Promise((resolve, reject) => {
        const cleanup = () => { socket.removeEventListener('message', message); socket.removeEventListener('close', close); };
        const close = () => { cleanup(); reject(new Error('Gateway authentication closed')); };
        const message = (event: MessageEvent) => {
            cleanup();
            if (typeof event.data !== 'string' || event.data.length > 4096) { reject(new Error('Invalid gateway authentication frame')); return; }
            try { resolve(JSON.parse(event.data)); } catch { reject(new Error('Invalid gateway authentication frame')); }
        };
        socket.addEventListener('message', message);
        socket.addEventListener('close', close);
    });
}
/** Pass this to C9pWebSocketTransport. Resolves only after the server proves
 * possession of the private key pinned at the original pairing. */
export function hexGatewayAuthenticator(pairing: GatewayPairing) {
    const pin = { ...pairing };
    return async (socket: WebSocket): Promise<void> => {
        if (!CLIENT_ID.test(pin.clientId)) throw new Error('Invalid gateway client id');
        const serverKey = bytes(pin.serverKey, 32, 'server key');
        bytes(pin.machineId, 32, 'machine id');
        const challenge = await authFrame(socket);
        const serverNonce = bytes(challenge?.challenge, 32, 'challenge');
        const clientNonce = crypto.getRandomValues(new Uint8Array(32));
        const sig = await crypto.subtle.sign('Ed25519', pin.privateKey, message('c9p-gateway auth-client v1', serverNonce, clientNonce, pin.machineId, pin.clientId));
        // Install the receiver before sending, including for immediate acks.
        const proof = authFrame(socket);
        socket.send(JSON.stringify({ auth: { clientId: pin.clientId, clientNonce: hex(clientNonce), sig: hex(new Uint8Array(sig)) } }));
        const reply = await proof;
        const signature = bytes(reply?.sig, 64, 'server proof');
        const publicKey = await crypto.subtle.importKey('raw', serverKey, 'Ed25519', false, ['verify']);
        if (!await crypto.subtle.verify('Ed25519', publicKey, signature, message('c9p-gateway auth-server v1', serverNonce, clientNonce, pin.machineId, pin.clientId))) throw new Error('Gateway server proof does not match the paired key');
    };
}
