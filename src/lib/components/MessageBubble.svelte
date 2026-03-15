<script lang="ts">
	import type { Message } from '$lib/types';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';

	interface Props {
		message: Message;
	}

	let { message }: Props = $props();

	function formatTime(isoTimestamp: string): string {
		const date = new Date(isoTimestamp);
		return date.toLocaleTimeString('en-US', {
			hour: '2-digit',
			minute: '2-digit'
		});
	}

	let isUser = $derived(message.messageType === 'User');
	let isAssistant = $derived(message.messageType === 'Assistant');
	let isThinking = $derived(message.messageType === 'Thinking');
	let isToolUse = $derived(message.messageType === 'ToolUse');
	let isToolResult = $derived(message.messageType === 'ToolResult');
	let isSystem = $derived(message.messageType === 'System');

	let roleLabel = $derived.by(() => {
		switch (message.messageType) {
			case 'User':
				return 'You';
			case 'Assistant':
				return 'Claude';
			case 'Thinking':
				return 'Thinking';
			case 'ToolUse':
				return 'Tool';
			case 'ToolResult':
				return 'Result';
			case 'System':
				return 'System';
			default:
				return 'Unknown';
		}
	});

	let roleIcon = $derived.by(() => {
		switch (message.messageType) {
			case 'User':
				return '→';
			case 'Assistant':
				return '◆';
			case 'Thinking':
				return '◇';
			case 'ToolUse':
				return '⚙';
			case 'ToolResult':
				return '↩';
			case 'System':
				return '◈';
			default:
				return '•';
		}
	});

	let renderedContent = $derived.by(() => {
		if (!message.content) return '';

		const renderer = new marked.Renderer();

		// Custom code block renderer for Vercel Noir style
		renderer.code = function ({ text, lang }) {
			const language = lang || 'code';
			return `
				<div class="code-block-wrapper">
					<div class="code-header">
						<span class="code-lang">${language}</span>
						<div class="code-actions">
							<span class="code-dot"></span>
							<span class="code-dot"></span>
						</div>
					</div>
					<pre><code class="language-${language}">${text}</code></pre>
				</div>
			`;
		};

		// Configure marked for safe rendering
		const rawHtml = marked.parse(message.content, {
			async: false,
			breaks: true,
			gfm: true,
			renderer
		});
		return DOMPurify.sanitize(rawHtml as string);
	});
</script>

<div
	class="message-bubble"
	class:user={isUser}
	class:assistant={isAssistant}
	class:thinking={isThinking}
	class:tool-use={isToolUse}
	class:tool-result={isToolResult}
	class:system={isSystem}
