<script lang="ts">
	import { slide } from 'svelte/transition';
	import { getDebugLogs } from '$lib/api';
	import type { LogEntry } from '$lib/types';

	interface Props {
		visible: boolean;
		onclose: () => void;
	}

	let { visible, onclose }: Props = $props();

	let logs = $state<LogEntry[]>([]);
	let logContainer: HTMLDivElement | undefined = $state();
	let pollTimer: ReturnType<typeof setInterval> | undefined;

	async function fetchLogs() {
		try {
			logs = await getDebugLogs();
			scrollToBottom();
		} catch {
			// Silently fail — we are the debug console, don't recurse errors
		}
	}

	function scrollToBottom() {
		requestAnimationFrame(() => {
			if (logContainer) {
				logContainer.scrollTop = logContainer.scrollHeight;
			}
		});
	}

	function formatTime(timestamp: string): string {
		try {
			const d = new Date(timestamp);
			return d.toLocaleTimeString('en-US', { hour12: false });
		} catch {
			return '??:??:??';
		}
	}

	async function copyAll() {
		const text = logs
			.map((l) => `[${formatTime(l.timestamp)}] [${l.level.toUpperCase()}] ${l.message}`)
			.join('\n');
		try {
			await navigator.clipboard.writeText(text);
		} catch {
			// Clipboard may not be available
		}
	}

	function clearLogs() {
		logs = [];
	}

	$effect(() => {
		if (visible) {
			fetchLogs();
			pollTimer = setInterval(fetchLogs, 3500);
		} else if (pollTimer) {
			clearInterval(pollTimer);
			pollTimer = undefined;
		}

		return () => {
			if (pollTimer) {
				clearInterval(pollTimer);
			}
		};
	});
</script>

{#if visible}
	<div class="debug-console" transition:slide={{ duration: 200 }}>
		<div class="console-header">
			<span class="console-title">DEBUG CONSOLE</span>
			<span class="console-count">{logs.length}</span>
			<div class="console-spacer"></div>
			<button class="console-btn" onclick={copyAll} title="Copy All">COPY</button>
			<button class="console-btn" onclick={clearLogs} title="Clear">CLEAR</button>
			<button class="console-btn close-btn" onclick={onclose} title="Close">&times;</button>
		</div>
		<div class="console-body" bind:this={logContainer}>
			{#each logs as entry}
				<div class="log-line {entry.level}">
					<span class="log-time">{formatTime(entry.timestamp)}</span>
					<span class="log-level">{entry.level.toUpperCase()}</span>
					<span class="log-msg">{entry.message}</span>
				</div>
			{:else}
				<div class="log-empty">No log entries yet.</div>
			{/each}
		</div>
	</div>
{/if}

<style>
	.debug-console {
		position: fixed;
		bottom: 0;
		left: 0;
		right: 0;
		height: 40vh;
		background: var(--bg-surface);
		border-top: 1px solid var(--border-default);
		display: flex;
		flex-direction: column;
		z-index: 9000;
	}

	.console-header {
		display: flex;
		align-items: center;
		gap: var(--space-md);
		padding: var(--space-sm) var(--space-lg);
		border-bottom: 1px solid var(--border-default);
		flex-shrink: 0;
	}

	.console-title {
		font-family: var(--font-pixel);
		font-size: 12px;
		color: var(--text-primary);
		letter-spacing: 0.1em;
	}

	.console-count {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-muted);
	}

	.console-spacer {
		flex: 1;
	}

	.console-btn {
		background: transparent;
		border: 1px solid var(--border-default);
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		letter-spacing: 0.08em;
		padding: 2px 8px;
		cursor: pointer;
		transition: all 0.2s ease;
	}

	.console-btn:hover {
		color: var(--text-secondary);
		border-color: var(--text-muted);
	}

	.close-btn {
		font-size: 16px;
		line-height: 1;
		padding: 0 6px;
	}

	.console-body {
		flex: 1;
		overflow-y: auto;
		padding: var(--space-sm) var(--space-lg);
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.6;
	}

	.log-line {
		display: flex;
		gap: var(--space-md);
		white-space: pre-wrap;
		word-break: break-all;
	}

	.log-time {
		color: var(--text-muted);
		flex-shrink: 0;
		opacity: 0.6;
	}

	.log-level {
		flex-shrink: 0;
		width: 40px;
		text-align: right;
	}

	.log-line.info .log-level { color: var(--text-muted); }
	.log-line.warn .log-level { color: var(--accent-amber); }
	.log-line.error .log-level { color: var(--status-permission); }

	.log-line.info .log-msg { color: var(--text-muted); }
	.log-line.warn .log-msg { color: rgba(255, 255, 255, 0.7); }
	.log-line.error .log-msg { color: rgba(255, 255, 255, 0.9); }

	.log-empty {
		color: var(--text-muted);
		text-align: center;
		padding: var(--space-xl);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
</style>
