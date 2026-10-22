/**
 * Message visibility rules shared by the conversation overlays and the
 * message nav map. Type-only imports keep this module pure, so
 * scripts/test-*.mjs can transpile and test it directly.
 */

import type { Message } from './types';

/** True when a message is shown in the conversation list. */
export function isMessageVisible(msg: Message, showTools: boolean, showThinking: boolean): boolean {
	return (
		(showTools || (msg.messageType !== 'ToolUse' && msg.messageType !== 'ToolResult')) &&
		(showThinking || msg.messageType !== 'Thinking')
	);
}

/** True when a message is a "milestone" item in the message nav map. */
export function isNavItem(msg: Message, showTools: boolean, showThinking: boolean): boolean {
	return (
		msg.messageType === 'User' ||
		msg.messageType === 'System' ||
		(showThinking && msg.messageType === 'Thinking') ||
		(showTools && msg.messageType === 'ToolUse' && (msg.content?.length ?? 0) > 0)
	);
}
