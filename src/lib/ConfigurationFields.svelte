<script lang="ts">
	import type { ConfigField, ConfigChoice } from './contracts/index';
	import ConfigurationFields from './ConfigurationFields.svelte';
	import StringListField from './StringListField.svelte';
	import JsonField from './JsonField.svelte';
	import { DesktopRequestError } from './desktop';

	/** Choices supplied by the owning integration for reusable set and map editors. */
	export interface FieldChoices {
		items?: ConfigChoice[] | null;
		groups?: { id: string; label: string; items: ConfigChoice[] }[] | null;
	}
	const instanceId = $props.id();
	let {
		fields,
		values,
		choices = {},
		prefix = instanceId,
		fieldPath = [],
		pickDirectory,
		disabled = false,
		updateOnInput = true,
		onChange
	}: {
		fields: ConfigField[];
		values: Record<string, unknown>;
		choices?: Record<string, FieldChoices>;
		prefix?: string;
		fieldPath?: string[];
		pickDirectory?: (field_path: string[], current_path: string | null) => Promise<string | null>;
		disabled?: boolean;
		/** Revisioned forms can commit text on blur instead of validating unfinished keystrokes. */
		updateOnInput?: boolean;
		onChange: (values: Record<string, unknown>) => void;
	} = $props();

	let directoryError = $state<Record<string, string>>({});
	let browsing = $state<string | null>(null);
	let searches = $state<Record<string, string>>({});

	/** Browse for one declared directory without changing its draft when the picker is cancelled. */
	async function browse(field: ConfigField) {
		if (!pickDirectory || disabled || browsing) return;
		browsing = field.id;
		directoryError = { ...directoryError, [field.id]: '' };
		try {
			const current = values[field.id];
			const selected = await pickDirectory(
				[...fieldPath, field.id],
				typeof current === 'string' && current ? current : null
			);
			if (selected !== null) update(field.id, selected);
		} catch (cause) {
			directoryError = {
				...directoryError,
				[field.id]:
					cause instanceof DesktopRequestError
						? cause.message
						: 'The folder could not be selected. You can enter its path instead.'
			};
		} finally {
			browsing = null;
		}
	}

	/** Replace one field while retaining the rest of the configuration draft. */
	function update(id: string, value: unknown) {
		onChange({ ...values, [id]: value });
	}
	/** Read an object without treating arrays or missing values as nested settings. */
	function object(value: unknown): Record<string, unknown> {
		return value && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: {};
	}
	/** Keep collection values typed, including empty defaults. */
	function strings(value: unknown): string[] {
		return Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : [];
	}
	/** Toggle one explicit choice without dropping selections missing from a refreshed catalog. */
	function toggle(value: unknown, id: string, checked: boolean): string[] {
		return checked ? [...new Set([...strings(value), id])] : strings(value).filter((v) => v !== id);
	}
	/** Add an editable map key without replacing any existing entry. */
	function addKey(id: string) {
		const map = object(values[id]);
		let key = 'key';
		let suffix = 1;
		while (Object.hasOwn(map, key)) key = `key${++suffix}`;
		update(id, { ...map, [key]: [] });
	}
	/** Rename a key only when it is nonempty and does not overwrite another set. */
	function renameKey(id: string, old: string, input: HTMLInputElement) {
		const map = object(values[id]);
		const key = input.value.trim();
		if (!key || (key !== old && Object.hasOwn(map, key))) {
			input.setCustomValidity('Enter a unique, nonempty key.');
			input.reportValidity();
			return;
		}
		input.setCustomValidity('');
		update(
			id,
			Object.fromEntries(
				Object.entries(map).map(([name, value]) => [name === old ? key : name, value])
			)
		);
	}
	/** Remove one set while keeping every other map entry. */
	function removeKey(id: string, key: string) {
		update(
			id,
			Object.fromEntries(Object.entries(object(values[id])).filter(([name]) => name !== key))
		);
	}
</script>

