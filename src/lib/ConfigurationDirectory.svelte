<script lang="ts">
	import type { ConfigurationCatalogEntry } from './contracts';
	let {
		title,
		entries,
		onSelect
	}: {
		title: string;
		entries: ConfigurationCatalogEntry[];
		onSelect: (entry: ConfigurationCatalogEntry) => void;
	} = $props();
	let query = $state('');
	const matches = $derived(
		entries.filter((entry) => entry.title.toLowerCase().includes(query.toLowerCase()))
	);
</script>

<section aria-label={title}>
	<header>
		<h1>{title}</h1>
		<p>Choose a service to configure.</p>
	</header>
	{#if entries.length > 5}<input
			type="search"
			aria-label={`Search ${title}`}
			placeholder="Search…"
			bind:value={query}
		/>{/if}
	{#if !matches.length}<p class="empty">
			{query ? 'No matching settings.' : 'No settings available.'}
		</p>{/if}
	{#each matches as entry (entry.id)}
		<button class="entry" onclick={() => onSelect(entry)}>
			<span
				><strong>{entry.title}</strong>{#if entry.error}<small class="entry-error"
						>{entry.error}</small
					>{/if}</span
			><span class="action">Configure <span aria-hidden="true">›</span></span>
		</button>
	{/each}
</section>

<style>
	section {
		max-width: 880px;
	}
	header {
		margin-bottom: 16px;
	}
	h1 {
		font-size: 20px;
		margin: 0 0 8px;
	}
	p {
		color: var(--muted);
		margin: 0 0 24px;
		line-height: 1.5;
	}
	button {
		min-height: 32px;
		border-radius: 4px;
		padding: 6px 12px;
	}
	input {
		width: 100%;
		background: var(--field);
		border: 1px solid var(--border);
		padding: 8px 12px;
		border-radius: 4px;
	}
	.empty {
		padding: 24px 0;
	}
	.entry {
		width: 100%;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 24px;
		padding: 20px 0;
		text-align: left;
		border: 0;
		border-bottom: 1px solid var(--border);
		background: transparent;
	}
	.entry:hover {
		background: var(--side);
	}
	strong {
		font-size: 16px;
		font-weight: 600;
	}
	.entry-error {
		display: block;
		margin-top: 6px;
		color: var(--muted);
		font-size: 12px;
	}
	.action {
		color: var(--muted);
		white-space: nowrap;
	}
</style>
