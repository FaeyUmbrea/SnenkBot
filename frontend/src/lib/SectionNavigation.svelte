<script lang="ts">
	/** A labeled section within one configuration workspace. */
	interface Section {
		id: string;
		label: string;
	}
	let {
		label,
		items,
		value,
		onChange
	}: { label: string; items: Section[]; value: string; onChange: (id: string) => void } = $props();
</script>

<nav aria-label={label}>
	{#each items as item (item.id)}<button
			aria-current={value === item.id ? 'page' : undefined}
			onclick={() => onChange(item.id)}>{item.label}</button
		>{/each}
</nav>

<style>
	nav {
		display: flex;
		gap: 24px;
		border-bottom: 1px solid var(--border);
		margin-bottom: 24px;
	}
	button {
		background: transparent;
		border: 0;
		border-bottom: 2px solid transparent;
		padding: 8px 0 12px;
		color: var(--muted);
	}
	button[aria-current] {
		border-bottom-color: var(--gold);
		color: var(--text);
	}
</style>
