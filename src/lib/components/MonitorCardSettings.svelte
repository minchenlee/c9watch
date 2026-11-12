<script lang="ts">
	import {
		monitorCardFields,
		monitorCardFieldKeys,
		monitorCardPreferences,
		setMonitorCardField
	} from '$lib/stores/monitor-card-preferences';
</script>

<fieldset>
	<legend>Monitor card details</legend>
	<p>Working duration is available for Codex sessions with a recorded turn start.</p>
	{#each monitorCardFieldKeys as field (field)}
		<label>
			<span>{monitorCardFields[field]}</span>
			<input
				type="checkbox"
				checked={$monitorCardPreferences[field]}
				onchange={(event) => setMonitorCardField(field, event.currentTarget.checked)}
			/>
		</label>
	{/each}
</fieldset>

<style>
	fieldset {
		border: 0;
		margin: 0;
		padding: 0;
		min-width: 0;
		color: var(--text-primary);
		font: 14px/1.5 var(--font-sans);
	}

	legend {
		padding: 0;
		font: 600 13px/1.5 var(--font-pixel);
		text-transform: uppercase;
		letter-spacing: 0.1em;
	}

	p {
		margin: var(--space-sm) 0;
		color: var(--text-description);
	}

	label {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: var(--space-md);
		padding: var(--space-md) 0;
		border-bottom: 1px solid var(--border-default);
		cursor: pointer;
	}
</style>
