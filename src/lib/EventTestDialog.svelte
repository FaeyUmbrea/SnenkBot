<script lang="ts">
	import { onDestroy } from 'svelte';
	import ConfigurationFields, { type FieldChoices } from './ConfigurationFields.svelte';
	import { DesktopRequestError } from './desktop';
	import type { ActionChoices, ConfigChoice, EventTestOptions, SendTestEvent } from './contracts';

	let {
		open,
		load,
		choices,
		send,
		onClose
	}: {
		open: boolean;
		load: () => Promise<EventTestOptions>;
		choices: (request: ActionChoices) => Promise<ConfigChoice[]>;
		send: (request: SendTestEvent) => Promise<void>;
		onClose: () => void;
	} = $props();
	const titleId = $props.id();
	let dialog: HTMLDialogElement;
	let options = $state<EventTestOptions | null>(null);
	let consumerId = $state('');
	let senderId = $state('');
	let values = $state<Record<string, unknown>>({});
	let fieldChoices = $state<Record<string, FieldChoices>>({});
	let loading = $state(false);
	let sending = $state(false);
	let error = $state('');
	let success = $state('');
	let generation = 0;
	let choiceGeneration = 0;
	const consumer = $derived(options?.consumers.find((item) => item.id === consumerId));
	const senders = $derived(
		options?.senders.filter((item) => consumer?.channels.includes(item.channel)) ?? []
	);
	const sender = $derived(senders.find((item) => item.definition.schema.id === senderId));
	const fields = $derived(
		sender?.definition.schema.fields.filter((field) => field.id !== sender.target_field) ?? []
	);

	onDestroy(() => {
		generation += 1;
		dialog?.close();
	});

	$effect(() => {
		if (open) {
			if (dialog && !dialog.open) dialog.showModal();
			void refresh();
		} else {
			generation += 1;
			dialog?.close();
		}
	});

	/** Reload current recipients and sender definitions whenever the test dialog opens. */
	async function refresh() {
		const owner = ++generation;
		loading = true;
		sending = false;
		error = '';
		success = '';
		options = null;
		try {
			const result = await load();
			if (owner !== generation) return;
			options = result;
			consumerId = result.consumers[0]?.id ?? '';
			selectConsumer(consumerId);
		} catch (cause) {
			if (owner === generation)
				error = message(cause, 'Event testing could not be loaded. Try again.');
		} finally {
			if (owner === generation) loading = false;
		}
	}

	/** Select an allowed sender for the chosen recipient and restore its generated defaults. */
	function selectConsumer(id: string) {
		consumerId = id;
		const current = options?.consumers.find((item) => item.id === id);
		const available =
			options?.senders.filter((item) => current?.channels.includes(item.channel)) ?? [];
		selectSender(
			available.find((item) => item.definition.schema.id === senderId)?.definition.schema.id ??
				available[0]?.definition.schema.id ??
				''
		);
	}

	/** Use the registered definition for both defaults and generated field choices. */
	function selectSender(id: string) {
		senderId = id;
		const selected = options?.senders.find((item) => item.definition.schema.id === id);
		values = $state.snapshot(selected?.definition.defaults ?? {});
		fieldChoices = {};
		error = '';
		success = '';
		if (selected) void loadChoices(selected.definition.schema.id, generation);
	}

	/** Resolve declared pickers while discarding responses belonging to an earlier selection. */
	async function loadChoices(id: string, owner: number) {
		const pickerOwner = ++choiceGeneration;
		const selected = options?.senders.find((item) => item.definition.schema.id === id);
		if (!selected) return;
		try {
			const entries = await Promise.all(
				selected.definition.schema.fields
					.filter((field) => field.choice_source && field.id !== selected.target_field)
					.map(async (field) => {
						const dependency = field.choice_source?.depends_on;
						const parent = dependency ? values[dependency] : null;
						const items =
							dependency && (typeof parent !== 'string' || !parent)
								? []
								: await choices({
										action_id: id,
										field_id: field.id,
										depends_on: typeof parent === 'string' ? parent : null
									});
						return [field.choice_source!.key, { items }] as const;
					})
			);
			if (owner === generation && pickerOwner === choiceGeneration && senderId === id)
				fieldChoices = Object.fromEntries(entries);
		} catch (cause) {
			if (owner === generation && pickerOwner === choiceGeneration && senderId === id)
				error = message(cause, 'Event choices could not be loaded. Try again.');
		}
	}

	/** Keep editable generated inputs and refresh dependent resource pickers. */
	function changeValues(next: Record<string, unknown>) {
		values = next;
		success = '';
		if (sender) void loadChoices(sender.definition.schema.id, generation);
	}

	/** Deliver a single output test while keeping its draft available for repeated sends. */
	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!sender || !consumer || sending) return;
		const owner = generation;
		sending = true;
		error = '';
		success = '';
		try {
			await send({
				action_id: sender.definition.schema.id,
				version: sender.definition.schema.version,
				consumer_id: consumer.id,
				values: $state.snapshot(values)
			});
			if (owner === generation) success = `Test event sent to ${consumer.name}.`;
		} catch (cause) {
			if (owner === generation)
				error = message(cause, 'The test event could not be sent. Try again.');
		} finally {
			if (owner === generation) sending = false;
		}
	}

	/** Display structured native errors without exposing arbitrary transport failures. */
	function message(cause: unknown, fallback: string): string {
		return cause instanceof DesktopRequestError ? cause.message : fallback;
	}
