import type { C9pSessionView } from '../c9p-1';
import type { Session, SessionStatus } from '../types';
/** SessionView already has Classic's wire shape. Keep every optional field. */
export function sessionFromView(view: C9pSessionView): Session {
    return { ...view, status: view.status as SessionStatus } as Session;
}
