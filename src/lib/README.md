# Frontend Library

This directory contains the core TypeScript/Svelte libraries for the c9watch frontend.

## Structure

```
lib/
├── types.ts              # TypeScript type definitions
├── api.ts                # Tauri command wrappers
├── stores/
│   └── sessions.ts       # Svelte stores for state management
└── index.ts              # Convenience re-exports
```

## Usage

### Importing Types

```typescript
import type { Session, Conversation, Message, SessionStatus } from '$lib';
```

### Using Stores

```typescript
import { sessions, selectedSessionId, currentConversation, initializeSessionListeners } from '$lib';

// Initialize event listeners (call once on app start)
await initializeSessionListeners();

// Subscribe to stores
$: activeSessions = $sessions;
$: selected = $selectedSessionId;
$: conversation = $currentConversation;
```

### Calling API Functions

```typescript
import { getSessions, getConversation, sendPrompt, stopSession, openSession } from '$lib';

// Get all sessions
const allSessions = await getSessions();

// Get conversation for a session
const conv = await getConversation(sessionId);

// Send a prompt
await sendPrompt(sessionId, "Add authentication to the API");

// Stop a session
await stopSession(sessionId);

// Open terminal/IDE window
await openSession(sessionId);
```

## Event Listeners

The stores automatically listen for Tauri events from the backend:

- `sessions-updated`: Emitted when the backend polling loop detects session changes
- `conversation-updated`: Emitted when a conversation is loaded or updated

Make sure to call `initializeSessionListeners()` once when your app starts (e.g., in a top-level `+layout.svelte` or `+page.svelte`).

## Type Definitions

### Session
Represents an active Claude Code session with metadata like project name, status, and activity timestamp.

### Conversation
Contains all messages for a specific session.

### Message
A single message in a conversation, either from the user or Claude, optionally with tool calls.

### ToolCall
Information about a tool being invoked, including input, output, and completion status.

### SessionStatus
Enum representing the current state: Working, NeedsAttention, WaitingForInput, or Connecting.
