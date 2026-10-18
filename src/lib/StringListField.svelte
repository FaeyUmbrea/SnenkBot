<script lang="ts">
	import type { ConfigChoice } from './contracts/index';

	let {
		id,
		label,
		description = '',
		values,
		unique = false,
		collapsible = false,
		disabled = false,
		suggestions = [],
		onChange
	}: {
		id: string;
		label: string;
		description?: string;
		values: string[];
		unique?: boolean;
		collapsible?: boolean;
		disabled?: boolean;
		suggestions?: ConfigChoice[];
		onChange: (values: string[]) => void;
	} = $props();
	let pending = $state('');
	let error = $state('');
	let input: HTMLInputElement;

	/** Append one trimmed value, retaining duplicates only for list fields. */
	function add() {
		const value = pending.trim();
		if (!value) return;
		if (unique && values.includes(value)) {
			error = 'This value is already listed.';
			return;
		}
		onChange([...values, value]);
		pending = '';
		error = '';
		input?.focus();
	}

	/** Commit a row edit or remove an emptied row without changing sibling values. */
	function replace(index: number, element: HTMLInputElement) {
		const value = element.value.trim();
		if (unique && values.some((entry, position) => position !== index && entry === value)) {
			element.setCustomValidity('This value is already listed.');
			element.reportValidity();
			return;
		}
		element.setCustomValidity('');
		error = '';
		onChange(
			value
				? values.map((entry, position) => (position === index ? value : entry))
				: values.filter((_, position) => position !== index)
		);
	}

	/** Keep Enter inside this editor and let Escape discard an unfinished row edit. */
	function rowKey(event: KeyboardEvent, index: number) {
		if (event.key === 'Enter') {
			event.preventDefault();
			(event.currentTarget as HTMLInputElement).blur();
		} else if (event.key === 'Escape') {
			event.preventDefault();
			const element = event.currentTarget as HTMLInputElement;
			element.value = values[index];
			element.setCustomValidity('');
			element.blur();
		}
	}

	/** Remove a committed row; the caller owns the updated array. */
	function remove(index: number) {
		onChange(values.filter((_, position) => position !== index));
	}
</script>

<details class="list-field" open={!collapsible}>
	<summary><strong>{label}</strong><span class="count">{values.length}</span></summary>
	{#if description}<p class="description">{description}</p>{/if}
	{#if values.length === 0}<p class="empty">No entries added.</p>{/if}
	<div class="entries">
		{#each values as entry, index (index)}
			<div class="entry">
				<input
					aria-label={`${label} item ${index + 1}`}
					value={entry}
					{disabled}
					oninput={(event) => event.currentTarget.setCustomValidity('')}
					onchange={(event) => replace(index, event.currentTarget)}
					onkeydown={(event) => rowKey(event, index)}
				/>
				<button
					class="field-button"
					type="button"
					{disabled}
					aria-label={`Remove ${label} item ${index + 1}`}
					onclick={() => remove(index)}>Remove</button
				>
			</div>
		{/each}
		<div class="entry add-entry">
			<input
				bind:this={input}
				bind:value={pending}
				aria-label={`Add ${label}`}
				aria-invalid={Boolean(error)}
				{disabled}
				list={suggestions.length ? `${id}-suggestions` : undefined}
				oninput={() => (error = '')}
				onkeydown={(event) => {
					if (event.key === 'Enter') {
						event.preventDefault();
						add();
					} else if (event.key === 'Escape') {
						event.preventDefault();
						pending = '';
						error = '';
					}
				}}
			/>
			{#if suggestions.length}<datalist id={`${id}-suggestions`}
					>{#each suggestions as choice (choice.value)}<option value={choice.value}
							>{choice.label}</option
						>{/each}</datalist
				>{/if}
			<button class="field-button" type="button" {disabled} onclick={add}>Add</button>
		</div>
	</div>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
</details>

<style>
	.list-field {
		margin: 0;
		padding: 0 16px;
		border: 1px solid var(--border);
		border-radius: 8px;
		background: var(--side);
	}
	summary {
		cursor: pointer;
		padding: 14px 0;
	}
	.count {
		display: inline-block;
		min-width: 20px;
		margin-left: 8px;
		padding: 1px 5px;
		border-radius: 10px;
		background: var(--surface);
		color: var(--muted);
		font-size: 11px;
		text-align: center;
	}
	.description,
	.empty {
		margin: 4px 0 10px;
		color: var(--muted);
		font-size: 12px;
	}
	.entries {
		display: grid;
		gap: 6px;
		max-width: none;
		padding: 4px 0 10px;
	}
	.entry {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 8px;
	}
	input {
		min-width: 0;
		box-sizing: border-box;
		padding: 6px 8px;
		color: var(--text);
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 4px;
	}
	button {
		min-width: 64px;
	}
	.error {
		margin: 4px 0;
		color: var(--danger, #c84f4f);
		font-size: 12px;
	}
</style>
