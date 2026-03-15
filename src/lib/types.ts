/**
 * TypeScript type definitions for c9watch
 */

/**
 * Session status enumeration
 */
export enum SessionStatus {
  Working = 'Working',              // Executing tools/thinking
  NeedsAttention = 'NeedsAttention',    // Waiting for user attention (approval, question, etc.)
  WaitingForInput = 'WaitingForInput', // Idle, ready for prompt
  Connecting = 'Connecting'            // Session starting up
}

/**
 * A Claude Code session
 */
export interface Session {
  /** Session UUID */
  id: string;

  /** Process ID of the running Claude instance */
  pid: number;

  /** Custom session name (defaults to project directory name) - shown as small badge */
  sessionName: string;

  /** Custom title override for the session - if set, shown instead of summary/firstPrompt */
  customTitle: string | null;

  /** Full path to project directory */
  projectPath: string;

  /** Git branch name (if available) */
  gitBranch: string | null;

  /** Summary of the first prompt (shown in list view) */
  firstPrompt: string;

  /** AI-generated summary of the session (from sessions-index.json) */
  summary: string | null;

  /** Total number of messages in the conversation */
  messageCount: number;

  /** Timestamp of last activity (ISO 8601 string) */
  modified: string;

  /** Current status of the session */
  status: SessionStatus;

  /** Content of the latest message */
  latestMessage: string;

  /** Name of the tool or reason awaiting attention (if status is NeedsAttention) */
  pendingToolName: string | null;
}

/**
 * Message type in conversation
 */
export type MessageType = 'User' | 'Assistant' | 'Thinking' | 'ToolUse' | 'ToolResult';

/**
 * A base64-encoded image attached to a message
 */
export interface MessageImage {
  /** MIME type, e.g. "image/png" */
  mediaType: string;
  /** Base64-encoded image data */
  data: string;
}

/**
 * A message in a conversation
 */
export interface Message {
  /** Message timestamp (ISO 8601 string) */
  timestamp: string;

  /** Message type */
  messageType: MessageType;

  /** Message content text */
  content: string;

  /** Images attached to this message (screenshots pasted by the user) */
  images?: MessageImage[];
}

/**
 * A conversation containing all messages for a session
 */
export interface Conversation {
  /** Session ID this conversation belongs to */
  sessionId: string;

  /** Array of messages in chronological order */
  messages: Message[];
}

/**
 * A single result from the deep search command.
 * Contains the session ID and a short snippet from the matching message.
 */
export interface DeepSearchHit {
  sessionId: string;
  /** ~200-char snippet from the first matching message line, with '…' padding if truncated. */
  snippet: string;
}

/**
 * A single entry from ~/.claude/history.jsonl (deduplicated by sessionId)
 */
export interface HistoryEntry {
  /** Session UUID */
  sessionId: string;

  /** The user's prompt text as displayed in Claude Code. May be an empty string. */
  display: string;

  /**
   * Timestamp in milliseconds since epoch (raw integer from history.jsonl).
   * Note: unlike other timestamps in this file which are ISO 8601 strings,
   * this is a Unix-ms number matching the raw format Claude Code writes.
   */
  timestamp: number;

  /** Full project path, e.g. /Users/you/Documents/GitHub/myproject */
  project: string;

  /** Last path segment of project, e.g. "myproject" */
  projectName: string;

  /** Custom title override — if set, shown instead of the first prompt */
  customTitle: string | null;
}

/**
 * Cost data for a single session.
 */
export interface SessionCostRecord {
  sessionId: string;
  project: string;
  projectName: string;
  /** Primary model (highest cost contributor) */
  model: string;
  cost: number;
  /** ISO 8601 timestamp of earliest assistant message */
  timestamp: string;
  /** Date portion "YYYY-MM-DD" */
  date: string;
  /** Total tokens (input + output) for this session */
  totalTokens: number;
}

/**
 * Daily cost aggregate.
 */
export interface DailyCost {
  date: string;
  cost: number;
  sessions: SessionCostRecord[];
}

/**
 * Per-project cost aggregate.
 */
export interface ProjectCost {
  project: string;
  projectName: string;
  totalCost: number;
  sessions: SessionCostRecord[];
}

/**
 * Per-model cost aggregate.
 */
export interface ModelCost {
  model: string;
  displayName: string;
  cost: number;
  percentage: number;
}

/**
 * Full cost data returned by get_cost_data command.
 */
export interface CostData {
  totalCost: number;
  /** Sum of all input + output tokens across all sessions */
  totalTokens: number;
  dailyCosts: DailyCost[];
  projectCosts: ProjectCost[];
  modelCosts: ModelCost[];
}

/**
 * A single memory file from a project's memory directory
 */
export interface MemoryFile {
  filename: string;
  content: string;
}

/**
 * All memory files for a single Claude Code project
 */
export interface ProjectMemory {
  projectName: string;
  projectPath: string;
  memoryDirPath: string;
  files: MemoryFile[];
}

export interface LogEntry {
  timestamp: string;
  level: 'info' | 'warn' | 'error';
  message: string;
}

export interface DetectionDiagnostics {
  claudeProcessesFound: number;
  processesWithCwd: number;
  fdaLikelyNeeded: boolean;
}
