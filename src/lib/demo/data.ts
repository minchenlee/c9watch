/**
 * Demo session and conversation data for UI exploration
 */

import type { Session, Conversation } from '../types';
import { SessionStatus } from '../types';

function minutesAgo(minutes: number): string {
	return new Date(Date.now() - minutes * 60 * 1000).toISOString();
}

/**
 * Status-specific messages that make transitions feel realistic.
 * Each session ID maps to messages for each possible status.
 */
export const statusMessages: Record<string, Record<SessionStatus, string>> = {
	'demo-1': {
		[SessionStatus.Working]: 'Writing OAuth2 callback handler for Google provider...',
		[SessionStatus.NeedsAttention]: 'I need to write to src/auth/providers.ts — may I proceed?',
		[SessionStatus.WaitingForInput]: 'OAuth2 providers are configured. Want me to add session persistence next?',
		[SessionStatus.Connecting]: 'Starting session...'
	},
	'demo-2': {
		[SessionStatus.Working]: 'Running the profiler on the VirtualizedTable component to identify the bottleneck...',
		[SessionStatus.NeedsAttention]: 'I need to modify src/components/Table.tsx — allow?',
		[SessionStatus.WaitingForInput]: 'Render time reduced from 340ms to 18ms. The fix is in the memoization of row components.',
		[SessionStatus.Connecting]: 'Starting session...'
	},
	'demo-3': {
		[SessionStatus.Working]: 'Implementing the token bucket algorithm with Redis MULTI/EXEC...',
		[SessionStatus.NeedsAttention]: 'I need to run `npm install ioredis` — allow?',
		[SessionStatus.WaitingForInput]: 'Rate limiter middleware is ready. Run `npm test` to verify.',
		[SessionStatus.Connecting]: 'Starting session...'
	},
	'demo-4': {
		[SessionStatus.Working]: 'Writing test case for subscription renewal webhook with idempotency check...',
		[SessionStatus.NeedsAttention]: 'I need to run `npm test -- --watch payments` — allow?',
		[SessionStatus.WaitingForInput]: 'All 23 webhook test cases pass. Coverage is at 94%.',
		[SessionStatus.Connecting]: 'Starting session...'
	},
	'demo-5': {
		[SessionStatus.Working]: 'Adding validation rules for the project name prompt...',
		[SessionStatus.NeedsAttention]: 'I need to write to src/commands/init.ts — allow?',
		[SessionStatus.WaitingForInput]: 'Done! The wizard is at src/commands/init.ts. Run `cli-tools init` to try it.',
		[SessionStatus.Connecting]: 'Starting session...'
	},
	'demo-6': {
		[SessionStatus.Working]: 'Migrating the `deploy` command handler to Result<T, E> pattern...',
		[SessionStatus.NeedsAttention]: 'I need to modify src/commands/deploy.ts — allow?',
		[SessionStatus.WaitingForInput]: 'All 14 command handlers have been migrated to the Result pattern. Tests pass.',
		[SessionStatus.Connecting]: 'Starting session...'
	}
};

/**
 * Defines the possible transitions from each status.
 * Working can transition to NeedsAttention or WaitingForInput.
 * NeedsAttention transitions to Working (user responded).
 * WaitingForInput transitions to Working (user gave new input).
 */
export const statusTransitions: Record<SessionStatus, SessionStatus[]> = {
	[SessionStatus.Working]: [SessionStatus.Working, SessionStatus.Working, SessionStatus.NeedsAttention, SessionStatus.WaitingForInput],
	[SessionStatus.NeedsAttention]: [SessionStatus.Working],
	[SessionStatus.WaitingForInput]: [SessionStatus.Working, SessionStatus.WaitingForInput],
	[SessionStatus.Connecting]: [SessionStatus.Working]
};

/**
 * Returns 6 demo sessions with dynamic timestamps relative to now.
 * Spread across 3 project paths to showcase project grouping.
 */