<div class="fields">
	{#each fields.filter((field) => field.kind !== 'secret') as field (field.id)}
		{@const id = `${prefix}-${field.id}`}
		{@const source = field.choice_source ? choices[field.choice_source.key] : undefined}
		{#if field.kind === 'directory'}
			<div class="field-row text-row">
				<label for={id}
					>{field.label}{#if field.description}<span>{field.description}</span>{/if}</label
				>
				<div class="directory-control">
					<input
						{id}
						{disabled}
						aria-label={field.label}
						value={typeof values[field.id] === 'string' ? values[field.id] : ''}
						oninput={(event) =>
							update(
								field.id,
								!field.required && !event.currentTarget.value ? null : event.currentTarget.value
							)}
					/>
					{#if pickDirectory}<button
							class="field-button"
							type="button"
							disabled={disabled || browsing !== null}
							onclick={() => browse(field)}>Browse…</button
						>{/if}
				</div>
				{#if directoryError[field.id]}<p role="alert">{directoryError[field.id]}</p>{/if}
			</div>
		{:else if field.kind === 'json'}
			<div class="field-group">
				<label for={id}>{field.label}</label>
				{#if field.description}<p class="description">{field.description}</p>{/if}
				<JsonField
					{id}
					label={field.label}
					value={values[field.id]}
					{disabled}
					onChange={(value) => update(field.id, value)}
				/>
			</div>
		{:else if typeof field.kind === 'object'}
			<section class="field-group" aria-labelledby={id}>
				<h3 {id}>{field.label}</h3>
				{#if field.description}<p class="description">{field.description}</p>{/if}
				<ConfigurationFields
					fields={field.kind.object}
					values={object(values[field.id])}
					{choices}
					prefix={id}
					fieldPath={[...fieldPath, field.id]}
					{pickDirectory}
					{disabled}
					{updateOnInput}
					onChange={(value) => update(field.id, value)}
				/>
			</section>
		{:else if field.kind === 'string_set_map'}
			<details class="permission-field">
				<summary
					><strong>{field.label}</strong><span class="selection-count"
						>{Object.values(object(values[field.id])).reduce<number>(
							(count, entries) => count + strings(entries).length,
							0
						)} selected</span
					></summary
				>
				{#if field.description}<p class="description">{field.description}</p>{/if}
				{#if (source?.groups?.length ?? 0) > 5 || (source?.groups?.reduce((count, group) => count + group.items.length, 0) ?? 0) > 5}<input
						type="search"
						aria-label={`Search ${field.label}`}
						placeholder="Find a service or lookup…"
						value={searches[field.id] ?? ''}
						oninput={(event) => (searches = { ...searches, [field.id]: event.currentTarget.value })}
					/>{/if}
				<div class="choice-list">
					{#each (source?.groups ?? []).filter( (group) => `${group.label} ${group.items.map((item) => item.label).join(' ')}`
								.toLowerCase()
								.includes((searches[field.id] ?? '').toLowerCase()) ) as group (group.id)}
						{@const selected = strings(object(values[field.id])[group.id])}
						<details class="choice-group" open={Boolean(searches[field.id]?.trim())}>
							<summary
								><strong>{group.label}</strong><span class="selection-count"
									>{selected.length} selected</span
								></summary
							>
							<div class="group-choices">
								{#each group.items.filter((item) => group.label
											.toLowerCase()
											.includes((searches[field.id] ?? '').toLowerCase()) || `${item.label} ${item.detail ?? ''}`
											.toLowerCase()
											.includes((searches[field.id] ?? '').toLowerCase())) as item (item.value)}
									<label class="choice"
										><input
											type="checkbox"
											{disabled}
											checked={selected.includes(item.value)}
											onchange={(event) =>
												update(field.id, {
													...object(values[field.id]),
													[group.id]: toggle(
														object(values[field.id])[group.id],
														item.value,
														event.currentTarget.checked
													)
												})}
										/><span class="choice-copy"
											><strong>{item.label}</strong>{#if item.detail}<small>{item.detail}</small
												>{/if}</span
										></label
									>
								{/each}
								{#each selected.filter((value) => !group.items.some((item) => item.value === value)) as value (value)}
									<p class="unavailable-choice">Saved choice unavailable: {value}</p>
								{/each}
							</div>
						</details>
					{/each}
					{#if !source?.groups}
						{#if field.choice_source}<p role="status">Choices are unavailable.</p>
						{:else}
							{#each Object.entries(object(values[field.id])) as [key, entries] (key)}
								<div class="map-entry">
									<input
										aria-label={`${field.label} key ${key}`}
										{disabled}
										value={key}
										oninput={(event) => event.currentTarget.setCustomValidity('')}
										onchange={(event) => renameKey(field.id, key, event.currentTarget)}
									/>
									<StringListField
										id={`${id}-${key}`}
										label={`${field.label} values for ${key}`}
										values={strings(entries)}
										unique
										{disabled}
										onChange={(next) =>
											update(field.id, { ...object(values[field.id]), [key]: next })}
									/><button
										class="field-button"
										type="button"
										{disabled}
										onclick={() => removeKey(field.id, key)}>Remove {key}</button
									>
								</div>
							{/each}
							<button class="field-button" type="button" {disabled} onclick={() => addKey(field.id)}
								>Add {field.label} entry</button
							>
						{/if}
					{/if}
				</div>
			</details>
		{:else if field.kind === 'string_set' || field.kind === 'string_list'}
			{#if field.kind === 'string_set' && source?.items}
				<details class="permission-field">
					<summary
						><strong>{field.label}</strong><span class="selection-count"
							>{strings(values[field.id]).length} selected</span
						></summary
					>
					{#if field.description}<p class="description">{field.description}</p>{/if}
					{#each source.items as item (item.value)}
						<label class="choice"
							><input
								type="checkbox"
								{disabled}
								checked={strings(values[field.id]).includes(item.value)}
								onchange={(event) =>
									update(
										field.id,
										toggle(values[field.id], item.value, event.currentTarget.checked)
									)}
							/><span class="choice-copy"
								><strong>{item.label}</strong>{#if item.detail}<small>{item.detail}</small
									>{/if}</span
							></label
						>
					{/each}
				</details>
			{:else if field.choice_source && !source}
				<p role="status">{field.label} choices are unavailable.</p>
			{:else}
				<StringListField
					{id}
					label={field.label}
					description={field.description}
					values={strings(values[field.id])}
					unique={field.kind === 'string_set'}
					collapsible={!field.choice_source}
					{disabled}
					suggestions={source?.items ?? []}
					onChange={(next) => update(field.id, next)}
				/>
			{/if}
		{:else}
			<div
				class="field-row"
				class:toggle-row={field.kind === 'toggle'}
				class:text-row={field.kind !== 'toggle'}
			>
				<label for={id}
					>{field.label}{#if field.description}<span>{field.description}</span>{/if}</label
				>
				{#if field.kind === 'toggle'}
					<input
						{id}
						{disabled}
						aria-label={field.label}
						type="checkbox"
						checked={Boolean(values[field.id])}
						onchange={(event) => update(field.id, event.currentTarget.checked)}
					/>
				{:else if field.choice_source}
					<select
						{id}
						aria-label={field.label}
						disabled={disabled || !source?.items?.length}
						required={field.required}
						value={String(values[field.id] ?? '')}
						onchange={(event) =>
							update(
								field.id,
								!event.currentTarget.value && !field.required
									? null
									: field.kind === 'integer'
										? Number(event.currentTarget.value)
										: event.currentTarget.value
							)}
					>
						<option value="" disabled={field.required}>
							{source?.items?.length
								? field.required
									? 'Select an option…'
									: 'None'
								: 'Choices unavailable'}
						</option>
						{#if values[field.id] != null && String(values[field.id]) !== '' && !source?.items?.some((item) => item.value === String(values[field.id]))}
							<option value={String(values[field.id])}
								>{String(values[field.id])} (unavailable)</option
							>
						{/if}
						{#each source?.items ?? [] as item (item.value)}
							<option value={item.value}>{item.label}</option>
						{/each}
					</select>
				{:else}
					<input
						{id}
						{disabled}
						aria-label={field.label}
						type={field.kind === 'integer' ? 'number' : 'text'}
						required={field.required}
						step={field.kind === 'integer' ? 1 : undefined}
						value={String(values[field.id] ?? '')}
						oninput={(event) =>
							updateOnInput &&
							update(
								field.id,
								field.kind === 'integer' && event.currentTarget.value !== ''
									? Number(event.currentTarget.value)
									: event.currentTarget.value
							)}
						onchange={(event) =>
							update(
								field.id,
								field.kind === 'integer' && event.currentTarget.value !== ''
									? Number(event.currentTarget.value)
									: event.currentTarget.value
							)}
					/>
				{/if}
			</div>
		{/if}
	{/each}
</div>

<style>
	.fields {
		display: grid;
		gap: 16px;
		min-width: 0;
	}
	.directory-control {
		display: flex;
		gap: 8px;
	}
	.directory-control input {
		flex: 1;
		min-width: 0;
	}
	.field-row {
		padding: 0;
	}
	.text-row {
		display: grid;
		gap: 6px;
	}
	.toggle-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		min-height: 36px;
		gap: 24px;
	}
	.map-entry {
		display: grid;
		grid-template-columns: minmax(140px, 220px) minmax(0, 1fr) auto;
		align-items: start;
		gap: 12px;
		padding: 8px 0;
	}
	.field-row label span,
	p {
		display: block;
		color: var(--muted);
		font-size: 12px;
		margin: 4px 0;
	}
	input:not([type='checkbox']),
	select {
		width: 100%;
		box-sizing: border-box;
		padding: 6px 8px;
		color: var(--text);
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 4px;
	}
	input[type='checkbox'] {
		width: 18px;
		height: 18px;
		justify-self: start;
	}
	.toggle-row input[type='checkbox'] {
		appearance: none;
		position: relative;
		flex: none;
		width: 38px;
		height: 22px;
		margin: 0;
		border: 1px solid var(--border);
		border-radius: 12px;
		background: var(--field);
		cursor: pointer;
	}
	.toggle-row input::before {
		content: '';
		position: absolute;
		top: 3px;
		left: 3px;
		width: 14px;
		height: 14px;
		border-radius: 50%;
		background: var(--muted);
	}
	.toggle-row input:checked {
		background: var(--maroon);
		border-color: var(--blood);
	}
	.toggle-row input:checked::before {
		transform: translateX(16px);
		background: var(--text);
	}
	.choice {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		margin: 7px 0;
		padding: 4px 0;
	}
	.choice-copy {
		display: grid;
		gap: 2px;
	}
	.choice-copy strong {
		font-weight: 500;
		color: var(--text);
	}
	.choice-copy small {
		color: var(--muted);
		font-size: 12px;
	}
	.field-group,
	.permission-field {
		margin: 0;
		padding: 0;
	}
	.field-group h3 {
		margin: 0 0 6px;
		font-size: 14px;
	}
	.choice-group {
		margin: 6px 0;
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 0 12px;
	}
	.permission-field > summary,
	.choice-group summary {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 10px 2px;
		cursor: pointer;
	}
	.permission-field > summary::marker,
	.choice-group summary::marker {
		content: '';
	}
	.permission-field > summary::-webkit-details-marker,
	.choice-group summary::-webkit-details-marker {
		display: none;
	}
	.permission-field > summary::before,
	.choice-group summary::before {
		content: '▸';
		color: var(--muted);
	}
	.permission-field[open] > summary::before,
	.choice-group[open] summary::before {
		content: '▾';
	}
	.permission-field > summary strong,
	.choice-group summary strong {
		flex: 1;
		font-size: 13px;
		font-weight: 500;
	}
	.selection-count {
		font-size: 12px;
		color: var(--muted);
		white-space: nowrap;
	}
	.group-choices {
		padding: 0 0 10px 20px;
	}
	.unavailable-choice {
		margin: 8px 0;
	}
	.choice-list {
		max-height: 360px;
		overflow: auto;
	}
	.permission-field {
		padding: 0 16px;
		background: var(--side);
		border: 1px solid var(--border);
		border-radius: 8px;
	}
	.permission-field > summary {
		padding: 14px 0;
	}
	.permission-field .choice {
		padding: 8px 0;
	}
	.description {
		margin: 4px 0 10px;
	}
	@media (max-width: 640px) {
		.map-entry {
			grid-template-columns: 1fr;
			gap: 8px;
		}
	}
</style>
