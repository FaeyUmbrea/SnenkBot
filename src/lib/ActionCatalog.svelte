<script lang="ts">
	import type { CatalogItem } from './canvas';
	import { endWorkflowDrag, startWorkflowDrag } from './drag';
	import Icon from './Icon.svelte';
	import './theme.css';

	let {
		items,
		disabled = false,
		onChoose,
		query,
		onQueryChange
	}: {
		items: readonly CatalogItem[];
		disabled?: boolean;
		onChoose?: (item: CatalogItem) => void;
		query?: string;
		onQueryChange?: (query: string) => void;
	} = $props();
	let localQuery = $state('');
	let limit = $state(50);
	const search = $derived(query ?? localQuery);
	const filtered = $derived(
		items.filter((item) =>
			`${item.title} ${item.category} ${item.description ?? ''}`
				.toLowerCase()
				.includes(search.toLowerCase())
		)
	);
</script>

<aside class="workflow-editor action-catalog" aria-label="Action library">
	<label class="search-label"
		>Find an action <input
			type="search"
			aria-label="Find an action"
			value={search}
			oninput={(e) => {
				localQuery = e.currentTarget.value;
				onQueryChange?.(localQuery);
				limit = 50;
			}}
		/></label
	>
	<div class="catalog-items">
		{#each filtered.slice(0, limit) as item (item.id)}
			<button
				class="catalog-item"
				draggable={!disabled}
				{disabled}
				ondragstart={(event) => startWorkflowDrag(event, { type: 'catalog', catalogId: item.id })}
				ondragend={endWorkflowDrag}
				onclick={() => onChoose?.(item)}
				aria-label={`Add ${item.title}`}
			>
				<span class="catalog-icon"><Icon name="zap" size={16} /></span>
				<span class="catalog-text"><strong>{item.title}</strong><span>{item.category}</span></span>
			</button>
		{/each}
		{#if filtered.length > limit}<button onclick={() => (limit += 50)}>Show more actions</button
			>{/if}
		{#if filtered.length === 0}<p>No matching actions.</p>{/if}
	</div>
</aside>

<style>
	.action-catalog {
		background: var(--side);
		padding: 16px;
		min-width: 220px;
	}
	.search-label {
		display: flex;
		flex-direction: column;
		gap: 8px;
		color: var(--muted);
		font-size: 12px;
	}
	input {
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
		min-height: 34px;
		width: 100%;
		padding: 7px 9px;
	}
	.catalog-items {
		margin-top: 14px;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.catalog-item {
		display: flex;
		width: 100%;
		text-align: left;
		align-items: center;
		gap: 10px;
		padding: 10px;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--surface);
		cursor: grab;
	}
	.catalog-item:active {
		cursor: grabbing;
	}
	.catalog-icon {
		display: grid;
		place-items: center;
		width: 28px;
		height: 28px;
		background: var(--maroon);
		border-radius: 6px;
		flex: none;
	}
	.catalog-text {
		display: flex;
		flex-direction: column;
		gap: 3px;
		overflow-wrap: anywhere;
	}
	.catalog-text strong {
		font-weight: 600;
	}
	.catalog-text > span,
	p {
		color: var(--muted);
		font-size: 12px;
	}
</style>
