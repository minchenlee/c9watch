// Pure: turn a workflow's phases + agents into a mermaid `flowchart TD` string.
// Phase = parent node, its agents = children. Used for the "Copy as mermaid"
// export in WorkflowGraph. No DOM, no deps — verified live + via the mermaid skill.
import type { WorkflowAgent } from './types';

// Mermaid node ids must be [A-Za-z0-9_]. Build a stable, collision-free id per
// node from a prefix + index.
function makeId(prefix: string, index: number): string {
	return `${prefix}${index}`;
}

// Escape a label for a mermaid `["..."]` node. Quotes break the bracket-string;
// replace with the HTML entity mermaid accepts. Collapse newlines to spaces.
function esc(label: string): string {
	return label.replace(/"/g, '&quot;').replace(/\s*\n\s*/g, ' ').trim();
}

const STATE_CLASS: Record<string, string> = {
	running: 'wfRunning',
	completed: 'wfDone',
	failed: 'wfFailed',
	queued: 'wfQueued',
};

export function toMermaid(
	workflowName: string,
	phases: string[],
	agents: WorkflowAgent[]
): string {
	const lines: string[] = ['flowchart TD'];
	const root = 'root';
	lines.push(`\t${root}["${esc(workflowName) || 'Workflow'}"]`);

	// Group agents by phase, preserving phase order; unmatched → "Other".
	const byPhase = new Map<string, WorkflowAgent[]>();
	for (const p of phases) byPhase.set(p, []);
	const other: WorkflowAgent[] = [];
	for (const a of agents) {
		const bucket = byPhase.get(a.phaseTitle);
		if (bucket) bucket.push(a);
		else other.push(a);
	}
	const ordered: [string, WorkflowAgent[]][] = [...byPhase.entries()].filter(
		([, v]) => v.length > 0
	);
	if (other.length) ordered.push(['Other', other]);

	const classLines: string[] = [];
	let pi = 0;
	for (const [phase, phaseAgents] of ordered) {
		const pid = makeId('p', pi);
		lines.push(`\t${pid}["${esc(phase)}"]`);
		lines.push(`\t${root} --> ${pid}`);
		let ai = 0;
		for (const a of phaseAgents) {
			const aid = makeId(`a${pi}_`, ai);
			lines.push(`\t${aid}["${esc(a.label)}"]`);
			lines.push(`\t${pid} --> ${aid}`);
			const cls = STATE_CLASS[a.state];
			if (cls) classLines.push(`\tclass ${aid} ${cls}`);
			ai++;
		}
		pi++;
	}

	lines.push('\tclassDef wfRunning fill:#3a2a00,stroke:#ff6600,color:#fff');
	lines.push('\tclassDef wfDone fill:#0a2a14,stroke:#2ecc71,color:#fff');
	lines.push('\tclassDef wfFailed fill:#2a0a0a,stroke:#e74c3c,color:#fff');
	lines.push('\tclassDef wfQueued fill:#1a1a1a,stroke:#666,color:#aaa');
	lines.push(...classLines);

	return lines.join('\n');
}
