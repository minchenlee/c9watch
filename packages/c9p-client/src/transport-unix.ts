import { createConnection, type Socket } from 'node:net';
import type { Transport } from './transport.js';

/** Node only. The directory/socket must be owned by your user and private. */
export class UnixTransport implements Transport {
  private socket?: Socket;
  constructor(readonly path: string, readonly maxFrame = 1 << 20, readonly connectTimeoutMs = 5000) {}
  open(onFrame: (v: unknown) => void, onClose: (e: Error) => void): Promise<void> {
    if (this.socket) return Promise.reject(new Error('Transport already open'));
    return new Promise((resolve, reject) => {
      const socket = this.socket = createConnection(this.path);
      let buffer = Buffer.alloc(0);
      let connected = false;
      let ended = false;
      const timer = setTimeout(() => fail(new Error('Socket connect timed out')), this.connectTimeoutMs);
      const fail = (e: Error) => {
        if (ended) return;
        ended = true; clearTimeout(timer); socket.destroy();
        if (!connected) reject(e);
        onClose(e);
      };
      socket.on('connect', () => { connected = true; clearTimeout(timer); resolve(); });
      socket.on('error', fail);
      socket.on('close', () => fail(new Error('Socket closed')));
      socket.on('data', (chunk: Buffer) => {
        buffer = Buffer.concat([buffer, chunk]);
        let end: number;
        while ((end = buffer.indexOf(10)) >= 0) {
          if (end > this.maxFrame) return fail(new Error('c9p frame too large'));
          const line = buffer.subarray(0, end);
          buffer = buffer.subarray(end + 1);
          try {
            // Fatal decoding rejects invalid UTF-8; split codepoints are buffered as bytes.
            onFrame(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line)));
          } catch (e) { return fail(e instanceof Error ? e : new Error(String(e))); }
        }
        if (buffer.length > this.maxFrame) fail(new Error('c9p frame too large'));
      });
    });
  }
  send(frame: unknown): void {
    if (!this.socket || this.socket.destroyed) throw new Error('Socket is closed');
    const line = JSON.stringify(frame) + '\n';
    if (Buffer.byteLength(line) - 1 > this.maxFrame) throw new Error('c9p frame too large');
    if (this.socket.writableLength > this.maxFrame * 4) throw new Error('Socket write queue full');
    this.socket.write(line);
  }
  close(): void { this.socket?.destroy(); }
}
