/** A transport delivers complete JSON objects and never reconnects by itself. */
export interface Transport {
  open(onFrame: (frame: unknown) => void, onClose: (error: Error) => void): Promise<void>;
  send(frame: unknown): void;
  close(): void;
}

export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Expected a c9p object');
  return value as Record<string, unknown>;
}