export function getDemoSessions(): Session[] {
	return [
		// Project 1: web-app — NeedsAttention + Working
		{
			id: 'demo-1',
			pid: 90001,
			sessionName: 'web-app',
			customTitle: null,
			projectPath: '/Users/demo/projects/web-app',
			gitBranch: 'feat/auth-flow',
			firstPrompt: 'Add OAuth2 login with Google and GitHub providers',
			summary: 'Implementing OAuth2 authentication flow with multiple providers',
			messageCount: 34,
			modified: minutesAgo(2),
			status: SessionStatus.NeedsAttention,
			latestMessage: 'I need to write to src/auth/providers.ts — may I proceed?',
			pendingToolName: 'Write'
		},
		{
			id: 'demo-2',
			pid: 90002,
			sessionName: 'web-app',
			customTitle: null,
			projectPath: '/Users/demo/projects/web-app',
			gitBranch: 'fix/perf-regression',
			firstPrompt: 'Profile and fix the rendering performance regression in the dashboard',
			summary: 'Investigating slow renders in dashboard table component',
			messageCount: 87,
			modified: minutesAgo(5),
			status: SessionStatus.Working,
			latestMessage: 'Running the profiler on the VirtualizedTable component to identify the bottleneck...',
			pendingToolName: null
		},

		// Project 2: api-server — NeedsAttention + Working
		{
			id: 'demo-3',
			pid: 90003,
			sessionName: 'api-server',
			customTitle: null,
			projectPath: '/Users/demo/projects/api-server',
			gitBranch: 'feat/rate-limiting',
			firstPrompt: 'Implement token-bucket rate limiting middleware',
			summary: 'Adding rate limiting with Redis-backed token bucket',
			messageCount: 21,
			modified: minutesAgo(1),
			status: SessionStatus.NeedsAttention,
			latestMessage: 'I need to run `npm install ioredis` — allow?',
			pendingToolName: 'Bash'
		},
		{
			id: 'demo-4',
			pid: 90004,
			sessionName: 'api-server',
			customTitle: null,
			projectPath: '/Users/demo/projects/api-server',
			gitBranch: 'main',
			firstPrompt: 'Write integration tests for the payments webhook handler',
			summary: 'Creating comprehensive test suite for Stripe webhook processing',
			messageCount: 156,
			modified: minutesAgo(8),
			status: SessionStatus.Working,
			latestMessage: 'Writing test case for subscription renewal webhook with idempotency check...',
			pendingToolName: null
		},

		// Project 3: cli-tools — WaitingForInput x2
		{
			id: 'demo-5',
			pid: 90005,
			sessionName: 'cli-tools',
			customTitle: null,
			projectPath: '/Users/demo/projects/cli-tools',
			gitBranch: 'feat/config-wizard',
			firstPrompt: 'Build an interactive configuration wizard for first-time setup',
			summary: 'Created interactive CLI wizard with prompts, validation, and config file generation',
			messageCount: 42,
			modified: minutesAgo(15),
			status: SessionStatus.WaitingForInput,
			latestMessage: 'Done! The wizard is at src/commands/init.ts. Run `cli-tools init` to try it.',
			pendingToolName: null
		},
		{
			id: 'demo-6',
			pid: 90006,
			sessionName: 'cli-tools',
			customTitle: null,
			projectPath: '/Users/demo/projects/cli-tools',
			gitBranch: 'refactor/error-handling',
			firstPrompt: 'Refactor error handling to use typed Result pattern',
			summary: 'Migrated all error handling from try/catch to Result<T, E> pattern',
			messageCount: 63,
			modified: minutesAgo(60),
			status: SessionStatus.WaitingForInput,
			latestMessage: 'All 14 command handlers have been migrated to the Result pattern. Tests pass.',
			pendingToolName: null
		}
	];
}

/**
 * Demo conversation data for expanded card view.
 * Provides realistic message histories for 3 sessions.
 */
