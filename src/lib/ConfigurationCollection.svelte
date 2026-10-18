<script lang="ts">
	import { onMount } from 'svelte';
	import ConfigurationFields, { type FieldChoices } from './ConfigurationFields.svelte';
	import type { ActionDefinition, ConfigurationActionDescriptor } from './contracts/index';
	import { DesktopRequestError } from './desktop';

	/** Public metadata and editable values for one configuration record. */
	export interface ConfigurationRecord {
		id: string;
		title: string;
		values: Record<string, unknown>;
	}
	/** A schema-backed collection whose persistence belongs to its integration. */
	export interface CollectionSnapshot {
		definition: ActionDefinition;
		records: ConfigurationRecord[];
		choices: Record<string, FieldChoices>;
		actions: ConfigurationActionDescriptor[];
	}
	let {
		title,
		itemLabel = 'entry',
		description = '',
		load,
		save,
		remove,
		open,
		close,
		act,
		pickDirectory,
		active = true
	}: {
		title: string;
		itemLabel?: string;
		description?: string;
		load: () => Promise<CollectionSnapshot>;
		save: (id: string | null, values: Record<string, unknown>) => Promise<string | void>;
		remove: (id: string) => Promise<void>;
		open?: (id: string) => Promise<Record<string, unknown>>;
		close?: () => Promise<void>;
		act?: (record_id: string, action_id: string) => Promise<string | void>;
		pickDirectory?: (field_path: string[], current_path: string | null) => Promise<string | null>;
		active?: boolean;
	} = $props();
	let snapshot = $state<CollectionSnapshot | null>(null);
	let draft = $state<Record<string, unknown>>({});
	let editing = $state<string | null>(null);
	let editorOpen = $state(false);
	let busy = $state(false);
	let error = $state('');
	let credential = $state('');
	let message = $state('');
	let confirmation = $state<ConfigurationActionDescriptor | null>(null);
	$effect(() => {
		if (!active) {
			credential = '';
			confirmation = null;
		}
	});
	let initialized = false;
	let mounted = true;
	/** Load public records without discarding an existing or unsuccessful draft. */
	async function refresh() {
		try {
			const next = await load();
			if (!mounted) return;
			snapshot = next;
			if (!initialized) {
				draft = structuredClone(next.definition.defaults);
				initialized = true;
			}
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError ? cause.message : 'Settings could not be loaded.';
		}
	}
	onMount(() => {
		void refresh();
		return () => {
			mounted = false;
			void close?.().catch(() => undefined);
		};
	});
	/** Start an independent draft, retaining nested record data without shared references. */
	async function edit(record: ConfigurationRecord) {
		if (busy || credential) return;
		busy = true;
		error = '';
		try {
			const values = open ? await open(record.id) : record.values;
			if (!mounted) {
				await close?.();
				return;
			}
			editing = record.id;
			editorOpen = true;
			draft = structuredClone($state.snapshot(values));
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError ? cause.message : 'The entry could not be opened.';
		} finally {
			if (mounted) busy = false;
		}
	}

	/** Reset only after success or an explicit cancellation. */
	async function reset() {
		await close?.();
		editing = null;
		confirmation = null;
		message = '';
		editorOpen = false;
		draft = structuredClone($state.snapshot(snapshot?.definition.defaults ?? {}));
	}
	/** Open a fresh record without mixing it with an existing edit session. */
	async function create() {
		if (busy || credential) return;
		error = '';
		try {
			await reset();
			editorOpen = true;
		} catch {
			error = 'The editor could not be opened.';
		}
	}
	/** Submit the schema-shaped draft and reveal any newly issued credential once. */
	async function submit() {
		if (busy || credential) return;
		busy = true;
		error = '';
		try {
			const result = await save(editing, structuredClone($state.snapshot(draft)));
			if (!mounted) return;
			credential = result ?? '';
			await reset();
			await refresh();
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError
						? cause.message
						: 'Settings could not be saved. Your entries are still available.';
		} finally {
			if (mounted) busy = false;
		}
	}
	/** Run one integration-declared action without discarding the current unsaved draft. */
	async function runAction(action: ConfigurationActionDescriptor) {
		if (!act || !editing || busy || credential) return;
		busy = true;
		error = '';
		message = '';
		confirmation = null;
		try {
			const result = await act(editing, action.id);
			if (!mounted) return;
			credential = result ?? '';
			message = action.success_message;
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError
						? cause.message
						: 'The action could not be completed. Your entries are still available.';
		} finally {
			if (mounted) busy = false;
		}
	}

	/** Ask for declared confirmation inline before running an irreversible record action. */
	function requestAction(action: ConfigurationActionDescriptor) {
		if (busy || credential) return;
		if (action.confirmation) confirmation = action;
		else void runAction(action);
	}

	/** Remove one record and leave unrelated drafts intact. */
	async function deleteRecord(id: string) {
		if (busy) return;
		busy = true;
		error = '';
		try {
			await remove(id);
			if (!mounted) return;
			if (editing === id) await reset();
			await refresh();
		} catch (cause) {
			if (mounted)
				error =
					cause instanceof DesktopRequestError ? cause.message : 'The entry could not be removed.';
		} finally {
			if (mounted) busy = false;
		}
	}
