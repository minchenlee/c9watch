import type { C9pMethods, C9pMethod, C9pFrames, C9pSubscribeParams } from '../c9p-1';
export type StreamFrame = C9pFrames['event'] | C9pFrames['progress'] | C9pFrames['response'];
export type Unsubscribe = () => void;
export interface Transport {
    request<M extends C9pMethod>(method: M, params: C9pMethods[M]['params']): Promise<C9pMethods[M]['result']>;
    subscribe(params: C9pSubscribeParams, onFrame: (frame: StreamFrame) => void): Promise<Unsubscribe>;
}
export class C9pError extends Error {
    constructor(public code: string, message: string, public retryable = false) { super(message); }
}