</script>

<dialog
	bind:this={dialog}
	class="workflow-editor"
	aria-labelledby={titleId}
	oncancel={(event) => {
		event.preventDefault();
		if (!sending) onClose();
	}}
>
	<form onsubmit={submit}>
		<header>
			<h2 id={titleId}>Send test event</h2>
			<p>Send a sample to one consumer without running an automation.</p>
		</header>
		<div class="fields">
			{#if loading}<p role="status">Loading event senders…</p>
			{:else if options}
				<label
					>Consumer<select
						value={consumerId}
						disabled={sending}
						onchange={(event) => selectConsumer(event.currentTarget.value)}
						><option value="" disabled>Select a consumer</option
						>{#each options.consumers as item (item.id)}<option value={item.id}>{item.name}</option
							>{/each}</select
					></label
				>
				{#if options.consumers.length === 0}<p>
						Create a consumer in API settings to send a test event.
					</p>{/if}
				<label
					>Event<select
						value={senderId}
						disabled={sending || senders.length === 0}
						onchange={(event) => selectSender(event.currentTarget.value)}
						><option value="" disabled>Select an event sender</option
						>{#each senders as item (item.definition.schema.id)}<option
								value={item.definition.schema.id}>{item.definition.schema.title}</option
							>{/each}</select
					></label
				>
				{#if consumer && senders.length === 0}<p>
						This consumer has no channels with available test senders.
					</p>{/if}
				<ConfigurationFields
					{fields}
					{values}
					choices={fieldChoices}
					disabled={sending}
					onChange={changeValues}
				/>
			{/if}
			{#if error}<p role="alert" class="error">{error}</p>{/if}
			{#if success}<p role="status">{success}</p>{/if}
		</div>
		<footer>
			<button type="button" disabled={sending} onclick={onClose}>Close</button><button
				type="submit"
				class="primary"
				disabled={loading || sending || !sender || !consumer}
				>{sending ? 'Sending…' : 'Send'}</button
			>
		</footer>
	</form>
</dialog>

<style>
	dialog {
		width: min(620px, calc(100vw - 32px));
		max-height: calc(100vh - 32px);
		padding: 0;
		color: var(--text);
		background: var(--surface);
		border: 1px solid var(--border);
		border-radius: 8px;
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 60%);
	}
	form {
		display: flex;
		flex-direction: column;
		max-height: calc(100vh - 34px);
	}
	header,
	footer {
		padding: 16px 20px;
		flex: none;
	}
	h2 {
		margin: 0;
		font-size: 18px;
	}
	header p {
		margin-bottom: 0;
		color: var(--muted);
	}
	.fields {
		padding: 0 20px;
		overflow: auto;
		display: grid;
		gap: 14px;
		min-height: 0;
	}
	label {
		display: grid;
		gap: 6px;
	}
	select {
		width: 100%;
		min-width: 0;
	}
	select,
	button {
		font: inherit;
		color: inherit;
		background: var(--field);
		border: 1px solid var(--border);
		border-radius: 6px;
		min-height: 34px;
		padding: 6px 10px;
	}
	footer {
		display: flex;
		justify-content: flex-end;
		gap: 8px;
	}
	.primary {
		background: var(--blood);
	}
	button:disabled,
	select:disabled {
		opacity: 0.5;
	}
	.error {
		color: var(--gold);
	}
</style>