export const demoConversations: Record<string, Conversation> = {
	'demo-1': {
		sessionId: 'demo-1',
		messages: [
			{
				timestamp: minutesAgo(30),
				messageType: 'User',
				content: 'Add OAuth2 login with Google and GitHub providers'
			},
			{
				timestamp: minutesAgo(29),
				messageType: 'Thinking',
				content:
					'The user wants OAuth2 authentication with Google and GitHub. I should:\n1. Set up the OAuth2 configuration\n2. Create provider-specific handlers\n3. Add the login routes\n4. Create the callback handler'
			},
			{
				timestamp: minutesAgo(29),
				messageType: 'Assistant',
				content:
					"I'll implement OAuth2 login with Google and GitHub providers. Let me start by examining the existing auth setup."
			},
			{
				timestamp: minutesAgo(28),
				messageType: 'ToolUse',
				content: 'Read src/auth/index.ts'
			},
			{
				timestamp: minutesAgo(28),
				messageType: 'ToolResult',
				content:
					'```typescript\nimport { Router } from "express";\nimport { sessionMiddleware } from "./session";\n\nexport const authRouter = Router();\nauthRouter.use(sessionMiddleware);\n\n// TODO: Add OAuth providers\n```'
			},
			{
				timestamp: minutesAgo(27),
				messageType: 'Assistant',
				content:
					"Good, there's a placeholder for OAuth providers. Let me set up the provider configuration and handlers.\n\nFirst, I'll create the OAuth2 provider configuration:"
			},
			{
				timestamp: minutesAgo(26),
				messageType: 'ToolUse',
				content: 'Write src/auth/oauth-config.ts'
			},
			{
				timestamp: minutesAgo(26),
				messageType: 'ToolResult',
				content: 'File written successfully.'
			},
			{
				timestamp: minutesAgo(10),
				messageType: 'Assistant',
				content:
					"Now I need to create the provider-specific handlers. I'll write to `src/auth/providers.ts` to add the Google and GitHub OAuth2 flows."
			},
			{
				timestamp: minutesAgo(2),
				messageType: 'ToolUse',
				content: 'Write src/auth/providers.ts — **Waiting for permission**'
			}
		]
	},
	'demo-3': {
		sessionId: 'demo-3',
		messages: [
			{
				timestamp: minutesAgo(18),
				messageType: 'User',
				content: 'Implement token-bucket rate limiting middleware'
			},
			{
				timestamp: minutesAgo(17),
				messageType: 'Thinking',
				content:
					'Token bucket rate limiting — I need to:\n1. Create a Redis-backed token bucket implementation\n2. Build Express middleware that checks the bucket\n3. Add configuration for different rate limit tiers\n4. Handle edge cases like Redis connection failures'
			},
			{
				timestamp: minutesAgo(17),
				messageType: 'Assistant',
				content:
					"I'll implement a Redis-backed token bucket rate limiter. This approach gives us:\n\n- **Distributed rate limiting** across multiple server instances\n- **Sliding window** accuracy with the token bucket algorithm\n- **Configurable tiers** for different API endpoints\n\nLet me start by checking the existing middleware setup."
			},
			{
				timestamp: minutesAgo(16),
				messageType: 'ToolUse',
				content: 'Read src/middleware/index.ts'
			},
			{
				timestamp: minutesAgo(16),
				messageType: 'ToolResult',
				content:
					'```typescript\nimport cors from "cors";\nimport helmet from "helmet";\nimport { requestLogger } from "./logging";\n\nexport { cors, helmet, requestLogger };\n```'
			},
			{
				timestamp: minutesAgo(15),
				messageType: 'Assistant',
				content:
					"I need to install `ioredis` as a dependency for the Redis client. This is the most widely-used Redis client for Node.js with built-in TypeScript support."
			},
			{
				timestamp: minutesAgo(1),
				messageType: 'ToolUse',
				content: 'Bash: npm install ioredis — **Waiting for permission**'
			}
		]
	},
	'demo-5': {
		sessionId: 'demo-5',
		messages: [
			{
				timestamp: minutesAgo(50),
				messageType: 'User',
				content: 'Build an interactive configuration wizard for first-time setup'
			},
			{
				timestamp: minutesAgo(49),
				messageType: 'Thinking',
				content:
					'An interactive CLI wizard. I should use inquirer or prompts for the interactive portions. Let me check what dependencies are already available.'
			},
			{
				timestamp: minutesAgo(49),
				messageType: 'Assistant',
				content:
					"I'll create an interactive setup wizard. Let me check the existing CLI structure first."
			},
			{
				timestamp: minutesAgo(48),
				messageType: 'ToolUse',
				content: 'Read src/commands/index.ts'
			},
			{
				timestamp: minutesAgo(48),
				messageType: 'ToolResult',
				content:
					'```typescript\nimport { Command } from "commander";\nimport { runCommand } from "./run";\nimport { buildCommand } from "./build";\n\nexport function registerCommands(program: Command) {\n  program.addCommand(runCommand);\n  program.addCommand(buildCommand);\n}\n```'
			},
			{
				timestamp: minutesAgo(40),
				messageType: 'Assistant',
				content:
					"I've created the wizard with the following features:\n\n- **Project name** — validated for npm naming conventions\n- **Template selection** — TypeScript, JavaScript, or minimal\n- **Package manager** — auto-detects from lockfile, or asks\n- **Git initialization** — optional, sets up `.gitignore`\n- **Config file** — writes `cli-tools.config.json`\n\n```typescript\n// src/commands/init.ts\nexport const initCommand = new Command('init')\n  .description('Interactive first-time setup wizard')\n  .action(async () => {\n    const config = await runWizard();\n    await writeConfig(config);\n    console.log('Setup complete!');\n  });\n```"
			},
			{
				timestamp: minutesAgo(15),
				messageType: 'Assistant',
				content:
					'Done! The wizard is at `src/commands/init.ts`. Run `cli-tools init` to try it. All existing tests still pass.'
			}
		]
	}
};
