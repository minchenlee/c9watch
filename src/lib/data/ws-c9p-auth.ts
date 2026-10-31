/** M-b challenge-response adapter. All wire byte strings use lowercase
 * hex; see docs/api/gui-gateway-bridge.md before integrating another codec.
 * The bearer secret never crosses the WebSocket after pairing. */
export interface GatewayPairing { clientId: string; token: string; serverKey: string; machineId: string }
function bytes(hex: string, length: number): Uint8Array {
    if (hex.length !== length * 2 || !/^[a-f\d]+$/i.test(hex)) throw new Error('Invalid gateway key or nonce');
    return Uint8Array.from(hex.match(/../g)!, pair => parseInt(pair, 16));
}
const hex = (bytes: Uint8Array) => Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
function join(...parts: Uint8Array[]): Uint8Array {
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
        const challenge = await authFrame(socket);
        if (typeof challenge.challenge !== 'string') throw new Error('Gateway sent no challenge');
        const serverNonce = bytes(challenge.challenge, 32);
        const clientNonce = crypto.getRandomValues(new Uint8Array(32));
        const key = await crypto.subtle.importKey('raw', bytes(pin.token, 32), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
        const mac = await crypto.subtle.sign('HMAC', key, join(serverNonce, clientNonce, new TextEncoder().encode(pin.machineId)));
        // Install the receiver before sending, including for immediate acks.
        const signature = authFrame(socket);
        socket.send(JSON.stringify({ auth: { clientId: pin.clientId, clientNonce: hex(clientNonce), mac: hex(new Uint8Array(mac)) } }));
        const proof = await signature;
        if (typeof proof.sig !== 'string') throw new Error('Gateway sent no server proof');
        const publicKey = await crypto.subtle.importKey('raw', bytes(pin.serverKey, 32), 'Ed25519', false, ['verify']);
        if (!await crypto.subtle.verify('Ed25519', publicKey, bytes(proof.sig, 64), join(serverNonce, clientNonce))) throw new Error('Gateway server proof does not match the paired key');
    };
}