</script>

<section aria-label={title} aria-busy={busy}>
	<header>
		<div>
			<h2>{title}</h2>
			{#if description}<p class="description">{description}</p>{/if}
		</div>
		{#if snapshot && !editorOpen && !credential}<button
				class="primary"
				onclick={create}
				disabled={busy}>Add {itemLabel}</button
			>{/if}
	</header>
	{#if error}<p role="alert">{error}</p>
		<button
			onclick={() => {
				error = '';
				void refresh();
			}}
			disabled={busy}>Retry</button
		>{/if}
	{#if snapshot}
		{#if !editorOpen}
			{#if !snapshot.records.length}<p class="empty">
					No {title.toLowerCase()} yet. Add one to get started.
				</p>{/if}
			{#each snapshot.records as record (record.id)}
				<div class="record">
					<strong>{record.title}</strong><button
						disabled={busy || Boolean(credential)}
						onclick={() => edit(record)}>Edit</button
					><button disabled={busy} onclick={() => deleteRecord(record.id)}>Remove</button>
				</div>
			{/each}
		{/if}
		{#if editorOpen}<form
				onsubmit={(event) => {
					event.preventDefault();
					void submit();
				}}
			>
				<h3>{editing ? `Edit ${itemLabel}` : `New ${itemLabel}`}</h3>
				<ConfigurationFields
					fields={snapshot.definition.schema.fields}
					values={draft}
					choices={snapshot.choices}
					disabled={busy}
					{pickDirectory}
					onChange={(value) => (draft = value)}
				/>
				{#if editing && act}
					{#each snapshot.actions ?? [] as action (action.id)}
						<div class="record-action">
							<button
								type="button"
								disabled={busy || Boolean(credential)}
								onclick={() => requestAction(action)}>{action.label}</button
							>
							{#if action.description}<p class="description">{action.description}</p>{/if}
						</div>
					{/each}
					{#if confirmation}<div
							class="confirmation"
							role="group"
							aria-label={`${confirmation.label} confirmation`}
						>
							<p>{confirmation.confirmation}</p>
							<button type="button" disabled={busy} onclick={() => (confirmation = null)}
								>Cancel action</button
							>
							<button
								type="button"
								disabled={busy}
								onclick={() => confirmation && runAction(confirmation)}>{confirmation.label}</button
							>
						</div>{/if}
				{/if}
				<div class="actions">
					<button
						type="button"
						disabled={busy}
						onclick={() => {
							void reset().catch(() => (error = 'The editor could not be closed.'));
						}}>Cancel</button
					><button class="primary" disabled={busy || Boolean(credential)}
						>{busy ? 'Working…' : editing ? 'Save changes' : `Create ${itemLabel}`}</button
					>
				</div>
			</form>{/if}
	{:else}<p>Loading settings…</p>{/if}
	{#if message}<p role="status">{message}</p>{/if}
	{#if credential}<div class="credential">
			<h3>Copy your credential</h3>
			<p>Keep this credential private. Copy it into your client before hiding it.</p>
			<input
				aria-label="New credential"
				readonly
				value={credential}
				onclick={(event) => event.currentTarget.select()}
			/><button onclick={() => (credential = '')}>Hide credential</button>
		</div>{/if}
</section>

<style>
	.record-action,
	.confirmation {
		margin-top: 16px;
	}
	section {
		max-width: 880px;
		margin-top: 24px;
	}
	h2,
	h3 {
		font-size: 16px;
		font-weight: 600;
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 24px;
	}
	h2 {
		margin: 0;
	}
	.description,
	.empty {
		color: var(--muted);
		line-height: 1.5;
	}
	.description {
		margin: 6px 0 0;
		max-width: 60ch;
	}
	.empty {
		padding: 24px 0;
	}
	button.primary {
		background: var(--maroon);
		border-color: var(--maroon);
	}
	form,
	.credential {
		padding: 16px;
		background: var(--side);
		border: 0;
		margin-top: 16px;
		border-radius: 4px;
	}
	.record,
	.actions {
		display: flex;
		gap: 12px;
		align-items: center;
		padding: 12px 0;
	}
	.record strong {
		flex: 1;
	}
	.actions {
		justify-content: flex-end;
	}
	button {
		min-height: 32px;
		padding: 4px 12px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--surface);
	}
	input {
		width: 100%;
		padding: 8px;
		box-sizing: border-box;
		background: var(--surface);
		border: 1px solid var(--border);
		color: var(--text);
	}
</style>
