<script lang="ts">
	import { toMermaid } from '$lib/workflow-mermaid';
	import type { WorkflowAgent } from '$lib/types';

	interface PhaseGroup {
		title: string;
		agents: WorkflowAgent[];
	}

	let { phaseGroups, workflowName }: { phaseGroups: PhaseGroup[]; workflowName: string } =
		$props();

	const PHASE_X = 24,
		PHASE_W = 200,
		AGENT_X = 300,
		AGENT_W = 320,
		NODE_H = 48,
		ROW_GAP = 20,
		AGENT_GAP = 10,
		TOP = 24;

	interface PlacedAgent {
		a: WorkflowAgent;
		x: number;
		y: number;
	}
	interface PlacedPhase {
		title: string;
		x: number;
		y: number;
		agents: PlacedAgent[];
	}

	let layout = $derived.by(() => {
		const phases: PlacedPhase[] = [];
		let cursorY = TOP;
		for (const g of phaseGroups) {
			const span = Math.max(NODE_H, g.agents.length * (NODE_H + AGENT_GAP) - AGENT_GAP);
			const phaseY = cursorY + span / 2 - NODE_H / 2;
			let ay = cursorY;
			const placed: PlacedAgent[] = g.agents.map((a) => {
				const node = { a, x: AGENT_X, y: ay };
				ay += NODE_H + AGENT_GAP;
				return node;
			});
			phases.push({ title: g.title, x: PHASE_X, y: phaseY, agents: placed });
			cursorY += span + ROW_GAP;
		}
		const height = Math.max(cursorY, TOP + NODE_H);
		const width = AGENT_X + AGENT_W + 24;
		return { phases, width, height };
	});

	// Elbow path from a phase node's right-mid to an agent node's left-mid.
	function edge(px: number, py: number, ax: number, ay: number): string {
		const x1 = px + PHASE_W,
			y1 = py + NODE_H / 2,
			x2 = ax,
			y2 = ay + NODE_H / 2,
			mx = (x1 + x2) / 2;
		return `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`;
	}

	function fmtTokens(n: number): string {
		if (n >= 1000) return `${Math.round(n / 1000)}k`;
		return String(n);
	}

	let svgEl: SVGSVGElement | undefined = $state();
	let copied = $state(false);

	async function copyMermaid() {
		const phases = phaseGroups.map((g) => g.title);
		const agents = phaseGroups.flatMap((g) => g.agents);
		const text = toMermaid(workflowName, phases, agents);
		try {
			await navigator.clipboard.writeText(text);
			copied = true;
			setTimeout(() => (copied = false), 1500);
		} catch {
			copied = false;
		}
	}

	function downloadSvg() {
		if (!svgEl) return;
		const clone = svgEl.cloneNode(true) as SVGSVGElement;
		clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
		const src = new XMLSerializer().serializeToString(clone);
		const blob = new Blob([src], { type: 'image/svg+xml' });
		const url = URL.createObjectURL(blob);
		const a = document.createElement('a');
		a.href = url;
		a.download = `${workflowName || 'workflow'}-graph.svg`;
		a.click();
		URL.revokeObjectURL(url);
	}
</script>

<div class="wf-graph">
	<div class="wf-graph-toolbar">
		<button class="wf-graph-btn" onclick={copyMermaid}>{copied ? 'Copied ✓' : 'Copy .mmd'}</button>
		<button class="wf-graph-btn" onclick={downloadSvg}>Download SVG</button>
	</div>
	<div class="wf-graph-scroll">
		<svg
			bind:this={svgEl}
			width={layout.width}
			height={layout.height}
			viewBox="0 0 {layout.width} {layout.height}"
		>
			<!-- edges -->
			{#each layout.phases as p}
				{#each p.agents as ag}
					<path class="wf-edge" d={edge(p.x, p.y, ag.x, ag.y)} />
				{/each}
			{/each}
			<!-- phase nodes -->
			{#each layout.phases as p}
				<g transform="translate({p.x},{p.y})">
					<rect class="wf-node-phase" width={PHASE_W} height={NODE_H} rx="6" />
					<text class="wf-node-phase-label" x="12" y="20">{p.title}</text>
					<text class="wf-node-sub" x="12" y="36">{p.agents.length} agents</text>
				</g>
			{/each}
			<!-- agent nodes -->
			{#each layout.phases as p}
				{#each p.agents as ag}
					<g transform="translate({ag.x},{ag.y})">
						<rect class="wf-node-agent {ag.a.state}" width={AGENT_W} height={NODE_H} rx="6" />
						{#if ag.a.state === 'running'}
							<circle class="wf-run-dot" cx="14" cy={NODE_H / 2} r="4" />
						{/if}
						<text class="wf-node-agent-label" x="28" y="20">{ag.a.label}</text>
						<text class="wf-node-sub" x="28" y="36"
							>{ag.a.model} · {fmtTokens(ag.a.tokens)} · {ag.a.toolCalls} tools</text
						>
					</g>
				{/each}
			{/each}
		</svg>
	</div>
</div>

<style>
	.wf-graph {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
	}
	.wf-graph-toolbar {
		display: flex;
		gap: var(--space-sm);
		justify-content: flex-end;
	}
	.wf-graph-btn {
		font-family: var(--font-mono);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		background: var(--bg-card);
		border: 1px solid var(--border-default);
		border-radius: 4px;
		padding: 4px 10px;
		cursor: pointer;
	}
	.wf-graph-btn:hover {
		color: var(--accent-amber);
		border-color: var(--accent-amber);
	}
	.wf-graph-scroll {
		overflow: auto;
		max-height: 60vh;
		border: 1px solid var(--border-default);
		border-radius: 6px;
		background: var(--bg-base);
	}
	.wf-edge {
		fill: none;
		stroke: var(--border-default);
		stroke-width: 1.5;
	}
	.wf-node-phase {
		fill: #1a1200;
		stroke: var(--accent-amber);
		stroke-width: 1.5;
	}
	.wf-node-phase-label {
		font-family: var(--font-pixel);
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		fill: var(--accent-amber);
	}
	.wf-node-agent {
		fill: var(--bg-card);
		stroke: var(--border-default);
		stroke-width: 1.5;
	}
	.wf-node-agent.completed {
		stroke: #2ecc71;
	}
	.wf-node-agent.failed {
		stroke: #e74c3c;
	}
	.wf-node-agent.running {
		stroke: var(--accent-amber);
	}
	.wf-node-agent-label {
		font-family: var(--font-mono);
		font-size: 12px;
		fill: var(--text-primary);
	}
	.wf-node-sub {
		font-family: var(--font-mono);
		font-size: 10px;
		fill: var(--text-muted);
	}
	.wf-run-dot {
		fill: var(--accent-amber);
		animation: wf-pulse 1.2s ease-in-out infinite;
	}
	@keyframes wf-pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.3;
		}
	}
</style>