>
	<div class="message-header">
		<span class="message-icon">{roleIcon}</span>
		<span class="message-role">{roleLabel}</span>
		<span class="message-time">{formatTime(message.timestamp)}</span>
	</div>

	{#if message.images?.length}
		<div class="message-images">
			{#each message.images as img}
				<img src="data:{img.mediaType};base64,{img.data}" alt="Attached screenshot" class="attached-image" />
			{/each}
		</div>
	{/if}

	{#if message.content}
		<div class="message-content">
			{#if isAssistant || isUser}
				{@html renderedContent}
			{:else}
				{message.content}
			{/if}
		</div>
	{/if}
</div>

<style>
	.message-bubble {
		margin: var(--space-sm) 0;
		padding: var(--space-md) var(--space-lg);
		max-width: 100%;
		border-left: 1px solid var(--border-default);
		transition: background var(--transition-fast);
		animation: fade-in 0.3s ease-out backwards;
	}

	.message-bubble:hover {
		background: rgba(255, 255, 255, 0.02);
	}

	.message-bubble.user {
		border-left: 2px solid var(--text-primary);
		background: rgba(255, 255, 255, 0.01);
	}

	.message-bubble.assistant {
		border-left-color: var(--text-muted);
	}

	.message-bubble.thinking {
		border-left: 1px dashed var(--status-permission);
		background: rgba(255, 102, 0, 0.03);
		opacity: 0.8;
	}

	.message-bubble.tool-use {
		border-left: 1px solid var(--status-input);
		background: rgba(0, 255, 136, 0.02);
	}

	.message-bubble.tool-result {
		border-left-color: var(--text-muted);
		background: rgba(255, 255, 255, 0.01);
		opacity: 0.6;
	}

	.message-bubble.system {
		border-left: 1px solid var(--accent-purple);
		background: color-mix(in srgb, var(--accent-purple) 3%, transparent);
		opacity: 0.7;
	}

	.message-header {
		display: flex;
		align-items: center;
		gap: var(--space-sm);
		margin-bottom: var(--space-sm);
	}

	.message-icon {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	.message-role {
		font-family: var(--font-mono);
		font-weight: 500;
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.message-bubble.user .message-role {
		color: var(--text-primary);
	}

	.message-bubble.thinking .message-role {
		color: var(--status-permission);
	}

	.message-bubble.tool-use .message-role {
		color: var(--status-input);
	}

	.message-bubble.system .message-role {
		color: var(--accent-purple);
	}

	.message-time {
		margin-left: auto;
		font-size: 12px;
		color: var(--text-muted);
		font-family: var(--font-mono);
		letter-spacing: 0.05em;
	}

	.message-content {
		color: var(--text-secondary);
		font-size: 15px;
		line-height: 1.6;
		white-space: normal;
		word-wrap: break-word;
	}

	.message-content :global(p) {
		margin: 0 0 var(--space-md) 0;
	}

	.message-content :global(p:last-child) {
		margin-bottom: 0;
	}

	.message-content :global(h1),
	.message-content :global(h2),
	.message-content :global(h3),
	.message-content :global(h4) {
		color: var(--text-primary);
		font-weight: 600;
		margin: var(--space-lg) 0 var(--space-sm) 0;
		line-height: 1.2;
	}

	.message-content :global(h1) { font-size: 18px; }
	.message-content :global(h2) { font-size: 17px; }
	.message-content :global(h3) { font-size: 16px; }
	.message-content :global(h4) { font-size: 15px; }

	.message-content :global(.code-block-wrapper) {
		margin: var(--space-md) 0;
		border: 1px solid var(--border-default);
		background: var(--bg-elevated);
	}

	.message-content :global(.code-header) {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--space-xs) var(--space-md);
		border-bottom: 1px solid var(--border-muted);
		background: var(--bg-card);
	}

	.message-content :global(.code-lang) {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	.message-content :global(.code-actions) {
		display: flex;
		gap: 4px;
	}

	.message-content :global(.code-dot) {
		width: 4px;
		height: 4px;
		background: var(--border-default);
	}

	.message-content :global(pre) {
		margin: 0;
		padding: var(--space-md);
		overflow-x: auto;
		background: transparent;
	}

	.message-content :global(code) {
		font-family: var(--font-mono);
		font-size: 14px;
		background: var(--bg-elevated);
		padding: 2px 4px;
		color: var(--text-primary);
	}

	.message-content :global(pre code) {
		padding: 0;
		background: transparent;
		display: block;
		line-height: 1.5;
		white-space: pre;
	}

	.message-content :global(ul),
	.message-content :global(ol) {
		margin: 0 0 var(--space-md) var(--space-lg);
		padding: 0;
	}

	.message-content :global(li) {
		margin-bottom: var(--space-xs);
		padding-left: 4px;
	}

	.message-content :global(li::marker) {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 13px;
	}

	.message-content :global(blockquote) {
		margin: var(--space-md) 0;
		padding: var(--space-sm) var(--space-md);
		border-left: 2px solid var(--border-default);
		background: var(--bg-elevated);
		color: var(--text-secondary);
		font-style: italic;
	}

	.message-content :global(hr) {
		border: 0;
		border-top: 1px solid var(--border-muted);
		margin: var(--space-lg) 0;
	}

	.message-content :global(strong),
	.message-content :global(b) {
		font-weight: 600;
		color: var(--text-primary);
	}

	.message-content :global(em),
	.message-content :global(i) {
		font-style: italic;
		color: var(--text-primary);
	}

	.message-content :global(a) {
		color: var(--status-input);
		text-decoration: underline;
		text-underline-offset: 2px;
		transition: color var(--transition-fast);
	}

	.message-content :global(a:hover) {
		color: var(--text-primary);
	}

	.message-images {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-sm);
		margin-bottom: var(--space-sm);
	}

	.attached-image {
		max-width: 100%;
		max-height: 400px;
		border: 1px solid var(--border-default);
		object-fit: contain;
	}

	.message-bubble.user .message-content {
		color: var(--text-primary);
	}

	.message-bubble.system .message-content {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 13px;
	}

	.message-bubble.tool-use .message-content,
	.message-bubble.tool-result .message-content {
		color: var(--text-muted);
		max-height: 250px;
		overflow-y: auto;
		white-space: pre-wrap;
		font-family: var(--font-mono);
		font-size: 13px;
		background: var(--bg-elevated);
		padding: var(--space-sm);
		border: 1px solid var(--border-muted);
	}
</style>

